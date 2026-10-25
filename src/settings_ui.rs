//! The General, Hotkeys and Paths & naming tabs of the Settings page, and
//! the form state they share with the Destinations tab.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::{Color32, RichText, TextEdit};
use winit::event::ElementState;
use winit::keyboard::{KeyCode, ModifiersState};

use crate::naming::{CATEGORIES, Context};
use crate::output;
use crate::secrets;
use crate::settings_search::found;
use crate::settings::{
    MagnifierScroll, Settings, Theme, ToolAction, ToolHotkey, TrayClick, TrayIconStyle, Upload, parse_hotkey,
};
use crate::sound::{self, Sound};
use crate::theme;
use crate::update;

pub enum Action {
    /// Apply these settings and store these new secret keys (upload id → key).
    Save(Settings, HashMap<String, String>),
    Reveal(PathBuf),
    /// Try an upload destination, with a secret key typed but not yet saved.
    TestUpload(Upload, Option<String>),
    /// Upload snapr's icon to a destination as a screenshot would be, with
    /// a secret key typed but not yet saved.
    UploadIcon(Upload, Option<String>),
    CheckForUpdates,
    /// Have the desktop ask the user to let snapr take screenshots.
    #[cfg(target_os = "linux")]
    AllowScreenshots,
    /// Quit and start snapr again: the updated program, if one's installed.
    RestartToUpdate,
    OpenUrl(String),
    /// Use the capture tools' settings from an imported file.
    ImportTools(crate::remembered::Remembered),
}

/// The hotkeys that can be set on the Hotkeys tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HotkeyField {
    Capture,
    Record,
    /// A tool hotkey, by its index in `tool_hotkeys`.
    Tool(usize),
}

/// How long the form waits after the last edit before saving, so typing
/// doesn't save (and re-register hotkeys) on every keystroke.
const AUTOSAVE_DELAY: Duration = Duration::from_millis(600);

pub const ERROR: Color32 = Color32::from_rgb(0xe5, 0x48, 0x4d);
pub const SUCCESS: Color32 = Color32::from_rgb(0x34, 0xc7, 0x59);

/// The startup setting's title, in Settings and its search.
pub const START_WITH_OS: &str =
    if cfg!(windows) { "Start snapr when you sign in to Windows" } else { "Start snapr when you sign in" };

/// The result of testing an upload destination.
pub enum Test {
    Running,
    Passed(String),
    Failed(String),
}

/// Editable state shown in the window.
pub struct Form {
    pub(crate) draft: Settings,
    pub(crate) saved: Settings,
    pub recording: bool,
    /// Which hotkey `recording` fills in.
    recording_field: HotkeyField,
    /// Message from the app, e.g. a save or hotkey error. `true` = error.
    pub status: Option<(String, bool)>,
    pub last_capture: Option<PathBuf>,
    /// Where checking for and installing updates is up to.
    pub update: update::Status,
    /// Secret keys typed but not saved yet, by upload id.
    pub(crate) secrets: HashMap<String, String>,
    /// Uploads whose secret key is in the credential store.
    pub(crate) stored_secrets: HashSet<String>,
    pub tests: HashMap<String, Test>,
    /// The upload destination being edited.
    pub(crate) expanded: Option<String>,
    /// Connected microphones, listed when the General tab first shows.
    microphones: Option<Vec<String>>,
    /// The draft and secret keys as last seen, and when they last changed.
    seen: (Settings, HashMap<String, String>, Instant),
    /// The last edit handed to the app to save, so one that failed isn't
    /// retried until something changes.
    submitted: Option<(Settings, HashMap<String, String>)>,
    /// The last FFmpeg check: the path and format checked, and the outcome
    /// once it's in.
    ffmpeg_check: Option<FfmpegCheck>,
    /// Put the upload secret keys in exported settings.
    export_secrets: bool,
    /// How the last export or import went. `true` = error.
    transfer_note: Option<(String, bool)>,
    /// "Run setup again" was clicked; the window takes it from there.
    pub run_setup: bool,
    pub about: crate::about::About,
}

struct FfmpegCheck {
    program: String,
    webm: bool,
    result: Arc<Mutex<Option<Result<String, String>>>>,
}

impl Form {
    pub fn new(settings: &Settings) -> Self {
        Self {
            draft: settings.clone(),
            saved: settings.clone(),
            recording: false,
            recording_field: HotkeyField::Capture,
            status: None,
            last_capture: None,
            update: update::Status::default(),
            secrets: HashMap::new(),
            stored_secrets: settings
                .uploads
                .iter()
                .filter(|u| secrets::get(u).is_some())
                .map(|u| u.id.clone())
                .collect(),
            tests: HashMap::new(),
            expanded: None,
            microphones: None,
            seen: (settings.clone(), HashMap::new(), Instant::now()),
            submitted: None,
            ffmpeg_check: None,
            export_secrets: false,
            transfer_note: None,
            run_setup: false,
            about: Default::default(),
        }
    }

    /// Called after the app applied a save.
    pub fn saved(&mut self, settings: &Settings) {
        self.saved = settings.clone();
        self.draft = settings.clone();
        self.stored_secrets
            .extend(self.secrets.drain().map(|(id, _)| id));
        self.stored_secrets
            .retain(|id| settings.uploads.iter().any(|u| u.id == *id));
    }

    pub(crate) fn has_secret(&self, id: &str) -> bool {
        self.stored_secrets.contains(id) || self.secrets.get(id).is_some_and(|s| !s.is_empty())
    }

    /// Why the draft can't be saved, if it can't.
    fn problem(&self) -> Option<String> {
        if let Err(e) = parse_hotkey(&self.draft.hotkey) {
            return Some(format!("Hotkey: {e}"));
        }
        if let Some(e) = self.record_hotkey_problem() {
            return Some(format!("Record hotkey: {e}"));
        }
        for (i, t) in self.draft.tool_hotkeys.iter().enumerate() {
            if let Some(e) = self.tool_hotkey_problem(i) {
                return Some(format!("{} hotkey: {e}", t.action.label()));
            }
        }
        if let Err(e) = self.draft.naming() {
            return Some(e);
        }
        if let Err(e) = self.draft.ffmpeg.extra_args() {
            return Some(format!("FFmpeg arguments: {e}"));
        }
        let redact = self.draft.redact_image.trim();
        if !redact.is_empty() && !std::path::Path::new(redact).is_file() {
            return Some("Redaction image: no such file".into());
        }
        for u in self.draft.uploads.iter().filter(|u| u.enabled) {
            if let Err(e) = u.validate() {
                return Some(format!("{}: {e}", u.name));
            }
            if u.needs_secret() && !self.has_secret(&u.id) {
                return Some(format!("{}: enter the secret access key", u.name));
            }
        }
        None
    }

    fn dirty(&self) -> bool {
        self.draft != self.saved || !self.secrets.is_empty()
    }

    /// The edit to save, if there is one that can be and it hasn't been
    /// tried already.
    fn unsaved(&self) -> Option<Action> {
        if !self.dirty()
            || self.recording
            || self
                .submitted
                .as_ref()
                .is_some_and(|(d, s)| *d == self.draft && *s == self.secrets)
            || self.problem().is_some()
        {
            return None;
        }
        let secrets = self
            .secrets
            .iter()
            .filter(|(_, s)| !s.is_empty())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        Some(Action::Save(self.draft.clone(), secrets))
    }

    /// Saves the form once it's valid and hasn't changed for a moment. Call
    /// every frame, whichever page is showing.
    pub fn autosave(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let now = Instant::now();
        if self.seen.0 != self.draft || self.seen.1 != self.secrets {
            self.seen = (self.draft.clone(), self.secrets.clone(), now);
        }
        let Some(save) = self.unsaved() else {
            return;
        };
        let wait = AUTOSAVE_DELAY.saturating_sub(now - self.seen.2);
        if !wait.is_zero() {
            ctx.request_repaint_after(wait);
            return;
        }
        self.submitted = Some((self.draft.clone(), self.secrets.clone()));
        actions.push(save);
    }

    /// Saves any edit still waiting out the delay, e.g. as the window closes.
    pub fn flush(&mut self) -> Option<Action> {
        let save = self.unsaved()?;
        self.submitted = Some((self.draft.clone(), self.secrets.clone()));
        Some(save)
    }

    /// The latest status message, or why the changes can't be saved.
    pub(crate) fn save_bar(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.add_space(10.0);
        ui.separator();
        // Messages wrap: some, like a failed upload's, are long.
        if let Some(p) = self.problem().filter(|_| self.dirty()) {
            ui.add(egui::Label::new(RichText::new(format!("Not saved \u{2014} {p}")).color(ERROR)).wrap());
        } else if let Some((msg, is_err)) = &self.status {
            // Shown at the top of the page instead.
            if is_missing_permission(msg) {
                return;
            }
            let color = if *is_err {
                ERROR
            } else {
                ui.visuals().weak_text_color()
            };
            ui.add(egui::Label::new(RichText::new(msg).color(color)).wrap());
        } else if let Some(path) = &self.last_capture
            && ui.link(format!("Last: {}", path.display())).clicked()
        {
            actions.push(Action::Reveal(path.clone()));
        }
    }

    /// The General tab.
    pub fn ui(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            if cfg!(any(windows, target_os = "linux")) {
                group(ui, "Startup", |ui| {
                    row(ui, "start-with-windows", START_WITH_OS,
                        Some("So the hotkeys work without starting it yourself"), false, |ui| {
                        theme::switch(ui, &mut self.draft.start_with_windows, "");
                    });
                    if cfg!(windows) {
                        row(ui, "start-menu", "Show snapr in the Start menu",
                            Some("To open it by searching for it, or pin it"), false, |ui| {
                            theme::switch(ui, &mut self.draft.start_menu, "");
                        });
                    }
                });
            }

            group(ui, "Appearance", |ui| {
                row(ui, "theme", "Theme", Some("For every snapr window, from this one to the capture toolbar"), false, |ui| {
                    theme::segmented(ui, |ui| {
                        for choice in Theme::ALL {
                            ui.selectable_value(&mut self.draft.theme, choice, choice.label());
                        }
                    });
                });
            });

            group(ui, "After each capture", |ui| {
                row(ui, "play-sounds", "Play sounds", Some("A shutter on capture, and a chime when something's done"), false, |ui| {
                    theme::switch(ui, &mut self.draft.play_sounds, "");
                });
                ui.add_enabled_ui(self.draft.play_sounds, |ui| {
                    row(ui, "sound-volume", "Volume", None, true, |ui| {
                        let slider = theme::slider(ui, egui::Slider::new(&mut self.draft.sound_volume, 0..=100).suffix("%"), "100%");
                        // Let it be heard once it's set, not all the way along a drag.
                        if slider.drag_stopped() || (slider.changed() && !slider.dragged()) {
                            sound::play(Sound::Done, self.draft.sound_volume);
                        }
                    });
                });
                row(ui, "show-toast", "Show a preview in the corner",
                    Some("Click it to open the link or the image, middle-click to copy the image, right-click to close it"),
                    false, |ui| {
                    theme::switch(ui, &mut self.draft.show_toast, "");
                });
            });

            let icon = if cfg!(target_os = "macos") { "Menu bar icon" } else { "Tray icon" };
            group(ui, icon, |ui| {
                row(ui, "tray-click", "Clicking it", Some("A right-click always shows the menu"), false, |ui| {
                    egui::ComboBox::from_id_salt("tray-click")
                        .selected_text(self.draft.tray_click.label())
                        .width(220.0)
                        .show_ui(ui, |ui| {
                            for click in TrayClick::all() {
                                ui.selectable_value(&mut self.draft.tray_click, click, click.label());
                            }
                        });
                });
                // macOS tints menu bar icons to suit the menu bar itself.
                if !cfg!(target_os = "macos") {
                    row(ui, "tray-icon", crate::locale::spelled!("Colour"),
                        Some("Each shown on a dark taskbar and a light one"), false, |ui| {
                        for style in TrayIconStyle::all() {
                            tray_icon_choice(ui, &mut self.draft.tray_icon, style);
                        }
                    });
                }
            });

            group(ui, "Setup", |ui| {
                row(ui, "transfer", "Move to another computer",
                    Some("Saves these settings, the hotkeys, upload destinations and capture tools' settings to a file, or loads them"),
                    false, |ui| {
                    if ui.button("Export\u{2026}").clicked() {
                        self.export_settings();
                    }
                    if ui
                        .button("Import\u{2026}")
                        .on_hover_text("Replaces these settings with ones exported from snapr")
                        .clicked()
                    {
                        self.import_settings(actions);
                    }
                });
                row(ui, "export-secrets", "Include upload secret keys in the export",
                    Some("So destinations work straight away after importing. They're written in the file as plain text: keep it private"),
                    true, |ui| {
                    theme::switch(ui, &mut self.export_secrets, "");
                });
                if let Some((note, is_err)) = &self.transfer_note {
                    let text = RichText::new(note.as_str());
                    row(ui, "transfer-note", if *is_err { text.color(ERROR) } else { text.weak() }, None, true, |_| {});
                }
                row(ui, "run-setup", "First-time setup",
                    Some("The questions from snapr's first start, with your current answers picked"), false, |ui| {
                    if ui.button("Run setup again").clicked() {
                        self.run_setup = true;
                    }
                });
            });

            self.save_bar(ui, actions);
        });
    }

    /// The About tab, which also checks for updates.
    pub fn about_ui(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        self.about.ui(ui, &mut self.draft.check_for_updates, &self.update, actions);
    }

    /// The Capture tab: how the overlay behaves while picking a region.
    pub fn capture_ui(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.label(
                RichText::new("Press the hotkey and drag a region to capture it (click for a whole screen). Draw first to annotate.")
                    .weak(),
            );

            group(ui, "While picking a region", |ui| {
                row(ui, "annotate", "Show the annotation toolbar",
                    Some("Arrows, text, boxes and more, to draw on the capture before it's taken"), false, |ui| {
                    theme::switch(ui, &mut self.draft.annotate, "");
                });
                row(ui, "overlay-fps", "Frame rate", Some("How often the screen redraws as the mouse moves"), false, |ui| {
                    // 0 matches the monitor. The custom rate goes before the
                    // choice, so the choice stays put at the right.
                    let mut custom = self.draft.overlay_fps != 0;
                    if custom {
                        ui.add(egui::DragValue::new(&mut self.draft.overlay_fps).range(15..=1000).suffix(" fps"));
                    }
                    theme::segmented(ui, |ui| {
                        ui.selectable_value(&mut custom, false, "Match monitor");
                        ui.selectable_value(&mut custom, true, "Custom");
                    });
                    if custom != (self.draft.overlay_fps != 0) {
                        self.draft.overlay_fps = if custom { 144 } else { 0 };
                    }
                });
            });

            group(ui, "Crosshair", |ui| {
                row(ui, "crosshair", "Beside it", Some("Shown next to the cursor while selecting a region"), false, |ui| {
                    let info = &mut self.draft.crosshair_info;
                    theme::checkbox(ui, &mut info.position, "Position")
                        .on_hover_text("The pixel's X and Y on the screen");
                    theme::checkbox(ui, &mut info.color, crate::locale::spelled!("Colour"))
                        .on_hover_text(crate::locale::spelled!("The pixel's colour, as hex and RGB"));
                    theme::checkbox(ui, &mut info.magnifier, "Magnifier")
                        .on_hover_text("The pixels around the cursor, enlarged");
                });
                row(ui, "magnifier-scroll", "Scrolling the magnifier",
                    Some(&crate::locale::spell("What the mouse wheel does to it, here and in the colour picker")),
                    false, |ui| {
                    let scroll = &mut self.draft.crosshair_info.scroll;
                    theme::segmented(ui, |ui| {
                        ui.selectable_value(scroll, MagnifierScroll::Resize, "Resizes it")
                            .on_hover_text("Bigger, or smaller until it's hidden");
                        ui.selectable_value(scroll, MagnifierScroll::Zoom, "Zooms it")
                            .on_hover_text("Fewer, bigger pixels, or more, smaller ones");
                    });
                });
            });

            group(ui, "Redaction", |ui| {
                row(ui, "redact-image", "Image", Some("The image redaction tool (I) covers areas with this picture"), false, |ui| {
                    ui.add(
                        TextEdit::singleline(&mut self.draft.redact_image).margin(theme::FIELD_MARGIN)
                            .hint_text("None (black boxes)")
                            .desired_width(260.0),
                    );
                    if ui.button("Browse\u{2026}").clicked()
                        && let Some(file) = rfd::FileDialog::new()
                            .add_filter("Images", &["png", "jpg", "jpeg", "gif", "webp", "bmp"])
                            .pick_file()
                    {
                        self.draft.redact_image = file.display().to_string();
                    }
                    if !self.draft.redact_image.is_empty() && ui.button("Clear").clicked() {
                        self.draft.redact_image.clear();
                    }
                });
                row(ui, "redact-stretch", "Stretch the image to fill each area",
                    Some("Squash or stretch it to the area's shape. Off, it keeps its proportions and is cropped to cover the area"),
                    true, |ui| {
                    theme::switch(ui, &mut self.draft.redact_stretch, "");
                });
            });

            self.save_bar(ui, actions);
        });
    }

    /// Asks where to, and exports the saved settings there.
    fn export_settings(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_file_name("snapr-settings.toml")
            .add_filter("snapr settings", &["toml"])
            .save_file()
        else {
            return;
        };
        self.transfer_note = Some(match crate::transfer::export(&path, &self.saved, self.export_secrets) {
            Ok(0) => (format!("Exported to {}", path.display()), false),
            Ok(n) => (
                format!(
                    "Exported to {}, with {n} secret key{}: keep the file private",
                    path.display(),
                    if n == 1 { "" } else { "s" }
                ),
                false,
            ),
            Err(e) => (e, true),
        });
    }

    /// Asks for a file of exported settings, and once that's confirmed, puts
    /// them in the form to be saved as if they'd been typed.
    fn import_settings(&mut self, actions: &mut Vec<Action>) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("snapr settings", &["toml"])
            .pick_file()
        else {
            return;
        };
        let imported = match crate::transfer::import(&path) {
            Ok(i) => i,
            Err(e) => {
                self.transfer_note = Some((e, true));
                return;
            }
        };
        let confirmed = rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Warning)
            .set_title("Import settings")
            .set_description(
                "Replace snapr's settings, hotkeys and upload destinations with the ones in this file?",
            )
            .set_buttons(rfd::MessageButtons::OkCancel)
            .show();
        if confirmed != rfd::MessageDialogResult::Ok {
            return;
        }
        self.draft = imported.settings;
        self.secrets = imported.secrets.into_iter().collect();
        if let Some(tools) = imported.tools {
            actions.push(Action::ImportTools(tools));
        }
        let mut note = format!("Imported {}", path.display());
        if !imported.notes.is_empty() {
            note.push_str(". For this computer: ");
            note.push_str(&imported.notes.join("; "));
        }
        self.transfer_note = Some((note, false));
    }

    /// The Recording tab.
    pub fn recording_ui(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.label(
                RichText::new(if cfg!(target_os = "linux") || self.draft.record_with_ffmpeg {
                    "Press the record hotkey and drag a region to record it; press it again to stop. \
                     Recordings are encoded with FFmpeg and saved next to your screenshots."
                } else {
                    "Press the record hotkey and drag a region to record it; press it again to stop. \
                     Recordings are saved next to your screenshots."
                })
                .weak(),
            );

            group(ui, "Video", |ui| {
                row(ui, "record-fps", "Frame rate", None, false, |ui| {
                    ui.add(egui::DragValue::new(&mut self.draft.record_fps).range(1..=120).suffix(" fps"));
                });

                if crate::encode::webm_supported() {
                    use crate::settings::RecordFormat;
                    let note = match self.draft.record_format {
                        RecordFormat::Mp4 => "H.264 and AAC: plays everywhere",
                        RecordFormat::Av1 => "AV1 from the graphics card: sharper for the size, but not for older devices",
                        RecordFormat::Webm => "VP9 and Opus: smaller files, plays in browsers",
                    };
                    row(ui, "record-format", "Format", Some(note), false, |ui| {
                        theme::segmented(ui, |ui| {
                            ui.selectable_value(&mut self.draft.record_format, RecordFormat::Mp4, "MP4")
                                .on_hover_text("H.264 and AAC: plays everywhere");
                            if cfg!(windows) {
                                ui.selectable_value(&mut self.draft.record_format, RecordFormat::Av1, "MP4 (AV1)")
                                    .on_hover_text(
                                        "AV1 and AAC, made by the graphics card (NVIDIA RTX 40, AMD RX 7000, Intel Arc or newer): \
                                         sharper for the size. Plays in browsers, Discord and Windows' apps; not on older devices. \
                                         Falls back to H.264 without such a card.",
                                    );
                            }
                            ui.selectable_value(&mut self.draft.record_format, RecordFormat::Webm, "WebM")
                                .on_hover_text("VP9 and Opus: smaller files, plays in browsers");
                        });
                    });
                }

                // Windows and macOS encode and play videos themselves, and
                // can encode with FFmpeg instead.
                if !cfg!(target_os = "linux") {
                    let note = if self.draft.record_with_ffmpeg {
                        "Sharper for the file size, but uses more CPU and needs FFmpeg installed"
                    } else {
                        "The system's own: nothing to install, and light on the CPU"
                    };
                    row(ui, "encoder", "Encoder", Some(note), false, |ui| {
                        theme::segmented(ui, |ui| {
                            ui.selectable_value(&mut self.draft.record_with_ffmpeg, false, "System")
                                .on_hover_text("The encoder the system provides: nothing to install, and light on the CPU");
                            ui.selectable_value(&mut self.draft.record_with_ffmpeg, true, "FFmpeg")
                                .on_hover_text(
                                    "x264 (MP4) or libvpx (WebM): sharper for the file size, but uses more CPU. \
                                     Needs FFmpeg installed; without it, recordings use the system's encoder. \
                                     AV1 and HDR recordings are still made by the graphics card.",
                                );
                        });
                    });
                }
                if cfg!(target_os = "linux") || self.draft.record_with_ffmpeg {
                    self.ffmpeg_rows(ui);
                }

                row(ui, "record-cursor", "Show the mouse pointer", None, false, |ui| {
                    theme::switch(ui, &mut self.draft.record_cursor, "");
                });
                if cfg!(windows) {
                    row(ui, "record-hdr", "Record in HDR when the display is in HDR mode",
                        Some("HDR10 MP4 (10-bit HEVC) made by the graphics card. Recordings start about a second later"),
                        false, |ui| {
                        theme::switch(ui, &mut self.draft.record_hdr, "");
                    });
                }
            });

            group(ui, "Sound", |ui| {
                row(ui, "record-system-audio", "What's playing", Some("Everything the default speakers or headphones play"), false, |ui| {
                    theme::switch(ui, &mut self.draft.record_system_audio, "");
                });
                row(ui, "record-microphone", "Microphone", None, false, |ui| {
                    let mics = self.microphones.get_or_insert_with(crate::audio::microphones);
                    let selected = if self.draft.microphone.is_empty() {
                        "Default".to_string()
                    } else {
                        self.draft.microphone.clone()
                    };
                    ui.add_enabled_ui(self.draft.record_microphone, |ui| {
                        egui::ComboBox::from_id_salt("microphone")
                            .selected_text(selected)
                            .width(260.0)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.draft.microphone, String::new(), "Default");
                                for m in mics.iter() {
                                    ui.selectable_value(&mut self.draft.microphone, m.clone(), m);
                                }
                            });
                    });
                    theme::switch(ui, &mut self.draft.record_microphone, "");
                });
            });

            self.save_bar(ui, actions);
        });
    }

    /// The FFmpeg rows of the Recording tab: where it is, whether it works,
    /// and its settings for the format being recorded.
    fn ffmpeg_rows(&mut self, ui: &mut egui::Ui) {
        let indent = !cfg!(target_os = "linux");
        row(ui, "ffmpeg", "FFmpeg", Some("The program, if it isn't on PATH"), indent, |ui| {
            ui.add(
                TextEdit::singleline(&mut self.draft.ffmpeg_path).margin(theme::FIELD_MARGIN)
                    .hint_text("ffmpeg (found on PATH)")
                    .desired_width(260.0),
            );
            if ui.button("Browse\u{2026}").clicked()
                && let Some(file) = rfd::FileDialog::new().pick_file()
            {
                self.draft.ffmpeg_path = file.display().to_string();
            }
            self.ffmpeg_check_button(ui);
        });
        match self.current_ffmpeg_check() {
            None => {}
            Some(None) => row(ui, "ffmpeg-check", RichText::new("Checking\u{2026}").weak(), None, indent, |ui| {
                ui.spinner();
            }),
            Some(Some(Ok(msg))) => row(ui, "ffmpeg-check", RichText::new(msg).color(SUCCESS), None, indent, |_| {}),
            Some(Some(Err(e))) => row(ui, "ffmpeg-check", RichText::new(e).color(ERROR), None, indent, |_| {}),
        }

        // The settings for the format being recorded.
        let ff = &mut self.draft.ffmpeg;
        if self.draft.record_format == crate::settings::RecordFormat::Webm && crate::encode::webm_supported() {
            row(ui, "ffmpeg-codec", "VP9 (WebM)", Some("Point at each for what it does"), indent, |ui| {
                ui.label("Speed");
                ui.add(egui::DragValue::new(&mut ff.vp9_speed).range(0..=8)).on_hover_text(
                    "0 (slowest, smallest files) to 8 (fastest). Below about 5 \
                     may not keep up with the recording.",
                );
                ui.label("Quality");
                ui.add(egui::DragValue::new(&mut ff.vp9_crf).range(0..=63).prefix("CRF "))
                    .on_hover_text("0 to 63: lower is sharper, with bigger files. 32 is the default.");
            });
        } else {
            row(ui, "ffmpeg-codec", "x264 (MP4)", Some("Point at each for what it does"), indent, |ui| {
                ui.label("Preset");
                egui::ComboBox::from_id_salt("x264_preset")
                    .selected_text(ff.x264_preset.as_str())
                    .show_ui(ui, |ui| {
                        for p in crate::settings::FfmpegOptions::X264_PRESETS {
                            ui.selectable_value(&mut ff.x264_preset, p.to_string(), *p);
                        }
                    })
                    .response
                    .on_hover_text(
                        "Slower presets make smaller files at the same quality, but use more CPU; \
                         too slow and the recording can't keep up. veryfast is the default.",
                    );
                ui.label("Quality");
                ui.add(egui::DragValue::new(&mut ff.x264_crf).range(0..=51).prefix("CRF "))
                    .on_hover_text(
                        "0 (lossless) to 51: lower is sharper, with bigger files. 23 is the default; \
                         18 looks about lossless.",
                    );
            });
        }
        row(ui, "ffmpeg-args", "Extra arguments", Some("Added after snapr's own, so they override them"), indent, |ui| {
            ui.add(
                TextEdit::singleline(&mut ff.extra_args).margin(theme::FIELD_MARGIN)
                    .hint_text("e.g. -tune stillimage")
                    .desired_width(260.0),
            );
        });
    }

    fn record_hotkey_problem(&self) -> Option<String> {
        let spec = self.draft.record_hotkey.trim();
        // The desktop picks the keys, and checks them.
        if spec.is_empty() || crate::hotkey::desktop_picks_keys() {
            return None;
        }
        match (parse_hotkey(spec), parse_hotkey(&self.draft.hotkey)) {
            (Err(e), _) => Some(e),
            (Ok(a), Ok(b)) if a == b => Some("same as the capture hotkey".into()),
            _ => None,
        }
    }

    /// Why a tool hotkey can't be used: it's missing, invalid, or taken by
    /// another hotkey (only the later of two equal tool hotkeys complains).
    fn tool_hotkey_problem(&self, i: usize) -> Option<String> {
        if crate::hotkey::desktop_picks_keys() {
            return None;
        }
        let spec = self.draft.tool_hotkeys[i].hotkey.trim();
        if spec.is_empty() {
            return Some("press Record, then the key combination".into());
        }
        let key = match parse_hotkey(spec) {
            Ok(k) => k,
            Err(e) => return Some(e),
        };
        let same = |s: &str| !s.trim().is_empty() && parse_hotkey(s).is_ok_and(|k| k == key);
        if same(&self.draft.hotkey) {
            Some("same as the capture hotkey".into())
        } else if same(&self.draft.record_hotkey) {
            Some("same as the record hotkey".into())
        } else {
            self.draft.tool_hotkeys[..i]
                .iter()
                .find(|t| same(&t.hotkey))
                .map(|t| format!("same as the {} hotkey", t.action.label().to_lowercase()))
        }
    }

    /// Adds a tool hotkey and starts recording its key combination (unless
    /// the desktop asks for it once it's saved).
    pub fn add_tool_hotkey(&mut self, action: ToolAction) {
        self.draft.tool_hotkeys.push(ToolHotkey {
            action,
            hotkey: String::new(),
        });
        if crate::hotkey::desktop_picks_keys() {
            return;
        }
        self.recording = true;
        self.recording_field = HotkeyField::Tool(self.draft.tool_hotkeys.len() - 1);
    }

    /// A hotkey text box with a Record button that captures the next key
    /// combination.
    fn hotkey_editor(&mut self, ui: &mut egui::Ui, field: HotkeyField) {
        if crate::hotkey::desktop_picks_keys() {
            let id = match field {
                HotkeyField::Capture => crate::hotkey::CAPTURE.to_string(),
                HotkeyField::Record => crate::hotkey::RECORD.to_string(),
                HotkeyField::Tool(i) => ToolHotkey::id(&self.draft.tool_hotkeys, i),
            };
            match crate::hotkey::desktop_keys(&id) {
                Some(keys) => match crate::hotkey::key_combinations(&keys) {
                    Some(combinations) => keycaps(ui, &combinations),
                    None => ui.label(keys),
                },
                None if crate::hotkey::desktop_asking() => {
                    ui.label(RichText::new("Waiting for the desktop\u{2026}").weak())
                }
                None => ui.label(RichText::new("No keys yet").weak()),
            };
            return;
        }
        if self.recording && self.recording_field == field {
            ui.add(egui::Button::new("Press a key combination\u{2026}").selected(true));
            if ui.button("Cancel").clicked() {
                self.recording = false;
            }
            return;
        }
        let value = match field {
            HotkeyField::Capture => &mut self.draft.hotkey,
            HotkeyField::Record => &mut self.draft.record_hotkey,
            HotkeyField::Tool(i) => &mut self.draft.tool_hotkeys[i].hotkey,
        };
        ui.add(
            TextEdit::singleline(value).margin(theme::FIELD_MARGIN)
                .hint_text("none")
                .desired_width(220.0),
        );
        if ui.button("Record").clicked() {
            self.recording = true;
            self.recording_field = field;
        }
    }

    fn hotkey_row(&mut self, ui: &mut egui::Ui, key: &str, title: &str, note: &str, field: HotkeyField) {
        let err = match field {
            HotkeyField::Capture => parse_hotkey(&self.draft.hotkey).err(),
            HotkeyField::Record => self.record_hotkey_problem(),
            HotkeyField::Tool(i) => self.tool_hotkey_problem(i),
        };
        row(ui, key, title, Some(note), false, |ui| self.hotkey_editor(ui, field));
        error_row(ui, &format!("{key}-error"), err.as_deref());
    }

    /// A tool hotkey: its tool, key combination, and a remove button.
    /// Returns whether it was removed.
    fn tool_hotkey_row(&mut self, ui: &mut egui::Ui, i: usize) -> bool {
        let err = self.tool_hotkey_problem(i);
        let key = format!("tool-hotkey-{i}");
        let mut remove = false;
        row(ui, &key, self.draft.tool_hotkeys[i].action.label(), None, false, |ui| {
            self.hotkey_editor(ui, HotkeyField::Tool(i));
            remove = ui.add(theme::danger(ui, "Remove")).clicked();
        });
        error_row(ui, &format!("{key}-error"), err.as_deref());
        remove
    }

    /// The Hotkeys tab: one row per action that can have a global hotkey.
    pub fn hotkeys_ui(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.label(
                    RichText::new("Global hotkeys work from any app while snapr is running.")
                        .weak(),
                );
                if crate::hotkey::desktop_picks_keys() {
                    #[cfg(target_os = "linux")]
                    let gnome = crate::portal::gnome();
                    #[cfg(not(target_os = "linux"))]
                    let gnome = false;
                    ui.label(
                        RichText::new(if gnome {
                            "Your desktop keeps the keys. To change them, open its settings \
                             and click Global Shortcuts. A new hotkey gets its keys when you \
                             save: the desktop asks for them."
                        } else {
                            "Your desktop keeps the keys: change them in its settings. A new \
                             hotkey gets its keys when you save: the desktop asks for them."
                        })
                        .weak(),
                    );
                    if crate::hotkey::desktop_asking() {
                        ui.label(
                            RichText::new(
                                "Your desktop is asking about snapr's hotkeys: answer its \
                                 dialog, which may be behind this window. Until then they \
                                 don't work.",
                            )
                            .color(ERROR),
                        );
                    }
                    if ui.button("Open system settings\u{2026}").clicked()
                        && let Err(e) = crate::hotkey::open_desktop_settings()
                    {
                        self.status = Some((e, true));
                    }
                }

                group(ui, "Capture and record", |ui| {
                    self.hotkey_row(ui, "capture-hotkey", "Capture region",
                        "Then drag a region, or click for a whole screen", HotkeyField::Capture);
                    let note = if crate::hotkey::desktop_picks_keys() {
                        "Press it again to stop"
                    } else {
                        "Press it again to stop. Leave it empty for no hotkey"
                    };
                    self.hotkey_row(ui, "record-hotkey", "Record region", note, HotkeyField::Record);
                });

                heading(ui, "Tools", Some(&crate::locale::spell(
                    "Run a tool straight away from anywhere, e.g. pick a colour without opening snapr",
                )));
                let mut removed = None;
                let r = card(ui, |ui| {
                    for i in 0..self.draft.tool_hotkeys.len() {
                        if self.tool_hotkey_row(ui, i) {
                            removed = Some(i);
                        }
                    }
                    block(ui, |ui| {
                        ui.menu_button("+ Add hotkey", |ui| {
                            for a in ToolAction::ALL {
                                if ui.button(a.label()).clicked() {
                                    self.add_tool_hotkey(a);
                                }
                            }
                        });
                    });
                });
                found(ui, "tool-hotkeys", r);
                if let Some(i) = removed {
                    self.draft.tool_hotkeys.remove(i);
                    self.recording = false;
                }

                self.save_bar(ui, actions);
            });
    }

    /// The Paths & naming tab: where screenshots are saved and what they're called.
    pub fn naming_ui(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let naming = self.draft.naming();

        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.label(
                RichText::new("Where saved screenshots go. Use Insert to add the date, window name, a counter and more.")
                    .weak(),
            );

            group(ui, "Saved screenshots", |ui| {
                row(ui, "folder", "Save folder", None, false, |ui| {
                    ui.add(
                        TextEdit::singleline(&mut self.draft.folder)
                            .margin(theme::FIELD_MARGIN)
                            .desired_width(300.0),
                    );
                    if ui.button("Browse\u{2026}").clicked()
                        && let Some(dir) = rfd::FileDialog::new().set_directory(&self.draft.folder).pick_folder()
                    {
                        self.draft.folder = dir.display().to_string();
                    }
                    if ui.button("Open").clicked() {
                        actions.push(Action::Reveal(PathBuf::from(&self.draft.folder)));
                    }
                });
                row(ui, "subfolder", "Sub-folder", Some("Inside the save folder, e.g. one for each month"), false, |ui| {
                    template_field(ui, "subfolder", &mut self.draft.subfolder, "none \u{2014} e.g. %y-%mo or %pn");
                });
                row(ui, "file-name", "File name", None, false, |ui| {
                    template_field(ui, "file_name", &mut self.draft.file_name, "");
                    ui.label(".png");
                });
                let example = match &naming {
                    Ok(n) => {
                        let example = n.preview(&Context {
                            process: Some("firefox".into()),
                            title: Some("Mozilla Firefox".into()),
                            width: 1280,
                            height: 720,
                            counter: output::peek_counter() + 1,
                            ..Context::new(chrono::Local::now())
                        });
                        RichText::new(format!("e.g. {}", example.display())).monospace().weak()
                    }
                    Err(e) => RichText::new(e.as_str()).color(ERROR),
                };
                row(ui, "naming-example", example, None, false, |_| {});
            });

            ui.add_space(6.0);
            ui.label(
                RichText::new("Uploaded files are named by each destination, under Destinations.")
                    .weak()
                    .size(11.5),
            );

            self.save_bar(ui, actions);
        });
    }

    /// Turns a key press into a hotkey string while recording. Returns true
    /// if the event was used.
    pub fn record(
        &mut self,
        modifiers: ModifiersState,
        code: KeyCode,
        state: ElementState,
    ) -> bool {
        use KeyCode::*;
        if matches!(
            code,
            ShiftLeft
                | ShiftRight
                | ControlLeft
                | ControlRight
                | AltLeft
                | AltRight
                | SuperLeft
                | SuperRight
        ) {
            return true;
        }
        // Windows only reports PrintScreen on release.
        if state == ElementState::Released && code != PrintScreen {
            return true;
        }
        if code == Escape && modifiers.is_empty() {
            self.recording = false;
            return true;
        }
        let mut parts = Vec::new();
        for (on, name) in [
            (modifiers.control_key(), "Ctrl"),
            (modifiers.alt_key(), "Alt"),
            (modifiers.super_key(), "Meta"),
            (modifiers.shift_key(), "Shift"),
        ] {
            if on {
                parts.push(name.to_string());
            }
        }
        parts.push(format!("{code:?}"));
        let hotkey = parts.join(" + ");
        match self.recording_field {
            HotkeyField::Capture => self.draft.hotkey = hotkey,
            HotkeyField::Record => self.draft.record_hotkey = hotkey,
            HotkeyField::Tool(i) => {
                if let Some(t) = self.draft.tool_hotkeys.get_mut(i) {
                    t.hotkey = hotkey;
                }
            }
        }
        self.recording = false;
        true
    }
}

/// A template text box with an "Insert" menu of placeholders by category,
/// which inserts at the text cursor.
pub(crate) fn template_field(ui: &mut egui::Ui, id_salt: &str, text: &mut String, hint: &str) {
    let id = ui.make_persistent_id(id_salt);
    ui.add(
        TextEdit::singleline(text).margin(theme::FIELD_MARGIN)
            .id(id)
            .hint_text(hint)
            .desired_width(250.0),
    );
    ui.menu_button("Insert\u{2026}", |ui| {
        for (category, items) in CATEGORIES {
            ui.menu_button(*category, |ui| {
                for (token, desc) in *items {
                    let button =
                        egui::Button::new(*desc).shortcut_text(RichText::new(*token).monospace());
                    if ui.add(button).clicked() {
                        insert_at_cursor(ui.ctx(), id, text, token);
                        ui.close();
                    }
                }
            });
        }
    });
}

impl Form {
    /// Checks in the background that the FFmpeg path runs and can record
    /// the chosen format.
    fn ffmpeg_check_button(&mut self, ui: &mut egui::Ui) {
        let webm = self.draft.record_format == crate::settings::RecordFormat::Webm;
        let running = self.current_ffmpeg_check().is_some_and(|r| r.is_none());
        if !ui
            .add_enabled(!running, egui::Button::new("Test"))
            .on_hover_text("Check that snapr can run this FFmpeg and record with it")
            .clicked()
        {
            return;
        }
        let program = self.draft.ffmpeg_path.clone();
        let result = Arc::new(Mutex::new(None));
        let (out, ctx, path) = (result.clone(), ui.ctx().clone(), program.clone());
        std::thread::spawn(move || {
            *out.lock().unwrap() = Some(crate::encode::check_ffmpeg(&path, webm));
            ctx.request_repaint();
        });
        self.ffmpeg_check = Some(FfmpegCheck { program, webm, result });
    }

    /// The last FFmpeg check's outcome (`None` while it runs), if it was of
    /// the path and format now set.
    fn current_ffmpeg_check(&self) -> Option<Option<Result<String, String>>> {
        let check = self.ffmpeg_check.as_ref()?;
        let webm = self.draft.record_format == crate::settings::RecordFormat::Webm;
        (check.program == self.draft.ffmpeg_path && check.webm == webm)
            .then(|| check.result.lock().unwrap().clone())
    }
}

fn insert_at_cursor(ctx: &egui::Context, id: egui::Id, text: &mut String, token: &str) {
    let mut state = TextEdit::load_state(ctx, id).unwrap_or_default();
    let len = text.chars().count();
    let at = state
        .cursor
        .char_range()
        .map_or(len, |r| r.primary.index.0.min(len));
    let byte = text.char_indices().nth(at).map_or(text.len(), |(b, _)| b);
    text.insert_str(byte, token);
    let after = egui::text::CCursor::new(at + token.chars().count());
    state
        .cursor
        .set_char_range(Some(egui::text::CCursorRange::one(after)));
    state.store(ctx, id);
    ctx.memory_mut(|m| m.request_focus(id));
}

/// Whether `msg` is about a permission snapr is missing, which
/// [`permission_card`] explains.
pub(crate) fn is_missing_permission(msg: &str) -> bool {
    #[cfg(target_os = "macos")]
    if crate::macos::missing_permission(msg).is_some() {
        return true;
    }
    #[cfg(target_os = "linux")]
    if msg.ends_with(crate::portal::SCREENSHOTS_OFF) {
        return true;
    }
    #[cfg(windows)]
    let _ = msg;
    false
}

/// A card for `msg` if it's about a permission snapr is missing: what's
/// off and buttons to turn it on. Returns whether it showed one.
#[cfg_attr(windows, allow(unused_variables))]
pub(crate) fn permission_card(ui: &mut egui::Ui, msg: &str, actions: &mut Vec<Action>) -> bool {
    // macOS only tells a program that started without the permission once
    // it's started again.
    #[cfg(target_os = "macos")]
    if let Some(missing) = crate::macos::missing_permission(msg) {
        missing_card(ui, missing.title, missing.steps, None, |ui| {
            if ui.add(theme::primary("Open System Settings")).clicked() {
                actions.push(Action::OpenUrl(missing.pane.into()));
            }
            if ui.button("Restart snapr").clicked() {
                actions.push(Action::RestartToUpdate);
            }
        });
        return true;
    }
    #[cfg(target_os = "linux")]
    if let Some(why) = msg.strip_suffix(crate::portal::SCREENSHOTS_OFF) {
        let why = why.trim_end_matches([' ', '\u{2014}']);
        missing_card(
            ui,
            "snapr can't take screenshots",
            "The desktop asks before letting an app take screenshots, and GNOME only asks \
                the app you're using. Click Allow screenshots, then Allow when the desktop asks.",
            (!why.is_empty()).then(|| format!("Last time, {why}.")).as_deref(),
            |ui| {
                if ui.add(theme::primary("Allow screenshots")).clicked() {
                    actions.push(Action::AllowScreenshots);
                }
            },
        );
        return true;
    }
    false
}

/// A missing permission's card: what's off (`title`), how to turn it on,
/// what went wrong last time if anything, and `buttons`.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn missing_card(
    ui: &mut egui::Ui,
    title: &str,
    steps: &str,
    detail: Option<&str>,
    buttons: impl FnOnce(&mut egui::Ui),
) {
    let p = theme::palette(ui);
    egui::Frame::new()
        .fill(p.surface)
        .stroke(egui::Stroke::new(1.0, p.danger))
        .corner_radius(10)
        .inner_margin(12)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(title).color(p.danger).strong());
            ui.add_space(2.0);
            ui.add(egui::Label::new(RichText::new(steps).color(p.text)).wrap());
            if let Some(detail) = detail {
                ui.add_space(2.0);
                ui.add(egui::Label::new(RichText::new(detail).weak()).wrap());
            }
            ui.add_space(8.0);
            ui.horizontal(buttons);
        });
}

/// A titled group of settings, on a card of [`row`]s. Returns where the
/// card is.
pub(crate) fn group(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) -> egui::Rect {
    heading(ui, title, None);
    card(ui, add)
}

/// The title over a group of settings, and what they're for if there's a
/// `note`. Returns where the title is.
pub(crate) fn heading(ui: &mut egui::Ui, title: &str, note: Option<&str>) -> egui::Rect {
    let p = theme::palette(ui);
    // Space from what's above, not from the top of the page.
    if ui.cursor().top() > ui.min_rect().top() + 0.5 {
        ui.add_space(18.0);
    }
    let r = ui.label(RichText::new(title.to_uppercase()).size(11.0).strong().color(p.text_weak));
    if let Some(note) = note {
        ui.label(RichText::new(note).size(11.5).color(p.text_weak));
    }
    ui.add_space(2.0);
    r.rect
}

/// A card of [`row`]s and [`block`]s, divided by lines. Returns where it is.
pub(crate) fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) -> egui::Rect {
    let p = theme::palette(ui);
    egui::Frame::new()
        .fill(p.card)
        .stroke(egui::Stroke::new(1.0, p.divider))
        .corner_radius(8)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            let top = ui.cursor().top();
            ui.data_mut(|d| d.insert_temp(group_top_id(), top));
            add(ui);
        })
        .response
        .rect
}

/// Anything on a [`card`] other than a setting's row, padded as rows are.
pub(crate) fn block<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> egui::InnerResponse<R> {
    let item_spacing = ui.ctx().global_style().spacing.item_spacing;
    let r = egui::Frame::new().inner_margin(egui::Margin::symmetric(14, 10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing = item_spacing;
        add(ui)
    });
    divider_above(ui, r.response.rect);
    r
}

/// The line between something on a card and what's above it, unless it's
/// the first thing on it.
fn divider_above(ui: &egui::Ui, rect: egui::Rect) {
    let first = ui.data(|d| d.get_temp::<f32>(group_top_id())).is_some_and(|t| rect.top() <= t + 0.5);
    if !first {
        ui.painter().hline(rect.x_range(), rect.top(), egui::Stroke::new(1.0, theme::palette(ui).divider));
    }
}

/// Where the group being laid out starts, so its first row knows it's first.
fn group_top_id() -> egui::Id {
    egui::Id::new("settings-group-top")
}

/// A row of a [`group`]: the setting's name, with what it does under it if
/// there's a `note`, and its control on the right. `indent` sets it in under
/// the row above, for a setting that only matters with that one. `key` is
/// what the search points out it by.
pub(crate) fn row(
    ui: &mut egui::Ui,
    key: &str,
    title: impl Into<egui::WidgetText>,
    note: Option<&str>,
    indent: bool,
    control: impl FnOnce(&mut egui::Ui),
) {
    use egui::{Align, Layout, Rect, TextStyle, TextWrapMode, UiBuilder, Vec2, WidgetText, pos2, vec2};
    const PAD: Vec2 = vec2(14.0, 9.0);
    const GAP: f32 = 16.0;
    let p = theme::palette(ui);
    let full = ui.available_rect_before_wrap();
    let top = ui.cursor().top();
    let left = full.left() + PAD.x + if indent { 20.0 } else { 0.0 };
    let right = full.right() - PAD.x;

    let width = right - left;

    // The control is put at the right once its size is known: the first
    // time it's laid out after the text and measured, and that pass is
    // thrown away for another. Where it would crowd the text, it goes
    // under it instead.
    let id = ui.id().with(("settings-row", key));
    let measured: Option<Vec2> = ui.data(|d| d.get_temp(id));
    let control_size = measured.unwrap_or(Vec2::ZERO);

    let title: WidgetText = title.into();
    let text_color = if ui.is_enabled() { p.text } else { p.text_weak };
    let title_width = title.clone().into_galley(ui, Some(TextWrapMode::Wrap), width, TextStyle::Body).size().x;
    // Room beside the control for the title, and a fair width for a note.
    let text_min = if note.is_some() { title_width.max(180.0) } else { title_width }.min(width);
    let stacked = control_size.x > 0.0 && control_size.x + GAP + text_min > width;
    let wrap = if stacked { width } else { width - control_size.x - GAP };
    let title = title.into_galley(ui, Some(TextWrapMode::Wrap), wrap, TextStyle::Body);
    let note = note.map(|n| {
        WidgetText::from(RichText::new(n).size(11.5).color(p.text_weak))
            .into_galley(ui, Some(TextWrapMode::Wrap), wrap, TextStyle::Body)
    });
    let text_height = title.size().y + note.as_ref().map_or(0.0, |n| n.size().y + 2.0);
    let height = if stacked {
        text_height + 8.0 + control_size.y
    } else {
        text_height.max(control_size.y).max(ui.spacing().interact_size.y)
    };
    let rect = Rect::from_min_max(pos2(full.left(), top), pos2(full.right(), top + height + 2.0 * PAD.y));
    let inner = rect.shrink2(vec2(0.0, PAD.y));

    let text_width = title.size().x.max(note.as_ref().map_or(0.0, |n| n.size().x));
    let control_rect = if stacked {
        Rect::from_min_max(pos2(left, inner.bottom() - control_size.y), pos2(right, inner.bottom()))
    } else {
        let control_left = match measured {
            Some(size) => right - size.x - 1.0,
            None => left + text_width + GAP,
        };
        Rect::from_min_max(pos2(control_left, inner.top()), pos2(right, inner.bottom()))
    };
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(control_rect)
            .layout(Layout::left_to_right(Align::Center)),
    );
    control(&mut child);
    let size = child.min_rect().size();
    if measured.is_none_or(|m| (m - size).length() > 0.5) {
        ui.data_mut(|d| d.insert_temp(id, size));
        ui.ctx().request_discard("settings row measured");
    }

    divider_above(ui, rect);
    let painter = ui.painter();
    let y = if stacked { inner.top() } else { inner.center().y - text_height / 2.0 };
    let below = y + title.size().y + 2.0;
    if indent {
        // An elbow from the row above into the title, to show it belongs to it.
        const R: f32 = 4.0;
        let x = full.left() + PAD.x + 4.5;
        let mid = (y + title.size().y / 2.0).round() + 0.5;
        let mut points = vec![pos2(x, inner.top() - 1.0)];
        points.extend((0..=6).map(|i| {
            let a = std::f32::consts::FRAC_PI_2 * i as f32 / 6.0;
            pos2(x + R - R * a.cos(), mid - R + R * a.sin())
        }));
        points.push(pos2(left - 6.0, mid));
        painter.line(points, egui::Stroke::new(1.5, p.text_weak));
    }
    painter.galley(pos2(left, y), title, text_color);
    if let Some(note) = note {
        painter.galley(pos2(left, below), note, p.text_weak);
    }
    ui.advance_cursor_after_rect(rect);
    // Outlined just inside the card's edge.
    found(ui, key, rect.shrink2(vec2(10.0, 6.0)));
}

/// Why the setting in the row above can't be used, if it can't.
/// Key combinations drawn as keys, e.g. [Ctrl] + [J], with "or" between
/// combinations.
fn keycaps(ui: &mut egui::Ui, combinations: &[Vec<String>]) -> egui::Response {
    let p = theme::palette(ui);
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (i, keys) in combinations.iter().enumerate() {
            if i > 0 {
                ui.label(RichText::new("or").weak());
            }
            for (j, key) in keys.iter().enumerate() {
                if j > 0 {
                    ui.label(RichText::new("+").weak());
                }
                egui::Frame::new()
                    .fill(p.control)
                    .stroke(egui::Stroke::new(1.0, p.stroke))
                    .corner_radius(4)
                    .inner_margin(egui::Margin::symmetric(6, 1))
                    .show(ui, |ui| ui.label(RichText::new(key).size(12.0).color(p.text)));
            }
        }
    })
    .response
}

fn error_row(ui: &mut egui::Ui, key: &str, err: Option<&str>) {
    if let Some(e) = err {
        row(ui, key, RichText::new(e).color(ERROR), None, true, |_| {});
    }
}

/// A tile for picking `style`: the tray icon in that style on a dark
/// taskbar and a light one, side by side, with its name below.
fn tray_icon_choice(ui: &mut egui::Ui, picked: &mut TrayIconStyle, style: TrayIconStyle) {
    use egui::{Align2, CornerRadius, FontId, Rect, Sense, Stroke, StrokeKind, vec2};
    // Roughly Windows 11's own taskbars.
    const DARK_TASKBAR: Color32 = Color32::from_rgb(0x20, 0x20, 0x20);
    const LIGHT_TASKBAR: Color32 = Color32::from_rgb(0xee, 0xee, 0xee);
    let p = theme::palette(ui);
    let (rect, mut response) = ui.allocate_exact_size(vec2(88.0, 66.0), Sense::click());
    if response.clicked() && *picked != style {
        *picked = style;
        response.mark_changed();
    }
    let on = *picked == style;
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::RadioButton, ui.is_enabled(), on, style.label())
    });
    if style == TrayIconStyle::Taskbar {
        response = response.on_hover_text("White on a dark taskbar, black on a light one, changing when the taskbar does");
    }
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter();
    let (fill, stroke) = if on {
        (p.selected, theme::ACCENT)
    } else if response.hovered() {
        (p.control_hover, p.stroke_hover)
    } else {
        (p.control, p.stroke)
    };
    painter.rect(rect, CornerRadius::same(8), fill, Stroke::new(1.0, stroke), StrokeKind::Inside);
    let swatch = Rect::from_min_size(rect.min + vec2(8.0, 8.0), vec2(rect.width() - 16.0, 30.0));
    let (dark, light) = swatch.split_left_right_at_fraction(0.5);
    painter.rect_filled(dark, CornerRadius { nw: 5, sw: 5, ..Default::default() }, DARK_TASKBAR);
    painter.rect_filled(light, CornerRadius { ne: 5, se: 5, ..Default::default() }, LIGHT_TASKBAR);
    for (half, is_light) in [(dark, false), (light, true)] {
        let [r, g, b, _] = crate::tray::color_on(style, is_light);
        paint_logo(painter, Rect::from_center_size(half.center(), vec2(18.0, 18.0)), Color32::from_rgb(r, g, b));
    }
    let text = if on { p.selected_text } else { p.text };
    painter.text(
        egui::pos2(rect.center().x, swatch.bottom() + 6.0),
        Align2::CENTER_TOP,
        style.label(),
        FontId::proportional(12.0),
        text,
    );
}

/// The viewfinder logo filling `rect`, as in assets/icon.svg: its 32-unit
/// grid scaled to fit.
pub(crate) fn paint_logo(painter: &egui::Painter, rect: egui::Rect, color: Color32) {
    const PARTS: [[f32; 4]; 9] = [
        [2.0, 2.0, 13.0, 6.0],
        [2.0, 6.0, 6.0, 13.0],
        [19.0, 2.0, 30.0, 6.0],
        [26.0, 6.0, 30.0, 13.0],
        [2.0, 26.0, 13.0, 30.0],
        [2.0, 19.0, 6.0, 26.0],
        [19.0, 26.0, 30.0, 30.0],
        [26.0, 19.0, 30.0, 26.0],
        [12.0, 12.0, 20.0, 20.0],
    ];
    let unit = rect.width() / 32.0;
    for [x0, y0, x1, y1] in PARTS {
        let part = egui::Rect::from_min_max(
            rect.min + egui::vec2(x0, y0) * unit,
            rect.min + egui::vec2(x1, y1) * unit,
        );
        painter.rect_filled(part, 0.0, color);
    }
}

#[cfg(test)]
mod tests {
    use super::Form;
    use crate::settings::{Settings, ToolAction, ToolHotkey};
    use winit::event::ElementState;
    use winit::keyboard::{KeyCode, ModifiersState};

    #[test]
    fn tool_hotkeys_must_be_set_and_unique() {
        let mut form = Form::new(&Settings::default());
        form.add_tool_hotkey(ToolAction::PickColor);
        assert!(form.recording);
        assert!(form.problem().is_some(), "no key combination yet");
        form.record(
            ModifiersState::CONTROL | ModifiersState::SHIFT,
            KeyCode::KeyC,
            ElementState::Pressed,
        );
        assert!(!form.recording);
        assert_eq!(form.draft.tool_hotkeys[0].hotkey, "Ctrl + Shift + KeyC");
        assert_eq!(form.problem(), None);

        form.draft.tool_hotkeys.push(ToolHotkey {
            action: ToolAction::ScanQr,
            hotkey: "Ctrl + Shift + KeyC".into(),
        });
        assert_eq!(
            form.problem().as_deref(),
            Some("Read a QR code hotkey: same as the pick a colour hotkey")
        );
        form.draft.tool_hotkeys[1].hotkey = form.draft.hotkey.clone();
        assert_eq!(
            form.problem().as_deref(),
            Some("Read a QR code hotkey: same as the capture hotkey")
        );
        form.draft.tool_hotkeys[1].hotkey = "Ctrl + Alt + KeyQ".into();
        assert_eq!(form.problem(), None);
    }

    /// Renders the General, Hotkeys, Paths & naming and Recording tabs to
    /// `target/settings-*-preview.png`: `cargo test settings_preview -- --ignored`.
    #[test]
    #[ignore]
    fn settings_preview() {
        let mut form = Form::new(&Settings::default());
        form.draft.tool_hotkeys = vec![
            ToolHotkey {
                action: ToolAction::PickColor,
                hotkey: "Ctrl + Shift + KeyC".into(),
            },
            ToolHotkey {
                action: ToolAction::ScanQr,
                hotkey: String::new(),
            },
        ];
        form.transfer_note = Some((
            "Imported D:\\snapr-settings.toml. For this computer: R2 is off until its secret \
             key is entered on Destinations"
                .into(),
            false,
        ));
        type Tab = fn(&mut Form, &mut egui::Ui);
        let tabs: [(&str, Tab); 5] = [
            ("general", |f, ui| f.ui(ui, &mut Vec::new())),
            ("capture", |f, ui| f.capture_ui(ui, &mut Vec::new())),
            ("hotkeys", |f, ui| f.hotkeys_ui(ui, &mut Vec::new())),
            ("naming", |f, ui| f.naming_ui(ui, &mut Vec::new())),
            ("recording", |f, ui| f.recording_ui(ui, &mut Vec::new())),
        ];
        for (name, tab) in tabs {
            crate::preview::render(
                &format!("settings-{name}-preview"),
                [920, 900],
                1.0,
                |root| {
                    crate::theme::apply(root.ctx());
                    egui::CentralPanel::default().show(root, |ui| tab(&mut form, ui));
                },
            );
        }
        form.draft.record_with_ffmpeg = true;
        crate::preview::render("settings-recording-ffmpeg-preview", [920, 900], 1.0, |root| {
            crate::theme::apply(root.ctx());
            egui::CentralPanel::default().show(root, |ui| form.recording_ui(ui, &mut Vec::new()));
        });
        crate::preview::render("settings-general-light-preview", [920, 900], 1.0, |root| {
            crate::theme::apply(root.ctx());
            root.ctx().set_theme(egui::Theme::Light);
            egui::CentralPanel::default().show(root, |ui| form.ui(ui, &mut Vec::new()));
        });
    }
}
