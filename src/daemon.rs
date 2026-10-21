//! The root daemon (`wnvctl daemon`, run by systemd).
//!
//! * listens on a Unix socket; status is open to every local user, writes
//!   only to root and to members of the allowed groups (default `wheel`,
//!   `wnvctl`), checked with `SO_PEERCRED` — no password prompts;
//! * one worker thread per GPU runs the fan curve (one temperature read per
//!   tick, a write only when the speed must change) and a watchdog that
//!   re-applies the profile if the driver dropped it (resume, driver reload);
//! * on exit the fans go back to automatic control.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::curve::{ControllerParams, FanController};
use crate::gpu::{Gpu, GpuError};
use crate::ipc::{self, GpuStatus, Reply, Request};
use crate::profile::{self, FanMode, Op, Profile, Tunables};

macro_rules! log {
    ($($t:tt)*) => { eprintln!("wnvctl: {}", format_args!($($t)*)) };
}

/// `/etc/wnvctl/daemon.toml` (`$WNVCTL_DAEMON_CONFIG`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DaemonConfig {
    /// Groups whose members may change GPU settings (root always may).
    pub allowed_groups: Vec<String>,
    /// Seconds between checks that the active profile is still in place
    /// (0 disables the watchdog).
    pub watchdog_secs: u64,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            allowed_groups: vec!["wheel".into(), "wnvctl".into()],
            watchdog_secs: 10,
        }
    }
}

fn env_path(var: &str, default: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default))
}

fn config_path() -> PathBuf {
    env_path("WNVCTL_DAEMON_CONFIG", "/etc/wnvctl/daemon.toml")
}

/// Profiles applied at start, by GPU UUID.
fn startup_path() -> PathBuf {
    env_path("WNVCTL_STATE_DIR", "/var/lib/wnvctl").join("startup.toml")
}

/// Driver values seen before we changed anything, kept for this boot only
/// (they live next to the socket, on a tmpfs).
fn stock_path() -> PathBuf {
    ipc::socket_path()
        .parent()
        .unwrap_or(Path::new("/run/wnvctl"))
        .join("stock.toml")
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StartupFile {
    #[serde(default)]
    gpu: BTreeMap<String, Profile>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct StockValues {
    target_temp: Option<u32>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StockFile {
    #[serde(default)]
    gpu: BTreeMap<String, StockValues>,
}

/// Everything the daemon knows about one GPU.
struct Slot {
    gpu: Gpu,
    stock: StockValues,
    active: Option<Profile>,
    /// What we last wrote (for the watchdog).
    applied: Option<Tunables>,
    /// Locked clock ceiling we set (NVML cannot read it back).
    locked_max: Option<u32>,
    fan_mode: FanMode,
    fan_params: Option<(FanController, Duration)>,
    fan_speed: Option<u32>,
    /// We put the fans in manual mode.
    fans_ours: bool,
    fan_errors: u32,
    last_error: Option<String>,
}

type Shared = Arc<Vec<Arc<Mutex<Slot>>>>;

fn lock(slot: &Mutex<Slot>) -> MutexGuard<'_, Slot> {
    slot.lock().unwrap_or_else(|e| e.into_inner())
}

impl Slot {
    fn status(&self) -> GpuStatus {
        GpuStatus {
            uuid: self.gpu.info.uuid.clone(),
            index: self.gpu.info.index,
            name: self.gpu.info.name.clone(),
            active: self.active.clone(),
            startup: load_startup()
                .gpu
                .get(self.gpu.uuid())
                .map(|p| p.name.clone()),
            fan_mode: self.fan_mode,
            fan_speed: self.fan_speed,
            external_fan_control: !self.fans_ours && self.gpu.fans_manual(),
            last_error: self.last_error.clone(),
        }
    }

    /// Applies `profile`, writing only what differs from the current state.
    fn apply(&mut self, profile: &Profile) -> Result<Vec<String>, String> {
        let limits = self.gpu.info.limits.clone();
        let desired = profile
            .resolve(&limits, self.stock.target_temp)
            .map_err(|e| e.to_string())?;
        let current = self
            .gpu
            .tunables(self.locked_max)
            .map_err(|e| e.to_string())?;
        let mut changes = Vec::new();
        for op in profile::plan(&current, &desired, limits.core_clock.0) {
            self.write(op).map_err(|e| {
                let msg = format!("{e} (after: {})", changes.join(", "));
                self.last_error = Some(msg.clone());
                msg
            })?;
            changes.push(describe(op));
        }
        changes.extend(self.set_fans(profile)?);
        self.active = Some(profile.clone());
        self.applied = Some(desired);
        self.last_error = None;
        Ok(changes)
    }

    fn write(&mut self, op: Op) -> Result<(), GpuError> {
        self.gpu.apply(op)?;
        match op {
            Op::LockCoreClock { max, .. } => self.locked_max = Some(max),
            Op::UnlockCoreClock => self.locked_max = None,
            _ => {}
        }
        Ok(())
    }

    fn set_fans(&mut self, profile: &Profile) -> Result<Vec<String>, String> {
        let f = &profile.fan;
        let (min, max) = self.gpu.info.limits.fan_speed;
        let mut changes = Vec::new();
        self.fan_params = None;
        match f.mode {
            FanMode::Auto => {
                // Only hand back fans we took: another tool may own them.
                if self.fans_ours {
                    self.gpu.set_fans_auto().map_err(|e| e.to_string())?;
                    changes.push("fans → automatic".into());
                }
                self.fans_ours = false;
                self.fan_speed = None;
            }
            FanMode::Fixed => {
                let speed = f.fixed_percent.clamp(min, max);
                if self.gpu.info.fans > 0 {
                    self.gpu.set_fan_speed(speed).map_err(|e| e.to_string())?;
                    self.fans_ours = true;
                    changes.push(format!("fans → {speed} %"));
                }
                self.fan_speed = Some(speed);
            }
            FanMode::Curve => {
                let params = ControllerParams {
                    hysteresis: f.hysteresis_c.max(0.0),
                    spindown_delay: Duration::from_millis(f.spindown_ms),
                    min_speed: min,
                    max_speed: max,
                };
                let mut ctl = FanController::new(f.curve.clone(), params);
                if self.gpu.info.fans > 0 {
                    let temp = self.gpu.temperature().map_err(|e| e.to_string())?;
                    if let Some(s) = ctl.update(temp as f32, Instant::now()) {
                        self.gpu.set_fan_speed(s).map_err(|e| e.to_string())?;
                        self.fan_speed = Some(s);
                    }
                    self.fans_ours = true;
                    changes.push(format!("fan curve ({} points)", f.curve.points().len()));
                }
                self.fan_params = Some((ctl, Duration::from_millis(f.interval_ms)));
            }
        }
        self.fan_mode = f.mode;
        self.fan_errors = 0;
        Ok(changes)
    }

    fn reset(&mut self) -> Result<Vec<String>, String> {
        let mut changes = self.apply(&Profile::default())?;
        // An explicit reset also takes the fans back from other tools.
        if self.gpu.info.fans > 0 && self.gpu.fans_manual() {
            self.gpu.set_fans_auto().map_err(|e| e.to_string())?;
            changes.push("fans → automatic".into());
        }
        self.active = None;
        self.applied = None;
        Ok(changes)
    }

    /// One fan-curve step. Returns how long to wait before the next one.
    fn fan_tick(&mut self, now: Instant) -> Duration {
        let Some((ctl, interval)) = self.fan_params.as_mut() else {
            return Duration::from_secs(1);
        };
        let interval = *interval;
        let temp = match self.gpu.temperature() {
            Ok(t) => t as f32,
            Err(e) => {
                self.fan_failure(&e.to_string());
                return interval;
            }
        };
        if let Some(speed) = ctl.update(temp, now) {
            match self.gpu.set_fan_speed(speed) {
                Ok(()) => {
                    self.fan_speed = Some(speed);
                    self.fan_errors = 0;
                }
                Err(e) => self.fan_failure(&e.to_string()),
            }
        }
        interval
    }

    /// After repeated failures the curve is abandoned and the fans go back to
    /// the driver: an unattended manual fan is the dangerous case.
    fn fan_failure(&mut self, err: &str) {
        self.fan_errors += 1;
        log!("GPU {}: fan control error: {err}", self.gpu.info.index);
        if self.fan_errors >= 5 {
            self.fan_params = None;
            self.fan_mode = FanMode::Auto;
            let _ = self.gpu.set_fans_auto();
            self.fans_ours = false;
            self.last_error = Some(format!("fan curve stopped: {err}"));
        }
    }

    /// Re-asserts the fan speed in case the driver or a resume reset it.
    fn fan_refresh(&mut self) {
        match self.fan_mode {
            FanMode::Curve => {
                if let Some((ctl, _)) = self.fan_params.as_mut() {
                    ctl.invalidate();
                }
            }
            FanMode::Fixed => {
                if let Some(s) = self.fan_speed
                    && let Err(e) = self.gpu.set_fan_speed(s)
                {
                    self.fan_failure(&e.to_string());
                }
            }
            FanMode::Auto => {}
        }
    }

    /// Re-applies the active profile if the GPU no longer matches it.
    fn watchdog(&mut self) {
        let (Some(applied), Some(profile)) = (self.applied, self.active.clone()) else {
            return;
        };
        let Ok(current) = self.gpu.tunables(self.locked_max) else {
            return;
        };
        if profile::plan(&current, &applied, 0).is_empty() {
            return;
        }
        log!(
            "GPU {}: settings were reset outside wnvctl, re-applying \"{}\"",
            self.gpu.info.index,
            profile.name
        );
        // Locked clocks cannot be read back: assume they were lost too.
        self.locked_max = None;
        if let Err(e) = self.apply(&profile) {
            log!("GPU {}: re-apply failed: {e}", self.gpu.info.index);
        }
    }

    fn release(&mut self) {
        if self.fans_ours {
            match self.gpu.set_fans_auto() {
                Ok(()) => log!("GPU {}: fans back to automatic", self.gpu.info.index),
                Err(e) => log!("GPU {}: could not reset fans: {e}", self.gpu.info.index),
            }
            self.fans_ours = false;
        }
    }
}

fn describe(op: Op) -> String {
    match op {
        Op::MemOffset(v) => format!("memory offset → {v:+} MHz"),
        Op::CoreOffset(v) => format!("core offset → {v:+} MHz"),
        Op::PowerLimit(mw) => format!("power limit → {} W", mw / 1000),
        Op::LockCoreClock { max, .. } => format!("core clock limit → {max} MHz"),
        Op::UnlockCoreClock => "core clock limit removed".into(),
        Op::TargetTemp(t) => format!("target temperature → {t} °C"),
    }
}

fn load_startup() -> StartupFile {
    profile::load_toml(&startup_path()).unwrap_or_default()
}

fn find_slot<'a>(slots: &'a Shared, id: &str) -> Option<&'a Arc<Mutex<Slot>>> {
    let id = id.trim();
    slots.iter().find(|s| {
        let s = lock(s);
        s.gpu.info.uuid.eq_ignore_ascii_case(id)
            || s.gpu.info.index.to_string() == id
            || s.gpu.info.pci_bus_id.eq_ignore_ascii_case(id)
    })
}

fn handle(slots: &Shared, req: Request) -> Reply {
    let done = |r: Result<Vec<String>, String>| match r {
        Ok(changes) => Reply::Done { changes },
        Err(message) => Reply::Error { message },
    };
    let missing = |gpu: &str| Reply::Error {
        message: GpuError::NotFound(gpu.into()).to_string(),
    };
    match req {
        Request::Status => Reply::Status {
            version: ipc::PROTOCOL_VERSION,
            daemon: env!("CARGO_PKG_VERSION").into(),
            gpus: slots.iter().map(|s| lock(s).status()).collect(),
        },
        Request::Apply { gpu, profile } => match find_slot(slots, &gpu) {
            Some(s) => {
                let r = lock(s).apply(&profile);
                if let Ok(c) = &r {
                    log!(
                        "applied \"{}\" to GPU {gpu}: {}",
                        profile.name,
                        c.join(", ")
                    );
                }
                done(r)
            }
            None => missing(&gpu),
        },
        Request::Reset { gpu } => match find_slot(slots, &gpu) {
            Some(s) => done(lock(s).reset()),
            None => missing(&gpu),
        },
        Request::SetStartup { gpu, profile } => match find_slot(slots, &gpu) {
            Some(s) => {
                let uuid = lock(s).gpu.info.uuid.clone();
                let mut file = load_startup();
                let msg = match &profile {
                    Some(p) => {
                        let check = {
                            let s = lock(s);
                            p.resolve(&s.gpu.info.limits, s.stock.target_temp)
                        };
                        if let Err(e) = check {
                            return Reply::Error {
                                message: e.to_string(),
                            };
                        }
                        file.gpu.insert(uuid, p.clone());
                        format!("\"{}\" will be applied at boot", p.name)
                    }
                    None => {
                        file.gpu.remove(&uuid);
                        "no profile at boot".into()
                    }
                };
                done(
                    profile::save_toml(&startup_path(), &file)
                        .map(|()| vec![msg])
                        .map_err(|e| e.to_string()),
                )
            }
            None => missing(&gpu),
        },
    }
}

// ---- authorization -----------------------------------------------------------

fn peer_allowed(stream: &UnixStream, cfg: &DaemonConfig) -> bool {
    use nix::sys::socket::{getsockopt, sockopt::PeerCredentials};
    use nix::unistd::{Gid, Group, Uid, User, getgrouplist};
    let Ok(cred) = getsockopt(stream, PeerCredentials) else {
        return false;
    };
    if cred.uid() == 0 {
        return true;
    }
    let Ok(Some(user)) = User::from_uid(Uid::from_raw(cred.uid())) else {
        return false;
    };
    let Ok(cname) = std::ffi::CString::new(user.name.clone()) else {
        return false;
    };
    let groups = getgrouplist(&cname, Gid::from_raw(cred.gid())).unwrap_or_default();
    cfg.allowed_groups
        .iter()
        .any(|name| matches!(Group::from_name(name), Ok(Some(g)) if groups.contains(&g.gid)))
}

fn serve(slots: Shared, cfg: Arc<DaemonConfig>, stream: UnixStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let allowed = peer_allowed(&stream, &cfg);
    let mut reader = BufReader::new(&stream);
    let mut line = String::new();
    while matches!(reader.read_line(&mut line), Ok(n) if n > 0) {
        let reply = match serde_json::from_str::<Request>(&line) {
            Err(e) => Reply::Error {
                message: format!("bad request: {e}"),
            },
            Ok(req) if !req.is_read_only() && !allowed => Reply::Error {
                message: format!(
                    "permission denied: join one of these groups to change GPU settings: {}",
                    cfg.allowed_groups.join(", ")
                ),
            },
            Ok(req) => handle(&slots, req),
        };
        line.clear();
        let Ok(mut out) = serde_json::to_string(&reply) else {
            return;
        };
        out.push('\n');
        if (&stream).write_all(out.as_bytes()).is_err() {
            return;
        }
    }
}

// ---- main --------------------------------------------------------------------

fn worker(slot: Arc<Mutex<Slot>>, watchdog: Duration) {
    let mut last_watchdog = Instant::now();
    let mut last_refresh = Instant::now();
    loop {
        let now = Instant::now();
        let wait = {
            let mut s = lock(&slot);
            if !watchdog.is_zero() && now.duration_since(last_watchdog) >= watchdog {
                s.watchdog();
                last_watchdog = now;
            }
            if now.duration_since(last_refresh) >= Duration::from_secs(30) {
                s.fan_refresh();
                last_refresh = now;
            }
            s.fan_tick(now)
        };
        std::thread::sleep(wait);
    }
}

fn load_stock(gpus: &[Gpu]) -> BTreeMap<String, StockValues> {
    let path = stock_path();
    let mut file: StockFile = profile::load_toml(&path).unwrap_or_default();
    let mut changed = false;
    for g in gpus {
        file.gpu.entry(g.uuid().to_string()).or_insert_with(|| {
            changed = true;
            StockValues {
                target_temp: g.target_temp(),
            }
        });
    }
    if changed && let Err(e) = profile::save_toml(&path, &file) {
        log!("cannot save {}: {e}", path.display());
    }
    file.gpu
}

pub fn run() -> Result<(), String> {
    if !nix::unistd::Uid::effective().is_root() {
        return Err("the daemon must run as root (it is started by systemd)".into());
    }
    let cfg: DaemonConfig = match profile::load_toml(&config_path()) {
        Ok(c) => c,
        Err(profile::StoreError::Io(_, e)) if e.kind() == std::io::ErrorKind::NotFound => {
            DaemonConfig::default()
        }
        Err(e) => return Err(e.to_string()),
    };
    let gpus = Gpu::all().map_err(|e| e.to_string())?;
    if gpus.is_empty() {
        return Err("no NVIDIA GPU found".into());
    }
    let socket = ipc::socket_path();
    if let Some(dir) = socket.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let mut stock = load_stock(&gpus);
    let startup = load_startup();
    let slots: Shared = Arc::new(
        gpus.into_iter()
            .map(|gpu| {
                log!(
                    "GPU {}: {} ({}), driver {}",
                    gpu.info.index,
                    gpu.info.name,
                    gpu.info.uuid,
                    gpu.info.driver
                );
                let mut slot = Slot {
                    stock: stock.remove(gpu.uuid()).unwrap_or_default(),
                    gpu,
                    active: None,
                    applied: None,
                    locked_max: None,
                    fan_mode: FanMode::Auto,
                    fan_params: None,
                    fan_speed: None,
                    fans_ours: false,
                    fan_errors: 0,
                    last_error: None,
                };
                if let Some(p) = startup.gpu.get(slot.gpu.uuid()) {
                    match slot.apply(p) {
                        Ok(c) => log!("startup profile \"{}\": {}", p.name, c.join(", ")),
                        Err(e) => log!("startup profile \"{}\" failed: {e}", p.name),
                    }
                }
                Arc::new(Mutex::new(slot))
            })
            .collect(),
    );

    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).map_err(|e| format!("{}: {e}", socket.display()))?;
    // Anyone may connect; writes are checked per peer.
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o666))
        .map_err(|e| e.to_string())?;

    {
        let slots = slots.clone();
        let socket = socket.clone();
        ctrlc::set_handler(move || {
            log!("stopping");
            for s in slots.iter() {
                lock(s).release();
            }
            let _ = std::fs::remove_file(&socket);
            std::process::exit(0);
        })
        .map_err(|e| e.to_string())?;
    }

    let watchdog = Duration::from_secs(cfg.watchdog_secs);
    for s in slots.iter() {
        let s = s.clone();
        std::thread::Builder::new()
            .name("wnvctl-gpu".into())
            .spawn(move || worker(s, watchdog))
            .map_err(|e| e.to_string())?;
    }
    log!("listening on {}", socket.display());
    let cfg = Arc::new(cfg);
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let (slots, cfg) = (slots.clone(), cfg.clone());
                let _ = std::thread::Builder::new()
                    .name("wnvctl-client".into())
                    .spawn(move || serve(slots, cfg, stream));
            }
            Err(e) => log!("accept: {e}"),
        }
    }
    Ok(())
}
