//! Background data: a thread samples the GPUs (directly through NVML, no
//! daemon needed) and asks the daemon for its state; daemon commands run on
//! short-lived threads. The UI only reads the shared snapshot, so it never
//! blocks on the hardware.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;

use crate::gpu::Gpu;
use crate::ipc::{self, GpuStatus, Request};
use crate::profile::{FanMode, Profile};
use crate::types::{GpuInfo, Stats};

#[derive(Default)]
pub struct Monitor {
    pub gpus: Vec<GpuInfo>,
    /// Per GPU, oldest first.
    pub history: Vec<VecDeque<Stats>>,
    /// `Err` = daemon not reachable.
    pub daemon: Option<Result<Vec<GpuStatus>, String>>,
    pub error: Option<String>,
}

impl Monitor {
    pub fn latest(&self, gpu: usize) -> Option<&Stats> {
        self.history.get(gpu).and_then(|h| h.back())
    }
}

pub struct Data {
    pub shared: Arc<Mutex<Monitor>>,
    stop: Arc<AtomicBool>,
    refresh_ms: Arc<AtomicU64>,
    demo: Option<Arc<Mutex<DemoDaemon>>>,
    results: (Sender<CmdResult>, Receiver<CmdResult>),
    ctx: egui::Context,
}

/// Outcome of a daemon command.
pub struct CmdResult {
    pub request: Request,
    pub result: Result<Vec<String>, String>,
}

impl Data {
    pub fn start(ctx: &egui::Context, refresh_ms: u64, history: usize, demo: bool) -> Self {
        let shared = Arc::new(Mutex::new(Monitor::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let refresh = Arc::new(AtomicU64::new(refresh_ms));
        let demo = demo.then(|| Arc::new(Mutex::new(DemoDaemon::default())));
        {
            let (shared, stop, refresh, ctx, demo) = (
                shared.clone(),
                stop.clone(),
                refresh.clone(),
                ctx.clone(),
                demo.clone(),
            );
            std::thread::Builder::new()
                .name("wnvctl-sampler".into())
                .spawn(move || sampler(shared, stop, refresh, history, ctx, demo))
                .ok();
        }
        Self {
            shared,
            stop,
            refresh_ms: refresh,
            demo,
            results: channel(),
            ctx: ctx.clone(),
        }
    }

    pub fn set_refresh(&self, ms: u64) {
        self.refresh_ms.store(ms, Ordering::Relaxed);
    }

    pub fn is_demo(&self) -> bool {
        self.demo.is_some()
    }

    /// Sends a request to the daemon without blocking the UI.
    pub fn send(&self, request: Request) {
        let tx = self.results.0.clone();
        let ctx = self.ctx.clone();
        let demo = self.demo.clone();
        let shared = self.shared.clone();
        std::thread::spawn(move || {
            let result = match &demo {
                Some(d) => Ok(d.lock().map(|mut d| d.handle(&request)).unwrap_or_default()),
                None => ipc::call_done(&request).map_err(|e| e.to_string()),
            };
            // Refresh the daemon state right away.
            let status = match &demo {
                Some(d) => d.lock().ok().map(|d| Ok(d.status())),
                None => Some(ipc::status().map_err(|e| e.to_string())),
            };
            if let (Some(st), Ok(mut m)) = (status, shared.lock()) {
                m.daemon = Some(st);
            }
            let _ = tx.send(CmdResult { request, result });
            ctx.request_repaint();
        });
    }

    pub fn poll(&self) -> Option<CmdResult> {
        self.results.1.try_recv().ok()
    }
}

impl Drop for Data {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn sampler(
    shared: Arc<Mutex<Monitor>>,
    stop: Arc<AtomicBool>,
    refresh: Arc<AtomicU64>,
    history: usize,
    ctx: egui::Context,
    demo: Option<Arc<Mutex<DemoDaemon>>>,
) {
    let mut gpus = if demo.is_some() {
        Vec::new()
    } else {
        match Gpu::all() {
            Ok(g) => g,
            Err(e) => {
                if let Ok(mut m) = shared.lock() {
                    m.error = Some(e.to_string());
                }
                ctx.request_repaint();
                return;
            }
        }
    };
    if let Ok(mut m) = shared.lock() {
        m.gpus = if demo.is_some() {
            vec![GpuInfo::demo()]
        } else {
            gpus.iter().map(|g| g.info.clone()).collect()
        };
        m.history = vec![VecDeque::with_capacity(history); m.gpus.len()];
    }
    let start = Instant::now();
    let mut last_daemon: Option<Instant> = None;
    // Pre-fill the demo charts so screenshots show history.
    if let Some(d) = &demo
        && let Ok(mut m) = shared.lock()
    {
        for i in 0..history {
            let t = i as f64 - history as f64;
            let s = demo_stats(t, &d.lock().map(|d| d.active.clone()).unwrap_or_default());
            m.history[0].push_back(s);
        }
    }
    while !stop.load(Ordering::Relaxed) {
        let stats: Vec<Stats> = match &demo {
            Some(d) => {
                let active = d.lock().map(|d| d.active.clone()).unwrap_or_default();
                vec![demo_stats(start.elapsed().as_secs_f64(), &active)]
            }
            None => gpus.iter_mut().map(Gpu::stats).collect(),
        };
        let daemon_due = last_daemon.is_none_or(|t| t.elapsed() >= Duration::from_secs(3));
        let daemon = daemon_due.then(|| match &demo {
            Some(d) => Ok(d.lock().map(|d| d.status()).unwrap_or_default()),
            None => ipc::status().map_err(|e| e.to_string()),
        });
        if daemon_due {
            last_daemon = Some(Instant::now());
        }
        if let Ok(mut m) = shared.lock() {
            for (h, s) in m.history.iter_mut().zip(stats) {
                if h.len() >= history {
                    h.pop_front();
                }
                h.push_back(s);
            }
            if let Some(d) = daemon {
                m.daemon = Some(d);
            }
        }
        ctx.request_repaint();
        std::thread::sleep(Duration::from_millis(
            refresh.load(Ordering::Relaxed).clamp(250, 5000),
        ));
    }
}

// ---- demo mode -------------------------------------------------------------------

/// Stands in for the daemon in demo mode (`WNVCTL_DEMO=1`).
#[derive(Default)]
pub struct DemoDaemon {
    active: Profile,
    startup: Option<String>,
}

impl DemoDaemon {
    fn status(&self) -> Vec<GpuStatus> {
        let info = GpuInfo::demo();
        vec![GpuStatus {
            uuid: info.uuid,
            index: 0,
            name: info.name,
            active: Some(self.active.clone()),
            startup: self.startup.clone(),
            fan_mode: self.active.fan.mode,
            fan_speed: None,
            external_fan_control: false,
            last_error: None,
        }]
    }

    fn handle(&mut self, req: &Request) -> Vec<String> {
        match req {
            Request::Apply { profile, .. } => {
                self.active = profile.clone();
                vec![format!("demo: applied \"{}\"", profile.name)]
            }
            Request::Reset { .. } => {
                self.active = Profile::default();
                vec!["demo: reset".into()]
            }
            Request::SetStartup { profile, .. } => {
                self.startup = profile.as_ref().map(|p| p.name.clone());
                vec!["demo: boot profile changed".into()]
            }
            Request::Status => vec![],
        }
    }
}

/// Plausible readings for a gaming load that follows the active profile.
fn demo_stats(t: f64, p: &Profile) -> Stats {
    let info = GpuInfo::demo();
    let wave = |period: f64, phase: f64| ((t / period + phase).sin() * 0.5 + 0.5) as f32;
    let load = 0.75 + 0.25 * wave(23.0, 0.0);
    let limit = p.power_limit_w.unwrap_or(115) as f32;
    let power = (limit * (0.82 + 0.16 * load)).min(limit);
    let temp = 52.0 + 18.0 * load + 3.0 * wave(7.0, 1.0);
    let max_clock = p.core_clock_max_mhz.unwrap_or(2775) as f32;
    let core = (2550.0 + p.core_offset_mhz as f32 * 0.6 + 120.0 * wave(5.0, 2.0)).min(max_clock);
    let fan = match p.fan.mode {
        FanMode::Auto => 30.0 + (temp - 40.0).max(0.0) * 1.4,
        FanMode::Fixed => p.fan.fixed_percent as f32,
        FanMode::Curve => p.fan.curve.speed_at(temp),
    }
    .clamp(30.0, 100.0);
    Stats {
        temp: Some(temp as u32),
        core_clock: Some(core as u32),
        mem_clock: Some(8501 + p.mem_offset_mhz.max(0) as u32 / 2),
        video_clock: Some(1890),
        power_mw: Some((power * 1000.0) as u32),
        power_limit_mw: Some((limit * 1000.0) as u32),
        util_gpu: Some((load * 100.0) as u32),
        util_mem: Some((load * 55.0) as u32),
        vram_used: Some(
            (((5.2 + wave(41.0, 0.5)) * (1u64 << 30) as f32) as u64).min(info.vram_total),
        ),
        fan_speed: vec![fan as u32],
        fan_rpm: vec![(fan * 32.0) as u32],
        pstate: Some(0),
        core_offset: Some(p.core_offset_mhz),
        mem_offset: Some(p.mem_offset_mhz),
        target_temp: Some(p.target_temp_c.unwrap_or(83)),
        throttle: if power >= limit * 0.97 {
            vec!["power".into()]
        } else {
            vec![]
        },
    }
}
