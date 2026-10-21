//! Tuning profiles: what the user asks for, validation against the GPU
//! limits, and the minimal list of writes to get there.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::curve::FanCurve;
use crate::types::Limits;

/// Fan behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FanMode {
    /// The GPU firmware decides (driver default).
    #[default]
    Auto,
    /// A fixed speed.
    Fixed,
    /// The daemon follows a temperature curve.
    Curve,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FanSettings {
    pub mode: FanMode,
    /// Used in [`FanMode::Fixed`].
    pub fixed_percent: u32,
    /// Used in [`FanMode::Curve`].
    pub curve: FanCurve,
    /// °C the temperature must drop before the fans slow down.
    pub hysteresis_c: f32,
    /// Minimum time between a speed change and a slowdown.
    pub spindown_ms: u64,
    /// How often the temperature is read.
    pub interval_ms: u64,
}

impl Default for FanSettings {
    fn default() -> Self {
        Self {
            mode: FanMode::Auto,
            fixed_percent: 50,
            curve: FanCurve::default(),
            hysteresis_c: 3.0,
            spindown_ms: 5000,
            interval_ms: 1000,
        }
    }
}

/// A named set of GPU settings. Missing values mean "driver default", so
/// applying a profile always leads to the same state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub name: String,
    /// Power limit in watts (`None` = default).
    pub power_limit_w: Option<u32>,
    /// Core clock offset (MHz).
    pub core_offset_mhz: i32,
    /// Memory clock offset (MHz, as NVML counts it).
    pub mem_offset_mhz: i32,
    /// Highest core clock the GPU may use (MHz, `None` = no limit). With a
    /// positive core offset this is the usual NVIDIA undervolt: the same
    /// clock is reached at a lower voltage and never exceeded.
    pub core_clock_max_mhz: Option<u32>,
    /// Target temperature in °C (`None` = leave the driver's value).
    pub target_temp_c: Option<u32>,
    pub fan: FanSettings,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            name: "Stock".into(),
            power_limit_w: None,
            core_offset_mhz: 0,
            mem_offset_mhz: 0,
            core_clock_max_mhz: None,
            target_temp_c: None,
            fan: FanSettings::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ProfileError {
    #[error("power limit {got} W is outside {min}–{max} W")]
    Power { got: u32, min: u32, max: u32 },
    #[error("core offset {got} MHz is outside {min}–{max} MHz")]
    CoreOffset { got: i32, min: i32, max: i32 },
    #[error("memory offset {got} MHz is outside {min}–{max} MHz")]
    MemOffset { got: i32, min: i32, max: i32 },
    #[error("clock limit {got} MHz is outside {min}–{max} MHz")]
    Clock { got: u32, min: u32, max: u32 },
    #[error("target temperature {got} °C is outside {min}–{max} °C")]
    TargetTemp { got: u32, min: u32, max: u32 },
    #[error("this GPU has no target temperature setting")]
    TargetTempUnsupported,
    #[error("fixed fan speed {got} % is outside {min}–{max} %")]
    FanSpeed { got: u32, min: u32, max: u32 },
    #[error("fan interval must be between 200 and 10000 ms")]
    FanInterval,
}

/// The writable GPU state, in NVML units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tunables {
    pub power_limit_mw: u32,
    pub core_offset: i32,
    pub mem_offset: i32,
    /// `None` = clocks not locked.
    pub core_clock_max: Option<u32>,
    /// `None` = do not touch.
    pub target_temp: Option<u32>,
}

/// One write to the GPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Op {
    MemOffset(i32),
    CoreOffset(i32),
    PowerLimit(u32),
    LockCoreClock { min: u32, max: u32 },
    UnlockCoreClock,
    TargetTemp(u32),
}

impl Profile {
    /// Checks the profile against the GPU limits and converts it to NVML
    /// units. `stock_target_temp` is the driver's own target temperature,
    /// restored when the profile does not set one.
    pub fn resolve(
        &self,
        limits: &Limits,
        stock_target_temp: Option<u32>,
    ) -> Result<Tunables, ProfileError> {
        let power_limit_mw = match self.power_limit_w {
            None => limits.power_default_mw,
            Some(w) => {
                let (min, max) = (
                    limits.power_min_mw.div_ceil(1000),
                    limits.power_max_mw / 1000,
                );
                if !(min..=max).contains(&w) {
                    return Err(ProfileError::Power { got: w, min, max });
                }
                w * 1000
            }
        };
        let (min, max) = limits.core_offset;
        if !(min..=max).contains(&self.core_offset_mhz) {
            return Err(ProfileError::CoreOffset {
                got: self.core_offset_mhz,
                min,
                max,
            });
        }
        let (min, max) = limits.mem_offset;
        if !(min..=max).contains(&self.mem_offset_mhz) {
            return Err(ProfileError::MemOffset {
                got: self.mem_offset_mhz,
                min,
                max,
            });
        }
        if let Some(c) = self.core_clock_max_mhz {
            let (min, max) = limits.core_clock;
            if !(min..=max).contains(&c) {
                return Err(ProfileError::Clock { got: c, min, max });
            }
        }
        let target_temp = match (self.target_temp_c, limits.target_temp) {
            (None, _) => stock_target_temp,
            (Some(_), None) => return Err(ProfileError::TargetTempUnsupported),
            (Some(t), Some((min, max))) => {
                if !(min..=max).contains(&t) {
                    return Err(ProfileError::TargetTemp { got: t, min, max });
                }
                Some(t)
            }
        };
        let (min, max) = limits.fan_speed;
        if self.fan.mode == FanMode::Fixed && !(min..=max).contains(&self.fan.fixed_percent) {
            return Err(ProfileError::FanSpeed {
                got: self.fan.fixed_percent,
                min,
                max,
            });
        }
        if self.fan.mode == FanMode::Curve && !(200..=10_000).contains(&self.fan.interval_ms) {
            return Err(ProfileError::FanInterval);
        }
        Ok(Tunables {
            power_limit_mw,
            core_offset: self.core_offset_mhz,
            mem_offset: self.mem_offset_mhz,
            core_clock_max: self.core_clock_max_mhz,
            target_temp,
        })
    }

    /// File name used to store the profile.
    pub fn file_stem(&self) -> String {
        slug(&self.name)
    }

    /// Same GPU settings, whatever the names.
    pub fn same_settings(&self, other: &Profile) -> bool {
        Profile {
            name: String::new(),
            ..self.clone()
        } == Profile {
            name: String::new(),
            ..other.clone()
        }
    }
}

/// The writes needed to go from `current` to `desired`, in a safe order:
/// memory offset first (on some drivers it clears the core V/F table), then
/// the core offset, the clock limit, the power limit and the target
/// temperature. Unchanged values are not written.
pub fn plan(current: &Tunables, desired: &Tunables, min_core_clock: u32) -> Vec<Op> {
    let mut ops = Vec::new();
    if current.mem_offset != desired.mem_offset {
        ops.push(Op::MemOffset(desired.mem_offset));
    }
    if current.core_offset != desired.core_offset {
        ops.push(Op::CoreOffset(desired.core_offset));
    }
    if current.core_clock_max != desired.core_clock_max {
        ops.push(match desired.core_clock_max {
            Some(max) => Op::LockCoreClock {
                min: min_core_clock.min(max),
                max,
            },
            None => Op::UnlockCoreClock,
        });
    }
    if current.power_limit_mw != desired.power_limit_mw {
        ops.push(Op::PowerLimit(desired.power_limit_mw));
    }
    if let Some(t) = desired.target_temp
        && current.target_temp != Some(t)
    {
        ops.push(Op::TargetTemp(t));
    }
    ops
}

/// Lower-case, dash-separated file name for a profile name.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() {
        "profile".into()
    } else {
        out
    }
}

// ---- storage -----------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("{0}: {1}")]
    Parse(PathBuf, toml::de::Error),
    #[error("{0}")]
    Serialize(#[from] toml::ser::Error),
}

pub fn load_toml<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, StoreError> {
    let text = std::fs::read_to_string(path).map_err(|e| StoreError::Io(path.into(), e))?;
    toml::from_str(&text).map_err(|e| StoreError::Parse(path.into(), e))
}

/// Writes atomically (temporary file + rename), creating the directory.
pub fn save_toml<T: Serialize>(path: &Path, value: &T) -> Result<(), StoreError> {
    let text = toml::to_string_pretty(value)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| StoreError::Io(dir.into(), e))?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text).map_err(|e| StoreError::Io(tmp.clone(), e))?;
    std::fs::rename(&tmp, path).map_err(|e| StoreError::Io(path.into(), e))
}

/// The user's profiles: one TOML file each in `~/.config/wnvctl/profiles`
/// (or `$WNVCTL_CONFIG_DIR/profiles`).
pub struct ProfileStore {
    dir: PathBuf,
}

impl ProfileStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn user() -> Self {
        Self::new(crate::config_dir().join("profiles"))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// All readable profiles, sorted by name. Broken files are reported
    /// separately instead of hiding the others.
    pub fn list(&self) -> (Vec<Profile>, Vec<StoreError>) {
        let mut ok = Vec::new();
        let mut bad = Vec::new();
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return (ok, bad);
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "toml") {
                match load_toml::<Profile>(&p) {
                    Ok(prof) => ok.push(prof),
                    Err(err) => bad.push(err),
                }
            }
        }
        ok.sort_by_key(|p| p.name.to_lowercase());
        (ok, bad)
    }

    /// Finds a profile by name (case-insensitive) or file stem.
    pub fn find(&self, name: &str) -> Option<Profile> {
        let key = slug(name);
        self.list().0.into_iter().find(|p| p.file_stem() == key)
    }

    pub fn path_of(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{}.toml", slug(name)))
    }

    pub fn save(&self, profile: &Profile) -> Result<PathBuf, StoreError> {
        let path = self.path_of(&profile.name);
        save_toml(&path, profile)?;
        Ok(path)
    }

    pub fn delete(&self, name: &str) -> Result<(), StoreError> {
        let path = self.path_of(name);
        std::fs::remove_file(&path).map_err(|e| StoreError::Io(path, e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stock() -> Tunables {
        Profile::default()
            .resolve(&Limits::demo(), Some(83))
            .unwrap()
    }

    #[test]
    fn default_profile_is_stock() {
        let t = stock();
        assert_eq!(t.power_limit_mw, 115_000);
        assert_eq!((t.core_offset, t.mem_offset), (0, 0));
        assert_eq!(t.core_clock_max, None);
        assert_eq!(t.target_temp, Some(83), "restores the driver value");
    }

    #[test]
    fn validates_ranges() {
        let l = Limits::demo();
        let p = Profile {
            power_limit_w: Some(80),
            ..Default::default()
        };
        assert_eq!(
            p.resolve(&l, None),
            Err(ProfileError::Power {
                got: 80,
                min: 90,
                max: 115
            })
        );
        let p = Profile {
            core_offset_mhz: 1500,
            ..Default::default()
        };
        assert!(matches!(
            p.resolve(&l, None),
            Err(ProfileError::CoreOffset { .. })
        ));
        let p = Profile {
            target_temp_c: Some(95),
            ..Default::default()
        };
        assert!(matches!(
            p.resolve(&l, None),
            Err(ProfileError::TargetTemp { .. })
        ));
        let mut p = Profile::default();
        p.fan.mode = FanMode::Fixed;
        p.fan.fixed_percent = 10;
        assert!(matches!(
            p.resolve(&l, None),
            Err(ProfileError::FanSpeed { .. })
        ));
    }

    #[test]
    fn plan_writes_only_changes_in_safe_order() {
        let cur = stock();
        assert!(plan(&cur, &cur, 210).is_empty());
        let p = Profile {
            power_limit_w: Some(100),
            core_offset_mhz: 150,
            mem_offset_mhz: 1000,
            core_clock_max_mhz: Some(2600),
            target_temp_c: Some(75),
            ..Default::default()
        };
        let want = p.resolve(&Limits::demo(), Some(83)).unwrap();
        assert_eq!(
            plan(&cur, &want, 210),
            vec![
                Op::MemOffset(1000),
                Op::CoreOffset(150),
                Op::LockCoreClock {
                    min: 210,
                    max: 2600
                },
                Op::PowerLimit(100_000),
                Op::TargetTemp(75),
            ]
        );
        // And back to stock.
        assert_eq!(
            plan(&want, &cur, 210),
            vec![
                Op::MemOffset(0),
                Op::CoreOffset(0),
                Op::UnlockCoreClock,
                Op::PowerLimit(115_000),
                Op::TargetTemp(83),
            ]
        );
    }

    #[test]
    fn profile_toml_is_readable_and_partial() {
        let p: Profile = toml::from_str(
            r#"
            name = "Quiet"
            power_limit_w = 95
            [fan]
            mode = "curve"
            curve = [{ temp = 40, speed = 30 }, { temp = 80, speed = 90 }]
            "#,
        )
        .unwrap();
        assert_eq!(p.name, "Quiet");
        assert_eq!(p.fan.mode, FanMode::Curve);
        assert_eq!(p.fan.curve.speed_at(60.0), 60.0);
        assert_eq!(p.fan.hysteresis_c, 3.0, "defaults fill the rest");
        let back: Profile = toml::from_str(&toml::to_string(&p).unwrap()).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn store_roundtrip() {
        let dir = std::env::temp_dir().join(format!("wnvctl-test-{}", std::process::id()));
        let store = ProfileStore::new(dir.clone());
        let p = Profile {
            name: "My Undervolt!".into(),
            core_offset_mhz: 120,
            ..Default::default()
        };
        let path = store.save(&p).unwrap();
        assert!(path.ends_with("my-undervolt.toml"));
        assert_eq!(store.find("my undervolt"), Some(p.clone()));
        assert_eq!(store.list().0, vec![p]);
        store.delete("My Undervolt!").unwrap();
        assert!(store.list().0.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("  Quiet / Night  "), "quiet-night");
        assert_eq!(slug("???"), "profile");
    }
}
