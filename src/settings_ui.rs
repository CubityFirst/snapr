//! The General, Hotkeys and Paths & naming tabs of the Settings page, and
//! the form state they share with the Destinations tab.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use egui::{Color32, RichText, TextEdit};
use winit::event::ElementState;
use winit::keyboard::{KeyCode, ModifiersState};

use crate::naming::{CATEGORIES, Context};
use crate::output;
use crate::secrets;
use crate::settings::{Settings, Upload, parse_hotkey};

pub enum Action {
    /// Apply these settings and store these new secret keys (upload id → key).
    Save(Settings, HashMap<String, String>),
    Reveal(PathBuf),
    /// Try an upload destination, with a secret key typed but not yet saved.
    TestUpload(Upload, Option<String>),
}

/// The hotkeys that can be set on the Hotkeys tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HotkeyField {
    Capture,
    Record,
}

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
    /// Secret keys typed but not saved yet, by upload id.
    pub(crate) secrets: HashMap<String, String>,
    /// Uploads whose secret key is in the credential store.
    pub(crate) stored_secrets: HashSet<String>,
    pub tests: HashMap<String, Test>,
    /// The upload destination being edited.
    pub(crate) expanded: Option<String>,
    /// Connected microphones, listed when the General tab first shows.
    microphones: Option<Vec<String>>,
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
        if let Err(e) = self.draft.naming() {
            return Some(e);
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

    /// Save / Revert, and the latest status message.
    pub(crate) fn save_bar(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.add_space(10.0);
        ui.separator();
        ui.horizontal(|ui| {
            let dirty = self.draft != self.saved || !self.secrets.is_empty();
            let problem = self.problem();
            let save = ui.add_enabled(dirty && problem.is_none(), egui::Button::new("Save"));
            let save = match &problem {
                Some(p) if dirty => save.on_disabled_hover_text(p),
                _ => save,
            };
            if save.clicked() {
                let secrets = self
                    .secrets
                    .iter()
                    .filter(|(_, s)| !s.is_empty())
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                actions.push(Action::Save(self.draft.clone(), secrets));
            }
            if ui.add_enabled(dirty, egui::Button::new("Revert")).clicked() {
                self.draft = self.saved.clone();
                self.secrets.clear();
                self.recording = false;
            }
            if let (Some(p), true) = (&problem, dirty) {
                ui.colored_label(ERROR, p);
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

                ui.label(RichText::new("Recording").strong());
                ui.end_row();

                ui.label("Frame rate");
                ui.add(egui::DragValue::new(&mut self.draft.record_fps).range(1..=120).suffix(" fps"));
                ui.end_row();

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
                ui.label("");
                ui.checkbox(&mut self.draft.record_cursor, "Show the mouse pointer");
                ui.end_row();

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
                    RichText::new("Recordings are encoded with FFmpeg and saved as MP4 next to your screenshots.")
                        .weak()
                        .small(),
                );
                ui.end_row();
            });

            self.save_bar(ui, actions);
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

    /// A hotkey text box with a Record button that captures the next key combination.
    fn hotkey_row(&mut self, ui: &mut egui::Ui, label: &str, field: HotkeyField) {
        let err = match field {
            HotkeyField::Capture => parse_hotkey(&self.draft.hotkey).err(),
            HotkeyField::Record => self.record_hotkey_problem(),
        };
        ui.label(label);
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
                };
                ui.add(TextEdit::singleline(value).desired_width(220.0));
                if ui.button("Record").clicked() {
                    self.recording = true;
                    self.recording_field = field;
                }
            }
        });
        ui.end_row();
        error_row(ui, err.as_deref());
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
    use crate::settings::Settings;

    /// Renders the General, Hotkeys and Paths & naming tabs to
    /// `target/settings-*-preview.png`: `cargo test settings_preview -- --ignored`.
    #[test]
    #[ignore]
    fn settings_preview() {
        let mut form = Form::new(&Settings::default());
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
