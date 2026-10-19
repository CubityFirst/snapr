//! First-time setup: a few questions on snapr's first start, in the main
//! window before its usual pages. Where screenshots are saved, whether snapr
//! starts with Windows, and whether it plays sounds. Each answer is a
//! setting, so the form saves it as it's picked; finishing (or skipping)
//! marks setup as done.

use egui::{Label, Margin, RichText, Sense, Stroke, TextEdit, vec2};

use crate::settings::default_folder;
use crate::settings_ui::{ERROR, Form};
use crate::sound::{self, Sound};
use crate::theme::{self, ACCENT};

/// The widest the questions get, however wide the window.
const WIDTH: f32 = 520.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Folder,
    /// Windows only: elsewhere snapr can't start itself yet.
    StartWithOs,
    Sounds,
    Done,
}

impl Step {
    fn all() -> Vec<Step> {
        let mut all = vec![Step::Folder];
        if cfg!(windows) {
            all.push(Step::StartWithOs);
        }
        all.extend([Step::Sounds, Step::Done]);
        all
    }
}

/// How setup ended.
pub enum Finish {
    /// On to Recent.
    Done,
    /// On to Settings, for the rest.
    OpenSettings,
}

#[derive(Default)]
pub struct Setup {
    /// The question showing, as an index into `Step::all()`.
    step: usize,
}

impl Setup {
    /// The question showing and the buttons to move between them. Answers
    /// go into `form.draft`; once it returns, setup is over.
    pub fn ui(&mut self, ui: &mut egui::Ui, form: &mut Form) -> Option<Finish> {
        let steps = Step::all();
        let step = steps[self.step.min(steps.len() - 1)];
        let mut finish = None;
        let width = ui.available_width().min(WIDTH);
        let side = ((ui.available_width() - width) / 2.0).max(0.0);
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.add_space((ui.available_height() * 0.1).clamp(8.0, 64.0));
            ui.horizontal(|ui| {
                ui.add_space(side);
                ui.vertical(|ui| {
                    ui.set_width(width);
                    // The questions are counted; the last page isn't one.
                    let questions = steps.len() - 1;
                    ui.horizontal(|ui| {
                        if step == Step::Done {
                            progress(ui, questions, questions);
                        } else {
                            progress(ui, self.step, questions);
                            ui.label(
                                RichText::new(format!("{} of {questions}", self.step + 1)).weak(),
                            );
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.link("Skip setup").clicked() {
                                    finish = Some(Finish::Done);
                                }
                            });
                        }
                    });
                    ui.add_space(20.0);
                    let ready = match step {
                        Step::Folder => folder(ui, form),
                        Step::StartWithOs => start_with_os(ui, form),
                        Step::Sounds => sounds(ui, form),
                        Step::Done => done(ui, form),
                    };
                    ui.add_space(28.0);
                    ui.horizontal(|ui| {
                        if self.step > 0 && ui.button("Back").clicked() {
                            self.step -= 1;
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if step == Step::Done {
                                if ui.add(theme::primary("Start using snapr")).clicked() {
                                    finish = Some(Finish::Done);
                                }
                                if ui.button("Open settings").clicked() {
                                    finish = Some(Finish::OpenSettings);
                                }
                            } else if ui.add_enabled(ready, theme::primary("Next")).clicked() {
                                self.step += 1;
                            }
                        });
                    });
                });
            });
        });
        if finish.is_some() {
            form.draft.setup_done = true;
        }
        finish
    }
}

/// Where screenshots are saved. Returns whether a folder's been given.
fn folder(ui: &mut egui::Ui, form: &mut Form) -> bool {
    heading(ui, "Where should screenshots go?");
    ui.add(
        Label::new(
            RichText::new(
                "Each one is saved as a file here, in a folder for the month. snapr makes the folder if it isn't there yet.",
            )
            .weak(),
        )
        .wrap(),
    );
    ui.add_space(14.0);
    ui.horizontal(|ui| {
        let browse = ui.spacing().interact_size.x + 40.0;
        ui.add(
            TextEdit::singleline(&mut form.draft.folder)
                .margin(theme::FIELD_MARGIN)
                .desired_width(ui.available_width() - browse),
        );
        if ui.button("Browse\u{2026}").clicked() {
            let mut dialog = rfd::FileDialog::new();
            if std::path::Path::new(&form.draft.folder).is_dir() {
                dialog = dialog.set_directory(&form.draft.folder);
            }
            if let Some(dir) = dialog.pick_folder() {
                form.draft.folder = dir.display().to_string();
            }
        }
    });
    let default = default_folder().display().to_string();
    if form.draft.folder.trim() != default {
        ui.add_space(4.0);
        if ui.link(format!("Use {default}")).clicked() {
            form.draft.folder = default;
        }
    }
    ui.add_space(6.0);
    ui.label(RichText::new("Sub-folders and file names are under Settings \u{203a} Paths & naming.").weak());
    !form.draft.folder.trim().is_empty()
}

fn start_with_os(ui: &mut egui::Ui, form: &mut Form) -> bool {
    heading(ui, "Start snapr with Windows?");
    ui.add(
        Label::new(
            RichText::new("snapr waits in the tray for its hotkeys, so they only work while it's running.")
                .weak(),
        )
        .wrap(),
    );
    ui.add_space(14.0);
    let on = &mut form.draft.start_with_windows;
    yes_no(
        ui,
        on,
        ("Yes, start with Windows", "It's ready whenever you sign in."),
        ("No, I'll start it myself", "You open it from the Start menu."),
    );
    true
}

fn sounds(ui: &mut egui::Ui, form: &mut Form) -> bool {
    heading(ui, "Play sounds?");
    ui.add(
        Label::new(
            RichText::new(
                "A shutter when you take a screenshot, a chime when a link's copied, and a buzz when something goes wrong.",
            )
            .weak(),
        )
        .wrap(),
    );
    ui.add_space(14.0);
    let draft = &mut form.draft;
    let was = draft.play_sounds;
    yes_no(
        ui,
        &mut draft.play_sounds,
        ("Yes, play sounds", "So you hear when it's done."),
        ("No, keep quiet", "Screenshots are taken silently."),
    );
    if draft.play_sounds && !was {
        sound::play(Sound::Capture, draft.sound_volume);
    }
    if draft.play_sounds {
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.label("Volume");
            let slider = theme::slider(
                ui,
                egui::Slider::new(&mut draft.sound_volume, 0..=100).suffix("%"),
            );
            // Let it be heard once it's set, not all the way along a drag.
            if slider.drag_stopped() || (slider.changed() && !slider.dragged()) {
                sound::play(Sound::Capture, draft.sound_volume);
            }
        });
    }
    true
}

/// The last page: how to take a screenshot, and anything that went wrong.
fn done(ui: &mut egui::Ui, form: &mut Form) -> bool {
    heading(ui, "You're all set");
    let draft = &form.draft;
    let mut how = format!("Press {} to capture a region", draft.hotkey);
    if !draft.record_hotkey.is_empty() {
        how += &format!(", or {} to record one", draft.record_hotkey);
    }
    ui.add(Label::new(RichText::new(how + ".").size(15.0)).wrap());
    ui.add_space(8.0);
    let home = if cfg!(target_os = "macos") { "menu bar" } else { "tray" };
    ui.add(
        Label::new(
            RichText::new(format!(
                "snapr keeps running in the {home}; click its icon for everything else. \
                 Anything here can be changed later in Settings."
            ))
            .weak(),
        )
        .wrap(),
    );
    // E.g. a hotkey another program has, found on starting.
    if let Some((msg, true)) = &form.status {
        ui.add_space(12.0);
        ui.add(Label::new(RichText::new(msg).color(ERROR)).wrap());
    }
    true
}

fn heading(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(22.0).strong());
    ui.add_space(6.0);
}

/// Dots for the questions, filled up to and including `at`.
fn progress(ui: &mut egui::Ui, at: usize, of: usize) {
    let (gap, radius) = (14.0, 4.0);
    let (rect, _) = ui.allocate_exact_size(vec2(gap * of as f32, 12.0), Sense::hover());
    let p = theme::palette(ui);
    for i in 0..of {
        let center = egui::pos2(rect.left() + radius + gap * i as f32, rect.center().y);
        let fill = if i <= at { ACCENT } else { p.stroke };
        ui.painter().circle_filled(center, radius, fill);
    }
}

/// Two cards side by side, yes and no, each a title and a line below it.
fn yes_no(ui: &mut egui::Ui, value: &mut bool, yes: (&str, &str), no: (&str, &str)) {
    let gap = 12.0;
    let width = (ui.available_width() - gap) / 2.0;
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for (answer, (title, detail)) in [(true, yes), (false, no)] {
            if card(ui, *value == answer, title, detail, width).clicked() {
                *value = answer;
            }
        }
    });
}

/// A choice as a card: outlined in the accent once it's picked.
fn card(ui: &mut egui::Ui, picked: bool, title: &str, detail: &str, width: f32) -> egui::Response {
    let id = ui.id().with(("card", title));
    let hovered = ui.ctx().read_response(id).is_some_and(|r| r.hovered());
    let p = theme::palette(ui);
    let (fill, stroke) = if picked {
        (p.selected, ACCENT)
    } else if hovered {
        (p.control_hover, p.stroke_hover)
    } else {
        (p.surface, p.stroke)
    };
    let margin = 14;
    let frame = egui::Frame::new()
        .fill(fill)
        .stroke(Stroke::new(1.0, stroke))
        .corner_radius(10)
        .inner_margin(Margin::same(margin));
    let shown = frame.show(ui, |ui| {
        ui.set_width(width - 2.0 * margin as f32);
        ui.vertical(|ui| {
            let color = if picked { p.selected_text } else { p.text_strong };
            ui.label(RichText::new(title).strong().color(color));
            ui.add_space(2.0);
            ui.add(Label::new(RichText::new(detail).weak()).wrap());
        });
    });
    ui.interact(shown.response.rect, id, Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;

    /// Renders each page of setup to `target/setup-preview-<n>.png`:
    /// `cargo test setup_preview -- --ignored`.
    #[test]
    #[ignore]
    fn setup_preview() {
        for n in 0..Step::all().len() {
            let mut form = Form::new(&Settings::default());
            form.draft.play_sounds = n % 2 == 0;
            let mut setup = Setup { step: n };
            let name = format!("setup-preview-{n}");
            crate::preview::render(&name, [980, 660], 1.0, |root| {
                crate::theme::apply(root.ctx());
                egui::CentralPanel::default().show(root, |ui| {
                    setup.ui(ui, &mut form);
                });
            });
        }
    }
}
