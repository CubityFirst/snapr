//! The About tab of the Settings page: what snapr is, its licence, and
//! credits for what it's made with: the colour picker's names and the
//! open-source crates compiled in (listed by `build.rs`).

use std::sync::LazyLock;

use egui::{RichText, TextEdit};
use serde::Deserialize;

use crate::settings::Settings;
use crate::settings_ui::Action;
use crate::theme;
use crate::update;

const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

/// A crate compiled into snapr.
#[derive(Deserialize)]
struct Crate {
    name: String,
    version: String,
    /// An SPDX expression, e.g. `MIT OR Apache-2.0`.
    license: Option<String>,
    #[serde(default)]
    authors: Vec<String>,
    repository: Option<String>,
    /// The licence files it ships, as indexes into `Credits::texts`.
    texts: Vec<usize>,
}

#[derive(Deserialize, Default)]
struct Credits {
    crates: Vec<Crate>,
    /// Each licence file's text, once however many crates ship it.
    texts: Vec<String>,
}

static CREDITS: LazyLock<Credits> = LazyLock::new(|| {
    let packed = include_bytes!(concat!(env!("OUT_DIR"), "/third-party.json.deflate"));
    miniz_oxide::inflate::decompress_to_vec(packed)
        .ok()
        .and_then(|json| serde_json::from_slice(&json).ok())
        .unwrap_or_default()
});

#[derive(Default)]
pub struct About {
    /// Shows only the crates whose name has this in it.
    filter: String,
}

impl About {
    pub fn ui(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.label(RichText::new(format!("snapr {}", update::VERSION)).strong().size(16.0));
            ui.add_space(8.0);
            egui::Grid::new("about").num_columns(2).spacing([14.0, 8.0]).show(ui, |ui| {
                ui.label("Source code");
                if ui.link(REPOSITORY).clicked() {
                    actions.push(Action::OpenUrl(REPOSITORY.into()));
                }
                ui.end_row();

                ui.label("Licence");
                ui.vertical(|ui| {
                    if ui.link("PolyForm Noncommercial 1.0.0").clicked() {
                        actions.push(Action::OpenUrl(format!("{REPOSITORY}/blob/main/LICENSE")));
                    }
                    ui.label(
                        RichText::new(
                            "Free to use, change and share for anything but commercial purposes. \
                             The snapr name and logo aren't covered by it.",
                        )
                        .weak(),
                    );
                });
                ui.end_row();

                if let Some(config) = Settings::config_dir() {
                    ui.label("Settings folder");
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(config.display().to_string()).monospace());
                        if ui.button("Show").clicked() {
                            actions.push(Action::Reveal(config));
                        }
                    })
                    .response
                    .on_hover_text("Where snapr keeps its settings, capture history and stats");
                    ui.end_row();
                }
            });

            ui.add_space(16.0);
            ui.label(RichText::new("Attributions").strong());
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.label(crate::locale::spelled!("The colour picker's colour names are from "));
                if ui.link("Name that Color").clicked() {
                    actions.push(Action::OpenUrl("https://chir.ag/projects/ntc/".into()));
                }
                ui.label(" by Chirag Mehta, under the ");
                if ui.link("Creative Commons Attribution 2.5").clicked() {
                    actions.push(Action::OpenUrl("https://creativecommons.org/licenses/by/2.5/".into()));
                }
                ui.label(
                    " licence. Many of them are from the Resene RGB Values List, \
                     copyright Resene Paints Ltd 2001.",
                );
            });
            egui::CollapsingHeader::new("Notices")
                .id_salt("color-names-notice")
                .show(ui, |ui| licence_text(ui, &crate::color_names::notice()));

            ui.add_space(16.0);
            ui.label(RichText::new("Open-source libraries").strong());
            ui.add_space(4.0);
            let credits = &*CREDITS;
            if credits.crates.is_empty() {
                ui.label(RichText::new("The list of libraries wasn't included in this build.").weak());
                return;
            }
            ui.label(
                RichText::new(format!(
                    "snapr is built with these {} open-source Rust crates. Click one for its licence.",
                    credits.crates.len()
                ))
                .weak(),
            );
            ui.add_space(4.0);
            ui.add(
                TextEdit::singleline(&mut self.filter)
                    .margin(theme::FIELD_MARGIN)
                    .hint_text("Search")
                    .desired_width(240.0),
            );
            ui.add_space(4.0);
            let filter = self.filter.trim().to_lowercase();
            for (i, c) in credits.crates.iter().enumerate() {
                if !c.name.to_lowercase().contains(&filter) {
                    continue;
                }
                let licence = c.license.as_deref().unwrap_or("see its licence file");
                let title = format!("{} {}  \u{00b7}  {licence}", c.name, c.version);
                egui::CollapsingHeader::new(title).id_salt(("crate", i)).show(ui, |ui| {
                    crate_ui(ui, c, &credits.texts, actions);
                });
            }
        });
    }
}

/// A crate's authors, link and licence files.
fn crate_ui(ui: &mut egui::Ui, c: &Crate, texts: &[String], actions: &mut Vec<Action>) {
    if !c.authors.is_empty() {
        ui.label(RichText::new(c.authors.join(", ")).weak());
    }
    if let Some(repo) = &c.repository
        && ui.link(repo).clicked()
    {
        actions.push(Action::OpenUrl(repo.clone()));
    }
    if c.texts.is_empty() {
        ui.label(
            RichText::new("Its package has no licence file, so its licence's standard text applies.")
                .weak(),
        );
        if let Some(licence) = &c.license {
            // Each licence named in the expression, e.g. MIT and Apache-2.0.
            ui.horizontal_wrapped(|ui| {
                for id in licence
                    .split(|ch: char| ch.is_whitespace() || "()/".contains(ch))
                    .filter(|s| !s.is_empty() && !["OR", "AND", "WITH"].contains(s))
                {
                    if ui.link(id).clicked() {
                        actions.push(Action::OpenUrl(format!("https://spdx.org/licenses/{id}.html")));
                    }
                }
            });
        }
    }
    for &t in &c.texts {
        if let Some(text) = texts.get(t) {
            licence_text(ui, text);
        }
    }
}

/// A licence's text, selectable so it can be copied.
fn licence_text(ui: &mut egui::Ui, text: &str) {
    let mut shown = text;
    ui.add(
        TextEdit::multiline(&mut shown)
            .margin(theme::FIELD_MARGIN)
            .font(egui::TextStyle::Monospace)
            .desired_rows(1)
            .desired_width(f32::INFINITY),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credits_are_built_in() {
        let credits = &*CREDITS;
        assert!(credits.crates.len() > 50, "{} crates", credits.crates.len());
        let egui = credits.crates.iter().find(|c| c.name == "egui").expect("egui is listed");
        assert!(egui.license.is_some());
        // Proc macros run at build time, so they aren't credited.
        assert!(!credits.crates.iter().any(|c| c.name == "serde_derive"));
        assert!(credits.crates.iter().all(|c| c.texts.iter().all(|&t| t < credits.texts.len())));
        assert!(crate::color_names::notice().contains("Copyright Resene Paints Ltd 2001"));
    }

    /// Renders the About tab to `target/settings-about-preview.png`:
    /// `cargo test about_preview -- --ignored`.
    #[test]
    #[ignore]
    fn about_preview() {
        let mut about = About::default();
        crate::preview::render("settings-about-preview", [920, 900], 1.0, |root| {
            crate::theme::apply(root.ctx());
            egui::CentralPanel::default().show(root, |ui| about.ui(ui, &mut Vec::new()));
        });
    }
}
