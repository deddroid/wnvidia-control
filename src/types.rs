//! Data shared by the library, the daemon and the clients.

use serde::{Deserialize, Serialize};

/// Ranges accepted by a GPU, read once from NVML.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Limits {
    pub power_min_mw: u32,
    pub power_max_mw: u32,
    pub power_default_mw: u32,
    pub core_offset: (i32, i32),
    pub mem_offset: (i32, i32),
    /// Graphics clock range usable for a clock limit (MHz).
    pub core_clock: (u32, u32),
    /// Target ("acoustic") temperature range in °C, if supported.
    pub target_temp: Option<(u32, u32)>,
    /// Speed range accepted by the fans (percent).
    pub fan_speed: (u32, u32),
}

impl Limits {
    /// Plausible limits for tests and the demo mode (an RTX 4060).
    pub fn demo() -> Self {
        Self {
            power_min_mw: 90_000,
            power_max_mw: 115_000,
            power_default_mw: 115_000,
            core_offset: (-1000, 1000),
            mem_offset: (-2000, 6000),
            core_clock: (210, 3105),
            target_temp: Some((65, 88)),
            fan_speed: (30, 100),
        }
    }
}

/// Fixed facts about a GPU.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GpuInfo {
    pub index: u32,
    pub uuid: String,
    pub name: String,
    pub pci_bus_id: String,
    pub architecture: String,
    pub vbios: String,
    pub driver: String,
    pub vram_total: u64,
    pub fans: u32,
    pub max_core_clock: u32,
    pub max_mem_clock: u32,
    pub temp_slowdown: Option<u32>,
    pub temp_shutdown: Option<u32>,
    pub temp_gpu_max: Option<u32>,
    pub limits: Limits,
}

impl GpuInfo {
    pub fn demo() -> Self {
        Self {
            index: 0,
            uuid: "GPU-00000000-demo-0000-0000-000000000000".into(),
            name: "NVIDIA GeForce RTX 4060".into(),
            pci_bus_id: "00000000:09:00.0".into(),
            architecture: "Ada".into(),
            vbios: "95.07.36.40.1D".into(),
            driver: "615.78.08".into(),
            vram_total: 8 << 30,
            fans: 1,
            max_core_clock: 3105,
            max_mem_clock: 8501,
            temp_slowdown: Some(95),
            temp_shutdown: Some(100),
            temp_gpu_max: Some(90),
            limits: Limits::demo(),
        }
    }

    /// A short label for menus: "0 · NVIDIA GeForce RTX 4060".
    pub fn label(&self) -> String {
        format!("{} · {}", self.index, self.name)
    }
}

/// Values that change over time, sampled periodically.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Stats {
    pub temp: Option<u32>,
    pub core_clock: Option<u32>,
    pub mem_clock: Option<u32>,
    pub video_clock: Option<u32>,
    /// Average power since the previous sample (mW).
    pub power_mw: Option<u32>,
    pub power_limit_mw: Option<u32>,
    pub util_gpu: Option<u32>,
    pub util_mem: Option<u32>,
    pub vram_used: Option<u64>,
    pub fan_speed: Vec<u32>,
    pub fan_rpm: Vec<u32>,
    pub pstate: Option<u8>,
    pub core_offset: Option<i32>,
    pub mem_offset: Option<i32>,
    pub target_temp: Option<u32>,
    /// Active clock limiters, as short names ("power", "thermal"…).
    pub throttle: Vec<String>,
}
