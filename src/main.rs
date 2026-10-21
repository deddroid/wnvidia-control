//! `wnvctl`: command-line interface and root daemon.

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};

use wnvctl::gpu::Gpu;
use wnvctl::ipc::{self, Request};
use wnvctl::profile::{FanMode, Profile, ProfileStore, load_toml};
use wnvctl::types::{GpuInfo, Stats};

#[derive(Parser)]
#[command(
    version,
    about = "Overclocking, undervolting, power limit and fan curves for NVIDIA GPUs"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List the GPUs.
    List,
    /// Show sensors, current settings and what the daemon is doing.
    Status {
        #[arg(short, long, default_value = "0")]
        gpu: String,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Print sensors continuously.
    Watch {
        #[arg(short, long, default_value = "0")]
        gpu: String,
        /// Interval in milliseconds.
        #[arg(short, long, default_value_t = 1000)]
        interval: u64,
    },
    /// Apply a saved profile (by name) or a profile file.
    Apply {
        profile: String,
        #[arg(short, long, default_value = "0")]
        gpu: String,
    },
    /// Change single settings, keeping the others as they are.
    Set {
        #[arg(short, long, default_value = "0")]
        gpu: String,
        /// Power limit in watts, or "default".
        #[arg(long)]
        power: Option<String>,
        /// Core clock offset in MHz.
        #[arg(long, allow_hyphen_values = true)]
        core: Option<i32>,
        /// Memory clock offset in MHz.
        #[arg(long, allow_hyphen_values = true)]
        mem: Option<i32>,
        /// Highest core clock in MHz, or "none" (undervolt with --core).
        #[arg(long)]
        clock_max: Option<String>,
        /// Target temperature in °C, or "default".
        #[arg(long)]
        target_temp: Option<String>,
        /// "auto", a fixed speed in percent, or "curve" (the profile's curve).
        #[arg(long)]
        fan: Option<String>,
    },
    /// Back to driver defaults (fans automatic).
    Reset {
        #[arg(short, long, default_value = "0")]
        gpu: String,
    },
    /// List saved profiles.
    Profiles,
    /// Choose the profile applied at boot.
    Startup {
        /// Profile name or file; omit with --clear.
        profile: Option<String>,
        #[arg(short, long, default_value = "0")]
        gpu: String,
        /// Apply nothing at boot.
        #[arg(long, conflicts_with = "profile")]
        clear: bool,
    },
    /// Run the root daemon (used by the systemd service).
    Daemon,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli.cmd) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("wnvctl: {e}");
            ExitCode::FAILURE
        }
    }
}

type Res = Result<(), Box<dyn std::error::Error>>;

fn run(cmd: Cmd) -> Res {
    match cmd {
        Cmd::Daemon => wnvctl::daemon::run().map_err(Into::into),
        Cmd::List => {
            for g in Gpu::all()? {
                let i = &g.info;
                println!("{}  {}  {}  {}", i.index, i.name, i.uuid, i.pci_bus_id);
            }
            Ok(())
        }
        Cmd::Status { gpu, json } => status(&gpu, json),
        Cmd::Watch { gpu, interval } => watch(&gpu, interval),
        Cmd::Apply { profile, gpu } => {
            let p = find_profile(&profile)?;
            send(&Request::Apply { gpu, profile: p })
        }
        Cmd::Set {
            gpu,
            power,
            core,
            mem,
            clock_max,
            target_temp,
            fan,
        } => {
            let mut p = active_profile(&gpu);
            p.name = "Custom".into();
            if let Some(v) = power {
                p.power_limit_w = parse_opt(&v, "default")?;
            }
            if let Some(v) = core {
                p.core_offset_mhz = v;
            }
            if let Some(v) = mem {
                p.mem_offset_mhz = v;
            }
            if let Some(v) = clock_max {
                p.core_clock_max_mhz = parse_opt(&v, "none")?;
            }
            if let Some(v) = target_temp {
                p.target_temp_c = parse_opt(&v, "default")?;
            }
            if let Some(v) = fan {
                match v.as_str() {
                    "auto" => p.fan.mode = FanMode::Auto,
                    "curve" => p.fan.mode = FanMode::Curve,
                    n => {
                        p.fan.mode = FanMode::Fixed;
                        p.fan.fixed_percent = n.trim_end_matches('%').parse()?;
                    }
                }
            }
            send(&Request::Apply { gpu, profile: p })
        }
        Cmd::Reset { gpu } => send(&Request::Reset { gpu }),
        Cmd::Profiles => {
            let store = ProfileStore::user();
            let (list, errors) = store.list();
            for p in &list {
                println!("{:<24} {}", p.name, summary(p));
            }
            for e in errors {
                eprintln!("skipped {e}");
            }
            if list.is_empty() {
                println!("no profiles in {}", store.dir().display());
            }
            Ok(())
        }
        Cmd::Startup {
            profile,
            gpu,
            clear,
        } => {
            let profile = match (profile, clear) {
                (Some(p), false) => Some(find_profile(&p)?),
                (None, true) => None,
                _ => return Err("give a profile name or --clear".into()),
            };
            send(&Request::SetStartup { gpu, profile })
        }
    }
}

fn parse_opt(v: &str, none_word: &str) -> Result<Option<u32>, std::num::ParseIntError> {
    if v.eq_ignore_ascii_case(none_word) {
        Ok(None)
    } else {
        v.parse().map(Some)
    }
}

fn find_profile(name: &str) -> Result<Profile, Box<dyn std::error::Error>> {
    let path = Path::new(name);
    if path.extension().is_some_and(|e| e == "toml") && path.exists() {
        return Ok(load_toml(path)?);
    }
    if name.eq_ignore_ascii_case("stock") {
        return Ok(Profile::default());
    }
    ProfileStore::user()
        .find(name)
        .ok_or_else(|| format!("no profile named \"{name}\" (see: wnvctl profiles)").into())
}

/// The profile the daemon last applied, or stock.
fn active_profile(gpu: &str) -> Profile {
    ipc::status()
        .ok()
        .and_then(|list| {
            list.into_iter()
                .find(|s| s.uuid == gpu || s.index.to_string() == gpu)
                .and_then(|s| s.active)
        })
        .unwrap_or_default()
}

fn send(req: &Request) -> Res {
    let changes = ipc::call_done(req)?;
    if changes.is_empty() {
        println!("nothing to change");
    }
    for c in changes {
        println!("{c}");
    }
    Ok(())
}

fn open(gpu: &str) -> Result<Gpu, Box<dyn std::error::Error>> {
    let all = Gpu::all()?;
    all.into_iter()
        .find(|g| {
            g.info.index.to_string() == gpu
                || g.info.uuid.eq_ignore_ascii_case(gpu)
                || g.info.pci_bus_id.eq_ignore_ascii_case(gpu)
        })
        .ok_or_else(|| format!("no GPU {gpu} (see: wnvctl list)").into())
}

pub fn summary(p: &Profile) -> String {
    let mut parts = Vec::new();
    parts.push(match p.power_limit_w {
        Some(w) => format!("{w} W"),
        None => "default power".into(),
    });
    if p.core_offset_mhz != 0 || p.mem_offset_mhz != 0 {
        parts.push(format!(
            "core {:+} / mem {:+} MHz",
            p.core_offset_mhz, p.mem_offset_mhz
        ));
    }
    if let Some(c) = p.core_clock_max_mhz {
        parts.push(format!("max {c} MHz"));
    }
    if let Some(t) = p.target_temp_c {
        parts.push(format!("target {t} °C"));
    }
    parts.push(match p.fan.mode {
        FanMode::Auto => "fans auto".into(),
        FanMode::Fixed => format!("fans {} %", p.fan.fixed_percent),
        FanMode::Curve => "fan curve".into(),
    });
    parts.join(" · ")
}

fn fmt<T: std::fmt::Display>(v: Option<T>, unit: &str) -> String {
    v.map_or("n/a".into(), |v| format!("{v}{unit}"))
}

fn print_stats(info: &GpuInfo, s: &Stats) {
    println!(
        "temp {}  core {}  mem {}  power {} / {}  load {}  vram {} / {} MiB  fan {}",
        fmt(s.temp, " °C"),
        fmt(s.core_clock, " MHz"),
        fmt(s.mem_clock, " MHz"),
        fmt(
            s.power_mw.map(|p| format!("{:.1}", f64::from(p) / 1000.0)),
            " W"
        ),
        fmt(s.power_limit_mw.map(|p| p / 1000), " W"),
        fmt(s.util_gpu, " %"),
        s.vram_used.map_or("?".into(), |v| (v >> 20).to_string()),
        info.vram_total >> 20,
        if s.fan_speed.is_empty() {
            "n/a".into()
        } else {
            s.fan_speed
                .iter()
                .zip(s.fan_rpm.iter().map(Some).chain(std::iter::repeat(None)))
                .map(|(p, r)| match r {
                    Some(r) => format!("{p} % ({r} rpm)"),
                    None => format!("{p} %"),
                })
                .collect::<Vec<_>>()
                .join(", ")
        },
    );
}

fn status(gpu: &str, json: bool) -> Res {
    let mut g = open(gpu)?;
    // Power is averaged over the driver's samples: take two readings.
    let _ = g.stats();
    std::thread::sleep(Duration::from_millis(200));
    let s = g.stats();
    let daemon = ipc::status();
    if json {
        let d = daemon
            .as_ref()
            .ok()
            .and_then(|l| l.iter().find(|x| x.uuid == g.info.uuid));
        println!(
            "{}",
            serde_json::json!({ "info": g.info, "stats": s, "daemon": d })
        );
        return Ok(());
    }
    let i = &g.info;
    println!(
        "{} ({}) · driver {} · {}",
        i.name, i.architecture, i.driver, i.uuid
    );
    print_stats(i, &s);
    println!(
        "offsets core {} mem {} MHz · target temp {} · p-state {} · limited by: {}",
        fmt(s.core_offset, ""),
        fmt(s.mem_offset, ""),
        fmt(s.target_temp, " °C"),
        fmt(s.pstate.map(|p| format!("P{p}")), ""),
        if s.throttle.is_empty() {
            "nothing".into()
        } else {
            s.throttle.join(", ")
        }
    );
    let l = &i.limits;
    println!(
        "limits: power {}–{} W (default {}), core offset {}..{}, mem offset {}..{}, fans {}–{} %",
        l.power_min_mw / 1000,
        l.power_max_mw / 1000,
        l.power_default_mw / 1000,
        l.core_offset.0,
        l.core_offset.1,
        l.mem_offset.0,
        l.mem_offset.1,
        l.fan_speed.0,
        l.fan_speed.1
    );
    match daemon {
        Ok(list) => {
            if let Some(d) = list.iter().find(|x| x.uuid == i.uuid) {
                println!(
                    "daemon: profile {} · fans {:?}{} · boot profile {}",
                    d.active
                        .as_ref()
                        .map_or("none".into(), |p| format!("\"{}\"", p.name)),
                    d.fan_mode,
                    d.fan_speed.map_or(String::new(), |s| format!(" ({s} %)")),
                    d.startup.as_deref().unwrap_or("none"),
                );
                if d.external_fan_control {
                    println!("warning: another program is controlling the fans");
                }
                if let Some(e) = &d.last_error {
                    println!("last error: {e}");
                }
            }
        }
        Err(e) => println!("daemon: {e}"),
    }
    Ok(())
}

fn watch(gpu: &str, interval: u64) -> Res {
    let mut g = open(gpu)?;
    loop {
        let s = g.stats();
        print_stats(&g.info, &s);
        std::thread::sleep(Duration::from_millis(interval.max(100)));
    }
}
