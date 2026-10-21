//! GUI preferences (`~/.config/wnvctl/settings.toml`), separate from the
//! GPU profiles.

use serde::{Deserialize, Serialize};

use crate::i18n::Lang;
use crate::profile::{load_toml, save_toml};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub language: Lang,
    /// How often the GUI samples the GPU (ms).
    pub refresh_ms: u64,
    /// Seconds of history in the charts.
    pub history_secs: u32,
    /// Closing the window keeps wnvctl in the tray.
    pub close_to_tray: bool,
    /// Start in the tray without a window.
    pub start_hidden: bool,
    /// Start at login (XDG autostart).
    pub autostart: bool,
    /// Profile last applied from the GUI.
    pub last_profile: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            language: Lang::from_env(),
            refresh_ms: 1000,
            history_secs: 120,
            close_to_tray: false,
            start_hidden: false,
            autostart: false,
            last_profile: None,
        }
    }
}

impl Settings {
    pub fn path() -> std::path::PathBuf {
        crate::config_dir().join("settings.toml")
    }

    pub fn load() -> Self {
        load_toml(&Self::path()).unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), crate::profile::StoreError> {
        save_toml(&Self::path(), self)
    }
}
