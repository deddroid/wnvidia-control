//! Custom widgets: metric tiles, area charts, bars and the fan-curve editor.

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Mesh, Pos2, Rect, Sense, Shape, Stroke, Vec2,
    pos2, vec2,
};

use super::theme;
use crate::curve::{FanCurve, Point};

/// Filled area chart of `values` (oldest first) between `min` and `max`.
pub fn area_chart(
    ui: &mut egui::Ui,
    values: &[f32],
    (min, max): (f32, f32),
    color: Color32,
    size: Vec2,
) {
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    paint_area(ui.painter(), rect, values, (min, max), color, true);
}

fn paint_area(
    painter: &egui::Painter,
    rect: Rect,
    values: &[f32],
    (min, max): (f32, f32),
    color: Color32,
    fill: bool,
) {
    if values.len() < 2 {
        return;
    }
    let span = (max - min).max(1e-3);
    let dx = rect.width() / (values.len() - 1) as f32;
    let pts: Vec<Pos2> = values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let t = ((v - min) / span).clamp(0.0, 1.0);
            pos2(
                rect.left() + i as f32 * dx,
                rect.bottom() - t * rect.height(),
            )
        })
        .collect();
    if fill {
        // Vertical gradient: strong under the line, transparent at the bottom.
        let mut mesh = Mesh::default();
        let top = color.gamma_multiply(0.35);
        let bottom = color.gamma_multiply(0.0);
        for p in &pts {
            mesh.colored_vertex(*p, top);
            mesh.colored_vertex(pos2(p.x, rect.bottom()), bottom);
        }
        for i in 0..pts.len() as u32 - 1 {
            let a = i * 2;
            mesh.add_triangle(a, a + 1, a + 2);
            mesh.add_triangle(a + 1, a + 3, a + 2);
        }
        painter.add(Shape::mesh(mesh));
    }
    painter.add(Shape::line(pts, Stroke::new(1.8, color)));
}

/// A tile with a caption, a large value, a detail line and a mini chart.
pub struct Tile<'a> {
    pub caption: &'a str,
    pub value: String,
    pub unit: &'a str,
    pub detail: String,
    pub color: Color32,
    pub history: Vec<f32>,
    pub range: (f32, f32),
    /// 0..1 fill for a bar under the value (e.g. power / limit).
    pub bar: Option<f32>,
}

impl Tile<'_> {
    pub fn show(self, ui: &mut egui::Ui, width: f32) {
        egui::Frame::new()
            .fill(theme::CARD)
            .stroke(Stroke::new(1.0, theme::LINE))
            .corner_radius(CornerRadius::same(14))
            .inner_margin(egui::Margin::same(14))
            .show(ui, |ui| {
                ui.set_width(width - 30.0);
                ui.vertical(|ui| self.content(ui));
            });
    }

    fn content(self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let (dot, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
            ui.painter().circle_filled(dot.center(), 4.0, self.color);
            ui.label(theme::dim(self.caption.to_uppercase()));
        });
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label(
                egui::RichText::new(&self.value)
                    .size(30.0)
                    .strong()
                    .color(theme::TEXT),
            );
            ui.label(egui::RichText::new(self.unit).color(theme::TEXT_DIM));
        });
        if let Some(f) = self.bar {
            bar(ui, f, self.color, 5.0);
        }
        ui.label(theme::dim(&self.detail));
        ui.add_space(2.0);
        area_chart(
            ui,
            &self.history,
            self.range,
            self.color,
            vec2(ui.available_width(), 44.0),
        );
    }
}

/// A thin rounded progress bar.
pub fn bar(ui: &mut egui::Ui, fraction: f32, color: Color32, height: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let r = CornerRadius::same((height / 2.0) as u8);
    ui.painter().rect_filled(rect, r, theme::CARD_HI);
    let mut fill = rect;
    fill.set_width(rect.width() * fraction.clamp(0.0, 1.0));
    ui.painter().rect_filled(fill, r, color);
}

/// Segmented control: returns the clicked index, if any.
pub fn segmented(ui: &mut egui::Ui, labels: &[&str], selected: usize) -> Option<usize> {
    let mut clicked = None;
    egui::Frame::new()
        .fill(theme::BG)
        .corner_radius(CornerRadius::same(10))
        .inner_margin(egui::Margin::same(3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for (i, l) in labels.iter().enumerate() {
                    let on = i == selected;
                    let text = egui::RichText::new(*l).color(if on {
                        Color32::from_rgb(0x05, 0x1a, 0x0d)
                    } else {
                        theme::TEXT_DIM
                    });
                    let b = egui::Button::new(text)
                        .fill(if on {
                            theme::ACCENT
                        } else {
                            Color32::TRANSPARENT
                        })
                        .stroke(Stroke::NONE)
                        .min_size(vec2(96.0, 28.0));
                    if ui.add(b).clicked() {
                        clicked = Some(i);
                    }
                }
            });
        });
    clicked
}

// ---- fan curve editor ----------------------------------------------------------

const T_MIN: f32 = 20.0;
const T_MAX: f32 = 100.0;

pub struct CurveView {
    /// Live (temperature, fan speed) marker.
    pub live: Option<(f32, f32)>,
    /// Lowest speed the fans accept: the area below is shaded.
    pub min_speed: f32,
    /// Driver slowdown temperature, drawn as a red line.
    pub slowdown: Option<f32>,
}

/// Interactive editor. Returns true when the curve changed.
pub fn curve_editor(ui: &mut egui::Ui, curve: &mut FanCurve, view: &CurveView) -> bool {
    let id = ui.make_persistent_id("fan-curve-drag");
    let height = (ui.available_width() * 0.42).clamp(220.0, 360.0);
    let (outer, resp) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click_and_drag());
    let plot = Rect::from_min_max(outer.min + vec2(44.0, 10.0), outer.max - vec2(12.0, 28.0));
    let painter = ui.painter_at(outer);

    let to_screen = |p: Point| {
        pos2(
            plot.left() + (p.temp - T_MIN) / (T_MAX - T_MIN) * plot.width(),
            plot.bottom() - p.speed / 100.0 * plot.height(),
        )
    };
    let from_screen = |s: Pos2| Point {
        temp: (T_MIN + (s.x - plot.left()) / plot.width() * (T_MAX - T_MIN)).round(),
        speed: ((plot.bottom() - s.y) / plot.height() * 100.0).round(),
    };

    // Grid and axes.
    painter.rect_filled(plot, CornerRadius::same(6), theme::BG);
    let font = FontId::proportional(11.0);
    for s in (0..=100).step_by(20) {
        let y = to_screen(Point::new(T_MIN, s as f32)).y;
        painter.line_segment(
            [pos2(plot.left(), y), pos2(plot.right(), y)],
            Stroke::new(1.0, theme::LINE),
        );
        painter.text(
            pos2(plot.left() - 8.0, y),
            Align2::RIGHT_CENTER,
            format!("{s}%"),
            font.clone(),
            theme::TEXT_DIM,
        );
    }
    for t in (T_MIN as u32..=T_MAX as u32).step_by(10) {
        let x = to_screen(Point::new(t as f32, 0.0)).x;
        painter.line_segment(
            [pos2(x, plot.top()), pos2(x, plot.bottom())],
            Stroke::new(1.0, theme::LINE.gamma_multiply(0.6)),
        );
        painter.text(
            pos2(x, plot.bottom() + 6.0),
            Align2::CENTER_TOP,
            format!("{t}°"),
            font.clone(),
            theme::TEXT_DIM,
        );
    }
    if view.min_speed > 0.0 {
        let y = to_screen(Point::new(T_MIN, view.min_speed)).y;
        painter.rect_filled(
            Rect::from_min_max(pos2(plot.left(), y), plot.max),
            CornerRadius::ZERO,
            theme::ERR.gamma_multiply(0.07),
        );
    }
    if let Some(t) = view.slowdown {
        let x = to_screen(Point::new(t.min(T_MAX), 0.0)).x;
        painter.line_segment(
            [pos2(x, plot.top()), pos2(x, plot.bottom())],
            Stroke::new(1.5, theme::ERR.gamma_multiply(0.7)),
        );
    }

    // Curve (sampled, so the flat ends are drawn too).
    let samples: Vec<f32> = (0..=160)
        .map(|i| curve.speed_at(T_MIN + i as f32 * (T_MAX - T_MIN) / 160.0))
        .collect();
    paint_area(&painter, plot, &samples, (0.0, 100.0), theme::C_FAN, true);

    // Interaction.
    let mut points: Vec<Point> = curve.points().to_vec();
    let hover = resp.hover_pos();
    let near = |pos: Pos2, pts: &[Point]| {
        pts.iter()
            .enumerate()
            .map(|(i, p)| (i, to_screen(*p).distance(pos)))
            .filter(|(_, d)| *d < 14.0)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    };
    let mut dragging: Option<usize> = ui.data(|d| d.get_temp(id));
    let mut changed = false;
    if resp.drag_started()
        && let Some(pos) = resp.interact_pointer_pos()
    {
        dragging = near(pos, &points);
    }
    if let (Some(i), Some(pos)) = (dragging, resp.interact_pointer_pos())
        && resp.dragged()
        && i < points.len()
    {
        let mut p = from_screen(pos);
        // Keep points ordered and at distinct temperatures.
        let lo = if i > 0 { points[i - 1].temp + 1.0 } else { 0.0 };
        let hi = if i + 1 < points.len() {
            points[i + 1].temp - 1.0
        } else {
            110.0
        };
        p.temp = p.temp.clamp(lo.max(T_MIN), hi.min(T_MAX));
        p.speed = p.speed.clamp(0.0, 100.0);
        if points[i] != p {
            points[i] = p;
            changed = true;
        }
    }
    if resp.drag_stopped() {
        dragging = None;
    }
    if resp.double_clicked()
        && let Some(pos) = resp.interact_pointer_pos()
        && plot.contains(pos)
        && near(pos, &points).is_none()
    {
        let p = from_screen(pos);
        if !points.iter().any(|q| q.temp == p.temp) && points.len() < 16 {
            points.push(p);
            changed = true;
        }
    }
    if resp.secondary_clicked()
        && let Some(pos) = resp.interact_pointer_pos()
        && let Some(i) = near(pos, &points)
        && points.len() > 2
    {
        points.remove(i);
        changed = true;
    }
    ui.data_mut(|d| match dragging {
        Some(i) => {
            d.insert_temp(id, i);
        }
        None => d.remove::<usize>(id),
    });
    if changed && let Ok(c) = FanCurve::new(points.clone()) {
        *curve = c;
    }

    // Points on top.
    let hovered = hover.and_then(|h| near(h, curve.points()));
    for (i, p) in curve.points().iter().enumerate() {
        let s = to_screen(*p);
        let active = Some(i) == dragging || Some(i) == hovered;
        painter.circle_filled(s, if active { 7.5 } else { 6.0 }, theme::BG);
        painter.circle_stroke(
            s,
            if active { 7.5 } else { 6.0 },
            Stroke::new(2.5, theme::C_FAN),
        );
        if active {
            painter.text(
                s + vec2(0.0, -14.0),
                Align2::CENTER_BOTTOM,
                format!("{:.0}° · {:.0}%", p.temp, p.speed),
                FontId::proportional(12.0),
                theme::TEXT,
            );
        }
    }
    if hovered.is_some() || dragging.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    }
    if let Some((t, s)) = view.live {
        let p = to_screen(Point::new(t.clamp(T_MIN, T_MAX), s.clamp(0.0, 100.0)));
        painter.circle_filled(p, 5.0, theme::C_TEMP);
        painter.circle_stroke(p, 9.0, Stroke::new(1.5, theme::C_TEMP.gamma_multiply(0.5)));
    }
    changed
}
