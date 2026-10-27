//! The Settings sidebar: a search box over the list of sections. Typing
//! finds a setting in any section by its name or what it's for, listed in
//! place of the sections; picking one goes to its section and points it out.

use std::sync::LazyLock;

use egui::{Align, Color32, Id, Key, Modifiers, Rect, RichText, Sense, Stroke, TextEdit, Vec2, pos2, vec2};

use crate::main_window::Page;
use crate::settings::Settings;
use crate::theme;

/// The sections of Settings, in the sidebar's order.
const SECTIONS: [(Page, &str); 7] = [
    (Page::Settings, "General"),
    (Page::Capture, "Capture"),
    (Page::Recording, "Recording"),
    (Page::Hotkeys, "Hotkeys"),
    (Page::Naming, "Paths & naming"),
    (Page::Destinations, "Destinations"),
    (Page::About, "About"),
];

/// How long a setting the search went to stays outlined, in seconds.
const HIGHLIGHT_SECS: f64 = 2.0;

/// A setting that can be searched for.
struct Entry {
    page: Page,
    /// The row to point out: what its tab passes to [`found`].
    key: &'static str,
    title: &'static str,
    /// Other words it might be looked for by.
    terms: &'static str,
}

/// Every setting on the Settings tabs, in the order they're shown, which
/// is also the order equally good matches are listed in.
static INDEX: LazyLock<Vec<Entry>> = LazyLock::new(|| {
    use Page::*;
    let mut index = Vec::new();
    let mut add = |page, key, title: &'static str, terms| index.push(Entry { page, key, title, terms });

    // General
    if cfg!(any(windows, target_os = "linux")) {
        add(Settings, "start-with-windows", crate::settings_ui::START_WITH_OS, "startup boot login logon autostart launch windows linux");
    }
    if cfg!(windows) {
        add(Settings, "start-menu", "Show snapr in the Start menu", "shortcut start menu programs pin search launch open windows");
    }
    add(Settings, "theme", "Theme", "light dark mode appearance colours colors system night");
    add(Settings, "play-sounds", "Play sounds", "shutter sound effects audio mute quiet");
    add(Settings, "sound-volume", "Sound volume", "loudness audio shutter");
    add(Settings, "show-toast", "Show a preview in the corner after each capture", "toast notification popup thumbnail");
    add(Settings, "tray-click", if cfg!(target_os = "macos") { "Clicking the menu bar icon" } else { "Clicking the tray icon" }, "left click system tray menu bar action");
    if !cfg!(target_os = "macos") {
        add(Settings, "tray-icon", "Tray icon colour", "color colour white black blue taskbar theme system tray");
    }
    add(Settings, "transfer", "Export settings", "backup move another computer transfer save file");
    add(Settings, "transfer", "Import settings", "restore move another computer transfer load file");
    add(Settings, "export-secrets", "Include upload secret keys in the export", "passwords credentials export");
    add(Settings, "run-setup", "Run setup again", "first-time setup welcome wizard onboarding");

    // Capture
    add(Capture, "annotate", "Show the annotation toolbar while capturing", "draw annotate arrows pen text toolbar");
    add(Capture, "overlay-fps", "Overlay frame rate", "fps refresh rate hz monitor smooth");
    add(Capture, "crosshair", "Beside the crosshair", "cursor position pixel coordinates colour color magnifier zoom");
    add(Capture, "magnifier-scroll", "Scrolling the magnifier", "mouse wheel zoom resize magnifier colour color picker");
    add(Capture, "redact-image", "Redaction image", "redact censor hide picture black boxes");
    add(Capture, "redact-stretch", "Stretch the redaction image to fill each area", "redact crop proportions aspect");

    // Recording
    add(Recording, "record-fps", "Recording frame rate", "fps video");
    if crate::encode::webm_supported() {
        add(Recording, "record-format", "Recording format", "mp4 webm av1 h.264 vp9 video codec container");
    }
    if !cfg!(target_os = "linux") {
        add(Recording, "encoder", "Encoder", "ffmpeg system hardware gpu cpu x264 libvpx video");
    }
    add(Recording, "ffmpeg", "FFmpeg", "ffmpeg path program executable install test");
    add(Recording, "ffmpeg-codec", "x264 preset and quality (MP4)", "crf preset h.264 ffmpeg");
    if crate::encode::webm_supported() {
        add(Recording, "ffmpeg-codec", "VP9 speed and quality (WebM)", "crf vp9 ffmpeg");
    }
    add(Recording, "ffmpeg-args", "Extra FFmpeg arguments", "ffmpeg options command line flags");
    add(Recording, "record-cursor", "Show the mouse pointer", "cursor recording video");
    if cfg!(windows) {
        add(Recording, "record-hdr", "Record in HDR", "hdr10 hevc high dynamic range 10-bit");
    }
    add(Recording, "record-system-audio", "Record system audio", "sound what's playing speakers headphones recording");
    add(Recording, "record-microphone", "Record the microphone", "mic voice input sound recording");

    // Hotkeys
    add(Hotkeys, "capture-hotkey", "Capture region hotkey", "shortcut keyboard key combination screenshot");
    add(Hotkeys, "record-hotkey", "Record region hotkey", "shortcut keyboard key combination video recording");
    add(Hotkeys, "tool-hotkeys", "Tool hotkeys", "shortcut keyboard colour color picker qr code pin tools");

    // Paths & naming
    add(Naming, "folder", "Save folder", "directory location path where screenshots are saved");
    add(Naming, "subfolder", "Sub-folder", "subfolder directory date template");
    add(Naming, "file-name", "File name", "filename template pattern counter date naming");

    // Destinations
    add(Destinations, "save-to-folder", "Save to folder", "disk save screenshots");
    add(Destinations, "copy-to-clipboard", "Copy image to clipboard", "paste clipboard");
    add(Destinations, "uploads", "Add S3 upload", "upload destination r2 cloudflare amazon s3 minio backblaze b2 bucket");
    add(Destinations, "uploads", "Add Pomf upload", "upload destination pomf uguu lain file host");
    add(Destinations, "uploads", "Test upload", "upload try check destination");
    add(Destinations, "uploads", "Reorder uploads", "upload destination order drag move up down sort");
    add(Destinations, "copy-link", "Copy the link after uploading", "clipboard url link upload");
    add(Destinations, "link-from", "Link from", "which destination link url upload");
    add(Destinations, "speed-unit", "Show upload speed in", "mb/s mbps megabytes megabits upload");
    add(Destinations, "tray-percentage", if cfg!(target_os = "macos") { "Show the percentage uploaded on the menu bar icon" } else { "Show the percentage uploaded on the tray icon" }, "progress upload");

    // About
    add(About, "version", "Check for updates", "version update upgrade release");
    add(About, "check-for-updates", "Check for updates automatically", "automatic updates auto-update release github never");
    add(About, "source-code", "Source code", "github repository");
    add(About, "licence", "Licence", "license polyform terms");
    add(About, "settings-folder", "Settings folder", "config configuration directory where settings are kept history stats");
    add(About, "libraries", "Open-source libraries", "crates credits attributions licences licenses");
    index
});

/// The name of the section a page of Settings is, as the sidebar lists it.
pub fn section_name(page: Page) -> &'static str {
    SECTIONS.iter().find(|(p, _)| *p == page).map_or("General", |(_, name)| name)
}

/// Where a setting shows while it's hidden until another is set: the FFmpeg
/// rows only show once FFmpeg is the encoder (on Linux it always is).
fn shown_key(key: &'static str, settings: &Settings) -> &'static str {
    let ffmpeg_row = matches!(key, "ffmpeg" | "ffmpeg-codec" | "ffmpeg-args");
    if ffmpeg_row && !cfg!(target_os = "linux") && !settings.record_with_ffmpeg {
        "encoder"
    } else {
        key
    }
}

/// The settings matching `query`, best first: each word of it must start a
/// word of a setting's title, its other terms or its section's name.
fn matches(query: &str) -> Vec<&'static Entry> {
    let query = query.trim().to_lowercase();
    let words: Vec<&str> = query.split_whitespace().collect();
    if words.is_empty() {
        return Vec::new();
    }
    let mut found: Vec<(u8, &Entry)> = INDEX
        .iter()
        .filter_map(|e| {
            let title = crate::locale::spell(e.title).to_lowercase();
            let all = format!("{title} {} {}", e.terms, section_name(e.page).to_lowercase());
            let starts = all.split(|c: char| c.is_whitespace() || "-/()&.,'".contains(c));
            if !words.iter().all(|w| starts.clone().any(|t| t.starts_with(w))) {
                return None;
            }
            let rank = if title.starts_with(&query) {
                0
            } else if title.split_whitespace().any(|t| t.starts_with(words[0])) {
                1
            } else if words.iter().all(|w| title.contains(w)) {
                2
            } else {
                3
            };
            Some((rank, e))
        })
        .collect();
    found.sort_by_key(|(rank, _)| *rank);
    found.into_iter().map(|(_, e)| e).collect()
}

/// A setting the search went to, kept in egui's memory until it's been
/// pointed out.
#[derive(Clone)]
struct Reveal {
    key: &'static str,
    /// When it was picked, in egui's time.
    at: f64,
    scrolled: bool,
}

fn reveal_id() -> Id {
    Id::new("settings-search-reveal")
}

/// Call with each setting's row as it's laid out: if the search just went
/// to `key`, scrolls the row into view and outlines it for a moment.
pub fn found(ui: &egui::Ui, key: &str, row: Rect) {
    let Some(mut reveal) = ui.data(|d| d.get_temp::<Reveal>(reveal_id())) else {
        return;
    };
    if reveal.key != key {
        return;
    }
    let now = ui.input(|i| i.time);
    let fade = 1.0 - ((now - reveal.at) / HIGHLIGHT_SECS) as f32;
    if fade <= 0.0 {
        ui.data_mut(|d| d.remove::<Reveal>(reveal_id()));
        return;
    }
    if !reveal.scrolled {
        ui.scroll_to_rect(row, Some(Align::Center));
        reveal.scrolled = true;
        ui.data_mut(|d| d.insert_temp(reveal_id(), reveal));
    }
    // Hold fully for a moment, then fade out.
    let alpha = (fade * 2.0).min(1.0);
    let rect = row.expand2(vec2(8.0, 4.0));
    ui.painter().rect(
        rect,
        8.0,
        theme::ACCENT.gamma_multiply(0.14 * alpha),
        Stroke::new(1.5, theme::ACCENT.gamma_multiply(alpha)),
        egui::StrokeKind::Outside,
    );
    ui.ctx().request_repaint();
}

/// The search box's state.
#[derive(Default)]
pub struct Search {
    query: String,
    /// The result Enter goes to, moved with the arrow keys; the one last
    /// gone to.
    selected: usize,
}

impl Search {
    #[cfg(test)]
    pub fn with_query(query: &str) -> Self {
        Self { query: query.into(), selected: 0 }
    }

    /// The Settings sidebar: its heading, the search box, and under it the
    /// sections, or while something's typed the settings matching it.
    /// Picking either shows its section (in `page`); a setting is pointed out.
    pub fn sidebar(&mut self, ui: &mut egui::Ui, page: &mut Page, settings: &Settings) {
        let id = Id::new("settings-search");
        let results = matches(&self.query);
        if ui.input_mut(|i| i.consume_shortcut(&egui::KeyboardShortcut::new(Modifiers::COMMAND, Key::F))) {
            ui.memory_mut(|m| m.request_focus(id));
        }
        // The arrow keys move through the results rather than the text.
        if ui.memory(|m| m.has_focus(id)) && !results.is_empty() {
            let (down, up) = ui.input_mut(|i| {
                (
                    i.consume_key(Modifiers::NONE, Key::ArrowDown),
                    i.consume_key(Modifiers::NONE, Key::ArrowUp),
                )
            });
            if down {
                self.selected = (self.selected + 1) % results.len();
            }
            if up {
                self.selected = self.selected.checked_sub(1).unwrap_or(results.len() - 1);
            }
        }
        self.selected = self.selected.min(results.len().saturating_sub(1));

        ui.heading("Settings");
        ui.add_space(10.0);
        let p = theme::palette(ui);
        let response = ui.add(
            TextEdit::singleline(&mut self.query)
                .id(id)
                .margin(egui::Margin { left: 30, right: 28, ..theme::FIELD_MARGIN })
                .hint_text("Search")
                .desired_width(ui.available_width()),
        );
        magnifier(ui.painter(), response.rect, p.text_weak);
        if response.changed() {
            self.selected = 0;
        }
        if !self.query.is_empty() && clear_button(ui, response.rect, id.with("clear")).clicked() {
            self.query.clear();
            ui.memory_mut(|m| m.request_focus(id));
        }
        if response.lost_focus() && ui.input(|i| i.key_pressed(Key::Escape)) {
            self.query.clear();
        }
        let entered = response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        ui.add_space(10.0);
        ui.spacing_mut().item_spacing.y = 2.0;

        if self.query.trim().is_empty() {
            for (section, name) in SECTIONS {
                if item(ui, *page == section, name, None).clicked() {
                    *page = section;
                }
            }
            return;
        }
        if results.is_empty() {
            ui.add_space(4.0);
            ui.label(RichText::new("No settings match").weak());
            return;
        }
        let mut picked = (entered).then_some(self.selected);
        egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
            for (i, e) in results.iter().enumerate() {
                let title = crate::locale::spell(e.title);
                if item(ui, i == self.selected, &title, Some(section_name(e.page))).clicked() {
                    picked = Some(i);
                }
            }
        });
        if let Some(i) = picked {
            self.selected = i;
            let e = results[i];
            *page = e.page;
            let at = ui.input(|i| i.time);
            let reveal = Reveal { key: shown_key(e.key, settings), at, scrolled: false };
            ui.data_mut(|d| d.insert_temp(reveal_id(), reveal));
            if entered {
                // Stay in the box, to go on to another with the arrow keys.
                ui.memory_mut(|m| m.request_focus(id));
            }
            ui.ctx().request_repaint();
        }
    }
}

/// A row of the sidebar: a section, or a setting found with its section
/// under it. The picked one is tinted with the accent.
fn item(ui: &mut egui::Ui, selected: bool, title: &str, section: Option<&str>) -> egui::Response {
    let p = theme::palette(ui);
    let pad = vec2(10.0, 7.0);
    let width = ui.available_width();
    let galley = |ui: &egui::Ui, text: RichText| {
        egui::WidgetText::from(text).into_galley(
            ui,
            Some(egui::TextWrapMode::Wrap),
            width - 2.0 * pad.x,
            egui::TextStyle::Button,
        )
    };
    let title = galley(ui, RichText::new(title).color(if selected { p.selected_text } else { p.text }));
    let section = section.map(|s| galley(ui, RichText::new(s).small().color(p.text_weak)));
    let height = title.size().y + section.as_ref().map_or(0.0, |s| s.size().y + 1.0) + 2.0 * pad.y;
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, ui.is_enabled(), selected, title.text())
    });
    if ui.is_rect_visible(rect) {
        let fill = if selected {
            p.selected
        } else if response.hovered() {
            p.control_hover
        } else {
            Color32::TRANSPARENT
        };
        let painter = ui.painter();
        painter.rect_filled(rect, 7.0, fill);
        let top = rect.min + pad;
        let below = top.y + title.size().y + 1.0;
        painter.galley(top, title, p.text);
        if let Some(section) = section {
            painter.galley(pos2(top.x, below), section, p.text_weak);
        }
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A cross at the right end of the search box `field`, which empties it.
fn clear_button(ui: &mut egui::Ui, field: Rect, id: Id) -> egui::Response {
    let rect = Rect::from_center_size(pos2(field.right() - 15.0, field.center().y), Vec2::splat(20.0));
    let response = ui
        .interact(rect, id, Sense::click())
        .on_hover_text("Clear")
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let p = theme::palette(ui);
    let color = if response.hovered() { p.text_strong } else { p.text_weak };
    let (c, d) = (rect.center(), 3.5);
    let stroke = Stroke::new(1.5, color);
    ui.painter().line_segment([c + vec2(-d, -d), c + vec2(d, d)], stroke);
    ui.painter().line_segment([c + vec2(-d, d), c + vec2(d, -d)], stroke);
    response
}

/// A magnifying glass at the left end of the search box `field`.
fn magnifier(painter: &egui::Painter, field: Rect, color: Color32) {
    let center = pos2(field.left() + 15.0, field.center().y - 1.0);
    let radius = 5.0;
    let stroke = Stroke::new(1.5, color);
    painter.circle_stroke(center, radius, stroke);
    let dir = vec2(1.0, 1.0).normalized();
    painter.line_segment([center + dir * radius, center + dir * (radius + 4.5)], stroke);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn titles(query: &str) -> Vec<&'static str> {
        matches(query).into_iter().map(|e| e.title).collect()
    }

    #[test]
    fn finds_settings_by_title_terms_and_tab() {
        assert_eq!(titles("sound vol").first(), Some(&"Sound volume"));
        assert!(titles("hotkey").contains(&"Capture region hotkey"));
        assert!(titles("cloudflare").contains(&"Add S3 upload"));
        assert!(titles("COLOR").iter().any(|t| t.contains("magnifier") || t.contains("crosshair")));
        assert!(titles("capture redact").contains(&"Redaction image"), "tab name and title");
        assert!(titles("").is_empty());
        assert!(titles("nothing like this").is_empty());
        assert!(titles("col").iter().any(|t| t.contains("magnifier")), "the start of a word");
        assert!(!titles("olour").iter().any(|t| t.contains("magnifier")), "not the middle of one");
    }

    #[test]
    fn title_matches_come_first() {
        let found = titles("folder");
        assert_eq!(found[0], "Save folder");
    }

    /// Renders the sidebar with nothing typed and with "wo" typed to
    /// `target/settings-sidebar-<query>-preview.png`:
    /// `cargo test settings_sidebar_preview -- --ignored`.
    #[test]
    #[ignore]
    fn settings_sidebar_preview() {
        let settings = Settings::default();
        for query in ["", "wo"] {
            let mut search = Search { query: query.into(), selected: 1 };
            let mut page = Page::Capture;
            let name = format!("settings-sidebar-{query}-preview");
            crate::preview::render(&name, [240, 560], 1.0, |root| {
                crate::theme::apply(root.ctx());
                egui::CentralPanel::default().show(root, |ui| search.sidebar(ui, &mut page, &settings));
            });
        }
    }

    /// Renders the Recording tab with the search gone to its Microphone row to
    /// `target/settings-found-preview.png`:
    /// `cargo test settings_found_preview -- --ignored`.
    #[test]
    #[ignore]
    fn settings_found_preview() {
        let mut form = crate::settings_ui::Form::new(&Settings::default());
        crate::preview::render("settings-found-preview", [820, 700], 1.0, |root| {
            crate::theme::apply(root.ctx());
            let reveal = Reveal { key: "record-microphone", at: root.input(|i| i.time), scrolled: false };
            root.data_mut(|d| d.insert_temp(reveal_id(), reveal));
            egui::CentralPanel::default().show(root, |ui| form.recording_ui(ui, &mut Vec::new()));
        });
    }

    #[test]
    fn hidden_ffmpeg_rows_point_to_the_encoder() {
        let mut s = Settings::default();
        s.record_with_ffmpeg = false;
        let expect = if cfg!(target_os = "linux") { "ffmpeg-args" } else { "encoder" };
        assert_eq!(shown_key("ffmpeg-args", &s), expect);
        s.record_with_ffmpeg = true;
        assert_eq!(shown_key("ffmpeg-args", &s), "ffmpeg-args");
        assert_eq!(shown_key("sound-volume", &s), "sound-volume");
    }
}
