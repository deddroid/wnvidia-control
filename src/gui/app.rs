//! The main window: sidebar navigation and five pages. Settings are edited
//! in a draft profile; nothing reaches the GPU until "Apply".

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText, vec2};

use super::data::{CmdResult, Data};
use super::desktop;
use super::theme::{self, card, chip, dim, title};
use super::widgets::{self, CurveView, Tile};
use crate::curve::{FanCurve, Point};
use crate::i18n::{Lang, T, tr};
use crate::ipc::{GpuStatus, Request};
use crate::profile::{FanMode, Profile, ProfileStore};
use crate::settings::Settings;
use crate::types::{GpuInfo, Stats};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Overview,
    Tuning,
    Fans,
    Profiles,
    Settings,
}

impl Tab {
    const ALL: [Tab; 5] = [
        Tab::Overview,
        Tab::Tuning,
        Tab::Fans,
        Tab::Profiles,
        Tab::Settings,
    ];

    fn label(self) -> T {
        match self {
            Tab::Overview => T::TabOverview,
            Tab::Tuning => T::TabTuning,
            Tab::Fans => T::TabFans,
            Tab::Profiles => T::TabProfiles,
            Tab::Settings => T::TabSettings,
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Tab::Overview => "▦",
            Tab::Tuning => "⚡",
            Tab::Fans => "✻",
            Tab::Profiles => "☰",
            Tab::Settings => "⚙",
        }
    }

    fn slug(self) -> &'static str {
        match self {
            Tab::Overview => "overview",
            Tab::Tuning => "tuning",
            Tab::Fans => "fans",
            Tab::Profiles => "profiles",
            Tab::Settings => "settings",
        }
    }
}

/// Per-frame copy of what the pages show.
#[derive(Default)]
struct View {
    info: Option<GpuInfo>,
    stats: Stats,
    temp: Vec<f32>,
    core: Vec<f32>,
    mem: Vec<f32>,
    power: Vec<f32>,
    fan: Vec<f32>,
    load: Vec<f32>,
    vram: Vec<f32>,
    max_core_seen: u32,
    daemon: Option<Result<GpuStatus, String>>,
    error: Option<String>,
}

impl View {
    fn daemon_ok(&self) -> bool {
        matches!(self.daemon, Some(Ok(_)))
    }

    fn status(&self) -> Option<&GpuStatus> {
        self.daemon.as_ref()?.as_ref().ok()
    }
}

struct Toast {
    text: String,
    error: bool,
    until: Instant,
}

/// Automatic screenshot run (`WNVCTL_SCREENSHOTS`).
struct ScreenshotRun {
    dir: PathBuf,
    steps: Vec<(Lang, Tab)>,
    index: usize,
    frames: u32,
    pending: Arc<AtomicBool>,
}

pub struct App {
    shell: Arc<super::Shell>,
    settings: Settings,
    tab: Tab,
    data: Data,
    gpu: usize,
    view: View,
    /// What the controls show.
    draft: Profile,
    /// What the GPU has (the daemon's active profile, else stock).
    base: Profile,
    last_active: Option<Option<Profile>>,
    busy: bool,
    toast: Option<Toast>,
    save_name: Option<String>,
    confirm_delete: Option<String>,
    store: ProfileStore,
    profiles: Vec<Profile>,
    profile_errors: Vec<String>,
    logo: Option<egui::TextureHandle>,
    shots: Option<ScreenshotRun>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, shell: Arc<super::Shell>) -> Self {
        if let Ok(mut c) = shell.ctx.lock() {
            *c = Some(cc.egui_ctx.clone());
        }
        theme::apply(&cc.egui_ctx);
        let settings = Settings::load();
        let shots = std::env::var_os("WNVCTL_SCREENSHOTS").map(|d| {
            let dir = PathBuf::from(d);
            let _ = std::fs::create_dir_all(&dir);
            ScreenshotRun {
                dir,
                steps: Lang::ALL
                    .iter()
                    .flat_map(|l| Tab::ALL.iter().map(move |t| (*l, *t)))
                    .collect(),
                index: 0,
                frames: 0,
                pending: Default::default(),
            }
        });
        // Screenshots use the demo unless WNVCTL_DEMO=0 asks for real data.
        let demo = match std::env::var_os("WNVCTL_DEMO") {
            Some(v) => v != "0",
            None => shots.is_some(),
        };
        let store = if demo {
            demo_store()
        } else {
            ProfileStore::user()
        };
        let history = (settings.history_secs as u64 * 1000 / settings.refresh_ms.max(250)) as usize;
        let data = Data::start(&cc.egui_ctx, settings.refresh_ms, history.max(10), demo);
        let logo = super::decode_png(super::ICON_PNG).map(|(rgba, w, h)| {
            cc.egui_ctx.load_texture(
                "logo",
                egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba),
                egui::TextureOptions::LINEAR,
            )
        });
        let mut app = Self {
            shell,
            settings,
            tab: Tab::Overview,
            data,
            gpu: 0,
            view: View::default(),
            draft: Profile::default(),
            base: Profile::default(),
            last_active: None,
            busy: false,
            toast: None,
            save_name: None,
            confirm_delete: None,
            store,
            profiles: Vec::new(),
            profile_errors: Vec::new(),
            logo,
            shots,
        };
        app.reload_profiles();
        app
    }

    fn t(&self, key: T) -> &'static str {
        tr(self.settings.language, key)
    }

    fn save_settings(&self) {
        if self.shots.is_none() && !self.data.is_demo() {
            let _ = self.settings.save();
        }
    }

    fn reload_profiles(&mut self) {
        let (ok, bad) = self.store.list();
        self.profiles = ok;
        self.profile_errors = bad.into_iter().map(|e| e.to_string()).collect();
    }

    fn toast(&mut self, text: impl Into<String>, error: bool) {
        self.toast = Some(Toast {
            text: text.into(),
            error,
            until: Instant::now() + Duration::from_secs(if error { 8 } else { 4 }),
        });
    }

    fn dirty(&self) -> bool {
        !self.draft.same_settings(&self.base)
    }

    fn uuid(&self) -> String {
        self.view
            .info
            .as_ref()
            .map_or_else(|| self.gpu.to_string(), |i| i.uuid.clone())
    }

    // ---- data ---------------------------------------------------------------------

    fn refresh_view(&mut self) {
        let Ok(m) = self.data.shared.lock() else {
            return;
        };
        let mut v = View {
            info: m.gpus.get(self.gpu).cloned(),
            stats: m.latest(self.gpu).cloned().unwrap_or_default(),
            error: m.error.clone(),
            ..Default::default()
        };
        if let Some(h) = m.history.get(self.gpu) {
            let col = |f: &dyn Fn(&Stats) -> Option<f32>| -> Vec<f32> {
                h.iter().map(|s| f(s).unwrap_or(0.0)).collect()
            };
            v.temp = col(&|s| s.temp.map(|x| x as f32));
            v.core = col(&|s| s.core_clock.map(|x| x as f32));
            v.mem = col(&|s| s.mem_clock.map(|x| x as f32));
            v.power = col(&|s| s.power_mw.map(|x| x as f32 / 1000.0));
            v.fan = col(&|s| s.fan_speed.first().map(|x| *x as f32));
            v.load = col(&|s| s.util_gpu.map(|x| x as f32));
            v.vram = col(&|s| s.vram_used.map(|x| x as f32 / (1u64 << 30) as f32));
            v.max_core_seen = h.iter().filter_map(|s| s.core_clock).max().unwrap_or(0);
        }
        if let Some(info) = &v.info {
            v.daemon = m.daemon.as_ref().map(|d| match d {
                Ok(list) => list
                    .iter()
                    .find(|g| g.uuid == info.uuid)
                    .cloned()
                    .ok_or_else(|| "GPU not managed by the daemon".to_string()),
                Err(e) => Err(e.clone()),
            });
        }
        drop(m);
        self.view = v;

        // Follow the daemon's active profile unless the user is editing.
        if let Some(d) = &self.view.daemon {
            let active = d.as_ref().ok().and_then(|s| s.active.clone());
            if self.last_active.as_ref() != Some(&active) {
                let new_base = active.clone().unwrap_or_default();
                if !self.dirty() || self.last_active.is_none() {
                    self.draft = new_base.clone();
                }
                self.base = new_base;
                self.last_active = Some(active);
            }
        }
    }

    fn handle_result(&mut self, r: CmdResult) {
        self.busy = false;
        match r.result {
            Ok(changes) => {
                if let Request::Apply { profile, .. } = &r.request {
                    self.base = profile.clone();
                    self.last_active = Some(Some(profile.clone()));
                    self.settings.last_profile = Some(profile.name.clone());
                    self.save_settings();
                }
                if let Request::Reset { .. } = &r.request {
                    self.draft = Profile::default();
                    self.base = Profile::default();
                    self.last_active = Some(None);
                }
                let text = if changes.is_empty() {
                    self.t(T::NothingToChange).to_string()
                } else {
                    format!("{} · {}", self.t(T::Applied), changes.join(" · "))
                };
                self.toast(text, false);
            }
            Err(e) => self.toast(format!("{}: {e}", self.t(T::Error)), true),
        }
    }

    fn apply(&mut self, profile: Profile) {
        self.busy = true;
        self.data.send(Request::Apply {
            gpu: self.uuid(),
            profile,
        });
    }

    // ---- frame -----------------------------------------------------------------------

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if let Some(logo) = &self.logo {
                ui.add(egui::Image::new((logo.id(), vec2(34.0, 34.0))));
            }
            ui.vertical(|ui| {
                ui.label(RichText::new("wnvctl").size(19.0).strong());
                let name = self
                    .view
                    .info
                    .as_ref()
                    .map_or("—".to_string(), |i| i.name.replace("NVIDIA ", ""));
                ui.label(dim(name));
            });
        });
        ui.add_space(14.0);
        let gpus: Vec<String> = self
            .data
            .shared
            .lock()
            .map(|m| m.gpus.iter().map(GpuInfo::label).collect())
            .unwrap_or_default();
        if gpus.len() > 1 {
            egui::ComboBox::from_id_salt("gpu")
                .selected_text(gpus.get(self.gpu).cloned().unwrap_or_default())
                .width(180.0)
                .show_ui(ui, |ui| {
                    for (i, g) in gpus.iter().enumerate() {
                        if ui.selectable_label(i == self.gpu, g).clicked() {
                            self.gpu = i;
                            self.last_active = None;
                        }
                    }
                });
            ui.add_space(8.0);
        }
        for tab in Tab::ALL {
            let on = self.tab == tab;
            let text = RichText::new(format!("{}   {}", tab.icon(), self.t(tab.label())))
                .size(15.0)
                .color(if on { theme::TEXT } else { theme::TEXT_DIM });
            let b = egui::Button::new(text)
                .fill(if on {
                    theme::CARD_HI
                } else {
                    Color32::TRANSPARENT
                })
                .stroke(egui::Stroke::NONE)
                .min_size(vec2(ui.available_width(), 38.0));
            let r = ui.add(b);
            if on {
                let rect = r.rect;
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(
                        rect.left_top() + vec2(0.0, 9.0),
                        vec2(3.0, rect.height() - 18.0),
                    ),
                    egui::CornerRadius::same(2),
                    theme::ACCENT,
                );
            }
            if r.clicked() {
                self.tab = tab;
                self.confirm_delete = None;
            }
        }
        // Status at the bottom.
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            ui.add_space(4.0);
            if self.data.is_demo() {
                chip(ui, self.t(T::Demo), theme::WARN);
            }
            let (txt, col) = if self.view.daemon_ok() {
                (self.t(T::DaemonOk), theme::OK)
            } else {
                (self.t(T::DaemonMissing), theme::WARN)
            };
            if let Some(name) = self
                .view
                .status()
                .and_then(|s| s.active.as_ref())
                .map(|p| p.name.clone())
            {
                ui.label(dim(format!("{} · {name}", self.t(T::Active))));
            }
            ui.horizontal(|ui| {
                let (dot, _) = ui.allocate_exact_size(vec2(8.0, 8.0), egui::Sense::hover());
                ui.painter().circle_filled(dot.center(), 4.0, col);
                ui.label(RichText::new(txt).small().color(col));
            });
        });
    }

    fn page_header(&self, ui: &mut egui::Ui, heading: &str) {
        ui.add_space(4.0);
        ui.label(RichText::new(heading).size(24.0).strong());
        ui.add_space(6.0);
    }

    // ---- overview ---------------------------------------------------------------

    fn page_overview(&mut self, ui: &mut egui::Ui) {
        let Some(info) = self.view.info.clone() else {
            self.no_gpu(ui);
            return;
        };
        let s = self.view.stats.clone();
        self.page_header(ui, &info.name);
        ui.horizontal_wrapped(|ui| {
            chip(ui, &info.architecture, theme::C_CORE);
            chip(
                ui,
                format!("{} {}", self.t(T::Driver), info.driver),
                theme::TEXT_DIM,
            );
            chip(
                ui,
                format!("{:.0} GiB", info.vram_total as f64 / (1u64 << 30) as f64),
                theme::C_VRAM,
            );
            if let Some(p) = s.pstate {
                chip(ui, format!("P{p}"), theme::TEXT_DIM);
            }
            let limiting: Vec<&str> = s
                .throttle
                .iter()
                .map(String::as_str)
                .filter(|t| *t != "idle")
                .collect();
            if limiting.is_empty() {
                chip(ui, self.t(T::NotLimited), theme::OK);
            } else {
                chip(
                    ui,
                    format!("{}: {}", self.t(T::LimitedBy), limiting.join(", ")),
                    theme::WARN,
                );
            }
        });
        ui.add_space(10.0);

        let fmt = |v: Option<u32>| v.map_or("—".to_string(), |v| v.to_string());
        let lang = self.settings.language;
        let max_power = info.limits.power_max_mw as f32 / 1000.0;
        let power = s.power_mw.map(|p| p as f32 / 1000.0);
        let limit = s.power_limit_mw.map(|p| p as f32 / 1000.0);
        let vram_gib = s.vram_used.map(|v| v as f32 / (1u64 << 30) as f32);
        let total_gib = info.vram_total as f32 / (1u64 << 30) as f32;
        let tiles = vec![
            Tile {
                caption: tr(lang, T::Temperature),
                value: fmt(s.temp),
                unit: "°C",
                detail: format!(
                    "{} {} °C · {} {} °C",
                    tr(lang, T::Slowdown),
                    fmt(info.temp_slowdown),
                    tr(lang, T::TargetTemp).to_lowercase(),
                    fmt(s.target_temp)
                ),
                color: theme::C_TEMP,
                history: self.view.temp.clone(),
                range: (20.0, 100.0),
                bar: None,
            },
            Tile {
                caption: tr(lang, T::CoreClock),
                value: fmt(s.core_clock),
                unit: "MHz",
                detail: format!(
                    "offset {:+} · max {} MHz",
                    s.core_offset.unwrap_or(0),
                    info.max_core_clock
                ),
                color: theme::C_CORE,
                history: self.view.core.clone(),
                range: (0.0, info.max_core_clock.max(1) as f32),
                bar: None,
            },
            Tile {
                caption: tr(lang, T::MemClock),
                value: fmt(s.mem_clock),
                unit: "MHz",
                detail: format!("offset {:+}", s.mem_offset.unwrap_or(0)),
                color: theme::C_MEM,
                history: self.view.mem.clone(),
                range: (0.0, (info.max_mem_clock as f32 * 1.1).max(1.0)),
                bar: None,
            },
            Tile {
                caption: tr(lang, T::Power),
                value: power.map_or("—".into(), |p| format!("{p:.0}")),
                unit: "W",
                detail: format!(
                    "{} {} W",
                    tr(lang, T::Limit),
                    limit.map_or("—".into(), |l| format!("{l:.0}"))
                ),
                color: theme::C_POWER,
                history: self.view.power.clone(),
                range: (0.0, max_power.max(1.0)),
                bar: power.zip(limit).map(|(p, l)| p / l.max(1.0)),
            },
            Tile {
                caption: tr(lang, T::Fan),
                value: fmt(s.fan_speed.first().copied()),
                unit: "%",
                detail: s
                    .fan_rpm
                    .first()
                    .map_or_else(String::new, |r| format!("{r} RPM")),
                color: theme::C_FAN,
                history: self.view.fan.clone(),
                range: (0.0, 100.0),
                bar: None,
            },
            Tile {
                caption: tr(lang, T::GpuLoad),
                value: fmt(s.util_gpu),
                unit: "%",
                detail: format!("{} {} %", tr(lang, T::MemLoad), fmt(s.util_mem)),
                color: theme::C_LOAD,
                history: self.view.load.clone(),
                range: (0.0, 100.0),
                bar: None,
            },
            Tile {
                caption: tr(lang, T::Vram),
                value: vram_gib.map_or("—".into(), |v| format!("{v:.1}")),
                unit: "GiB",
                detail: format!("/ {total_gib:.1} GiB"),
                color: theme::C_VRAM,
                history: self.view.vram.clone(),
                range: (0.0, total_gib.max(1.0)),
                bar: vram_gib.map(|v| v / total_gib.max(0.1)),
            },
        ];
        let gap = 12.0;
        let avail = ui.available_width();
        let cols = ((avail + gap) / 260.0).floor().clamp(2.0, 4.0) as usize;
        let w = (avail - gap * (cols - 1) as f32) / cols as f32;
        let mut iter = tiles.into_iter().peekable();
        while iter.peek().is_some() {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for _ in 0..cols {
                    if let Some(t) = iter.next() {
                        t.show(ui, w);
                    }
                }
            });
            ui.add_space(gap - 8.0);
        }

        ui.add_space(4.0);
        card(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                for (k, v) in [
                    (T::Vbios, info.vbios.clone()),
                    (T::Bus, info.pci_bus_id.clone()),
                    (T::Arch, info.architecture.clone()),
                ] {
                    ui.label(dim(self.t(k)));
                    ui.label(v);
                    ui.add_space(14.0);
                }
            });
            ui.label(dim(&info.uuid));
        });
    }

    fn no_gpu(&self, ui: &mut egui::Ui) {
        ui.add_space(60.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new(self.t(T::NoGpu)).size(22.0).strong());
            ui.label(dim(self.t(T::NoGpuHint)));
            if let Some(e) = &self.view.error {
                ui.add_space(8.0);
                ui.label(RichText::new(e).color(theme::ERR));
            }
        });
    }

    // ---- tuning -------------------------------------------------------------------

    fn page_tuning(&mut self, ui: &mut egui::Ui) {
        let Some(info) = self.view.info.clone() else {
            self.no_gpu(ui);
            return;
        };
        let l = &info.limits;
        self.page_header(ui, self.t(T::TabTuning));

        card(ui, |ui| {
            ui.label(title(format!("⚡ {}", self.t(T::PowerLimit))));
            ui.label(dim(self.t(T::PowerLimitHint)));
            ui.add_space(4.0);
            let (min, max, def) = (
                l.power_min_mw.div_ceil(1000),
                l.power_max_mw / 1000,
                l.power_default_mw / 1000,
            );
            let mut w = self.draft.power_limit_w.unwrap_or(def);
            ui.horizontal(|ui| {
                ui.add_enabled_ui(min < max, |ui| {
                    if ui
                        .add(egui::Slider::new(&mut w, min..=max).suffix(" W"))
                        .changed()
                    {
                        self.draft.power_limit_w = (w != def).then_some(w);
                    }
                });
                ui.label(dim(format!("{} {def} W", self.t(T::DefaultValue))));
                if self.draft.power_limit_w.is_some() && ui.small_button("↺").clicked() {
                    self.draft.power_limit_w = None;
                }
            });
        });
        ui.add_space(10.0);

        card(ui, |ui| {
            ui.label(title(format!("◷ {}", self.t(T::Clocks))));
            ui.label(dim(self.t(T::ClocksHint)));
            ui.add_space(4.0);
            egui::Grid::new("offsets")
                .num_columns(3)
                .spacing(vec2(16.0, 10.0))
                .show(ui, |ui| {
                    ui.label(self.t(T::CoreOffset));
                    ui.add(
                        egui::Slider::new(
                            &mut self.draft.core_offset_mhz,
                            l.core_offset.0..=l.core_offset.1,
                        )
                        .step_by(5.0)
                        .suffix(" MHz"),
                    );
                    if self.draft.core_offset_mhz != 0 && ui.small_button("↺").clicked() {
                        self.draft.core_offset_mhz = 0;
                    }
                    ui.end_row();
                    ui.label(self.t(T::MemOffset));
                    ui.add(
                        egui::Slider::new(
                            &mut self.draft.mem_offset_mhz,
                            l.mem_offset.0..=l.mem_offset.1,
                        )
                        .step_by(25.0)
                        .suffix(" MHz"),
                    );
                    if self.draft.mem_offset_mhz != 0 && ui.small_button("↺").clicked() {
                        self.draft.mem_offset_mhz = 0;
                    }
                    ui.end_row();
                });
        });
        ui.add_space(10.0);

        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(title(format!("↘ {}", self.t(T::Undervolt))));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut on = self.draft.core_clock_max_mhz.is_some();
                    if ui.checkbox(&mut on, self.t(T::Enabled)).changed() {
                        self.draft.core_clock_max_mhz =
                            on.then(|| suggested_clock_limit(&info, self.view.max_core_seen));
                    }
                });
            });
            ui.label(dim(self.t(T::UndervoltHint)));
            ui.add_space(4.0);
            let clock_limit = self.t(T::ClockLimit);
            if let Some(max) = self.draft.core_clock_max_mhz.as_mut() {
                ui.horizontal(|ui| {
                    ui.label(clock_limit);
                    ui.add(
                        egui::Slider::new(max, l.core_clock.0.max(300)..=l.core_clock.1)
                            .step_by(15.0)
                            .suffix(" MHz"),
                    );
                });
            }
            ui.horizontal(|ui| {
                if ui
                    .button(format!("✦ {}", self.t(T::UndervoltWizard)))
                    .clicked()
                {
                    self.draft.core_clock_max_mhz =
                        Some(suggested_clock_limit(&info, self.view.max_core_seen));
                    self.draft.core_offset_mhz = 150.clamp(l.core_offset.0, l.core_offset.1);
                }
                ui.label(dim(self.t(T::UndervoltWizardHint)));
            });
        });
        ui.add_space(10.0);

        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(title(format!("♨ {}", self.t(T::TargetTemp))));
                if let Some((min, max)) = l.target_temp {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let mut on = self.draft.target_temp_c.is_some();
                        if ui.checkbox(&mut on, self.t(T::Enabled)).changed() {
                            let cur = self.view.stats.target_temp.unwrap_or(max);
                            self.draft.target_temp_c = on.then_some(cur.clamp(min, max));
                        }
                    });
                }
            });
            match l.target_temp {
                None => {
                    ui.label(dim(self.t(T::Unsupported)));
                }
                Some((min, max)) => {
                    ui.label(dim(self.t(T::TargetTempHint)));
                    if let Some(t) = self.draft.target_temp_c.as_mut() {
                        ui.add(egui::Slider::new(t, min..=max).suffix(" °C"));
                    }
                }
            }
        });
    }

    // ---- fans ---------------------------------------------------------------------

    fn page_fans(&mut self, ui: &mut egui::Ui) {
        let Some(info) = self.view.info.clone() else {
            self.no_gpu(ui);
            return;
        };
        self.page_header(ui, self.t(T::TabFans));
        if self.view.status().is_some_and(|s| s.external_fan_control)
            && self.draft.fan.mode == FanMode::Auto
        {
            egui::Frame::new()
                .fill(theme::WARN.gamma_multiply(0.12))
                .corner_radius(egui::CornerRadius::same(10))
                .inner_margin(egui::Margin::same(12))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(
                        RichText::new(format!("⚠ {}", self.t(T::ExternalFans))).color(theme::WARN),
                    );
                });
            ui.add_space(10.0);
        }
        let (min_speed, _) = info.limits.fan_speed;
        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(title(self.t(T::FanMode)));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let modes = [FanMode::Auto, FanMode::Fixed, FanMode::Curve];
                    let labels = [self.t(T::FanAuto), self.t(T::FanFixed), self.t(T::FanCurve)];
                    let cur = modes
                        .iter()
                        .position(|m| *m == self.draft.fan.mode)
                        .unwrap_or(0);
                    if let Some(i) = widgets::segmented(ui, &labels, cur) {
                        self.draft.fan.mode = modes[i];
                    }
                });
            });
            ui.add_space(4.0);
            match self.draft.fan.mode {
                FanMode::Auto => {
                    ui.label(dim(self.t(T::FanAutoHint)));
                }
                FanMode::Fixed => {
                    ui.label(dim(self.t(T::FanFixedHint)));
                    ui.add(
                        egui::Slider::new(&mut self.draft.fan.fixed_percent, min_speed..=100)
                            .suffix(" %"),
                    );
                }
                FanMode::Curve => {
                    ui.horizontal(|ui| {
                        ui.label(dim(self.t(T::FanCurveHint)));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            for (label, curve) in [
                                (T::PresetCool, preset_cool()),
                                (T::PresetBalanced, FanCurve::default_curve()),
                                (T::PresetSilent, preset_silent()),
                            ] {
                                if ui.small_button(self.t(label)).clicked() {
                                    self.draft.fan.curve = curve;
                                }
                            }
                        });
                    });
                    ui.add_space(6.0);
                    let s = &self.view.stats;
                    let view = CurveView {
                        live: s
                            .temp
                            .zip(s.fan_speed.first().copied())
                            .map(|(t, f)| (t as f32, f as f32)),
                        min_speed: min_speed as f32,
                        slowdown: info.temp_slowdown.map(|t| t as f32),
                    };
                    widgets::curve_editor(ui, &mut self.draft.fan.curve, &view);
                    if min_speed > 0 {
                        ui.label(dim(format!("{} {min_speed} %.", self.t(T::FanMinNote))));
                    }
                }
            }
        });
        if self.draft.fan.mode == FanMode::Curve {
            ui.add_space(10.0);
            card(ui, |ui| {
                ui.label(title(self.t(T::Advanced)));
                egui::Grid::new("fan-adv")
                    .num_columns(2)
                    .spacing(vec2(16.0, 10.0))
                    .show(ui, |ui| {
                        let lang = self.settings.language;
                        let f = &mut self.draft.fan;
                        ui.label(tr(lang, T::Hysteresis))
                            .on_hover_text(tr(lang, T::HysteresisHint));
                        ui.add(
                            egui::Slider::new(&mut f.hysteresis_c, 0.0..=10.0)
                                .step_by(1.0)
                                .fixed_decimals(0)
                                .suffix(" °C"),
                        );
                        ui.end_row();
                        ui.label(tr(lang, T::Spindown));
                        let mut secs = f.spindown_ms as f32 / 1000.0;
                        if ui
                            .add(
                                egui::Slider::new(&mut secs, 0.0..=30.0)
                                    .step_by(1.0)
                                    .fixed_decimals(0)
                                    .suffix(" s"),
                            )
                            .changed()
                        {
                            f.spindown_ms = (secs * 1000.0) as u64;
                        }
                        ui.end_row();
                        ui.label(tr(lang, T::Interval));
                        ui.add(
                            egui::Slider::new(&mut f.interval_ms, 250..=5000)
                                .step_by(250.0)
                                .suffix(" ms"),
                        );
                        ui.end_row();
                    });
            });
        }
    }

    // ---- profiles -------------------------------------------------------------------

    fn page_profiles(&mut self, ui: &mut egui::Ui) {
        self.page_header(ui, self.t(T::Profiles));
        ui.horizontal(|ui| {
            ui.label(dim(self.t(T::ProfilesHint)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(self.t(T::OpenFolder).to_string()).clicked() {
                    let _ = std::fs::create_dir_all(self.store.dir());
                    desktop::open_external(&self.store.dir().to_string_lossy());
                }
            });
        });
        ui.add_space(6.0);
        let daemon_ok = self.view.daemon_ok();
        let active = self.view.status().and_then(|s| s.active.clone());
        let startup = self.view.status().and_then(|s| s.startup.clone());
        let mut stock = vec![Profile {
            name: self.t(T::Stock).into(),
            ..Default::default()
        }];
        stock.extend(self.profiles.iter().cloned());
        for (i, p) in stock.iter().enumerate() {
            let is_stock = i == 0;
            let is_active = active
                .as_ref()
                .unwrap_or(&Profile::default())
                .same_settings(p);
            let at_boot = !is_stock && startup.as_deref() == Some(&p.name);
            card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(title(&p.name));
                            if is_active {
                                chip(ui, self.t(T::Active), theme::OK);
                            }
                            if at_boot {
                                chip(ui, format!("★ {}", self.t(T::AtBoot)), theme::WARN);
                            }
                        });
                        ui.label(dim(self.summary(p)));
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if !is_stock {
                            let confirm = self.confirm_delete.as_deref() == Some(&p.name);
                            let del = if confirm {
                                egui::Button::new(
                                    RichText::new(self.t(T::ConfirmDelete)).color(theme::ERR),
                                )
                            } else {
                                egui::Button::new("✖")
                            };
                            if ui.add(del).on_hover_text(self.t(T::Delete)).clicked() {
                                if confirm {
                                    if let Err(e) = self.store.delete(&p.name) {
                                        self.toast(e.to_string(), true);
                                    }
                                    self.confirm_delete = None;
                                    self.reload_profiles();
                                } else {
                                    self.confirm_delete = Some(p.name.clone());
                                }
                            }
                            let star = if at_boot { "★" } else { "☆" };
                            let tip = if at_boot {
                                self.t(T::RemoveAtBoot)
                            } else {
                                self.t(T::SetAtBoot)
                            };
                            if ui
                                .add_enabled(daemon_ok && !self.busy, egui::Button::new(star))
                                .on_hover_text(tip)
                                .clicked()
                            {
                                self.busy = true;
                                self.data.send(Request::SetStartup {
                                    gpu: self.uuid(),
                                    profile: (!at_boot).then(|| p.clone()),
                                });
                            }
                            if ui.button(format!("✏ {}", self.t(T::Edit))).clicked() {
                                self.draft = p.clone();
                                self.tab = Tab::Tuning;
                            }
                        }
                        if ui
                            .add_enabled(daemon_ok && !self.busy, theme::primary(self.t(T::Apply)))
                            .clicked()
                        {
                            if is_stock {
                                self.busy = true;
                                self.data.send(Request::Reset { gpu: self.uuid() });
                            } else {
                                self.draft = p.clone();
                                self.apply(p.clone());
                            }
                        }
                    });
                });
            });
            ui.add_space(8.0);
        }
        if self.profiles.is_empty() {
            ui.label(dim(self.t(T::NoProfiles)));
        }
        for e in &self.profile_errors {
            ui.label(RichText::new(e).color(theme::ERR).small());
        }
    }

    fn summary(&self, p: &Profile) -> String {
        let mut parts = vec![match p.power_limit_w {
            Some(w) => format!("⚡ {w} W"),
            None => format!("⚡ {}", self.t(T::DefaultValue)),
        }];
        if p.core_offset_mhz != 0 || p.mem_offset_mhz != 0 {
            parts.push(format!(
                "core {:+} · mem {:+} MHz",
                p.core_offset_mhz, p.mem_offset_mhz
            ));
        }
        if let Some(c) = p.core_clock_max_mhz {
            parts.push(format!("≤ {c} MHz"));
        }
        if let Some(t) = p.target_temp_c {
            parts.push(format!("♨ {t} °C"));
        }
        parts.push(format!(
            "✻ {}",
            match p.fan.mode {
                FanMode::Auto => self.t(T::FanAuto).to_string(),
                FanMode::Fixed => format!("{} %", p.fan.fixed_percent),
                FanMode::Curve => self.t(T::FanCurve).to_string(),
            }
        ));
        parts.join("   ")
    }

    // ---- settings -----------------------------------------------------------------------

    fn page_settings(&mut self, ui: &mut egui::Ui) {
        self.page_header(ui, self.t(T::TabSettings));
        let mut changed = false;
        card(ui, |ui| {
            egui::Grid::new("settings")
                .num_columns(2)
                .spacing(vec2(20.0, 12.0))
                .show(ui, |ui| {
                    ui.label(self.t(T::Language));
                    let cur = Lang::ALL
                        .iter()
                        .position(|l| *l == self.settings.language)
                        .unwrap_or(0);
                    let names: Vec<&str> = Lang::ALL.iter().map(|l| l.name()).collect();
                    if let Some(i) = widgets::segmented(ui, &names, cur) {
                        self.settings.language = Lang::ALL[i];
                        changed = true;
                    }
                    ui.end_row();
                    ui.label(self.t(T::Refresh));
                    if ui
                        .add(
                            egui::Slider::new(&mut self.settings.refresh_ms, 250..=5000)
                                .step_by(250.0)
                                .suffix(" ms"),
                        )
                        .changed()
                    {
                        self.data.set_refresh(self.settings.refresh_ms);
                        changed = true;
                    }
                    ui.end_row();
                    ui.label(self.t(T::HistoryLen));
                    changed |= ui
                        .add(
                            egui::Slider::new(&mut self.settings.history_secs, 30..=600)
                                .step_by(30.0)
                                .suffix(" s"),
                        )
                        .changed();
                    ui.end_row();
                });
        });
        ui.add_space(10.0);
        let lang = self.settings.language;
        card(ui, |ui| {
            changed |= ui
                .checkbox(&mut self.settings.close_to_tray, tr(lang, T::CloseToTray))
                .changed();
            changed |= ui
                .checkbox(&mut self.settings.start_hidden, tr(lang, T::StartHidden))
                .changed();
            if ui
                .checkbox(&mut self.settings.autostart, tr(lang, T::Autostart))
                .changed()
            {
                changed = true;
                if self.shots.is_none()
                    && !self.data.is_demo()
                    && let Err(e) = desktop::set_autostart(self.settings.autostart)
                {
                    self.toast(e.to_string(), true);
                }
            }
            ui.label(dim(self.t(T::TrayRestart)));
        });
        ui.add_space(10.0);
        card(ui, |ui| {
            ui.label(title(self.t(T::Daemon)));
            match &self.view.daemon {
                Some(Ok(_)) => {
                    ui.label(RichText::new(format!("● {}", self.t(T::DaemonOk))).color(theme::OK));
                }
                other => {
                    ui.label(
                        RichText::new(format!("● {}", self.t(T::DaemonMissing))).color(theme::WARN),
                    );
                    if let Some(Err(e)) = other {
                        ui.label(dim(e));
                    }
                    ui.label(self.t(T::DaemonHint));
                    ui.code("sudo systemctl enable --now wnvctl");
                    ui.label(dim(self.t(T::PermissionHint)));
                }
            }
            if let Some(e) = self.view.status().and_then(|s| s.last_error.clone()) {
                ui.label(RichText::new(e).color(theme::ERR));
            }
        });
        ui.add_space(10.0);
        card(ui, |ui| {
            ui.label(title(format!(
                "{} · wnvctl {}",
                self.t(T::About),
                env!("CARGO_PKG_VERSION")
            )));
            ui.label(self.t(T::AboutText));
            ui.add_space(4.0);
            ui.label(dim(self.t(T::Credits)));
            ui.label("deddroid · Claude (Anthropic)");
            ui.horizontal_wrapped(|ui| {
                for (name, url) in [
                    ("sLimit", "https://github.com/ssprea/ssprea-nvidia-control"),
                    ("LACT", "https://github.com/ilya-zlobintsev/LACT"),
                    ("nvidia_oc", "https://github.com/Dreaming-Codes/nvidia_oc"),
                    ("nvuv", "https://github.com/doums/nvuv"),
                    ("PenguinBurner", "https://github.com/jpietek/PenguinBurner"),
                ] {
                    ui.hyperlink_to(name, url);
                }
            });
            ui.hyperlink_to(
                "github.com/deddroid/wnvidia-control",
                "https://github.com/deddroid/wnvidia-control",
            );
        });
        if changed {
            self.save_settings();
        }
    }

    // ---- action bar and dialogs -------------------------------------------------------

    fn action_bar(&mut self, ui: &mut egui::Ui) {
        let daemon_ok = self.view.daemon_ok();
        let dirty = self.dirty();
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            if dirty {
                chip(ui, format!("● {}", self.t(T::Unsaved)), theme::WARN);
            } else {
                chip(
                    ui,
                    format!("✔ {} · {}", self.t(T::InSync), self.base.name),
                    theme::OK,
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(4.0);
                let label = if self.busy {
                    self.t(T::Applying)
                } else {
                    self.t(T::Apply)
                };
                let r = ui.add_enabled(daemon_ok && !self.busy, theme::primary(label));
                if !daemon_ok {
                    r.clone().on_disabled_hover_text(self.t(T::DaemonHint));
                }
                if r.clicked() {
                    match self
                        .view
                        .info
                        .as_ref()
                        .map(|i| self.draft.resolve(&i.limits, None))
                    {
                        Some(Err(e)) => self.toast(e.to_string(), true),
                        _ => self.apply(self.draft.clone()),
                    }
                }
                if ui.button(format!("⬇ {}", self.t(T::SaveAs))).clicked() {
                    let name = if self.draft.name == "Stock" || self.draft.name == "Custom" {
                        String::new()
                    } else {
                        self.draft.name.clone()
                    };
                    self.save_name = Some(name);
                }
                if ui
                    .add_enabled(
                        daemon_ok && !self.busy,
                        egui::Button::new(self.t(T::ResetStock)),
                    )
                    .clicked()
                {
                    self.busy = true;
                    self.data.send(Request::Reset { gpu: self.uuid() });
                }
                if ui
                    .add_enabled(dirty, egui::Button::new(self.t(T::Revert)))
                    .clicked()
                {
                    self.draft = self.base.clone();
                }
            });
        });
    }

    fn save_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut name) = self.save_name.take() else {
            return;
        };
        let mut open = true;
        let mut done = false;
        egui::Window::new(self.t(T::SaveAs))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(self.t(T::ProfileName));
                let r = ui.add(egui::TextEdit::singleline(&mut name).desired_width(280.0));
                r.request_focus();
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.horizontal(|ui| {
                    let ok = !name.trim().is_empty();
                    if (ui
                        .add_enabled(ok, theme::primary(self.t(T::Save)))
                        .clicked()
                        || enter)
                        && ok
                    {
                        done = true;
                    }
                    if ui.button(self.t(T::Cancel)).clicked() {
                        name.clear();
                        done = true;
                    }
                });
            });
        if done {
            if !name.trim().is_empty() {
                self.draft.name = name.trim().to_string();
                match self.store.save(&self.draft) {
                    Ok(_) => {
                        self.toast(format!("⬇ {}", self.draft.name), false);
                        self.reload_profiles();
                    }
                    Err(e) => self.toast(e.to_string(), true),
                }
            }
        } else if open {
            self.save_name = Some(name);
        }
    }

    fn show_toast(&mut self, ctx: &egui::Context) {
        let Some(t) = &self.toast else { return };
        if Instant::now() > t.until {
            self.toast = None;
            return;
        }
        egui::Area::new(egui::Id::unique("wnvctl-toast"))
            .anchor(egui::Align2::RIGHT_BOTTOM, vec2(-20.0, -70.0))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(theme::CARD_HI)
                    .stroke(egui::Stroke::new(
                        1.0,
                        if t.error { theme::ERR } else { theme::ACCENT },
                    ))
                    .corner_radius(egui::CornerRadius::same(10))
                    .inner_margin(egui::Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.set_max_width(460.0);
                        ui.label(&t.text);
                    });
            });
        ctx.request_repaint_after(Duration::from_millis(500));
    }

    fn drive_screenshots(&mut self, ctx: &egui::Context) {
        let Some(run) = &mut self.shots else { return };
        if run.pending.load(Ordering::SeqCst) {
            ctx.request_repaint();
            return;
        }
        let Some(&(lang, tab)) = run.steps.get(run.index) else {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        };
        self.settings.language = lang;
        self.tab = tab;
        if tab == Tab::Fans {
            self.draft.fan.mode = FanMode::Curve;
        }
        if tab == Tab::Tuning {
            self.draft.power_limit_w = Some(100);
            self.draft.core_offset_mhz = 150;
            self.draft.core_clock_max_mhz = Some(2610);
        }
        run.frames += 1;
        // Let the charts fill and layout settle.
        if run.frames < 6 {
            ctx.request_repaint();
            return;
        }
        run.frames = 0;
        run.index += 1;
        let path = run.dir.join(format!(
            "{}-{}.png",
            tab.slug(),
            if lang == Lang::It { "it" } else { "en" }
        ));
        let pending = run.pending.clone();
        pending.store(true, Ordering::SeqCst);
        let repaint = ctx.clone();
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(
            egui::ScreenshotCallback::new(move |img| {
                let _ = save_png(&path, &img);
                pending.store(false, Ordering::SeqCst);
                repaint.request_repaint();
            }),
        ));
        ctx.request_repaint();
    }
}

impl eframe::App for App {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Ok(mut c) = self.shell.ctx.lock() {
            *c = None;
        }
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Some(r) = self.data.poll() {
            self.handle_result(r);
        }
        self.refresh_view();
        self.drive_screenshots(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::left("nav")
            .resizable(false)
            .exact_size(214.0)
            .frame(
                egui::Frame::new()
                    .fill(theme::BG)
                    .inner_margin(egui::Margin::same(12)),
            )
            .show(ui, |ui| self.sidebar(ui));
        if matches!(self.tab, Tab::Tuning | Tab::Fans) && self.view.info.is_some() {
            egui::Panel::bottom("actions")
                .frame(
                    egui::Frame::new()
                        .fill(theme::PANEL)
                        .stroke(egui::Stroke::new(1.0, theme::LINE))
                        .inner_margin(egui::Margin::symmetric(16, 10)),
                )
                .show(ui, |ui| self.action_bar(ui));
        }
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(theme::PANEL)
                    .inner_margin(egui::Margin::symmetric(22, 16)),
            )
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false; 2])
                    .show(ui, |ui| match self.tab {
                        Tab::Overview => self.page_overview(ui),
                        Tab::Tuning => self.page_tuning(ui),
                        Tab::Fans => self.page_fans(ui),
                        Tab::Profiles => self.page_profiles(ui),
                        Tab::Settings => self.page_settings(ui),
                    });
            });
        let ctx = ui.ctx().clone();
        self.save_dialog(&ctx);
        self.show_toast(&ctx);
    }
}

// ---- helpers --------------------------------------------------------------------

/// Undervolt starting point: 92 % of the highest clock seen under load (or
/// of the table maximum), rounded to the 15 MHz clock steps.
fn suggested_clock_limit(info: &GpuInfo, max_seen: u32) -> u32 {
    let base = if max_seen > 1500 {
        max_seen
    } else {
        info.max_core_clock * 9 / 10
    };
    let v = base * 92 / 100 / 15 * 15;
    v.clamp(info.limits.core_clock.0.max(300), info.limits.core_clock.1)
}

fn preset_silent() -> FanCurve {
    FanCurve::new(vec![
        Point::new(50.0, 30.0),
        Point::new(65.0, 40.0),
        Point::new(75.0, 60.0),
        Point::new(85.0, 100.0),
    ])
    .unwrap_or_default()
}

fn preset_cool() -> FanCurve {
    FanCurve::new(vec![
        Point::new(35.0, 35.0),
        Point::new(50.0, 50.0),
        Point::new(60.0, 70.0),
        Point::new(70.0, 90.0),
        Point::new(78.0, 100.0),
    ])
    .unwrap_or_default()
}

/// Sample profiles for the demo and the screenshots, in a temporary folder.
fn demo_store() -> ProfileStore {
    let dir = std::env::temp_dir().join(format!("wnvctl-demo-{}", std::process::id()));
    let store = ProfileStore::new(dir);
    let mut quiet = Profile {
        name: "Quiet".into(),
        power_limit_w: Some(95),
        ..Default::default()
    };
    quiet.fan.mode = FanMode::Curve;
    quiet.fan.curve = preset_silent();
    let uv = Profile {
        name: "Undervolt".into(),
        core_offset_mhz: 150,
        core_clock_max_mhz: Some(2610),
        ..Default::default()
    };
    let mut perf = Profile {
        name: "Performance".into(),
        core_offset_mhz: 120,
        mem_offset_mhz: 1000,
        target_temp_c: Some(80),
        ..Default::default()
    };
    perf.fan.mode = FanMode::Curve;
    perf.fan.curve = preset_cool();
    for p in [quiet, uv, perf] {
        let _ = store.save(&p);
    }
    store
}

fn save_png(path: &std::path::Path, img: &egui::ColorImage) -> std::io::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut enc = png::Encoder::new(
        std::io::BufWriter::new(file),
        img.size[0] as u32,
        img.size[1] as u32,
    );
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().map_err(std::io::Error::other)?;
    let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
    w.write_image_data(&bytes).map_err(std::io::Error::other)?;
    Ok(())
}
