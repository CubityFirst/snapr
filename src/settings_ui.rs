//! The General, Hotkeys and Paths & naming tabs of the Settings page, and
//! the form state they share with the Destinations tab.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::{Color32, RichText, TextEdit};
use winit::event::ElementState;
use winit::keyboard::{KeyCode, ModifiersState};

use crate::naming::{CATEGORIES, Context};
use crate::output;
use crate::secrets;
use crate::settings::{Settings, ToolAction, ToolHotkey, Upload, parse_hotkey};
use crate::update;

pub enum Action {
    /// Apply these settings and store these new secret keys (upload id → key).
    Save(Settings, HashMap<String, String>),
    Reveal(PathBuf),
    /// Try an upload destination, with a secret key typed but not yet saved.
    TestUpload(Upload, Option<String>),
    CheckForUpdates,
    /// Quit and start the updated program.
    RestartToUpdate,
    OpenUrl(String),
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
                .filter(|u| secrets::get(&u.id).is_some())
                .map(|u| u.id.clone())
                .collect(),
            tests: HashMap::new(),
            expanded: None,
            microphones: None,
            seen: (settings.clone(), HashMap::new(), Instant::now()),
            submitted: None,
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
            if !self.has_secret(&u.id) {
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
        ui.horizontal(|ui| {
            if let Some(p) = self.problem().filter(|_| self.dirty()) {
                ui.colored_label(ERROR, format!("Not saved \u{2014} {p}"));
            } else if let Some((msg, is_err)) = &self.status {
                let color = if *is_err {
                    ERROR
                } else {
                    ui.visuals().weak_text_color()
                };
                ui.colored_label(color, msg);
            } else if let Some(path) = &self.last_capture
                && ui.link(format!("Last: {}", path.display())).clicked()
            {
                actions.push(Action::Reveal(path.clone()));
            }
        });
    }

    /// The General tab.
    pub fn ui(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.label(
                RichText::new("Press the hotkey and drag a region to capture it (click for a whole screen). Draw first to annotate.")
                    .weak(),
            );
            ui.add_space(10.0);

            egui::Grid::new("settings").num_columns(2).spacing([14.0, 8.0]).show(ui, |ui| {
                ui.label("");
                ui.checkbox(&mut self.draft.annotate, "Show the annotation toolbar while capturing");
                ui.end_row();

                ui.label("");
                ui.checkbox(&mut self.draft.play_sounds, "Play sounds (shutter on capture)");
                ui.end_row();

                ui.label("");
                ui.checkbox(&mut self.draft.show_toast, "Show a preview in the corner after each capture")
                    .on_hover_text("Click it to open the link (or the image), middle-click to copy the image, right-click to close it");
                ui.end_row();

                ui.label("Overlay frame rate");
                ui.horizontal(|ui| {
                    let mut custom = self.draft.overlay_fps != 0;
                    ui.radio_value(&mut custom, false, "Match monitor");
                    ui.radio_value(&mut custom, true, "Custom");
                    if custom {
                        if self.draft.overlay_fps == 0 {
                            self.draft.overlay_fps = 144;
                        }
                        ui.add(egui::DragValue::new(&mut self.draft.overlay_fps).range(15..=1000).suffix(" fps"));
                    } else {
                        self.draft.overlay_fps = 0;
                    }
                });
                ui.end_row();

                ui.label("Redaction image").on_hover_text(
                    "The image redaction tool (I) covers areas with this picture",
                );
                ui.horizontal(|ui| {
                    ui.add(
                        TextEdit::singleline(&mut self.draft.redact_image)
                            .hint_text("None (black boxes)")
                            .desired_width(300.0),
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
                ui.end_row();

                ui.label("");
                ui.checkbox(&mut self.draft.redact_stretch, "Stretch the image to fill each area")
                    .on_hover_text(
                        "Squash or stretch it to the area's shape. Off, it keeps its proportions \
                         and is cropped to cover the area",
                    );
                ui.end_row();

                ui.label(RichText::new("Recording").strong());
                ui.end_row();

                ui.label("Frame rate");
                ui.add(egui::DragValue::new(&mut self.draft.record_fps).range(1..=120).suffix(" fps"));
                ui.end_row();

                if crate::encode::webm_supported() {
                    use crate::settings::RecordFormat;
                    ui.label("Format");
                    ui.horizontal(|ui| {
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
                    ui.end_row();
                }

                // Windows and macOS encode and play videos themselves, and
                // can encode with FFmpeg instead.
                if !cfg!(target_os = "linux") {
                    ui.label("Encoder");
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut self.draft.record_with_ffmpeg, false, "System")
                            .on_hover_text("The encoder the system provides: nothing to install, and light on the CPU");
                        ui.selectable_value(&mut self.draft.record_with_ffmpeg, true, "FFmpeg")
                            .on_hover_text(
                                "x264 (MP4) or libvpx (WebM): sharper for the file size, but uses more CPU. \
                                 Needs FFmpeg installed; without it, recordings use the system's encoder. \
                                 AV1 and HDR recordings are still made by the graphics card.",
                            );
                    });
                    ui.end_row();
                }
                if cfg!(target_os = "linux") || self.draft.record_with_ffmpeg {
                    ui.label("FFmpeg");
                    ui.horizontal(|ui| {
                        ui.add(
                            TextEdit::singleline(&mut self.draft.ffmpeg_path)
                                .hint_text("ffmpeg (found on PATH)")
                                .desired_width(300.0),
                        );
                        if ui.button("Browse\u{2026}").clicked()
                            && let Some(file) = rfd::FileDialog::new().pick_file()
                        {
                            self.draft.ffmpeg_path = file.display().to_string();
                        }
                    });
                    ui.end_row();

                    // The settings for the format being recorded.
                    let ff = &mut self.draft.ffmpeg;
                    if self.draft.record_format == crate::settings::RecordFormat::Webm && crate::encode::webm_supported() {
                        ui.label("VP9 (WebM)");
                        ui.horizontal(|ui| {
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
                        ui.label("x264 (MP4)");
                        ui.horizontal(|ui| {
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
                    ui.end_row();
                    ui.label("Extra arguments");
                    ui.add(
                        TextEdit::singleline(&mut ff.extra_args)
                            .hint_text("e.g. -tune stillimage")
                            .desired_width(300.0),
                    )
                    .on_hover_text(
                        "Added to FFmpeg's output options, after snapr's own, so they override them",
                    );
                    ui.end_row();
                }
                ui.label("");
                ui.checkbox(&mut self.draft.record_cursor, "Show the mouse pointer");
                ui.end_row();
                if cfg!(windows) {
                    ui.label("");
                    ui.checkbox(&mut self.draft.record_hdr, "Record in HDR when the display is in HDR mode")
                        .on_hover_text(
                            "HDR10 MP4 (10-bit HEVC) made by the graphics card. Recordings start about a second later.",
                        );
                    ui.end_row();
                }

                ui.label("Sound");
                ui.vertical(|ui| {
                    ui.checkbox(&mut self.draft.record_system_audio, "What's playing (system audio)")
                        .on_hover_text("Everything the default speakers or headphones play");
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.draft.record_microphone, "Microphone");
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
                    });
                });
                ui.end_row();

                ui.label("");
                ui.label(
                    RichText::new(if cfg!(target_os = "linux") || self.draft.record_with_ffmpeg {
                        "Recordings are encoded with FFmpeg and saved next to your screenshots."
                    } else {
                        "Recordings are saved next to your screenshots."
                    })
                        .weak()
                        .small(),
                );
                ui.end_row();

                ui.label(RichText::new("Updates").strong());
                ui.end_row();

                ui.label("");
                let mut never = !self.draft.check_for_updates;
                if ui
                    .checkbox(&mut never, "Do not check for updates")
                    .on_hover_text(
                        "Otherwise snapr looks for a new release on GitHub when it starts and twice a day, \
                         and installs it to run from the next start",
                    )
                    .changed()
                {
                    self.draft.check_for_updates = !never;
                }
                ui.end_row();

                ui.label("Version");
                ui.horizontal(|ui| {
                    ui.label(update::VERSION);
                    if ui
                        .add_enabled(!self.update.busy(), egui::Button::new("Check for updates now"))
                        .clicked()
                    {
                        actions.push(Action::CheckForUpdates);
                    }
                });
                ui.end_row();

                if !matches!(self.update, update::Status::Idle) {
                    ui.label("");
                    self.update_status_ui(ui, actions);
                    ui.end_row();
                }
            });

            self.save_bar(ui, actions);
        });
    }

    /// What the last update check found, and what can be done about it.
    fn update_status_ui(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        use update::Status;
        ui.horizontal(|ui| match &self.update {
            Status::Idle => {}
            Status::Checking => {
                ui.spinner();
                ui.label("Checking for updates\u{2026}");
            }
            Status::UpToDate(at) => {
                ui.label(
                    RichText::new(format!("Up to date (checked {})", at.format("%H:%M"))).weak(),
                );
            }
            Status::Downloading(version) => {
                ui.spinner();
                ui.label(format!("Downloading snapr {version}\u{2026}"));
            }
            Status::Available(version, page) => {
                ui.label(format!("snapr {version} is available"));
                if ui.link("Release notes").clicked() {
                    actions.push(Action::OpenUrl(page.clone()));
                }
                ui.label(RichText::new("(development builds don't install it)").weak());
            }
            Status::Installed(version, page) => {
                ui.colored_label(SUCCESS, format!("snapr {version} is installed"));
                if ui.button("Restart now").clicked() {
                    actions.push(Action::RestartToUpdate);
                }
                if ui.link("Release notes").clicked() {
                    actions.push(Action::OpenUrl(page.clone()));
                }
            }
            Status::Failed(e) => {
                ui.colored_label(ERROR, format!("Couldn't update: {e}"));
            }
        });
    }

    fn record_hotkey_problem(&self) -> Option<String> {
        let spec = self.draft.record_hotkey.trim();
        if spec.is_empty() {
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

    /// Adds a tool hotkey and starts recording its key combination.
    pub fn add_tool_hotkey(&mut self, action: ToolAction) {
        self.draft.tool_hotkeys.push(ToolHotkey {
            action,
            hotkey: String::new(),
        });
        self.recording = true;
        self.recording_field = HotkeyField::Tool(self.draft.tool_hotkeys.len() - 1);
    }

    /// A hotkey text box with a Record button that captures the next key
    /// combination, and anything `extra` adds after them.
    fn hotkey_editor(
        &mut self,
        ui: &mut egui::Ui,
        field: HotkeyField,
        extra: impl FnOnce(&mut egui::Ui),
    ) {
        ui.horizontal(|ui| {
            if self.recording && self.recording_field == field {
                ui.add(egui::Button::new("Press a key combination\u{2026}").selected(true));
                if ui.button("Cancel").clicked() {
                    self.recording = false;
                }
            } else {
                let value = match field {
                    HotkeyField::Capture => &mut self.draft.hotkey,
                    HotkeyField::Record => &mut self.draft.record_hotkey,
                    HotkeyField::Tool(i) => &mut self.draft.tool_hotkeys[i].hotkey,
                };
                ui.add(
                    TextEdit::singleline(value)
                        .hint_text("none")
                        .desired_width(220.0),
                );
                if ui.button("Record").clicked() {
                    self.recording = true;
                    self.recording_field = field;
                }
            }
            extra(ui);
        });
    }

    fn hotkey_row(&mut self, ui: &mut egui::Ui, label: &str, field: HotkeyField) {
        let err = match field {
            HotkeyField::Capture => parse_hotkey(&self.draft.hotkey).err(),
            HotkeyField::Record => self.record_hotkey_problem(),
            HotkeyField::Tool(i) => self.tool_hotkey_problem(i),
        };
        ui.label(label);
        self.hotkey_editor(ui, field, |_| {});
        ui.end_row();
        error_row(ui, err.as_deref());
    }

    /// A tool hotkey: which tool, its key combination, and a remove button.
    /// Returns whether it was removed.
    fn tool_hotkey_row(&mut self, ui: &mut egui::Ui, i: usize) -> bool {
        let err = self.tool_hotkey_problem(i);
        let action = &mut self.draft.tool_hotkeys[i].action;
        egui::ComboBox::from_id_salt(("tool-hotkey", i))
            .selected_text(action.label())
            .width(150.0)
            .show_ui(ui, |ui| {
                for a in ToolAction::ALL {
                    ui.selectable_value(action, a, a.label());
                }
            });
        let mut remove = false;
        self.hotkey_editor(ui, HotkeyField::Tool(i), |ui| {
            remove = ui.button("Remove").clicked();
        });
        ui.end_row();
        error_row(ui, err.as_deref());
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
                ui.add_space(10.0);

                egui::Grid::new("hotkeys")
                    .num_columns(2)
                    .spacing([14.0, 8.0])
                    .show(ui, |ui| {
                        self.hotkey_row(ui, "Capture region", HotkeyField::Capture);
                        self.hotkey_row(ui, "Record region", HotkeyField::Record);
                    });
                ui.label(
                    RichText::new("Leave Record region empty for no hotkey. Press it again to stop recording.")
                        .weak()
                        .small(),
                );

                ui.add_space(16.0);
                ui.label(RichText::new("Tool hotkeys").strong());
                ui.label(
                    RichText::new("Run a tool straight away from anywhere, e.g. pick a colour without opening snapr.")
                        .weak(),
                );
                ui.add_space(6.0);
                let mut removed = None;
                egui::Grid::new("tool-hotkeys")
                    .num_columns(2)
                    .spacing([14.0, 8.0])
                    .show(ui, |ui| {
                        for i in 0..self.draft.tool_hotkeys.len() {
                            if self.tool_hotkey_row(ui, i) {
                                removed = Some(i);
                            }
                        }
                    });
                if let Some(i) = removed {
                    self.draft.tool_hotkeys.remove(i);
                    self.recording = false;
                }
                if ui.button("+ Add hotkey").clicked() {
                    // The first tool without one, else the first tool.
                    let action = ToolAction::ALL
                        .into_iter()
                        .find(|a| !self.draft.tool_hotkeys.iter().any(|t| t.action == *a))
                        .unwrap_or(ToolAction::ALL[0]);
                    self.add_tool_hotkey(action);
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
            ui.add_space(10.0);

            egui::Grid::new("naming").num_columns(2).spacing([14.0, 8.0]).show(ui, |ui| {
                ui.label("Save folder");
                ui.horizontal(|ui| {
                    ui.add(TextEdit::singleline(&mut self.draft.folder).desired_width(300.0));
                    if ui.button("Browse\u{2026}").clicked()
                        && let Some(dir) = rfd::FileDialog::new().set_directory(&self.draft.folder).pick_folder()
                    {
                        self.draft.folder = dir.display().to_string();
                    }
                    if ui.button("Open").clicked() {
                        actions.push(Action::Reveal(PathBuf::from(&self.draft.folder)));
                    }
                });
                ui.end_row();

                ui.label("Sub-folder");
                ui.horizontal(|ui| {
                    template_field(ui, "subfolder", &mut self.draft.subfolder, "none \u{2014} e.g. %y-%mo or %pn");
                });
                ui.end_row();

                ui.label("File name");
                ui.horizontal(|ui| {
                    template_field(ui, "file_name", &mut self.draft.file_name, "");
                    ui.label(".png");
                });
                ui.end_row();

                ui.label("");
                match &naming {
                    Ok(n) => {
                        let example = n.preview(&Context {
                            process: Some("firefox".into()),
                            title: Some("Mozilla Firefox".into()),
                            width: 1280,
                            height: 720,
                            counter: output::peek_counter() + 1,
                            ..Context::new(chrono::Local::now())
                        });
                        ui.label(RichText::new(format!("e.g. {}", example.display())).monospace().weak());
                    }
                    Err(e) => {
                        ui.colored_label(ERROR, e);
                    }
                }
                ui.end_row();
            });

            ui.add_space(6.0);
            ui.label(
                RichText::new("Uploaded object names are set per destination, under Destinations.")
                    .weak()
                    .small(),
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
        TextEdit::singleline(text)
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

fn error_row(ui: &mut egui::Ui, err: Option<&str>) {
    if let Some(e) = err {
        ui.label("");
        ui.colored_label(Color32::from_rgb(0xe5, 0x48, 0x4d), e);
        ui.end_row();
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

    /// Renders the General, Hotkeys and Paths & naming tabs to
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
        type Tab = fn(&mut Form, &mut egui::Ui);
        let tabs: [(&str, Tab); 3] = [
            ("general", |f, ui| f.ui(ui, &mut Vec::new())),
            ("hotkeys", |f, ui| f.hotkeys_ui(ui, &mut Vec::new())),
            ("naming", |f, ui| f.naming_ui(ui, &mut Vec::new())),
        ];
        for (name, tab) in tabs {
            crate::preview::render(
                &format!("settings-{name}-preview"),
                [920, 420],
                1.0,
                |root| {
                    egui::CentralPanel::default().show(root, |ui| tab(&mut form, ui));
                },
            );
        }
    }
}
