//! NVIDIA GPUs through NVML (`libnvidia-ml.so.1`, shipped with the driver).
//!
//! Reading works as any user; writes need root, so clients go through the
//! daemon for those.

use std::sync::OnceLock;

use nvml_wrapper::bitmasks::device::ThrottleReasons;
use nvml_wrapper::enum_wrappers::device::{
    Clock, PerformanceState, Sampling, TemperatureSensor, TemperatureThreshold,
};
use nvml_wrapper::enums::device::{GpuLockedClocksSetting, SampleValue};
use nvml_wrapper::error::NvmlError;
use nvml_wrapper::{Device, Nvml};

use crate::profile::{Op, Tunables};
use crate::types::{GpuInfo, Limits, Stats};

#[derive(Debug, thiserror::Error)]
pub enum GpuError {
    #[error("cannot load NVML (is the NVIDIA driver installed?): {0}")]
    Init(String),
    #[error("{what}: {err}")]
    Nvml {
        what: &'static str,
        #[source]
        err: NvmlError,
    },
    #[error("no GPU with id {0}")]
    NotFound(String),
}

impl GpuError {
    fn nvml(what: &'static str) -> impl FnOnce(NvmlError) -> Self {
        move |err| Self::Nvml { what, err }
    }

    /// True for "you need root" errors.
    pub fn is_permission(&self) -> bool {
        matches!(
            self,
            Self::Nvml {
                err: NvmlError::NoPermission,
                ..
            }
        )
    }
}

static NVML: OnceLock<Result<Nvml, String>> = OnceLock::new();

/// The process-wide NVML handle (initialised once, never shut down).
pub fn nvml() -> Result<&'static Nvml, GpuError> {
    NVML.get_or_init(|| Nvml::init().map_err(|e| e.to_string()))
        .as_ref()
        .map_err(|e| GpuError::Init(e.clone()))
}

/// Power draw of GPU `index` in mW, with as few NVML calls as possible (for
/// overlays that run a command every frame period). Falls back to the
/// average of the driver's power samples (about the last second) where the
/// instant reading is unsupported.
pub fn power_now(index: u32) -> Result<u32, GpuError> {
    let dev = nvml()?
        .device_by_index(index)
        .map_err(GpuError::nvml("open device"))?;
    match dev.power_usage() {
        Err(NvmlError::NotSupported) => {}
        r => return r.map_err(GpuError::nvml("power usage")),
    }
    let samples = dev
        .samples(Sampling::Power, None)
        .map_err(GpuError::nvml("power samples"))?;
    let (sum, n) = samples
        .iter()
        .fold((0u64, 0u64), |(s, n), x| match x.value {
            SampleValue::U32(v) => (s + u64::from(v), n + 1),
            _ => (s, n),
        });
    sum.checked_div(n)
        .map(|avg| avg as u32)
        .ok_or(GpuError::Nvml {
            what: "power samples",
            err: NvmlError::NotFound,
        })
}

/// How the power draw is read on this GPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PowerSource {
    Unknown,
    /// `nvmlDeviceGetPowerUsage`.
    Direct,
    /// Average of the driver's power samples since the last read; used where
    /// the instant reading is "not supported" (e.g. RTX 4060 on 6xx drivers,
    /// where `nvidia-smi` shows N/A).
    Samples,
    None,
}

pub struct Gpu {
    dev: Device<'static>,
    pub info: GpuInfo,
    power_source: PowerSource,
    last_sample_ts: Option<u64>,
    last_power: Option<u32>,
    /// Use the per-P-state offset API (newer drivers) or the old VF one.
    new_offset_api: bool,
}

impl Gpu {
    /// Every GPU NVML knows about.
    pub fn all() -> Result<Vec<Gpu>, GpuError> {
        let nvml = nvml()?;
        let n = nvml
            .device_count()
            .map_err(GpuError::nvml("device count"))?;
        (0..n).map(Gpu::open).collect()
    }

    pub fn open(index: u32) -> Result<Gpu, GpuError> {
        let nvml = nvml()?;
        let dev = nvml
            .device_by_index(index)
            .map_err(GpuError::nvml("open device"))?;
        let new_offset_api = dev
            .clock_offset(Clock::Graphics, PerformanceState::Zero)
            .is_ok();
        let info = read_info(nvml, &dev, index, new_offset_api)?;
        Ok(Gpu {
            dev,
            info,
            power_source: PowerSource::Unknown,
            last_sample_ts: None,
            last_power: None,
            new_offset_api,
        })
    }

    pub fn uuid(&self) -> &str {
        &self.info.uuid
    }

    /// GPU temperature only: the single read the fan loop needs.
    pub fn temperature(&self) -> Result<u32, GpuError> {
        self.dev
            .temperature(TemperatureSensor::Gpu)
            .map_err(GpuError::nvml("temperature"))
    }

    pub fn stats(&mut self) -> Stats {
        let d = &self.dev;
        let fans = self.info.fans;
        let pstate = d.performance_state().ok().and_then(pstate_number);
        let (core_offset, mem_offset) = self.offsets();
        Stats {
            temp: d.temperature(TemperatureSensor::Gpu).ok(),
            core_clock: d.clock_info(Clock::Graphics).ok(),
            mem_clock: d.clock_info(Clock::Memory).ok(),
            video_clock: d.clock_info(Clock::Video).ok(),
            power_mw: self.power(),
            power_limit_mw: self.dev.enforced_power_limit().ok(),
            util_gpu: self.dev.utilization_rates().ok().map(|u| u.gpu),
            util_mem: self.dev.utilization_rates().ok().map(|u| u.memory),
            vram_used: self.dev.memory_info().ok().map(|m| m.used),
            fan_speed: (0..fans)
                .filter_map(|f| self.dev.fan_speed(f).ok())
                .collect(),
            fan_rpm: (0..fans)
                .filter_map(|f| self.dev.fan_speed_rpm(f).ok())
                .collect(),
            pstate,
            core_offset,
            mem_offset,
            target_temp: self
                .dev
                .temperature_threshold(TemperatureThreshold::AcousticCurr)
                .ok(),
            throttle: self
                .dev
                .current_throttle_reasons()
                .map(throttle_names)
                .unwrap_or_default(),
        }
    }

    fn power(&mut self) -> Option<u32> {
        if matches!(
            self.power_source,
            PowerSource::Unknown | PowerSource::Direct
        ) {
            match self.dev.power_usage() {
                Ok(p) => {
                    self.power_source = PowerSource::Direct;
                    return Some(p);
                }
                Err(NvmlError::NotSupported) => self.power_source = PowerSource::Samples,
                Err(_) => return None,
            }
        }
        if self.power_source != PowerSource::Samples {
            return None;
        }
        match self.dev.samples(Sampling::Power, self.last_sample_ts) {
            Ok(samples) => {
                let mut sum = 0u64;
                let mut n = 0u64;
                for s in &samples {
                    if Some(s.timestamp) <= self.last_sample_ts {
                        continue;
                    }
                    if let SampleValue::U32(v) = s.value {
                        sum += u64::from(v);
                        n += 1;
                    }
                    self.last_sample_ts = Some(s.timestamp);
                }
                if let Some(avg) = sum.checked_div(n) {
                    self.last_power = Some(avg as u32);
                }
                self.last_power
            }
            // No new samples since the last call.
            Err(NvmlError::NotFound) => self.last_power,
            Err(NvmlError::NotSupported) => {
                self.power_source = PowerSource::None;
                None
            }
            Err(_) => self.last_power,
        }
    }

    fn offsets(&self) -> (Option<i32>, Option<i32>) {
        if self.new_offset_api {
            let get = |c| {
                self.dev
                    .clock_offset(c, PerformanceState::Zero)
                    .ok()
                    .map(|o| o.clock_offset_mhz)
            };
            (get(Clock::Graphics), get(Clock::Memory))
        } else {
            (
                self.dev.gpc_clock_vf_offset().ok(),
                self.dev.mem_clock_vf_offset().ok(),
            )
        }
    }

    pub fn target_temp(&self) -> Option<u32> {
        self.dev
            .temperature_threshold(TemperatureThreshold::AcousticCurr)
            .ok()
    }

    /// The current writable state. NVML cannot read back locked clocks, so
    /// the caller passes what it last applied.
    pub fn tunables(&self, core_clock_max: Option<u32>) -> Result<Tunables, GpuError> {
        let (core, mem) = self.offsets();
        Ok(Tunables {
            power_limit_mw: self
                .dev
                .power_management_limit()
                .map_err(GpuError::nvml("read power limit"))?,
            core_offset: core.unwrap_or(0),
            mem_offset: mem.unwrap_or(0),
            core_clock_max,
            target_temp: self.target_temp(),
        })
    }

    pub fn apply(&mut self, op: Op) -> Result<(), GpuError> {
        let d = &mut self.dev;
        match op {
            Op::MemOffset(mhz) => if self.new_offset_api {
                d.set_clock_offset(Clock::Memory, PerformanceState::Zero, mhz)
            } else {
                d.set_mem_clock_vf_offset(mhz)
            }
            .map_err(GpuError::nvml("set memory offset")),
            Op::CoreOffset(mhz) => if self.new_offset_api {
                d.set_clock_offset(Clock::Graphics, PerformanceState::Zero, mhz)
            } else {
                d.set_gpc_clock_vf_offset(mhz)
            }
            .map_err(GpuError::nvml("set core offset")),
            Op::PowerLimit(mw) => d
                .set_power_management_limit(mw)
                .map_err(GpuError::nvml("set power limit")),
            Op::LockCoreClock { min, max } => d
                .set_gpu_locked_clocks(GpuLockedClocksSetting::Numeric {
                    min_clock_mhz: min,
                    max_clock_mhz: max,
                })
                .map_err(GpuError::nvml("lock core clock")),
            Op::UnlockCoreClock => d
                .reset_gpu_locked_clocks()
                .map_err(GpuError::nvml("unlock core clock")),
            Op::TargetTemp(t) => d
                .set_temperature_threshold(TemperatureThreshold::AcousticCurr, t as i32)
                .map_err(GpuError::nvml("set target temperature")),
        }
    }

    pub fn set_fan_speed(&mut self, percent: u32) -> Result<(), GpuError> {
        for f in 0..self.info.fans {
            self.dev
                .set_fan_speed(f, percent)
                .map_err(GpuError::nvml("set fan speed"))?;
        }
        Ok(())
    }

    /// Gives the fans back to the driver's automatic control.
    pub fn set_fans_auto(&mut self) -> Result<(), GpuError> {
        for f in 0..self.info.fans {
            self.dev
                .set_default_fan_speed(f)
                .map_err(GpuError::nvml("reset fan to auto"))?;
        }
        Ok(())
    }

    /// True when some fan is under manual control (by us or another tool).
    pub fn fans_manual(&self) -> bool {
        use nvml_wrapper::enums::device::FanControlPolicy;
        (0..self.info.fans)
            .any(|f| matches!(self.dev.fan_control_policy(f), Ok(FanControlPolicy::Manual)))
    }
}

fn read_info(
    nvml: &Nvml,
    d: &Device<'static>,
    index: u32,
    new_offset_api: bool,
) -> Result<GpuInfo, GpuError> {
    let constraints = d.power_management_limit_constraints().ok();
    let power_default_mw = d
        .power_management_limit_default()
        .or_else(|_| d.power_management_limit())
        .unwrap_or(0);
    let offset_range = |c, fallback| {
        if new_offset_api {
            d.clock_offset(c, PerformanceState::Zero)
                .map(|o| (o.min_clock_offset_mhz, o.max_clock_offset_mhz))
                .unwrap_or(fallback)
        } else {
            fallback
        }
    };
    let max_core_clock = d.max_clock_info(Clock::Graphics).unwrap_or(0);
    let min_core_clock = d
        .min_max_clock_of_pstate(Clock::Graphics, PerformanceState::Zero)
        .map(|(min, _)| min)
        .unwrap_or(210);
    let target_temp = match (
        d.temperature_threshold(TemperatureThreshold::AcousticMin),
        d.temperature_threshold(TemperatureThreshold::AcousticMax),
    ) {
        (Ok(min), Ok(max)) if min < max => Some((min, max)),
        _ => None,
    };
    let thr = |t| d.temperature_threshold(t).ok();
    Ok(GpuInfo {
        index,
        uuid: d.uuid().map_err(GpuError::nvml("read UUID"))?,
        name: d.name().unwrap_or_else(|_| "NVIDIA GPU".into()),
        pci_bus_id: d.pci_info().map(|p| p.bus_id).unwrap_or_default(),
        architecture: d
            .architecture()
            .map(|a| a.to_string())
            .unwrap_or_else(|_| "?".into()),
        vbios: d.vbios_version().unwrap_or_default(),
        driver: nvml.sys_driver_version().unwrap_or_default(),
        vram_total: d.memory_info().map(|m| m.total).unwrap_or(0),
        fans: d.num_fans().unwrap_or(0),
        max_core_clock,
        max_mem_clock: d.max_clock_info(Clock::Memory).unwrap_or(0),
        temp_slowdown: thr(TemperatureThreshold::Slowdown),
        temp_shutdown: thr(TemperatureThreshold::Shutdown),
        temp_gpu_max: thr(TemperatureThreshold::GpuMax),
        limits: Limits {
            power_min_mw: constraints
                .as_ref()
                .map_or(power_default_mw, |c| c.min_limit),
            power_max_mw: constraints
                .as_ref()
                .map_or(power_default_mw, |c| c.max_limit),
            power_default_mw,
            core_offset: offset_range(Clock::Graphics, (-500, 500)),
            mem_offset: offset_range(Clock::Memory, (-1000, 2000)),
            core_clock: (min_core_clock, max_core_clock.max(min_core_clock)),
            target_temp,
            fan_speed: d.min_max_fan_speed().unwrap_or((0, 100)),
        },
    })
}

fn pstate_number(p: PerformanceState) -> Option<u8> {
    use PerformanceState::*;
    Some(match p {
        Zero => 0,
        One => 1,
        Two => 2,
        Three => 3,
        Four => 4,
        Five => 5,
        Six => 6,
        Seven => 7,
        Eight => 8,
        Nine => 9,
        Ten => 10,
        Eleven => 11,
        Twelve => 12,
        Thirteen => 13,
        Fourteen => 14,
        Fifteen => 15,
        Unknown => return None,
    })
}

fn throttle_names(r: ThrottleReasons) -> Vec<String> {
    let table = [
        (ThrottleReasons::GPU_IDLE, "idle"),
        (ThrottleReasons::APPLICATIONS_CLOCKS_SETTING, "clock-limit"),
        (ThrottleReasons::SW_POWER_CAP, "power"),
        (ThrottleReasons::HW_SLOWDOWN, "hw-slowdown"),
        (ThrottleReasons::SYNC_BOOST, "sync-boost"),
        (ThrottleReasons::SW_THERMAL_SLOWDOWN, "thermal"),
        (ThrottleReasons::HW_THERMAL_SLOWDOWN, "hw-thermal"),
        (ThrottleReasons::HW_POWER_BRAKE_SLOWDOWN, "power-brake"),
        (ThrottleReasons::DISPLAY_CLOCK_SETTING, "display"),
    ];
    table
        .iter()
        .filter(|(bit, _)| r.contains(*bit))
        .map(|(_, n)| (*n).to_string())
        .collect()
}
