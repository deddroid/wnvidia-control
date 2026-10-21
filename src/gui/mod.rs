//! Desktop GUI (egui/eframe, native Wayland or X11).
//!
//! Environment variables:
//! * `WNVCTL_DEMO=1` — simulated GPU and daemon, nothing is read or written;
//! * `WNVCTL_SCREENSHOTS=<dir>` — saves a PNG of every page (demo data unless
//!   `WNVCTL_DEMO=0`) in
//!   every language to `<dir>`, then exits.

mod app;
mod data;
pub mod desktop;
mod theme;
mod widgets;

pub use app::App;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use eframe::egui;

use crate::i18n::{T, tr};
use crate::settings::Settings;
use desktop::Cmd;

/// Window icon (embedded PNG).
pub const ICON_PNG: &[u8] = include_bytes!("../../assets/icon-256.png");
/// Tray icon (embedded PNG).
pub const TRAY_PNG: &[u8] = include_bytes!("../../assets/icon-64.png");

/// RGBA pixels and size of a PNG.
pub fn decode_png(bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    if info.color_type != png::ColorType::Rgba {
        return None;
    }
    buf.truncate(info.buffer_size());
    Some((buf, info.width, info.height))
}

/// State shared between the window, the tray and the single-instance listener.
#[derive(Default)]
pub struct Shell {
    /// Context of the open window, if any.
    pub ctx: Mutex<Option<egui::Context>>,
    /// Quit requested from the tray.
    pub quit: AtomicBool,
}

fn env_size(var: &str, default: f32) -> f32 {
    std::env::var(var)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn native_options() -> eframe::NativeOptions {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("wnvctl")
        // Matches wnvctl.desktop, so Wayland compositors find the icon.
        .with_app_id("wnvctl")
        .with_inner_size([
            env_size("WNVCTL_WINDOW_WIDTH", 1120.0),
            env_size("WNVCTL_WINDOW_HEIGHT", 760.0),
        ])
        .with_min_inner_size([880.0, 600.0]);
    if let Some((rgba, width, height)) = decode_png(ICON_PNG) {
        viewport = viewport.with_icon(egui::IconData {
            rgba,
            width,
            height,
        });
    }
    eframe::NativeOptions {
        viewport,
        ..Default::default()
    }
}

/// Runs the GUI. With the tray enabled, closing the window keeps the program
/// in the tray: the window is destroyed and created again on "Show"
/// (Wayland cannot hide a window). Sampling stops while there is no window.
pub fn run() -> eframe::Result {
    let hidden_arg = std::env::args().any(|a| a == "--hidden");
    let screenshots = std::env::var_os("WNVCTL_SCREENSHOTS").is_some();
    let settings = Settings::load();
    let lang = settings.language;

    let (tx, rx) = mpsc::channel::<Cmd>();
    if !screenshots && !desktop::claim_single_instance(tx.clone()) {
        return Ok(()); // the running instance shows its window
    }
    let tray = !screenshots && (settings.close_to_tray || settings.start_hidden || hidden_arg);
    if tray {
        desktop::spawn_tray(
            tx.clone(),
            desktop::Labels {
                show: tr(lang, T::Show).into(),
                quit: tr(lang, T::Quit).into(),
                reset: tr(lang, T::ResetStock).into(),
                tooltip: "wnvctl".into(),
            },
        );
    }

    // Commands arriving while the window is open act on it directly; the
    // others wake the main loop below.
    let shell = Arc::new(Shell::default());
    let (main_tx, main_rx) = mpsc::channel::<Cmd>();
    {
        let shell = shell.clone();
        std::thread::spawn(move || {
            for cmd in rx {
                let ctx = shell.ctx.lock().ok().and_then(|c| c.clone());
                match (cmd, ctx) {
                    (Cmd::Show, Some(ctx)) => {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    }
                    (Cmd::Quit, Some(ctx)) => {
                        shell.quit.store(true, Ordering::SeqCst);
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    (cmd, None) => {
                        if main_tx.send(cmd).is_err() {
                            return;
                        }
                    }
                }
            }
        });
    }

    let mut visible = !tray || !(hidden_arg || settings.start_hidden);
    loop {
        if visible {
            let sh = shell.clone();
            eframe::run_native(
                "wnvctl",
                native_options(),
                Box::new(move |cc| Ok(Box::new(App::new(cc, sh)))),
            )?;
        }
        // The tray may have been switched off in the settings meanwhile.
        let keep = tray && Settings::load().close_to_tray;
        if shell.quit.load(Ordering::SeqCst) || !keep {
            return Ok(());
        }
        match main_rx.recv() {
            Ok(Cmd::Show) => visible = true,
            Ok(Cmd::Quit) | Err(_) => return Ok(()),
        }
    }
}
