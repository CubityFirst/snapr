//! The Destinations tab of the Settings page: where each screenshot goes
//! after it's captured.

use egui::{RichText, TextEdit};

use crate::naming::Context;
use crate::settings::{SpeedUnit, Upload, UploadKind};
use crate::settings_search::found;
use crate::settings_ui::{Action, ERROR, Form, SUCCESS, Test, block, card, group, heading, row, template_field};
use crate::theme;
use crate::upload::{DEFAULT_CONCURRENCY, MAX_CONCURRENCY};

/// How wide the text boxes of an upload destination are.
const FIELD_WIDTH: f32 = 320.0;

impl Form {
    pub fn destinations_ui(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.label(RichText::new("Where each screenshot goes after you capture it.").weak());

                group(ui, "On this computer", |ui| {
                    let note = format!("{}  (change it under Paths & naming)", self.draft.folder);
                    row(ui, "save-to-folder", "Save to folder", Some(&note), false, |ui| {
                        theme::switch(ui, &mut self.draft.save_to_folder, "");
                    });
                    row(ui, "copy-to-clipboard", "Copy image to clipboard", None, false, |ui| {
                        theme::switch(ui, &mut self.draft.copy_to_clipboard, "");
                    });
                });

                let r = heading(ui, "Uploads", None);
                found(ui, "uploads", r);
                if crate::policy::uploads_disabled() {
                    ui.label(RichText::new(crate::policy::UPLOADS_OFF).color(ui.visuals().warn_fg_color));
                }
                ui.add_enabled_ui(!crate::policy::uploads_disabled(), |ui| {
                    if self.draft.uploads.is_empty() {
                        ui.label(RichText::new("No upload destinations yet.").weak());
                    }
                    let mut remove = None;
                    for i in 0..self.draft.uploads.len() {
                        if self.upload_card(ui, i, actions) {
                            remove = Some(i);
                        }
                        ui.add_space(8.0);
                    }
                    if let Some(i) = remove {
                        let u = self.draft.uploads.remove(i);
                        self.secrets.remove(&u.id);
                        self.tests.remove(&u.id);
                    }
                    ui.horizontal(|ui| {
                        for (label, hover, new) in [
                            (
                                "+ Add S3 upload",
                                "A bucket on Cloudflare R2, Amazon S3, MinIO, Backblaze B2, ...",
                                Upload::default as fn() -> Upload,
                            ),
                            (
                                "+ Add Pomf upload",
                                "A Pomf-compatible file host, e.g. pomf.lain.la or uguu.se",
                                Upload::pomf,
                            ),
                            (
                                "+ Add img.fish upload",
                                "img.fish, anonymously or into your account with an API key",
                                Upload::img_fish,
                            ),
                        ] {
                            if ui.button(label).on_hover_text(hover).clicked() {
                                let u = new();
                                self.expanded = Some(u.id.clone());
                                self.draft.uploads.push(u);
                            }
                        }
                        let ticked: Vec<Upload> =
                            self.draft.uploads.iter().filter(|u| u.enabled).cloned().collect();
                        let running = ticked
                            .iter()
                            .any(|u| matches!(self.tests.get(&u.id), Some(Test::Running)));
                        if ui
                            .add_enabled(!ticked.is_empty() && !running, egui::Button::new("Test upload"))
                            .on_hover_text(
                                "Upload snapr's icon to each ticked destination, named as a screenshot \
                                 would be, and show its link. The icon stays there.",
                            )
                            .on_disabled_hover_text("Tick a destination to test it")
                            .clicked()
                        {
                            for u in ticked {
                                let secret = self.secrets.get(&u.id).filter(|s| !s.is_empty()).cloned();
                                self.tests.insert(u.id.clone(), Test::Running);
                                actions.push(Action::UploadIcon(u, secret));
                            }
                        }
                    });

                    group(ui, "After uploading", |ui| {
                        row(ui, "copy-link", "Copy the link to the clipboard", Some("Instead of the image"), false, |ui| {
                            theme::switch(ui, &mut self.draft.copy_link, "");
                        });
                        let enabled: Vec<(String, String)> = self
                            .draft
                            .uploads
                            .iter()
                            .filter(|u| u.enabled)
                            .map(|u| (u.id.clone(), u.name.clone()))
                            .collect();
                        row(ui, "link-from", "Link from",
                            Some("Uploading to more than one destination, the link that's copied, shown in the corner preview and kept in Recent"),
                            false, |ui| {
                            ui.add_enabled_ui(enabled.len() > 1, |ui| {
                                const FIRST: &str = "Whichever finishes first";
                                let selected = enabled
                                    .iter()
                                    .find(|(id, _)| *id == self.draft.link_from)
                                    .map_or(FIRST, |(_, name)| name.as_str());
                                egui::ComboBox::from_id_salt("link-from")
                                    .selected_text(selected)
                                    .width(220.0)
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(&mut self.draft.link_from, String::new(), FIRST);
                                        for (id, name) in &enabled {
                                            ui.selectable_value(&mut self.draft.link_from, id.clone(), name)
                                                .on_hover_text("Or, if it fails, the first that works");
                                        }
                                    });
                            })
                            .response
                            .on_disabled_hover_text("Turn on more than one destination to choose");
                        });
                        row(ui, "speed-unit", "Show upload speed in", None, false, |ui| {
                            theme::segmented(ui, |ui| {
                                ui.selectable_value(&mut self.draft.speed_unit, SpeedUnit::Bytes, "MB/s")
                                    .on_hover_text("Megabytes per second, like file sizes");
                                ui.selectable_value(&mut self.draft.speed_unit, SpeedUnit::Bits, "Mbps")
                                    .on_hover_text("Megabits per second, like internet speeds");
                            });
                        });
                        let icon = if cfg!(target_os = "macos") { "menu bar icon" } else { "tray icon" };
                        row(ui, "tray-percentage", &format!("Show the percentage uploaded on the {icon}"),
                            Some("Above the progress bar the icon shows while uploading"), false, |ui| {
                            theme::switch(ui, &mut self.draft.tray_percentage, "");
                        });
                    });
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
        ui.push_id(&id, |ui| card(ui, |ui| {
            block(ui, |ui| {
                let u = &mut self.draft.uploads[i];
                ui.horizontal(|ui| {
                    theme::switch(ui, &mut u.enabled, RichText::new(&u.name).strong());
                    let host = u
                        .endpoint
                        .split("://")
                        .nth(1)
                        .unwrap_or(&u.endpoint)
                        .split('/')
                        .next()
                        .unwrap_or("");
                    let what = match u.kind {
                        UploadKind::Pomf => "Pomf",
                        UploadKind::ImgFish if stored => "API key",
                        UploadKind::ImgFish => "anonymous",
                        UploadKind::S3 if u.bucket.is_empty() => "no bucket",
                        UploadKind::S3 => &u.bucket,
                    };
                    ui.label(RichText::new(format!("{what} \u{00b7} {host}")).weak().size(11.5));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(theme::danger(ui, "Remove")).clicked() {
                            remove = true;
                        }
                        if ui.button(if expanded { "Done" } else { "Edit" }).clicked() {
                            self.expanded = if expanded { None } else { Some(id.clone()) };
                        }
                        let running = matches!(self.tests.get(&id), Some(Test::Running));
                        if ui
                            .add_enabled(!running, egui::Button::new("Test"))
                            .on_hover_text(if u.kind == UploadKind::ImgFish {
                                "Upload snapr's icon and delete it again"
                            } else if u.deletable() {
                                "Upload a tiny file and delete it again"
                            } else {
                                "Upload snapr's icon and show its link (it stays there: Pomf \
                                 uploads can't be deleted)"
                            })
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
            });
            if !expanded {
                return;
            }
            let u = &mut self.draft.uploads[i];
            let field = |ui: &mut egui::Ui, title: &str, note: Option<&str>, value: &mut String, hint: &str| {
                row(ui, title, title, note, false, |ui| {
                    ui.add(
                        TextEdit::singleline(value).margin(theme::FIELD_MARGIN)
                            .hint_text(hint)
                            .desired_width(FIELD_WIDTH),
                    );
                });
            };
            field(ui, "Name", None, &mut u.name, "");
            if u.kind == UploadKind::Pomf {
                field(ui, "Upload URL", None, &mut u.endpoint, "https://pomf.lain.la/upload.php");
                row(ui, "File name", "File name", Some("Most hosts give the file a name of their own"), false, |ui| {
                    template_field(ui, &format!("key-{id}"), &mut u.key_template, "%rna{10}");
                    ui.label(".png");
                });
                field(
                    ui,
                    "Public URL",
                    Some("Optional: put links under your own domain instead"),
                    &mut u.public_url,
                    "https://\u{2026}",
                );
            } else if u.kind == UploadKind::ImgFish {
                field(ui, "Address", None, &mut u.endpoint, "https://img.fish");
                let secret = self.secrets.entry(id.clone()).or_default();
                let hint = if stored { "saved \u{2014} type to replace" } else { "optional" };
                row(ui, "API key", "API key",
                    Some("Optional: keeps uploads in your account. Kept in the credential store"),
                    false, |ui| {
                    ui.add(
                        TextEdit::singleline(secret).margin(theme::FIELD_MARGIN)
                            .password(true)
                            .hint_text(hint)
                            .desired_width(FIELD_WIDTH),
                    );
                });
                row(ui, "File name", "File name", Some("img.fish gives the file a name of its own"), false, |ui| {
                    template_field(ui, &format!("key-{id}"), &mut u.key_template, "%rna{10}");
                    ui.label(".png");
                });
            } else {
                field(ui, "Endpoint", None, &mut u.endpoint, "https://<account>.r2.cloudflarestorage.com");
                field(ui, "Region", None, &mut u.region, "auto (R2) or e.g. us-east-1");
                field(ui, "Bucket", None, &mut u.bucket, "");
                field(ui, "Access key ID", None, &mut u.access_key_id, "");
                let secret = self.secrets.entry(id.clone()).or_default();
                let hint = if stored { "saved \u{2014} type to replace" } else { "" };
                row(ui, "Secret access key", "Secret access key",
                    Some("Kept in your system's credential store, not in the config file"), false, |ui| {
                    ui.add(
                        TextEdit::singleline(secret).margin(theme::FIELD_MARGIN)
                            .password(true)
                            .hint_text(hint)
                            .desired_width(FIELD_WIDTH),
                    );
                });
                row(ui, "Object key", "Object key", Some("Its name in the bucket"), false, |ui| {
                    template_field(ui, &format!("key-{id}"), &mut u.key_template, "%y-%mo/%rna{10}");
                    ui.label(".png");
                });
                field(ui, "Public URL", None, &mut u.public_url, "https://pub-\u{2026}.r2.dev or your own domain");
                row(ui, "Path-style requests", "Path-style requests", Some("endpoint/bucket/key, rather than bucket.endpoint/key"), false, |ui| {
                    theme::switch(ui, &mut u.path_style, "");
                });
                row(ui, "Signed payload", "Signed payload", Some("Hash the file into the request's signature"), false, |ui| {
                    theme::switch(ui, &mut u.signed_payload, "");
                });
                let note = format!(
                    "How many pieces of a file over 16 MB are sent at once (default {DEFAULT_CONCURRENCY}). \
                     Each holds 8 MB or more in memory while it's sent"
                );
                row(ui, "Parallel parts", "Parallel parts", Some(&note), false, |ui| {
                    ui.add(egui::DragValue::new(&mut u.parallel_parts).range(1..=MAX_CONCURRENCY).speed(0.1));
                });
            }

            let link = match u.validate() {
                Ok(_) if u.kind == UploadKind::ImgFish => {
                    RichText::new("Link: the one img.fish gives").monospace().weak()
                }
                Ok(_) if u.kind == UploadKind::Pomf => {
                    let public = u.public_url.trim().trim_end_matches('/');
                    let link = if public.is_empty() {
                        "the one the host gives".to_string()
                    } else {
                        format!("{public}/<name the host gives>")
                    };
                    RichText::new(format!("Link: {link}")).monospace().weak()
                }
                Ok(key) => {
                    let ctx = Context {
                        process: Some("firefox".into()),
                        ..Context::new(chrono::Local::now())
                    };
                    fastrand::seed(0x5eed);
                    let link = u.link(&key.render_key(&ctx, "png")).unwrap_or_default();
                    RichText::new(format!("Link: {link}")).monospace().weak()
                }
                Err(e) => RichText::new(e).color(ERROR),
            };
            row(ui, "link", link, None, false, |_| {});
            // Don't keep empty drafts around (they'd mark the form as changed).
            if self.secrets.get(&id).is_some_and(|s| s.is_empty()) {
                self.secrets.remove(&id);
            }
        }));
        remove
    }
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
            crate::theme::apply(root.ctx());
            egui::CentralPanel::default()
                .show(root, |ui| form.destinations_ui(ui, &mut Vec::new()));
        });
    }
}
