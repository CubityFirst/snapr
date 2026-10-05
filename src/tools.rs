//! The Tools page: small capture-related utilities, one tab each: the QR
//! code tool (read codes from a region of the screen, or make one) and the
//! colour picker.

use egui::{Color32, RichText, Sense, Stroke, TextEdit, TextureHandle, TextureOptions, vec2};
use image::RgbaImage;

use crate::qr;
use crate::settings_ui::ERROR;

const ACCENT: Color32 = Color32::from_rgb(0x3d, 0x9b, 0xff);
/// Largest side of the scanned region's preview, in points.
const REGION_PREVIEW: f32 = 140.0;
/// Side of a generated code's preview, in points.
const CODE_PREVIEW: f32 = 220.0;
/// How many picked colours are remembered.
const RECENT_COLORS: usize = 24;

pub enum Action {
    /// Pick a region of the screen to read QR codes from.
    ScanQr,
    /// Pick a pixel's colour from the screen.
    PickColor,
    CopyText(String),
    OpenUrl(String),
    CopyImage(RgbaImage),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    Qr,
    Color,
}

/// What reading a region found.
struct Scan {
    texts: Vec<String>,
    /// The region, until it's uploaded as `texture` on the next paint.
    region: Option<egui::ColorImage>,
    texture: Option<TextureHandle>,
}

/// A generated code and the text it was made from.
struct Generated {
    text: String,
    code: Result<(RgbaImage, TextureHandle), String>,
}

pub struct Tools {
    tool: Tool,
    scan: Option<Scan>,
    text: String,
    generated: Option<Generated>,
    /// Result of the last save; `true` = error.
    notice: Option<(String, bool)>,
    /// Picked colours, newest first.
    colors: Vec<[u8; 3]>,
    /// The colour shown in full.
    color: Option<[u8; 3]>,
}

impl Tools {
    pub fn new() -> Self {
        Self {
            tool: Tool::Qr,
            scan: None,
            text: String::new(),
            generated: None,
            notice: None,
            colors: Vec::new(),
            color: None,
        }
    }

    /// A colour was picked from the screen (and its hex code copied).
    pub fn picked(&mut self, rgb: [u8; 3]) {
        self.tool = Tool::Color;
        self.colors.retain(|c| *c != rgb);
        self.colors.insert(0, rgb);
        self.colors.truncate(RECENT_COLORS);
        self.color = Some(rgb);
    }

    /// A region was read: the codes found in it, and a preview of it.
    pub fn scanned(&mut self, texts: Vec<String>, region: egui::ColorImage) {
        self.tool = Tool::Qr;
        self.scan = Some(Scan {
            texts,
            region: Some(region),
            texture: None,
        });
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            ui.heading("Tools");
            ui.add_space(12.0);
            ui.selectable_value(&mut self.tool, Tool::Qr, "QR code");
            ui.selectable_value(&mut self.tool, Tool::Color, "Colour picker");
        });
        ui.add_space(4.0);
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| match self.tool {
                Tool::Qr => self.qr_ui(ui, actions),
                Tool::Color => self.color_ui(ui, actions),
            });
    }

    fn color_ui(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.label(RichText::new("Pick a colour from the screen").strong());
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let pick = egui::Button::new(
                RichText::new("Pick colour\u{2026}")
                    .strong()
                    .color(Color32::WHITE),
            )
            .fill(ACCENT);
            if ui.add(pick).clicked() {
                actions.push(Action::PickColor);
            }
            ui.label(
                RichText::new(
                    "Click any pixel to copy its hex code. Arrow keys move one pixel, Esc cancels.",
                )
                .weak(),
            );
        });
        let Some(rgb) = self.color else {
            return;
        };
        ui.add_space(12.0);
        ui.horizontal_top(|ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(96.0, 96.0), Sense::hover());
            let [r, g, b] = rgb;
            ui.painter()
                .rect_filled(rect, 6.0, Color32::from_rgb(r, g, b));
            ui.painter().rect_stroke(
                rect,
                6.0,
                Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
                egui::StrokeKind::Inside,
            );
            ui.add_space(8.0);
            egui::Grid::new("color-formats")
                .num_columns(3)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    for (name, text) in formats(rgb) {
                        ui.label(RichText::new(name).weak());
                        ui.label(RichText::new(&text).monospace());
                        if ui.button("Copy").clicked() {
                            actions.push(Action::CopyText(text));
                        }
                        ui.end_row();
                    }
                });
        });

        ui.add_space(16.0);
        ui.label(RichText::new("Recent").weak());
        ui.add_space(2.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            for c in &self.colors {
                let (rect, resp) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::click());
                let [r, g, b] = *c;
                ui.painter()
                    .rect_filled(rect, 4.0, Color32::from_rgb(r, g, b));
                let (width, color) = if self.color == Some(*c) {
                    (2.0, ui.visuals().strong_text_color())
                } else {
                    (1.0, ui.visuals().widgets.noninteractive.bg_stroke.color)
                };
                ui.painter().rect_stroke(
                    rect,
                    4.0,
                    Stroke::new(width, color),
                    egui::StrokeKind::Outside,
                );
                if resp
                    .on_hover_text(hex(*c))
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    self.color = Some(*c);
                }
            }
        });
    }

    fn qr_ui(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.label(RichText::new("Read a QR code").strong());
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let scan = egui::Button::new(
                RichText::new("Scan region\u{2026}")
                    .strong()
                    .color(Color32::WHITE),
            )
            .fill(ACCENT);
            if ui.add(scan).clicked() {
                actions.push(Action::ScanQr);
            }
            ui.label(
                RichText::new(
                    "Drag a region around a code on screen, or click to search a whole window.",
                )
                .weak(),
            );
        });
        if let Some(scan) = &mut self.scan {
            ui.add_space(10.0);
            scan_ui(ui, scan, actions);
        }

        ui.add_space(16.0);
        ui.separator();
        ui.add_space(8.0);
        ui.label(RichText::new("Make a QR code").strong());
        ui.add_space(4.0);
        ui.add(
            TextEdit::multiline(&mut self.text)
                .hint_text("Text or link to encode")
                .desired_rows(3)
                .desired_width(f32::INFINITY),
        );
        self.regenerate(ui.ctx());
        let Some(generated) = &self.generated else {
            return;
        };
        ui.add_space(8.0);
        match &generated.code {
            Err(e) => {
                ui.colored_label(ERROR, format!("Can't make a QR code: {e}"));
            }
            Ok((image, texture)) => {
                ui.horizontal_top(|ui| {
                    ui.add(
                        egui::Image::new(texture)
                            .fit_to_exact_size(vec2(CODE_PREVIEW, CODE_PREVIEW))
                            .corner_radius(4.0),
                    );
                    ui.vertical(|ui| {
                        if ui.button("Copy image").clicked() {
                            actions.push(Action::CopyImage(image.clone()));
                        }
                        if ui.button("Save as\u{2026}").clicked() {
                            self.notice = save(image);
                        }
                        ui.label(
                            RichText::new(format!(
                                "{} \u{00d7} {} px",
                                image.width(),
                                image.height()
                            ))
                            .weak()
                            .small(),
                        );
                        if let Some((msg, is_error)) = &self.notice {
                            let color = if *is_error {
                                ERROR
                            } else {
                                ui.visuals().weak_text_color()
                            };
                            ui.colored_label(color, msg);
                        }
                    });
                });
            }
        }
    }

    /// Makes the code again after the text changed.
    fn regenerate(&mut self, ctx: &egui::Context) {
        if self.text.is_empty() {
            self.generated = None;
            return;
        }
        if self.generated.as_ref().is_some_and(|g| g.text == self.text) {
            return;
        }
        let code = qr::encode(&self.text).map(|image| {
            let pixels = egui::ColorImage::from_rgba_unmultiplied(
                [image.width() as usize, image.height() as usize],
                image.as_raw(),
            );
            let texture = ctx.load_texture("qr-code", pixels, TextureOptions::NEAREST);
            (image, texture)
        });
        self.generated = Some(Generated {
            text: self.text.clone(),
            code,
        });
        self.notice = None;
    }
}

fn scan_ui(ui: &mut egui::Ui, scan: &mut Scan, actions: &mut Vec<Action>) {
    if let Some(region) = scan.region.take() {
        scan.texture = Some(
            ui.ctx()
                .load_texture("qr-region", region, TextureOptions::LINEAR),
        );
    }
    ui.horizontal_top(|ui| {
        if let Some(texture) = &scan.texture {
            let size = texture.size_vec2();
            let scale = (REGION_PREVIEW / size.x.max(size.y)).min(1.0);
            ui.add(
                egui::Image::new(texture)
                    .fit_to_exact_size(size * scale)
                    .corner_radius(4.0),
            );
        }
        ui.vertical(|ui| {
            match scan.texts.len() {
                0 => {
                    ui.colored_label(
                        ERROR,
                        "No QR code found there. Try selecting just the code, with a little margin.",
                    );
                }
                1 => {
                    ui.label("Found a QR code:");
                }
                n => {
                    ui.label(format!("Found {n} QR codes:"));
                }
            }
            for (i, text) in scan.texts.iter().enumerate() {
                ui.push_id(i, |ui| {
                    ui.add_space(4.0);
                    // Read-only, but selectable.
                    let mut shown = text.as_str();
                    ui.add(
                        TextEdit::multiline(&mut shown)
                            .desired_rows(1)
                            .desired_width(f32::INFINITY),
                    );
                    ui.horizontal(|ui| {
                        if ui.button("Copy").clicked() {
                            actions.push(Action::CopyText(text.clone()));
                        }
                        if qr::is_link(text)
                            && ui.button("Open link").on_hover_text(text.trim()).clicked()
                        {
                            actions.push(Action::OpenUrl(text.trim().to_string()));
                        }
                    });
                });
            }
        });
    });
}

/// A colour as `#RRGGBB`.
fn hex([r, g, b]: [u8; 3]) -> String {
    format!("#{r:02X}{g:02X}{b:02X}")
}

/// A colour written the ways it's usually pasted: hex, CSS rgb() and hsl().
fn formats(rgb: [u8; 3]) -> [(&'static str, String); 3] {
    let [r, g, b] = rgb;
    let (h, s, l) = hsl(rgb);
    [
        ("HEX", hex(rgb)),
        ("RGB", format!("rgb({r}, {g}, {b})")),
        ("HSL", format!("hsl({h}, {s}%, {l}%)")),
    ]
}

/// Hue in degrees, saturation and lightness in percent, rounded.
fn hsl(rgb: [u8; 3]) -> (u32, u32, u32) {
    let [r, g, b] = rgb.map(|c| c as f32 / 255.0);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let d = max - min;
    let l = (max + min) / 2.0;
    if d == 0.0 {
        return (0, 0, (l * 100.0).round() as u32);
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    } * 60.0;
    (
        h.round() as u32 % 360,
        (s * 100.0).round() as u32,
        (l * 100.0).round() as u32,
    )
}

/// Asks where to save a generated code and writes it as a PNG.
fn save(image: &RgbaImage) -> Option<(String, bool)> {
    let mut path = rfd::FileDialog::new()
        .add_filter("PNG image", &["png"])
        .set_file_name("qr-code.png")
        .save_file()?;
    if path.extension().is_none() {
        path.set_extension("png");
    }
    Some(match image.save(&path) {
        Ok(()) => (format!("Saved to {}", path.display()), false),
        Err(e) => (format!("Couldn't save {}: {e}", path.display()), true),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_formats() {
        let f = formats([0x3d, 0x9b, 0xff]);
        assert_eq!(f[0].1, "#3D9BFF");
        assert_eq!(f[1].1, "rgb(61, 155, 255)");
        assert_eq!(f[2].1, "hsl(211, 100%, 62%)");
        assert_eq!(hsl([255, 0, 0]), (0, 100, 50));
        assert_eq!(hsl([255, 0, 128]), (330, 100, 50));
        assert_eq!(hsl([0, 128, 0]), (120, 100, 25));
        assert_eq!(hsl([128, 128, 128]), (0, 0, 50));
        assert_eq!(hsl([0, 0, 0]), (0, 0, 0));
        assert_eq!(hsl([255, 255, 255]), (0, 0, 100));
    }

    /// Renders the colour picker after a few picks to
    /// `target/tools-color-preview.png`: `cargo test tools_preview -- --ignored`.
    #[test]
    #[ignore]
    fn tools_color_preview() {
        let mut tools = Tools::new();
        for c in [
            [230, 72, 77],
            [255, 200, 40],
            [30, 30, 30],
            [0x3d, 0x9b, 0xff],
        ] {
            tools.picked(c);
        }
        crate::preview::render("tools-color-preview", [920, 420], 1.0, |root| {
            egui::CentralPanel::default().show(root, |ui| tools.ui(ui, &mut Vec::new()));
        });
    }

    /// Renders the QR code tool, after a scan and with text to encode, to
    /// `target/tools-preview.png`: `cargo test tools_preview -- --ignored`.
    #[test]
    #[ignore]
    fn tools_preview() {
        let mut tools = Tools::new();
        let region = qr::encode("https://example.com/snapr").unwrap();
        let (preview, _) = crate::thumbnail::to_egui(region.clone().into(), (320, 320));
        tools.scanned(qr::decode(&region), preview);
        tools.text = "Hello from snapr".into();
        crate::preview::render("tools-preview", [920, 760], 1.0, |root| {
            egui::CentralPanel::default().show(root, |ui| tools.ui(ui, &mut Vec::new()));
        });
    }
}
