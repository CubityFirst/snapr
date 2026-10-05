//! The Stats page: screenshots and recordings taken, when in the day and
//! week they were taken, and how often the tools were used.

use egui::{Color32, CornerRadius, Rect, RichText, Sense, Stroke, pos2, vec2};

use crate::stats::{Captures, Stats, Tally, WEEKDAYS};

/// Gap between stacked segments and between tiles.
const GAP: f32 = 2.0;

/// Screenshots, recordings and other files, in categorical order, stepped
/// for the light or dark surface.
fn series_colors(dark: bool) -> [Color32; 3] {
    if dark {
        [
            Color32::from_rgb(0x39, 0x87, 0xe5),
            Color32::from_rgb(0xd9, 0x59, 0x26),
            Color32::from_rgb(0x19, 0x9e, 0x70),
        ]
    } else {
        [
            Color32::from_rgb(0x2a, 0x78, 0xd6),
            Color32::from_rgb(0xeb, 0x68, 0x34),
            Color32::from_rgb(0x1b, 0xaf, 0x7a),
        ]
    }
}

const SERIES: [&str; 3] = ["Screenshots", "Recordings", "Other files"];

fn parts(t: &Tally) -> [u64; 3] {
    [t.screenshots, t.recordings, t.other]
}

pub fn ui(ui: &mut egui::Ui, stats: &Stats) {
    ui.horizontal(|ui| {
        ui.heading("Stats");
        ui.add_space(12.0);
        ui.label(RichText::new(format!("Counting since {}", date(stats.since))).weak());
    });
    ui.add_space(8.0);
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            captures_ui(ui, &stats.captures);
            ui.add_space(20.0);
            tools_ui(ui, stats);
            ui.add_space(8.0);
        });
}

fn captures_ui(ui: &mut egui::Ui, all: &Captures) {
    let colors = series_colors(ui.visuals().dark_mode);
    let total = all.total;
    let share = |n: u64| {
        if total.total() == 0 {
            String::new()
        } else {
            format!("{:.1}% of captures", n as f64 * 100.0 / total.total() as f64)
        }
    };
    let mut tiles = vec![
        ("Captures".to_string(), total.total(), String::new()),
        (
            SERIES[0].to_string(),
            total.screenshots,
            share(total.screenshots),
        ),
        (SERIES[1].to_string(), total.recordings, share(total.recordings)),
    ];
    if total.other > 0 {
        tiles.push((SERIES[2].to_string(), total.other, share(total.other)));
    }
    tile_row(ui, &tiles);
    ui.add_space(10.0);
    split_bar(ui, &total, colors);
    ui.add_space(6.0);
    legend(ui, &total, colors);

    ui.add_space(20.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Time of day").strong());
        if let Some(h) = busiest(&all.hours) {
            ui.label(RichText::new(format!("busiest {h:02}:00\u{2013}{:02}:00", (h + 1) % 24)).weak());
        }
    });
    ui.add_space(4.0);
    let hours: Vec<Bar> = all
        .hours
        .iter()
        .enumerate()
        .map(|(h, t)| Bar {
            label: (h % 3 == 0).then(|| format!("{h:02}:00")),
            title: format!("{h:02}:00\u{2013}{:02}:00", (h + 1) % 24),
            tally: *t,
        })
        .collect();
    bar_chart(ui, &hours, colors, 150.0);

    ui.add_space(16.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Day of the week").strong());
        if let Some(d) = busiest(&all.days) {
            ui.label(RichText::new(format!("busiest on {}s", WEEKDAYS[d])).weak());
        }
    });
    ui.add_space(4.0);
    let days: Vec<Bar> = all
        .days
        .iter()
        .zip(WEEKDAYS)
        .map(|(t, d)| Bar {
            label: Some(d[..3].to_string()),
            title: d.to_string(),
            tally: *t,
        })
        .collect();
    bar_chart(ui, &days, colors, 110.0);
}

fn tools_ui(ui: &mut egui::Ui, s: &Stats) {
    ui.label(RichText::new("Tool Use").strong());
    ui.add_space(6.0);
    let found = match s.qr_scans {
        0 => String::new(),
        n => format!("{:.1} per scan", s.qr_codes as f64 / n as f64),
    };
    tile_row(
        ui,
        &[
            ("QR scans".into(), s.qr_scans, String::new()),
            ("QR codes read".into(), s.qr_codes, found),
            ("Colours picked".into(), s.colors_picked, String::new()),
            ("Images pinned".into(), s.pins, String::new()),
        ],
    );
}

/// Equal-width tiles of a label, a big number and a note under it.
fn tile_row(ui: &mut egui::Ui, tiles: &[(String, u64, String)]) {
    let n = tiles.len() as f32;
    let spacing = 8.0;
    let width = ((ui.available_width() - spacing * (n - 1.0)) / n).max(80.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = spacing;
        for (label, value, note) in tiles {
            egui::Frame::new()
                .fill(ui.visuals().faint_bg_color)
                .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
                .corner_radius(8.0)
                .inner_margin(12.0)
                .show(ui, |ui| {
                    // Less the margins and the stroke.
                    ui.set_width(width - 26.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.label(RichText::new(label).weak());
                        ui.label(RichText::new(thousands(*value)).size(24.0).strong());
                        // Keep the tiles the same height with or without a note.
                        let note = if note.is_empty() { " " } else { note };
                        ui.label(RichText::new(note).small().weak());
                    });
                });
        }
    });
}

/// One bar across the page, split into screenshots, recordings and other.
fn split_bar(ui: &mut egui::Ui, t: &Tally, colors: [Color32; 3]) {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 12.0), Sense::hover());
    let p = ui.painter();
    let total = t.total();
    if total == 0 {
        p.rect_filled(rect, 4.0, ui.visuals().faint_bg_color);
        return;
    }
    let values = parts(t);
    let shown: Vec<usize> = (0..3).filter(|&i| values[i] > 0).collect();
    let gaps = GAP * (shown.len() as f32 - 1.0);
    let mut x = rect.left();
    for (n, &i) in shown.iter().enumerate() {
        let w = (rect.width() - gaps) * values[i] as f32 / total as f32;
        let r = CornerRadius {
            nw: if n == 0 { 4 } else { 0 },
            sw: if n == 0 { 4 } else { 0 },
            ne: if n + 1 == shown.len() { 4 } else { 0 },
            se: if n + 1 == shown.len() { 4 } else { 0 },
        };
        let seg = Rect::from_min_max(pos2(x, rect.top()), pos2(x + w.max(1.0), rect.bottom()));
        p.rect_filled(seg, r, colors[i]);
        x += w + GAP;
    }
    resp.on_hover_ui(|ui| tally_tooltip(ui, "All captures", t, colors));
}

/// A swatch, name and count for each kind there's any of.
fn legend(ui: &mut egui::Ui, t: &Tally, colors: [Color32; 3]) {
    ui.horizontal(|ui| {
        for (i, v) in parts(t).into_iter().enumerate() {
            if i == 2 && v == 0 {
                continue;
            }
            let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
            ui.painter().rect_filled(r, 2.0, colors[i]);
            ui.label(format!("{} {}", SERIES[i], thousands(v)));
            ui.add_space(10.0);
        }
    });
}

fn tally_tooltip(ui: &mut egui::Ui, title: &str, t: &Tally, colors: [Color32; 3]) {
    ui.label(RichText::new(title).strong());
    egui::Grid::new("stats-tip").num_columns(2).spacing([16.0, 2.0]).show(ui, |ui| {
        for (i, v) in parts(t).into_iter().enumerate() {
            if i == 2 && v == 0 {
                continue;
            }
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                ui.painter().rect_filled(r, 2.0, colors[i]);
                ui.label(SERIES[i]);
            });
            ui.label(thousands(v));
            ui.end_row();
        }
        ui.label("Total");
        ui.label(RichText::new(thousands(t.total())).strong());
        ui.end_row();
    });
}

struct Bar {
    /// Shown under the bar, if any.
    label: Option<String>,
    /// Heads its tooltip.
    title: String,
    tally: Tally,
}

/// Stacked columns with a recessive grid, a label under some and a tooltip
/// on each.
fn bar_chart(ui: &mut egui::Ui, bars: &[Bar], colors: [Color32; 3], height: f32) {
    let axis_w = 44.0;
    let label_h = 18.0;
    let (rect, resp) =
        ui.allocate_exact_size(vec2(ui.available_width(), height + label_h), Sense::hover());
    let plot = Rect::from_min_max(
        pos2(rect.left() + axis_w, rect.top() + 6.0),
        pos2(rect.right(), rect.bottom() - label_h),
    );
    let visuals = ui.visuals().clone();
    let p = ui.painter();
    let grid = visuals.widgets.noninteractive.bg_stroke.color;
    let weak = visuals.weak_text_color();
    let small = egui::FontId::proportional(11.0);
    let max = bars.iter().map(|b| b.tally.total()).max().unwrap_or(0);
    if max == 0 {
        p.line_segment([plot.left_bottom(), plot.right_bottom()], Stroke::new(1.0, grid));
        p.text(
            plot.center(),
            egui::Align2::CENTER_CENTER,
            "Nothing yet",
            egui::FontId::proportional(13.0),
            weak,
        );
        return;
    }
    let step = nice_step(max as f64 / 3.0);
    let top = (max as f64 / step).ceil() * step;
    let y_of = |v: f64| plot.bottom() - (v / top) as f32 * plot.height();
    let mut v = 0.0;
    while v <= top + 0.5 {
        let y = y_of(v);
        p.line_segment([pos2(plot.left(), y), pos2(plot.right(), y)], Stroke::new(1.0, grid));
        p.text(
            pos2(plot.left() - 8.0, y),
            egui::Align2::RIGHT_CENTER,
            compact(v as u64),
            small.clone(),
            weak,
        );
        v += step;
    }

    let slot = plot.width() / bars.len() as f32;
    let bar_w = (slot * 0.68).min(36.0);
    let hovered = resp
        .hover_pos()
        .filter(|pos| pos.x >= plot.left())
        .map(|pos| (((pos.x - plot.left()) / slot) as usize).min(bars.len() - 1));
    for (i, bar) in bars.iter().enumerate() {
        let cx = plot.left() + slot * (i as f32 + 0.5);
        if hovered == Some(i) {
            let col = Rect::from_center_size(pos2(cx, plot.center().y), vec2(slot, plot.height()));
            p.rect_filled(col, 4.0, visuals.widgets.hovered.weak_bg_fill.gamma_multiply(0.5));
        }
        let values = parts(&bar.tally);
        let shown: Vec<usize> = (0..3).filter(|&k| values[k] > 0).collect();
        let mut base = 0.0;
        for (n, &k) in shown.iter().enumerate() {
            let y0 = y_of(base);
            base += values[k] as f64;
            let last = n + 1 == shown.len();
            // A surface gap between segments, from the one below's top.
            let y1 = y_of(base) + if last { 0.0 } else { GAP };
            if y0 - y1 < 0.5 {
                continue;
            }
            let r = if last {
                CornerRadius { nw: 3, ne: 3, sw: 0, se: 0 }
            } else {
                CornerRadius::ZERO
            };
            let seg = Rect::from_min_max(pos2(cx - bar_w / 2.0, y1), pos2(cx + bar_w / 2.0, y0));
            p.rect_filled(seg, r, colors[k]);
        }
        if let Some(label) = &bar.label {
            p.text(
                pos2(cx, plot.bottom() + 4.0),
                egui::Align2::CENTER_TOP,
                label,
                small.clone(),
                weak,
            );
        }
    }
    if let Some(i) = hovered {
        let bar = &bars[i];
        resp.on_hover_ui_at_pointer(|ui| tally_tooltip(ui, &bar.title, &bar.tally, colors));
    }
}

/// The busiest slot, if anything was taken at all.
fn busiest(slots: &[Tally]) -> Option<usize> {
    let (i, t) = slots.iter().enumerate().max_by_key(|(_, t)| t.total())?;
    (t.total() > 0).then_some(i)
}

/// A round grid step (1, 2 or 5 × a power of ten) of about `rough`.
fn nice_step(rough: f64) -> f64 {
    if rough <= 1.0 {
        return 1.0;
    }
    let mag = 10f64.powf(rough.log10().floor());
    let f = rough / mag;
    let nice = if f <= 1.0 {
        1.0
    } else if f <= 2.0 {
        2.0
    } else if f <= 5.0 {
        5.0
    } else {
        10.0
    };
    nice * mag
}

/// 1,234,567.
fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// 950, 1.5k, 12k, 3.2M: for axis labels.
fn compact(n: u64) -> String {
    let (v, unit) = match n {
        0..1_000 => return n.to_string(),
        1_000..1_000_000 => (n as f64 / 1e3, "k"),
        _ => (n as f64 / 1e6, "M"),
    };
    if v < 10.0 && v.fract() != 0.0 {
        format!("{v:.1}{unit}")
    } else {
        format!("{v:.0}{unit}")
    }
}

/// A Unix time as a local date, like 5 Oct 2026.
fn date(secs: i64) -> String {
    chrono::DateTime::from_timestamp(secs, 0)
        .map(|d| {
            d.with_timezone(&chrono::Local)
                .format("%-d %b %Y")
                .to_string()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_numbers() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(112172), "112,172");
        assert_eq!(thousands(1234567), "1,234,567");
        assert_eq!(compact(950), "950");
        assert_eq!(compact(1500), "1.5k");
        assert_eq!(compact(2000), "2k");
        assert_eq!(compact(12000), "12k");
        assert_eq!(nice_step(2836.0), 5000.0);
        assert_eq!(nice_step(1.4), 2.0);
    }

    /// Renders the page with a busy year's numbers to
    /// `target/stats-preview.png`: `cargo test stats_preview -- --ignored`.
    #[test]
    #[ignore]
    fn stats_preview() {
        let mut captures = Captures::default();
        for (h, t) in captures.hours.iter_mut().enumerate() {
            let busy = [6, 4, 2, 1, 1, 0, 0, 1, 2, 3, 5, 4, 5, 6, 6, 6, 6, 6, 7, 7, 7, 8, 8, 8][h];
            *t = Tally {
                screenshots: busy * 1000 + 133,
                recordings: busy * 18 + 5,
                other: busy * 3,
            };
        }
        for (d, t) in captures.days.iter_mut().enumerate() {
            *t = Tally {
                screenshots: 15000 + d as u64 * 300,
                recordings: 300,
                other: 50,
            };
        }
        captures.total = Tally {
            screenshots: 109_685,
            recordings: 2_253,
            other: 234,
        };
        let stats = Stats {
            captures,
            qr_scans: 12,
            qr_codes: 15,
            colors_picked: 48,
            ..Stats::default()
        };
        crate::preview::render("stats-preview", [900, 900], 1.0, |root| {
            egui::CentralPanel::default()
                .frame(egui::Frame::central_panel(root.style()).inner_margin(egui::Margin::symmetric(20, 16)))
                .show(root, |ui| super::ui(ui, &stats));
        });
    }
}
