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
                    let mut cards = Vec::new();
                    let movable = self.draft.uploads.len() > 1;
                    for i in 0..self.draft.uploads.len() {
                        let (gone, rect, handle) = self.upload_card(ui, i, movable, actions);
                        if gone {
                            remove = Some(i);
                        }
                        cards.push((rect, handle));
                        ui.add_space(8.0);
                    }
                    if let Some(i) = remove {
                        let u = self.draft.uploads.remove(i);
                        self.secrets.remove(&u.id);
                        self.tests.remove(&u.id);
                    } else {
                        self.drag_to_reorder(ui, &cards);
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

    /// While a destination's handle is dragged, a line where it would go;
    /// let go, it moves there. `cards` are where each one is, and its handle.
    fn drag_to_reorder(&mut self, ui: &egui::Ui, cards: &[(egui::Rect, Option<egui::Response>)]) {
        let Some((from, handle)) = cards
            .iter()
            .enumerate()
            .find_map(|(i, (_, h))| h.as_ref().filter(|h| h.dragged() || h.drag_stopped()).map(|h| (i, h)))
        else {
            return;
        };
        let Some(y) = ui.input(|i| i.pointer.latest_pos()).map(|p| p.y) else {
            return;
        };
        // The gap it goes into: after every card whose middle is above the pointer.
        let to = cards.iter().filter(|(r, _)| r.center().y < y).count();
        let moves = to != from && to != from + 1;
        if handle.drag_stopped() {
            if moves {
                let u = self.draft.uploads.remove(from);
                self.draft.uploads.insert(if to > from { to - 1 } else { to }, u);
            }
            return;
        }
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        let dragged = cards[from].0;
        ui.painter().rect_stroke(dragged, 8, egui::Stroke::new(1.5, theme::ACCENT), egui::StrokeKind::Inside);
        if moves {
            // Halfway across the 8 points between cards.
            let y = if to == 0 { cards[0].0.top() - 4.0 } else { cards[to - 1].0.bottom() + 4.0 };
            ui.painter().hline(dragged.x_range(), y, egui::Stroke::new(2.0, theme::ACCENT));
        }
    }

    /// One upload destination, with a handle to drag it by if it's
    /// `movable`. Returns whether it should be removed, where it is, and
    /// its handle.
    fn upload_card(
        &mut self,
        ui: &mut egui::Ui,
        i: usize,
        movable: bool,
        actions: &mut Vec<Action>,
    ) -> (bool, egui::Rect, Option<egui::Response>) {
        let mut remove = false;
        let mut handle = None;
        let id = self.draft.uploads[i].id.clone();
        let expanded = self.expanded.as_deref() == Some(id.as_str());
        let stored = self.stored_secrets.contains(&id);
        let rect = ui.push_id(&id, |ui| card(ui, |ui| {
            block(ui, |ui| {
                let u = &mut self.draft.uploads[i];
                ui.horizontal(|ui| {
                    if movable {
                        handle = Some(grab_handle(ui, &id));
                    }
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
                            .on_hover_text(if u.deletable() {
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
                    // The same random part every frame, so it doesn't flicker;
                    // the shared generator goes back to how it was after.
                    let saved = fastrand::get_seed();
                    fastrand::seed(0x5eed);
                    let link = u.link(&key.render_key(&ctx, "png")).unwrap_or_default();
                    fastrand::seed(saved);
                    RichText::new(format!("Link: {link}")).monospace().weak()
                }
                Err(e) => RichText::new(e).color(ERROR),
            };
            row(ui, "link", link, None, false, |_| {});
            // Don't keep empty drafts around (they'd mark the form as changed).
            if self.secrets.get(&id).is_some_and(|s| s.is_empty()) {
                self.secrets.remove(&id);
            }
        }))
        .inner;
        (remove, rect, handle)
    }
}

/// The dots at the left of the destination with id `id` that drag it up or
/// down the list.
fn grab_handle(ui: &mut egui::Ui, id: &str) -> egui::Response {
    let rect = ui.allocate_space(egui::vec2(12.0, 20.0)).1;
    let r = ui.interact(rect, grab_id(id), egui::Sense::drag());
    let p = theme::palette(ui);
    let color = if r.hovered() || r.dragged() { p.text } else { p.text_weak };
    for row in [-1.0, 0.0, 1.0] {
        for col in [-0.5, 0.5] {
            ui.painter().circle_filled(rect.center() + egui::vec2(col, row) * 5.0, 1.4, color);
        }
    }
    r.on_hover_cursor(egui::CursorIcon::Grab).on_hover_text("Drag to reorder")
}

fn grab_id(id: &str) -> egui::Id {
    egui::Id::new(("upload-grab", id))
}

#[cfg(test)]
mod tests {
    use crate::settings::{Settings, Upload};
    use crate::settings_ui::{Form, Test};

    /// The link preview seeds the shared generator to draw the same name
    /// each frame. It used to leave it seeded, so every destination added
    /// afterwards got the same id, and with it another's stored key.
    #[test]
    fn link_preview_leaves_the_shared_rng_alone() {
        let r2 = Upload {
            endpoint: "https://acct.r2.cloudflarestorage.com".into(),
            bucket: "shots".into(),
            access_key_id: "AK".into(),
            ..Upload::default()
        };
        let mut form = Form::new(&Settings {
            uploads: vec![r2.clone()],
            ..Settings::default()
        });
        form.expanded = Some(r2.id.clone());
        fastrand::seed(42);
        let before = fastrand::get_seed();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 900.0))),
            ..Default::default()
        };
        egui::Context::default()
            .run_ui(input, |ui| form.destinations_ui(ui, &mut Vec::new()))
            .textures_delta
            .clear();
        assert_eq!(fastrand::get_seed(), before);
    }

    /// Dragging a destination's handle moves it to the gap it's let go
    /// over: the last one to the top, then the (new) first to the bottom.
    #[test]
    fn drag_handle_reorders_uploads() {
        use egui::{Event, PointerButton, Pos2, vec2};
        let uploads: Vec<Upload> = ["a", "b", "c"]
            .into_iter()
            .map(|name| Upload { name: name.into(), ..Upload::default() })
            .collect();
        let mut form = Form::new(&Settings { uploads, ..Settings::default() });
        let ctx = egui::Context::default();
        let mut frame = |form: &mut Form, events: Vec<Event>| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, vec2(900.0, 900.0))),
                events,
                ..Default::default()
            };
            ctx.run_ui(input, |ui| form.destinations_ui(ui, &mut Vec::new())).textures_delta.clear();
        };
        let names = |form: &Form| form.draft.uploads.iter().map(|u| u.name.clone()).collect::<Vec<_>>();
        let drag = |form: &mut Form, frame: &mut dyn FnMut(&mut Form, Vec<Event>), from: usize, to: usize, dy: f32| {
            frame(form, vec![]);
            let handle = |i: usize| {
                ctx.read_response(super::grab_id(&form.draft.uploads[i].id)).unwrap().rect.center()
            };
            let (start, end) = (handle(from), handle(to) + vec2(0.0, dy));
            let button = |pos, pressed| Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            };
            frame(form, vec![Event::PointerMoved(start), button(start, true)]);
            frame(form, vec![Event::PointerMoved(start.lerp(end, 0.5))]);
            frame(form, vec![Event::PointerMoved(end)]);
            frame(form, vec![button(end, false)]);
            frame(form, vec![]);
        };
        drag(&mut form, &mut frame, 2, 0, -10.0);
        assert_eq!(names(&form), ["c", "a", "b"]);
        drag(&mut form, &mut frame, 0, 2, 10.0);
        assert_eq!(names(&form), ["a", "b", "c"]);
    }

    /// Renders the Destinations page with the last of three destinations
    /// being dragged to the top to `target/destinations-reorder-preview.png`:
    /// `cargo test destinations_reorder_preview -- --ignored`.
    #[test]
    #[ignore]
    fn destinations_reorder_preview() {
        use egui::{Event, PointerButton, vec2};
        let uploads = vec![
            Upload {
                name: "Cloudflare R2".into(),
                endpoint: "https://0123456789abcdef.r2.cloudflarestorage.com".into(),
                bucket: "screenshots".into(),
                ..Upload::default()
            },
            Upload {
                name: "Amazon S3".into(),
                endpoint: "https://s3.us-east-1.amazonaws.com".into(),
                bucket: "team-shots".into(),
                enabled: false,
                ..Upload::default()
            },
            Upload {
                name: "pomf.lain.la".into(),
                endpoint: "https://pomf.lain.la/upload.php".into(),
                ..Upload::pomf()
            },
        ];
        let ids: Vec<String> = uploads.iter().map(|u| u.id.clone()).collect();
        let mut form = Form::new(&Settings { uploads, ..Settings::default() });
        let handles = std::cell::RefCell::new(Vec::new());
        let mut ui = |root: &mut egui::Ui| {
            crate::theme::apply(root.ctx());
            egui::CentralPanel::default().show(root, |ui| form.destinations_ui(ui, &mut Vec::new()));
            *handles.borrow_mut() = ids
                .iter()
                .filter_map(|id| root.ctx().read_response(super::grab_id(id)))
                .map(|r| r.rect.center())
                .collect();
        };
        let mut screen = crate::preview::Offscreen::new([1380, 840], 1.5);
        for _ in 0..3 {
            screen.run(vec![], &mut ui);
        }
        let (start, end) = {
            let h = handles.borrow();
            (h[2], h[0] - vec2(0.0, 10.0))
        };
        let press = Event::PointerButton {
            pos: start,
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Default::default(),
        };
        screen.run(vec![Event::PointerMoved(start), press], &mut ui);
        screen.run(vec![Event::PointerMoved(start.lerp(end, 0.5))], &mut ui);
        screen.run(vec![Event::PointerMoved(end)], &mut ui);
        screen.run(vec![], &mut ui);
        screen.save("destinations-reorder-preview");
    }

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
