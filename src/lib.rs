//! wnvctl: overclocking, undervolting, power limit and fan curves for NVIDIA
//! GPUs on Linux.
//!
//! * [`curve`], [`profile`], [`types`]: pure logic, tested without hardware;
//! * [`gpu`]: NVML access;
//! * [`daemon`] and [`ipc`]: the root service and its JSON-lines protocol;
//! * `gui` (feature `gui`): the egui desktop app.

pub mod curve;
pub mod daemon;
pub mod gpu;
pub mod i18n;
pub mod ipc;
pub mod profile;
pub mod settings;
pub mod types;

#[cfg(feature = "gui")]
pub mod gui;

use std::path::PathBuf;

/// The user's configuration folder: `$WNVCTL_CONFIG_DIR`, else
/// `~/.config/wnvctl`.
pub fn config_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("WNVCTL_CONFIG_DIR") {
        return PathBuf::from(d);
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("wnvctl")
}

/// `WNVCTL_DEBUG=1` prints extra diagnostics on stderr.
pub fn debug_enabled() -> bool {
    std::env::var_os("WNVCTL_DEBUG").is_some_and(|v| v != "0")
}
