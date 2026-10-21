//! Visual style: a dark, rounded theme, one colour per metric, and fallback
//! fonts for symbols.

use eframe::egui::{self, Color32, CornerRadius, FontData, FontFamily, Stroke};
use egui::epaint::text::{FontInsert, FontPriority, InsertFontFamily};

pub const ACCENT: Color32 = Color32::from_rgb(0x4a, 0xde, 0x80);
pub const ACCENT_DARK: Color32 = Color32::from_rgb(0x16, 0x3a, 0x26);
pub const BG: Color32 = Color32::from_rgb(0x0f, 0x11, 0x15);
pub const PANEL: Color32 = Color32::from_rgb(0x15, 0x18, 0x1e);
pub const CARD: Color32 = Color32::from_rgb(0x1c, 0x20, 0x28);
pub const CARD_HI: Color32 = Color32::from_rgb(0x24, 0x29, 0x33);
pub const LINE: Color32 = Color32::from_rgb(0x2c, 0x32, 0x3e);
pub const TEXT: Color32 = Color32::from_rgb(0xe6, 0xe9, 0xef);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x8b, 0x93, 0xa5);
pub const OK: Color32 = Color32::from_rgb(0x4a, 0xde, 0x80);
pub const WARN: Color32 = Color32::from_rgb(0xfb, 0xbf, 0x24);
pub const ERR: Color32 = Color32::from_rgb(0xf8, 0x71, 0x71);

pub const C_TEMP: Color32 = Color32::from_rgb(0xfb, 0x92, 0x3c);
pub const C_CORE: Color32 = Color32::from_rgb(0x60, 0xa5, 0xfa);
pub const C_MEM: Color32 = Color32::from_rgb(0xa7, 0x8b, 0xfa);
pub const C_POWER: Color32 = Color32::from_rgb(0xfa, 0xcc, 0x15);
pub const C_FAN: Color32 = Color32::from_rgb(0x22, 0xd3, 0xee);
pub const C_LOAD: Color32 = Color32::from_rgb(0x4a, 0xde, 0x80);
pub const C_VRAM: Color32 = Color32::from_rgb(0xf4, 0x72, 0xb6);

pub fn apply(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = PANEL;
    v.window_fill = CARD;
    v.extreme_bg_color = BG;
    v.faint_bg_color = CARD;
    v.override_text_color = Some(TEXT);
    v.selection.bg_fill = ACCENT_DARK;
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.hyperlink_color = ACCENT;
    v.slider_trailing_fill = true;
    let r = CornerRadius::same(8);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = r;
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
    v.widgets.inactive.weak_bg_fill = CARD_HI;
    v.widgets.inactive.bg_fill = Color32::from_rgb(0x33, 0x3a, 0x47);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x2e, 0x34, 0x40);
    v.widgets.hovered.bg_fill = Color32::from_rgb(0x3d, 0x45, 0x54);
    v.widgets.active.weak_bg_fill = ACCENT_DARK;
    v.widgets.active.bg_fill = ACCENT;
    v.window_corner_radius = CornerRadius::same(12);
    ctx.set_theme(egui::Theme::Dark);
    ctx.set_visuals_of(egui::Theme::Dark, v);
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = egui::vec2(10.0, 8.0);
        s.spacing.button_padding = egui::vec2(14.0, 7.0);
        s.spacing.slider_width = 300.0;
        s.spacing.interact_size.y = 28.0;
        use egui::{FontId, TextStyle};
        s.text_styles
            .insert(TextStyle::Heading, FontId::proportional(21.0));
        s.text_styles
            .insert(TextStyle::Body, FontId::proportional(14.5));
        s.text_styles
            .insert(TextStyle::Button, FontId::proportional(14.5));
        s.text_styles
            .insert(TextStyle::Small, FontId::proportional(12.0));
    });
    install_fallback_fonts(ctx);
}

/// System fonts that cover arrows, dots and other symbols egui's bundled
/// fonts lack. The first one found is added as a low-priority fallback.
fn install_fallback_fonts(ctx: &egui::Context) {
    const CANDIDATES: &[&str] = &[
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/noto/NotoSansSymbols2-Regular.ttf",
        "/usr/share/fonts/google-noto/NotoSansSymbols2-Regular.ttf",
    ];
    for path in CANDIDATES {
        if let Ok(bytes) = std::fs::read(path) {
            ctx.add_font(FontInsert::new(
                "symbols-fallback",
                FontData::from_owned(bytes),
                vec![
                    InsertFontFamily {
                        family: FontFamily::Proportional,
                        priority: FontPriority::Lowest,
                    },
                    InsertFontFamily {
                        family: FontFamily::Monospace,
                        priority: FontPriority::Lowest,
                    },
                ],
            ));
            return;
        }
    }
}

/// A rounded card filling the available width.
pub fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    // The margin is added around the content, so reserve it up front.
    let width = (ui.available_width() - 36.0).max(0.0);
    egui::Frame::new()
        .fill(CARD)
        .stroke(Stroke::new(1.0, LINE))
        .corner_radius(CornerRadius::same(14))
        .inner_margin(egui::Margin::same(18))
        .show(ui, |ui| {
            ui.set_width(width);
            add(ui)
        })
        .inner
}

/// Small grey caption.
pub fn dim(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text).color(TEXT_DIM).small()
}

/// Card title.
pub fn title(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text).size(16.0).strong()
}

/// A green call-to-action button.
pub fn primary(text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(
        egui::RichText::new(text.into())
            .color(Color32::from_rgb(0x05, 0x1a, 0x0d))
            .strong(),
    )
    .fill(ACCENT)
    .min_size(egui::vec2(110.0, 32.0))
}

/// A small rounded label ("chip").
pub fn chip(ui: &mut egui::Ui, text: impl Into<String>, color: Color32) {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.16))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(egui::Margin::symmetric(9, 3))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).color(color).small());
        });
}
