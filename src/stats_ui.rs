//! The Stats page: screenshots and recordings taken, on which days and at
//! what time of day and week, and how often the tools were used.

use std::collections::BTreeMap;

use chrono::{Datelike, Duration, Local, NaiveDate};
use egui::{Align, Align2, Color32, CornerRadius, FontId, Layout, Rect, RichText, Sense, Stroke, StrokeKind, pos2, vec2};

use crate::stats::{Captures, Stats, Tally, WEEKDAYS};
use crate::theme;

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

/// Screenshots and recordings a step further from the surface, for the
/// busiest bar.
fn peak_colors(dark: bool) -> [Color32; 2] {
    if dark {
        [Color32::from_rgb(0x86, 0xb6, 0xef), Color32::from_rgb(0xf2, 0x9a, 0x72)]
    } else {
        [Color32::from_rgb(0x18, 0x4f, 0x95), Color32::from_rgb(0xb2, 0x46, 0x1a)]
    }
}

/// The calendar's fills, from a day with nothing to the busiest: one blue
/// ramp, lighter on dark and darker on light.
fn ramp(dark: bool) -> [Color32; 5] {
    if dark {
        [
            Color32::from_rgb(0x26, 0x27, 0x2b),
            Color32::from_rgb(0x18, 0x4f, 0x95),
            Color32::from_rgb(0x25, 0x6a, 0xbf),
            Color32::from_rgb(0x39, 0x87, 0xe5),
            Color32::from_rgb(0x86, 0xb6, 0xef),
        ]
    } else {
        [
            Color32::from_rgb(0xec, 0xee, 0xf1),
            Color32::from_rgb(0xb7, 0xd3, 0xf6),
            Color32::from_rgb(0x6d, 0xa7, 0xec),
            Color32::from_rgb(0x2a, 0x78, 0xd6),
            Color32::from_rgb(0x18, 0x4f, 0x95),
        ]
    }
}

const SERIES: [&str; 3] = ["Screenshots", "Recordings", "Other files"];

fn parts(t: &Tally) -> [u64; 3] {
    [t.screenshots, t.recordings, t.other]
}

/// Which captures the time-of-day chart counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Show {
    #[default]
    All,
    Screenshots,
    Recordings,
}

impl Show {
    fn count(self, t: &Tally) -> u64 {
        match self {
            Show::All => t.total(),
            Show::Screenshots => t.screenshots,
            Show::Recordings => t.recordings,
        }
    }
}

pub fn ui(ui: &mut egui::Ui, stats: &Stats) {
    let today = Local::now().date_naive();
    let days = days_since(stats.since, today);
    ui.horizontal(|ui| {
        ui.heading("Stats");
        ui.add_space(12.0);
        ui.label(RichText::new(format!("Counting since {} \u{b7} {}", date(stats.since), plural(days, "day"))).weak());
    });
    ui.add_space(8.0);
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            summary_ui(ui, &stats.captures.total, days);
            ui.add_space(8.0);
            highlights_ui(ui, &stats.daily, today);
            ui.add_space(20.0);
            calendar_ui(ui, &stats.daily, today);
            ui.add_space(20.0);
            hours_ui(ui, &stats.captures);
            ui.add_space(20.0);
            ui.columns(2, |cols| {
                weekdays_ui(&mut cols[0], &stats.captures);
                tools_ui(&mut cols[1], stats);
            });
            ui.add_space(8.0);
        });
}

/// The page's cards: the settings cards' fill and outline, padded.
fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let p = theme::palette(ui);
    egui::Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0, p.divider))
        .corner_radius(8)
        .inner_margin(egui::Margin::symmetric(14, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// A section's title with a weak note beside it, and `right` at the far end.
fn heading(ui: &mut egui::Ui, title: &str, note: &str, right: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).strong());
        ui.label(RichText::new(note).weak());
        ui.with_layout(Layout::right_to_left(Align::Center), right);
    });
    ui.add_space(2.0);
}

/// The total, how many a day, and its split into screenshots and recordings.
fn summary_ui(ui: &mut egui::Ui, total: &Tally, days: u64) {
    let colors = series_colors(ui.visuals().dark_mode);
    card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label(RichText::new("Captures").weak());
                ui.label(RichText::new(thousands(total.total())).size(34.0).strong());
                ui.label(RichText::new(rate(total.total(), days)).small().weak());
            });
            ui.add_space(24.0);
            ui.vertical(|ui| {
                ui.add_space(18.0);
                split_bar(ui, total, colors);
                ui.add_space(6.0);
                legend(ui, total, colors);
            });
        });
    });
}

/// "about 26 a day", or a week for the occasional user.
fn rate(total: u64, days: u64) -> String {
    if total == 0 {
        return "Nothing yet".into();
    }
    let a_day = total as f64 / days.max(1) as f64;
    if a_day >= 1.0 {
        format!("about {} a day", round(a_day))
    } else {
        format!("about {} a week", round(a_day * 7.0))
    }
}

/// What the tiles under the summary say, from the daily log.
#[derive(Debug, Default, PartialEq)]
struct Highlights {
    today: Tally,
    /// Monday to today, and the same days the week before.
    week: u64,
    last_week: u64,
    /// Days in a row with a capture, up to today, or yesterday while today
    /// has none yet.
    streak: u64,
    /// The longest run, and the day it ended.
    longest: Option<(u64, NaiveDate)>,
    busiest: Option<(NaiveDate, u64)>,
}

fn highlights(daily: &BTreeMap<NaiveDate, Tally>, today: NaiveDate) -> Highlights {
    let total = |d: NaiveDate| daily.get(&d).map_or(0, Tally::total);
    let sum = |from: NaiveDate, to: NaiveDate| daily.range(from..=to).map(|(_, t)| t.total()).sum();
    let monday = monday_of(today);
    let mut streak = 0;
    let mut day = if total(today) > 0 { today } else { today - Duration::days(1) };
    while total(day) > 0 {
        streak += 1;
        day -= Duration::days(1);
    }
    let mut longest: Option<(u64, NaiveDate)> = None;
    let mut run: Option<(u64, NaiveDate)> = None;
    for (&d, _) in daily.iter().filter(|(_, t)| t.total() > 0) {
        let n = match run {
            Some((n, last)) if last + Duration::days(1) == d => n + 1,
            _ => 1,
        };
        run = Some((n, d));
        if longest.is_none_or(|(best, _)| n > best) {
            longest = Some((n, d));
        }
    }
    Highlights {
        today: daily.get(&today).copied().unwrap_or_default(),
        week: sum(monday, today),
        last_week: sum(monday - Duration::days(7), today - Duration::days(7)),
        streak,
        longest,
        busiest: daily.iter().filter(|(_, t)| t.total() > 0).map(|(&d, t)| (d, t.total())).max_by_key(|&(_, n)| n),
    }
}

/// Today, this week against last, the streak and the busiest day, as a row
/// of tiles, their notes shortened when a tile is too narrow for them.
fn highlights_ui(ui: &mut egui::Ui, daily: &BTreeMap<NaiveDate, Tally>, today: NaiveDate) {
    let h = highlights(daily, today);
    let (up, down) = if ui.visuals().dark_mode {
        (Color32::from_rgb(0x4c, 0xc3, 0x8a), Color32::from_rgb(0xff, 0x8a, 0x8f))
    } else {
        (Color32::from_rgb(0x1a, 0x7f, 0x4f), Color32::from_rgb(0xc8, 0x32, 0x3b))
    };
    let weak = |s: String| RichText::new(s).small().weak();
    let today_note = match (h.today.total(), h.today.recordings) {
        (0, _) => weak("nothing yet".into()),
        (_, 0) => weak("screenshots only".into()),
        (_, r) => weak(plural(r, "recording")),
    };
    // Up or down on last week, drawn: the font has no arrows.
    let mut arrow: Option<(bool, Color32)> = None;
    let week_notes: Vec<Vec<RichText>> = if h.last_week == 0 {
        vec![vec![weak(if h.week == 0 { "none last week either" } else { "none by now last week" }.into())]]
    } else {
        let change = (h.week as f64 - h.last_week as f64) / h.last_week as f64 * 100.0;
        let color = if change >= 0.0 { up } else { down };
        arrow = Some((change >= 0.0, color));
        let pct = RichText::new(format!("{:.0}%", change.abs())).small().color(color);
        vec![
            vec![pct.clone(), weak("vs same time last week".into())],
            vec![pct, weak("vs last week".into())],
        ]
    };
    let streak_notes = match h.longest {
        Some((n, _)) if n == h.streak => vec![weak("your longest yet".into())],
        Some((n, end)) => vec![
            weak(format!("longest {}, to {}", thousands(n), end.format("%-d %b %Y"))),
            weak(format!("longest {}", thousands(n))),
        ],
        None => vec![weak("from your first capture".into())],
    };
    let busiest_notes = match h.busiest {
        Some((d, _)) => vec![weak(d.format("%a %-d %b %Y").to_string()), weak(d.format("%-d %b %Y").to_string())],
        None => vec![weak("nothing yet".into())],
    };
    // Each tile's note, then shorter ones for when it doesn't fit.
    let tiles: [(&str, String, Vec<Vec<RichText>>); 4] = [
        ("Today", thousands(h.today.total()), vec![vec![today_note]]),
        ("This week", thousands(h.week), week_notes),
        ("Streak", plural(h.streak, "day"), streak_notes.into_iter().map(|n| vec![n]).collect()),
        (
            "Busiest day",
            h.busiest.map_or("\u{2014}".into(), |(_, n)| thousands(n)),
            busiest_notes.into_iter().map(|n| vec![n]).collect(),
        ),
    ];
    let tile = |ui: &mut egui::Ui, (label, value, notes): &(&str, String, Vec<Vec<RichText>>)| {
        card(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.spacing_mut().interact_size.y = 0.0;
            ui.label(RichText::new(*label).weak());
            ui.label(RichText::new(value).size(24.0).strong());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let marker = if *label == "This week" && arrow.is_some() { 7.0 + 5.0 } else { 0.0 };
                let width = |note: &Vec<RichText>| {
                    let text: f32 = note
                        .iter()
                        .map(|part| {
                            egui::WidgetText::from(part.clone())
                                .into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Body)
                                .size()
                                .x
                        })
                        .sum();
                    marker + text + 4.0 * note.len().saturating_sub(1) as f32
                };
                let room = ui.available_width();
                let note = notes.iter().find(|n| width(n) <= room).or(notes.last()).map_or(&[][..], |n| &n[..]);
                for (i, part) in note.iter().enumerate() {
                    match arrow.filter(|_| *label == "This week" && i == 0) {
                        Some((up, color)) => marked(ui, part.clone(), 7.0, |painter, at| {
                            let (tip, base) = if up { (-3.0, 3.0) } else { (3.0, -3.0) };
                            let points = vec![at + vec2(0.0, tip), at + vec2(3.5, base), at + vec2(-3.5, base)];
                            painter.add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
                        }),
                        None => {
                            ui.add(egui::Label::new(part.clone()).truncate());
                        }
                    }
                }
            });
        });
    };
    ui.columns(tiles.len(), |cols| {
        for (col, t) in cols.iter_mut().zip(&tiles) {
            tile(col, t);
        }
    });
}

/// Weeks of days the calendar shows at most: a year, and the week it's in.
const WEEKS: usize = 53;
/// Columns for a calendar year: up to 52 full weeks, and the part-weeks
/// holding 1 January and 31 December.
const YEAR_WEEKS: usize = 54;
/// The most a calendar square and the gap after it take across.
const MAX_STEP: f32 = 36.0;

/// Which days the calendar shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Span {
    /// Up to today, as many weeks back as fit, up to a year.
    #[default]
    Recent,
    Year(i32),
    /// Every year counted, a row each, the newest or the oldest at the top.
    All,
}

/// A row of the calendar: the days from `from` to `to`, a column a week from
/// the Monday on or before `from`.
struct Run {
    /// Beside the row, for all years.
    label: Option<String>,
    from: NaiveDate,
    to: NaiveDate,
}

fn monday_of(day: NaiveDate) -> NaiveDate {
    day - Duration::days(day.weekday().num_days_from_monday() as i64)
}

/// 1 January and 31 December of `year`.
fn year_bounds(year: i32) -> (NaiveDate, NaiveDate) {
    let jan = NaiveDate::from_ymd_opt(year, 1, 1).unwrap_or_default();
    (jan, NaiveDate::from_ymd_opt(year, 12, 31).unwrap_or(jan))
}

/// A square for each day, a column a week, filled by how many were captured:
/// the last year, one calendar year, or every year a row each.
fn calendar_ui(ui: &mut egui::Ui, daily: &BTreeMap<NaiveDate, Tally>, today: NaiveDate) {
    let p = theme::palette(ui);
    let ramp = ramp(ui.visuals().dark_mode);
    // Days before the first one counted are unknown, not empty.
    let first = daily.keys().next().copied();
    let years: Vec<i32> = first.map_or_else(Vec::new, |f| (f.year()..=today.year()).rev().collect());
    let id = egui::Id::new("stats-span");
    let mut span: Span = ui.data(|d| d.get_temp(id)).unwrap_or_default();
    let oldest_id = egui::Id::new("stats-oldest-first");
    let mut oldest_first: bool = ui.data(|d| d.get_temp(oldest_id)).unwrap_or_default();
    if years.len() < 2 || matches!(span, Span::Year(y) if !years.contains(&y)) {
        span = Span::Recent;
    }
    let note = match (first, span) {
        (None, _) => "counted from your next capture".to_string(),
        (Some(first), span) => {
            let (from, to) = match span {
                Span::Year(y) => {
                    let (jan, dec) = year_bounds(y);
                    (jan.max(first), dec.min(today))
                }
                _ => (first, today),
            };
            let counted = (to - from).num_days().max(0) as u64 + 1;
            let busy = daily.range(from..=to).filter(|(_, t)| t.total() > 0).count() as u64;
            format!("{} of {} with a capture", thousands(busy), plural(counted, "day"))
        }
    };
    heading(ui, "Every day", &note, |ui| {
        if years.len() > 1 {
            let name = |s: Span| match s {
                Span::Recent => "Past year".to_string(),
                Span::Year(y) => y.to_string(),
                Span::All => "All years".to_string(),
            };
            egui::ComboBox::from_id_salt("stats-span")
                .selected_text(name(span))
                .width(110.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut span, Span::Recent, name(Span::Recent));
                    ui.selectable_value(&mut span, Span::All, name(Span::All));
                    ui.separator();
                    for &y in &years {
                        ui.selectable_value(&mut span, Span::Year(y), name(Span::Year(y)));
                    }
                });
            if span == Span::All {
                ui.add_space(8.0);
                theme::segmented(ui, |ui| {
                    // Added right to left, so they read Newest first, Oldest first.
                    ui.selectable_value(&mut oldest_first, true, "Oldest first");
                    ui.selectable_value(&mut oldest_first, false, "Newest first");
                });
            }
            ui.add_space(12.0);
        }
        // Right to left: "More", the fills from the busiest down, "Less".
        ui.spacing_mut().item_spacing.x = 3.0;
        ui.label(RichText::new("More").small().weak());
        for &fill in ramp.iter().rev() {
            let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
            ui.painter().rect_filled(r, 2.0, fill);
        }
        ui.label(RichText::new("Less").small().weak());
    });
    ui.data_mut(|d| {
        d.insert_temp(id, span);
        d.insert_temp(oldest_id, oldest_first);
    });

    card(ui, |ui| {
        let all = span == Span::All;
        let (left, top) = (if all { 40.0 } else { 30.0 }, 16.0);
        let width = ui.available_width();
        // Squares fill the card's width, the last column without a gap after
        // it, up to a size past which they'd just be big.
        let fill = |cols: usize| ((width - left + 3.0) / cols as f32).min(MAX_STEP);
        let year_step = fill(YEAR_WEEKS).max(6.0);
        let year_run = |y: i32| {
            let (jan, dec) = year_bounds(y);
            Run { label: all.then(|| y.to_string()), from: jan, to: dec.min(today) }
        };
        let (runs, cols, step) = match span {
            Span::Recent => {
                // As many weeks as fit, up to a year, each square no smaller than 8 px.
                let weeks = (((width - left + 3.0) / 11.0) as usize).clamp(1, WEEKS);
                let step = fill(weeks).max(11.0);
                let from = monday_of(today) - Duration::weeks(weeks as i64 - 1);
                (vec![Run { label: None, from, to: today }], weeks, step)
            }
            Span::Year(y) => (vec![year_run(y)], YEAR_WEEKS, year_step),
            Span::All => {
                let mut runs: Vec<Run> = years.iter().map(|&y| year_run(y)).collect();
                if oldest_first {
                    runs.reverse();
                }
                (runs, YEAR_WEEKS, year_step)
            }
        };
        // Gaps, corners and the space between years grow with the squares, so
        // a wide calendar looks like a narrow one, bigger.
        let gap = (step * 0.18).clamp(3.0, 6.0);
        let row_gap = (step * 0.6).max(10.0);
        let cell = step - gap;
        let radius = (cell * 0.2).clamp(3.0, 6.0);
        let row_h = 7.0 * step - gap;
        let height = top + runs.len() as f32 * (row_h + row_gap) - row_gap;
        let (rect, resp) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
        let steps = steps(runs.iter().flat_map(|r| daily.range(r.from..=r.to)).map(|(_, t)| t.total()));
        let fill_of = |n: u64| ramp[if n == 0 { 0 } else { 1 + steps.iter().filter(|&&s| n > s).count() }];
        // Centred, if the squares are as big as they get and don't fill it.
        let x0 = rect.left() + ((width - (left + cols as f32 * step - gap)) / 2.0).max(0.0);
        let grid = pos2(x0 + left, rect.top() + top);
        let row_top = |i: usize| grid.y + i as f32 * (row_h + row_gap);
        let square = |i: usize, week: usize, day: usize| {
            Rect::from_min_size(pos2(grid.x + week as f32 * step, row_top(i) + day as f32 * step), vec2(cell, cell))
        };
        let small = FontId::proportional(11.0);
        let weak = p.text_weak;
        let painter = ui.painter();

        // Months over the top row, each over the first week starting in it,
        // unless that crowds the last one or runs off the end. Every year's
        // row lines up to within a column, so one set of names does for all.
        let (label_from, label_to) = match span {
            Span::Recent => (runs[0].from, today),
            _ => year_bounds(runs[0].from.year()),
        };
        let origin = monday_of(runs[0].from);
        let mut labelled: Option<usize> = None;
        let mut month = label_from.with_day(1).unwrap_or(label_from);
        while month <= label_to {
            let monday = month + Duration::days((7 - month.weekday().num_days_from_monday() as i64) % 7);
            let week = ((monday - origin).num_days() / 7) as usize;
            let x = grid.x + week as f32 * step;
            if monday >= label_from && week < cols && labelled.is_none_or(|l| week - l >= 3) && x + 24.0 < rect.right() {
                painter.text(pos2(x, rect.top()), Align2::LEFT_TOP, month.format("%b").to_string(), small.clone(), weak);
                labelled = Some(week);
            }
            let Some(next) = month.checked_add_months(chrono::Months::new(1)) else { break };
            month = next;
        }

        for (i, run) in runs.iter().enumerate() {
            match &run.label {
                Some(year) => {
                    let y = row_top(i) + row_h / 2.0;
                    painter.text(pos2(x0, y), Align2::LEFT_CENTER, year, FontId::proportional(12.0), p.text);
                }
                None => {
                    for d in [0, 2, 4] {
                        let y = square(i, 0, d).center().y;
                        painter.text(pos2(x0, y), Align2::LEFT_CENTER, &WEEKDAYS[d][..3], small.clone(), weak);
                    }
                }
            }
            let monday0 = monday_of(run.from);
            for week in 0..cols {
                for d in 0..7 {
                    let day = monday0 + Duration::days((week * 7 + d) as i64);
                    if day < run.from || day > run.to {
                        continue;
                    }
                    let r = square(i, week, d);
                    if first.is_some_and(|f| day >= f) {
                        let n = daily.get(&day).map_or(0, Tally::total);
                        painter.rect_filled(r, radius, fill_of(n));
                    } else {
                        painter.rect_stroke(r, radius, Stroke::new(1.0, p.divider), StrokeKind::Inside);
                    }
                }
            }
        }

        let Some(pos) = resp.hover_pos() else { return };
        let at = pos - grid;
        let i = (at.y / (row_h + row_gap)).floor();
        if !(0.0..runs.len() as f32).contains(&i) {
            return;
        }
        let i = i as usize;
        let (week, d) = ((at.x / step).floor(), ((pos.y - row_top(i)) / step).floor());
        if !(0.0..cols as f32).contains(&week) || !(0.0..7.0).contains(&d) {
            return;
        }
        let (week, d) = (week as usize, d as usize);
        let run = &runs[i];
        let day = monday_of(run.from) + Duration::days((week * 7 + d) as i64);
        if day < run.from || day > run.to {
            return;
        }
        let r = square(i, week, d);
        painter.rect_stroke(r.expand(1.0), radius + 1.0, Stroke::new(1.5, p.text_strong), StrokeKind::Outside);
        // Shown at once and above the square, not after a pause at the
        // pointer, so it keeps up with a sweep and never hides the days
        // beside the one being read.
        let mut tip = egui::Tooltip::always_open(ui.ctx().clone(), ui.layer_id(), resp.id, egui::PopupAnchor::ParentRect(r));
        use egui::RectAlign as At;
        tip.popup = tip.popup.align(At::TOP).align_alternatives(&[At::TOP_END, At::TOP_START, At::BOTTOM, At::BOTTOM_END, At::BOTTOM_START]).gap(6.0).frame(egui::Frame::popup(ui.style()).inner_margin(egui::Margin::symmetric(12, 9)));
        let colors = series_colors(ui.visuals().dark_mode);
        tip.show(|ui| day_tooltip(ui, day, first.is_none_or(|f| day < f), daily.get(&day), colors));
    });
}

/// One line for the date and the count, and a second splitting the count
/// by kind when there's more than one.
fn day_tooltip(ui: &mut egui::Ui, day: NaiveDate, before: bool, t: Option<&Tally>, colors: [Color32; 3]) {
    // Rows as tall as their text, not the theme's button height.
    ui.spacing_mut().interact_size.y = 0.0;
    ui.spacing_mut().item_spacing.y = 4.0;
    let total = t.map_or(0, Tally::total);
    let head = if before {
        "Not counted".to_owned()
    } else if total == 0 {
        "No captures".to_owned()
    } else {
        plural(total, "capture")
    };
    ui.horizontal(|ui| {
        ui.label(RichText::new(day.format("%a %-d %b %Y").to_string()).strong());
        ui.label(RichText::new(head).weak());
    });
    let Some(t) = t.filter(|t| parts(t).iter().filter(|&&v| v > 0).count() > 1) else { return };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 5.0;
        let units = ["screenshot", "recording", "other file"];
        for (n, (i, v)) in parts(t).into_iter().enumerate().filter(|&(_, v)| v > 0).enumerate() {
            if n > 0 {
                ui.add_space(6.0);
            }
            let color = colors[i];
            marked(ui, RichText::new(plural(v, units[i])).weak(), 8.0, |painter, at| {
                painter.rect_filled(Rect::from_center_size(at, vec2(8.0, 8.0)), 2.0, color);
            });
        }
    });
}

/// `text` after a `width`-wide mark that `paint` draws about the point it's
/// given: the middle of the text's first character, which for digits sits
/// lower than the middle of the line.
fn marked(ui: &mut egui::Ui, text: RichText, width: f32, paint: impl FnOnce(&egui::Painter, egui::Pos2)) {
    let text = egui::WidgetText::from(text).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Body);
    let gap = 5.0;
    let (r, _) = ui.allocate_exact_size(vec2(width + gap + text.size().x, text.size().y), Sense::hover());
    let row = &text.rows[0];
    let mid = row.pos.y + row.glyphs.first().map_or(row.size.y / 2.0, |g| g.pos.y + g.uv_rect.offset.y + g.uv_rect.size.y / 2.0);
    paint(ui.painter(), pos2(r.left() + width / 2.0, r.top() + mid));
    ui.painter().galley(pos2(r.left() + width + gap, r.top()), text, ui.visuals().text_color());
}

/// Where the calendar's fill steps up: the quartiles of the days with any
/// captures, so one very busy day doesn't wash out the rest.
fn steps(counts: impl Iterator<Item = u64>) -> [u64; 3] {
    let mut busy: Vec<u64> = counts.filter(|&n| n > 0).collect();
    if busy.is_empty() {
        return [0; 3];
    }
    busy.sort_unstable();
    let at = |q: usize| busy[(busy.len() - 1) * q / 4];
    [at(1), at(2), at(3)]
}

/// Captures by hour, of one kind or all, and the share in each part of the day.
fn hours_ui(ui: &mut egui::Ui, all: &Captures) {
    let id = egui::Id::new("stats-show");
    let mut show: Show = ui.data(|d| d.get_temp(id)).unwrap_or_default();
    let peak = busiest(&all.hours, show);
    let note = peak.map_or(String::new(), |h| format!("busiest {}", hour_span(h)));
    heading(ui, "Time of day", &note, |ui| {
        theme::segmented(ui, |ui| {
            // Added right to left, so they read All, Screenshots, Recordings.
            ui.selectable_value(&mut show, Show::Recordings, "Recordings");
            ui.selectable_value(&mut show, Show::Screenshots, "Screenshots");
            ui.selectable_value(&mut show, Show::All, "All");
        });
    });
    ui.data_mut(|d| d.insert_temp(id, show));

    let dark = ui.visuals().dark_mode;
    let k = usize::from(show == Show::Recordings);
    let (color, peak_color) = (series_colors(dark)[k], peak_colors(dark)[k]);
    let hours: Vec<Bar> = all
        .hours
        .iter()
        .enumerate()
        .map(|(h, t)| Bar {
            label: (h % 3 == 0).then(|| format!("{h:02}:00")),
            title: hour_span(h),
            tally: *t,
        })
        .collect();
    let plot = bar_chart(ui, &hours, show, color, peak.map(|h| (h, peak_color)), 150.0);
    day_parts(ui, all, show, plot);
}

/// Night, morning, afternoon and evening under the hour chart, each under its
/// quarter of it, with its share.
fn day_parts(ui: &mut egui::Ui, all: &Captures, show: Show, plot: (f32, f32)) {
    const PARTS: [&str; 4] = ["Night", "Morning", "Afternoon", "Evening"];
    let p = theme::palette(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
    let (left, right) = plot;
    let quarter = (right - left) / 4.0;
    let total: u64 = all.hours.iter().map(|t| show.count(t)).sum();
    let painter = ui.painter();
    let font = FontId::proportional(11.0);
    for (i, name) in PARTS.into_iter().enumerate() {
        let n: u64 = all.hours[i * 6..i * 6 + 6].iter().map(|t| show.count(t)).sum();
        let (x0, x1) = (left + i as f32 * quarter + GAP, left + (i + 1) as f32 * quarter - GAP);
        painter.line_segment([pos2(x0, rect.top()), pos2(x1, rect.top())], Stroke::new(2.0, p.divider));
        let name = painter.layout_no_wrap(format!("{name} "), font.clone(), p.text_weak);
        let value = painter.layout_no_wrap(share(n, total), font.clone(), p.text_strong);
        let x = (x0 + x1 - name.size().x - value.size().x) / 2.0;
        let y = rect.top() + 6.0;
        let after = x + name.size().x;
        painter.galley(pos2(x, y), name, p.text_weak);
        painter.galley(pos2(after, y), value, p.text_strong);
    }
}

/// A row a weekday: its bar against the busiest and its share of the week.
fn weekdays_ui(ui: &mut egui::Ui, all: &Captures) {
    let peak = busiest(&all.days, Show::All);
    let note = peak.map_or(String::new(), |d| format!("busiest on {}s", WEEKDAYS[d]));
    heading(ui, "Day of the week", &note, |_| {});
    let p = theme::palette(ui);
    let dark = ui.visuals().dark_mode;
    let colors = series_colors(dark);
    let total: u64 = all.days.iter().map(Tally::total).sum();
    let max = all.days.iter().map(Tally::total).max().unwrap_or(0);
    card(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for (d, t) in all.days.iter().enumerate() {
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
            let top = peak == Some(d);
            let painter = ui.painter();
            let font = FontId::proportional(12.0);
            let ink = if top { p.text_strong } else { p.text };
            painter.text(pos2(rect.left(), rect.center().y), Align2::LEFT_CENTER, &WEEKDAYS[d][..3], font.clone(), ink);
            painter.text(pos2(rect.right(), rect.center().y), Align2::RIGHT_CENTER, share(t.total(), total), font, p.text_weak);
            let track = Rect::from_min_max(pos2(rect.left() + 40.0, rect.center().y - 5.0), pos2(rect.right() - 52.0, rect.center().y + 5.0));
            painter.rect_filled(track, 3.0, ramp(dark)[0]);
            if max > 0 && t.total() > 0 {
                let w = (track.width() * t.total() as f32 / max as f32).max(2.0);
                let fill = if top { peak_colors(dark)[0] } else { colors[0] };
                painter.rect_filled(Rect::from_min_size(track.min, vec2(w, track.height())), 3.0, fill);
            }
            resp.on_hover_ui_at_pointer(|ui| tally_tooltip(ui, WEEKDAYS[d], t, colors));
        }
    });
}

fn tools_ui(ui: &mut egui::Ui, s: &Stats) {
    heading(ui, "Tool use", "", |_| {});
    let found = match s.qr_scans {
        0 => String::new(),
        n => format!("{:.1} per scan", s.qr_codes as f64 / n as f64),
    };
    let rows = [
        ("QR scans", s.qr_scans, String::new()),
        ("QR codes read", s.qr_codes, found),
        (crate::locale::spelled!("Colours picked"), s.colors_picked, String::new()),
        ("Images pinned", s.pins, String::new()),
    ];
    let p = theme::palette(ui);
    card(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for (i, (label, value, note)) in rows.iter().enumerate() {
            let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 42.0), Sense::hover());
            let painter = ui.painter();
            if i > 0 {
                painter.hline(rect.x_range(), rect.top(), Stroke::new(1.0, p.divider));
            }
            let mid = rect.center().y;
            let body = egui::TextStyle::Body.resolve(ui.style());
            if note.is_empty() {
                painter.text(pos2(rect.left(), mid), Align2::LEFT_CENTER, label, body, p.text);
            } else {
                painter.text(pos2(rect.left(), mid + 1.0), Align2::LEFT_BOTTOM, label, body, p.text);
                painter.text(pos2(rect.left(), mid + 2.0), Align2::LEFT_TOP, note, FontId::proportional(11.0), p.text_weak);
            }
            let value = thousands(*value);
            painter.text(pos2(rect.right(), mid), Align2::RIGHT_CENTER, value, FontId::proportional(16.0), p.text_strong);
        }
    });
}

/// One bar across the page, split into screenshots, recordings and other.
/// No tooltip: the legend under it already gives each part's count.
fn split_bar(ui: &mut egui::Ui, t: &Tally, colors: [Color32; 3]) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 12.0), Sense::hover());
    let p = ui.painter();
    let total = t.total();
    if total == 0 {
        p.rect_filled(rect, 4.0, ramp(ui.visuals().dark_mode)[0]);
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
}

/// A swatch, name, count and share for each kind there's any of.
fn legend(ui: &mut egui::Ui, t: &Tally, colors: [Color32; 3]) {
    ui.horizontal(|ui| {
        for (i, v) in parts(t).into_iter().enumerate() {
            if i == 2 && v == 0 {
                continue;
            }
            let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
            ui.painter().rect_filled(r, 2.0, colors[i]);
            ui.spacing_mut().item_spacing.x = 5.0;
            ui.label(SERIES[i]);
            ui.label(RichText::new(thousands(v)).strong());
            ui.label(RichText::new(share(v, t.total())).weak());
            ui.add_space(12.0);
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

/// Columns of what `show` counts, in `color` with the `peak` one in its own,
/// over a recessive grid, a label under some and a tooltip on each. Returns
/// the plot's left and right, for what lines up under it.
fn bar_chart(
    ui: &mut egui::Ui,
    bars: &[Bar],
    show: Show,
    color: Color32,
    peak: Option<(usize, Color32)>,
    height: f32,
) -> (f32, f32) {
    let axis_w = 44.0;
    let label_h = 18.0;
    let (rect, resp) =
        ui.allocate_exact_size(vec2(ui.available_width(), height + label_h), Sense::hover());
    let plot = Rect::from_min_max(
        pos2(rect.left() + axis_w, rect.top() + 6.0),
        pos2(rect.right(), rect.bottom() - label_h),
    );
    let span = (plot.left(), plot.right());
    let visuals = ui.visuals().clone();
    let p = ui.painter();
    let grid = visuals.widgets.noninteractive.bg_stroke.color;
    let weak = visuals.weak_text_color();
    let small = FontId::proportional(11.0);
    let max = bars.iter().map(|b| show.count(&b.tally)).max().unwrap_or(0);
    if max == 0 {
        p.line_segment([plot.left_bottom(), plot.right_bottom()], Stroke::new(1.0, grid));
        p.text(plot.center(), Align2::CENTER_CENTER, "Nothing yet", FontId::proportional(13.0), weak);
        return span;
    }
    let step = nice_step(max as f64 / 3.0);
    let top = (max as f64 / step).ceil() * step;
    let y_of = |v: f64| plot.bottom() - (v / top) as f32 * plot.height();
    let mut v = 0.0;
    while v <= top + 0.5 {
        let y = y_of(v);
        p.line_segment([pos2(plot.left(), y), pos2(plot.right(), y)], Stroke::new(1.0, grid));
        p.text(pos2(plot.left() - 8.0, y), Align2::RIGHT_CENTER, compact(v as u64), small.clone(), weak);
        v += step;
    }

    let slot = plot.width() / bars.len() as f32;
    let bar_w = (slot * 0.68).min(36.0);
    let hovered = resp
        .hover_pos()
        .filter(|pos| pos.x >= plot.left())
        .map(|pos| (((pos.x - plot.left()) / slot) as usize).min(bars.len() - 1));
    let colors = series_colors(visuals.dark_mode);
    for (i, bar) in bars.iter().enumerate() {
        let cx = plot.left() + slot * (i as f32 + 0.5);
        if hovered == Some(i) {
            let col = Rect::from_center_size(pos2(cx, plot.center().y), vec2(slot, plot.height()));
            p.rect_filled(col, 4.0, visuals.widgets.hovered.weak_bg_fill.gamma_multiply(0.5));
        }
        let n = show.count(&bar.tally);
        if n > 0 {
            let y = y_of(n as f64).min(plot.bottom() - 1.0);
            let seg = Rect::from_min_max(pos2(cx - bar_w / 2.0, y), pos2(cx + bar_w / 2.0, plot.bottom()));
            let fill = match peak {
                Some((k, c)) if k == i => c,
                _ => color,
            };
            p.rect_filled(seg, CornerRadius { nw: 3, ne: 3, sw: 0, se: 0 }, fill);
        }
        if let Some(label) = &bar.label {
            p.text(pos2(cx, plot.bottom() + 4.0), Align2::CENTER_TOP, label, small.clone(), weak);
        }
    }
    if let Some(i) = hovered {
        let bar = &bars[i];
        resp.on_hover_ui_at_pointer(|ui| tally_tooltip(ui, &bar.title, &bar.tally, colors));
    }
    span
}

/// The busiest slot by what `show` counts, if there's anything at all.
fn busiest(slots: &[Tally], show: Show) -> Option<usize> {
    let (i, t) = slots.iter().enumerate().max_by_key(|(_, t)| show.count(t))?;
    (show.count(t) > 0).then_some(i)
}

/// 14:00–15:00.
fn hour_span(h: usize) -> String {
    format!("{h:02}:00\u{2013}{:02}:00", (h + 1) % 24)
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
        if i > 0 && (s.len() - i).is_multiple_of(3) {
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

/// 26, 2.5 or 0.3: whole from ten up, otherwise to a tenth.
fn round(v: f64) -> String {
    if v >= 10.0 {
        format!("{v:.0}")
    } else {
        let s = format!("{v:.1}");
        s.strip_suffix(".0").map_or(s.clone(), str::to_string)
    }
}

/// `n` as a share of `total`: 42%, or 6.2% under ten. A dash out of nothing.
fn share(n: u64, total: u64) -> String {
    if total == 0 {
        return "\u{2013}".into();
    }
    let pct = n as f64 * 100.0 / total as f64;
    if pct >= 10.0 { format!("{pct:.0}%") } else { format!("{pct:.1}%") }
}

/// 1 day, 368 days.
fn plural(n: u64, unit: &str) -> String {
    if n == 1 {
        format!("1 {unit}")
    } else {
        format!("{} {unit}s", thousands(n))
    }
}

/// Days from `since` (Unix seconds) to `today`, counting both.
fn days_since(since: i64, today: NaiveDate) -> u64 {
    let start = chrono::DateTime::from_timestamp(since, 0)
        .map(|d| d.with_timezone(&Local).date_naive())
        .unwrap_or(today);
    (today - start).num_days().max(0) as u64 + 1
}

/// A Unix time as a local date, like 5 Oct 2026.
fn date(secs: i64) -> String {
    chrono::DateTime::from_timestamp(secs, 0)
        .map(|d| d.with_timezone(&Local).format("%-d %b %Y").to_string())
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
        assert_eq!(share(42, 100), "42%");
        assert_eq!(share(62, 1000), "6.2%");
        assert_eq!(share(1, 0), "\u{2013}");
        assert_eq!(rate(9483, 368), "about 26 a day");
        assert_eq!(rate(5, 2), "about 2.5 a day");
        assert_eq!(rate(3, 7), "about 3 a week");
        assert_eq!(plural(1, "day"), "1 day");
        assert_eq!(plural(1200, "day"), "1,200 days");
    }

    #[test]
    fn highlights_from_the_log() {
        let d = |m, day| NaiveDate::from_ymd_opt(2026, m, day).unwrap();
        let t = |screenshots, recordings| Tally { screenshots, recordings, other: 0 };
        // Thursday 22 Oct, with nothing yet today: the streak runs to yesterday.
        let today = d(10, 22);
        let mut daily = BTreeMap::new();
        for day in 1..=5 {
            daily.insert(d(9, day), t(10, 0));
        }
        daily.insert(d(9, 20), t(90, 2));
        for day in 13..=21 {
            daily.insert(d(10, day), t(4, 1));
        }
        daily.insert(d(10, 22), t(0, 0));
        let h = highlights(&daily, today);
        assert_eq!(h.today, t(0, 0));
        // Mon 19 to Thu 22 against Mon 12 to Thu 15, which has no Monday.
        assert_eq!((h.week, h.last_week), (15, 15));
        assert_eq!(h.streak, 9);
        assert_eq!(h.longest, Some((9, d(10, 21))));
        assert_eq!(h.busiest, Some((d(9, 20), 92)));

        daily.insert(d(10, 22), t(3, 0));
        daily.remove(&d(10, 16));
        let h = highlights(&daily, today);
        assert_eq!(h.streak, 6);
        assert_eq!(h.longest, Some((6, d(10, 22))));
        assert_eq!(highlights(&BTreeMap::new(), today), Highlights::default());
    }

    #[test]
    fn steps_are_quartiles_of_busy_days() {
        assert_eq!(steps([0, 0].into_iter()), [0, 0, 0]);
        assert_eq!(steps([0, 1, 2, 3, 4, 5].into_iter()), [2, 3, 4]);
    }

    /// Renders the page with a busy couple of years' numbers to
    /// `target/stats-preview.png` (and `-light`, and `-all` with every
    /// year in the calendar):
    /// `cargo test stats_preview -- --ignored`.
    #[test]
    #[ignore]
    fn stats_preview() {
        let today = Local::now().date_naive();
        let mut stats = Stats {
            since: (Local::now() - Duration::days(820)).timestamp(),
            qr_scans: 31,
            qr_codes: 38,
            colors_picked: 148,
            pins: 22,
            ..Stats::default()
        };
        // Days counted from 800 days ago: busier midweek, quiet at
        // weekends, with a holiday and the odd very busy day.
        let mut seed = 7u64;
        let mut rnd = move || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) as f64 / (1u64 << 31) as f64
        };
        for back in 0..800 {
            let day = today - Duration::days(back);
            let wd = day.weekday().num_days_from_monday() as usize;
            let weight = [1.05, 1.15, 1.1, 1.0, 0.85, 0.45, 0.4][wd];
            let away = rnd() < if wd >= 5 { 0.3 } else { 0.06 } || (60..74).contains(&back);
            let busy = if rnd() < 0.04 { 3.0 } else { 1.0 };
            let screenshots = if away { 0 } else { (weight * (18.0 + rnd() * 26.0) * busy) as u64 };
            let recordings = if screenshots > 0 && rnd() < 0.35 { 1 } else { 0 };
            let t = Tally { screenshots, recordings, other: 0 };
            stats.daily.insert(day, t);
            let c = &mut stats.captures;
            c.total.screenshots += screenshots;
            c.total.recordings += recordings;
            c.days[wd].screenshots += screenshots;
            c.days[wd].recordings += recordings;
        }
        let shape = [3, 1, 1, 0, 0, 0, 0, 1, 4, 7, 8, 8, 5, 7, 9, 8, 8, 6, 4, 4, 4, 5, 4, 4];
        let sum: u64 = shape.iter().sum();
        let total = stats.captures.total;
        for (h, t) in stats.captures.hours.iter_mut().enumerate() {
            let afternoon = (13..=17).contains(&h);
            *t = Tally {
                screenshots: total.screenshots * shape[h] / sum,
                recordings: total.recordings * shape[h] * if afternoon { 2 } else { 1 } / (sum + 30),
                other: 0,
            };
        }
        let shots = [
            ("stats-preview", true, Span::Recent, [900, 1140]),
            ("stats-preview-light", false, Span::Recent, [900, 1140]),
            ("stats-preview-all", true, Span::All, [900, 1400]),
            ("stats-preview-narrow", true, Span::Recent, [720, 1240]),
        ];
        for (name, dark, span, size) in shots {
            crate::preview::render(name, size, 1.0, |root| {
                crate::theme::apply(root.ctx());
                root.ctx().data_mut(|d| d.insert_temp(egui::Id::new("stats-span"), span));
                root.ctx().set_theme(if dark { egui::Theme::Dark } else { egui::Theme::Light });
                egui::CentralPanel::default()
                    .frame(egui::Frame::central_panel(root.style()).inner_margin(egui::Margin::symmetric(20, 16)))
                    .show(root, |ui| super::ui(ui, &stats));
            });
        }
    }
}
