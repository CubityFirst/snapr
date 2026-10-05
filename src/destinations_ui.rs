//! The Destinations tab of the Settings page: where each screenshot goes
//! after it's captured.

use egui::{RichText, TextEdit};

use crate::naming::Context;
use crate::settings::{SpeedUnit, Upload};
use crate::settings_ui::{Action, ERROR, Form, SUCCESS, Test, template_field};
use crate::upload::{DEFAULT_CONCURRENCY, MAX_CONCURRENCY};

impl Form {
    pub fn destinations_ui(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.label(RichText::new("Where each screenshot goes after you capture it.").weak());
                ui.add_space(10.0);

                card(ui, |ui| {
                    ui.checkbox(
                        &mut self.draft.save_to_folder,
                        RichText::new("Save to folder").strong(),
                    );
                    ui.label(
                        RichText::new(format!(
                            "{}  (change the folder and file names under General)",
                            self.draft.folder
                        ))
                        .weak()
                        .small(),
                    );
                });
                card(ui, |ui| {
                    ui.checkbox(
                        &mut self.draft.copy_to_clipboard,
                        RichText::new("Copy image to clipboard").strong(),
                    );
                });

                ui.add_space(8.0);
                ui.label(RichText::new("Uploads").heading().size(16.0));
                ui.add_space(2.0);
                if self.draft.uploads.is_empty() {
                    ui.label(RichText::new("No upload destinations yet.").weak());
                }
                let mut remove = None;
                for i in 0..self.draft.uploads.len() {
                    if self.upload_card(ui, i, actions) {
                        remove = Some(i);
                    }
                }
                if let Some(i) = remove {
                    let u = self.draft.uploads.remove(i);
                    self.secrets.remove(&u.id);
                    self.tests.remove(&u.id);
                }
                if ui.button("+ Add upload").clicked() {
                    let u = Upload::default();
                    self.expanded = Some(u.id.clone());
                    self.draft.uploads.push(u);
                }
                ui.add_space(6.0);
                ui.checkbox(
                    &mut self.draft.copy_link,
                    "Copy the link to the clipboard after uploading (instead of the image)",
                );
                ui.horizontal(|ui| {
                    ui.label("Show upload speed in");
                    ui.selectable_value(&mut self.draft.speed_unit, SpeedUnit::Bytes, "MB/s")
                        .on_hover_text("Megabytes per second, like file sizes");
                    ui.selectable_value(&mut self.draft.speed_unit, SpeedUnit::Bits, "Mbps")
                        .on_hover_text("Megabits per second, like internet speeds");
                });

                self.save_bar(ui, actions);
            });
    }

    /// One upload destination. Returns true if it should be removed.
    fn upload_card(&mut self, ui: &mut egui::Ui, i: usize, actions: &mut Vec<Action>) -> bool {
        let mut remove = false;
        let id = self.draft.uploads[i].id.clone();
        let expanded = self.expanded.as_deref() == Some(id.as_str());
        let stored = self.stored_secrets.contains(&id);
        card(ui, |ui| {
            let u = &mut self.draft.uploads[i];
            ui.horizontal(|ui| {
                ui.checkbox(&mut u.enabled, RichText::new(&u.name).strong());
                let host = u
                    .endpoint
                    .split("://")
                    .nth(1)
                    .unwrap_or(&u.endpoint)
                    .split('/')
                    .next()
                    .unwrap_or("");
                ui.label(
                    RichText::new(format!(
                        "{} \u{00b7} {host}",
                        if u.bucket.is_empty() {
                            "no bucket"
                        } else {
                            &u.bucket
                        }
                    ))
                    .weak()
                    .small(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Remove").clicked() {
                        remove = true;
                    }
                    if ui.button(if expanded { "Done" } else { "Edit" }).clicked() {
                        self.expanded = if expanded { None } else { Some(id.clone()) };
                    }
                    let running = matches!(self.tests.get(&id), Some(Test::Running));
                    if ui
                        .add_enabled(!running, egui::Button::new("Test"))
                        .on_hover_text("Upload a tiny file and delete it again")
                        .clicked()
                    {
                        let secret = self.secrets.get(&id).filter(|s| !s.is_empty()).cloned();
                        actions.push(Action::TestUpload(u.clone(), secret));
                        self.tests.insert(id.clone(), Test::Running);
                    }
                });
            });
            match self.tests.get(&id) {
                Some(Test::Running) => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Testing\u{2026}");
                    });
                }
                Some(Test::Passed(msg)) => {
                    ui.colored_label(SUCCESS, msg);
                }
                Some(Test::Failed(msg)) => {
                    ui.colored_label(ERROR, msg);
                }
                None => {}
            }
            if !expanded {
                return;
            }
            ui.add_space(4.0);
            let u = &mut self.draft.uploads[i];
            egui::Grid::new(("upload", &id))
                .num_columns(2)
                .spacing([14.0, 6.0])
                .show(ui, |ui| {
                    let field = |ui: &mut egui::Ui, label: &str, value: &mut String, hint: &str| {
                        ui.label(label);
                        ui.add(
                            TextEdit::singleline(value)
                                .hint_text(hint)
                                .desired_width(360.0),
                        );
                        ui.end_row();
                    };
                    field(ui, "Name", &mut u.name, "");
                    field(
                        ui,
                        "Endpoint",
                        &mut u.endpoint,
                        "https://<account>.r2.cloudflarestorage.com",
                    );
                    field(ui, "Region", &mut u.region, "auto (R2) or e.g. us-east-1");
                    field(ui, "Bucket", &mut u.bucket, "");
                    field(ui, "Access key ID", &mut u.access_key_id, "");

                    ui.label("Secret access key");
                    let secret = self.secrets.entry(id.clone()).or_default();
                    let hint = if stored {
                        "saved \u{2014} type to replace"
                    } else {
                        ""
                    };
                    ui.add(
                        TextEdit::singleline(secret)
                            .password(true)
                            .hint_text(hint)
                            .desired_width(360.0),
                    );
                    ui.end_row();
                    ui.label("");
                    ui.label(
                        RichText::new(
                            "Kept in your system's credential store, not in the config file.",
                        )
                        .weak()
                        .small(),
                    );
                    ui.end_row();

                    ui.label("Object key");
                    ui.horizontal(|ui| {
                        template_field(
                            ui,
                            &format!("key-{id}"),
                            &mut u.key_template,
                            "%y-%mo/%rna{10}",
                        );
                        ui.label(".png");
                    });
                    ui.end_row();

                    field(
                        ui,
                        "Public URL",
                        &mut u.public_url,
                        "https://pub-\u{2026}.r2.dev or your own domain",
                    );
                    ui.label("");
                    ui.checkbox(
                        &mut u.path_style,
                        "Path-style requests (endpoint/bucket/key)",
                    );
                    ui.end_row();

                    ui.label("");
                    ui.checkbox(
                        &mut u.signed_payload,
                        "Signed payload (hash the file into the signature)",
                    );
                    ui.end_row();

                    ui.label("Parallel parts");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::DragValue::new(&mut u.parallel_parts)
                                .range(1..=MAX_CONCURRENCY)
                                .speed(0.1),
                        )
                        .on_hover_text(
                            "How many pieces of a large file (over 16 MB) are sent at once. \
                             More can be faster on a fast connection; each one holds \
                             8 MB or more in memory while it's sent.",
                        );
                        ui.label(
                            RichText::new(format!("for files over 16 MB (default {DEFAULT_CONCURRENCY})"))
                                .weak()
                                .small(),
                        );
                    });
                    ui.end_row();

                    ui.label("");
                    match u.validate() {
                        Ok(key) => {
                            let ctx = Context {
                                process: Some("firefox".into()),
                                ..Context::new(chrono::Local::now())
                            };
                            fastrand::seed(0x5eed);
                            let link = u.link(&key.render_key(&ctx, "png")).unwrap_or_default();
                            ui.label(
                                RichText::new(format!("Link: {link}"))
                                    .monospace()
                                    .weak()
                                    .small(),
                            );
                        }
                        Err(e) => {
                            ui.colored_label(ERROR, e);
                        }
                    }
                    ui.end_row();
                });
            // Don't keep empty drafts around (they'd mark the form as changed).
            if self.secrets.get(&id).is_some_and(|s| s.is_empty()) {
                self.secrets.remove(&id);
            }
        });
        remove
    }
}

fn card(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::group(ui.style())
        .corner_radius(8)
        .inner_margin(10)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            contents(ui);
        });
    ui.add_space(6.0);
}

#[cfg(test)]
mod tests {
    use crate::settings::{Settings, Upload};
    use crate::settings_ui::{Form, Test};

    /// Renders the Destinations page with an R2 destination being edited to
    /// `target/destinations-preview.png`: `cargo test destinations_preview -- --ignored`.
    #[test]
    #[ignore]
    fn destinations_preview() {
        let r2 = Upload {
            name: "Cloudflare R2".into(),
            endpoint: "https://0123456789abcdef.r2.cloudflarestorage.com".into(),
            region: "auto".into(),
            path_style: true,
            bucket: "screenshots".into(),
            access_key_id: "AKEXAMPLE".into(),
            public_url: "https://img.example.com".into(),
            ..Upload::default()
        };
        let s3 = Upload {
            name: "Amazon S3".into(),
            endpoint: "https://s3.us-east-1.amazonaws.com".into(),
            bucket: "team-shots".into(),
            enabled: false,
            ..Upload::default()
        };
        let settings = Settings {
            uploads: vec![r2.clone(), s3],
            ..Settings::default()
        };
        let mut form = Form::new(&settings);
        form.expanded = Some(r2.id.clone());
        form.stored_secrets.insert(r2.id.clone());
        form.tests.insert(
            r2.id.clone(),
            Test::Passed(
                "Upload works. Links will look like https://img.example.com/snapr-test.txt".into(),
            ),
        );
        crate::preview::render("destinations-preview", [920, 900], 1.0, |root| {
            egui::CentralPanel::default()
                .show(root, |ui| form.destinations_ui(ui, &mut Vec::new()));
        });
    }
}
