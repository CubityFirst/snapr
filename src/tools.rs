//! The Tools page: small capture-related utilities, one tab each: the QR
//! code tool (read codes from a region of the screen, or make one), the
//! colour picker, and pinning images to the screen.

use std::path::PathBuf;

use egui::{
    Align, Color32, Layout, RichText, Sense, Stroke, TextEdit, TextureHandle, TextureOptions, vec2,
};
use image::RgbaImage;

use crate::qr;
use crate::settings::{ColorFormat, ColorPicker, ToolAction, ToolHotkey};
use crate::settings_ui::{ERROR, block, card, heading, row};
use crate::theme;

/// Side of the square the scanned region is shown in, in points.
const REGION_PREVIEW: f32 = 112.0;
/// Side of the square a generated code is shown in, in points.
const CODE_PREVIEW: f32 = 196.0;
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
    /// Give a tool a global hotkey (on the Hotkeys settings tab).
    AddHotkey(ToolAction),
    /// Pick a region of the screen to pin on top of everything.
    PinRegion,
    /// Pin this image (from the clipboard) to the screen.
    PinImage(RgbaImage),
    PinFile(PathBuf),
    /// Close every pinned image.
    ClosePins,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    Qr,
    Color,
    Pin,
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
    /// How many images are pinned to the screen.
    pub pinned: usize,
    /// Why the last pin didn't work, e.g. no image on the clipboard.
    pin_error: Option<String>,
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
            pinned: 0,
            pin_error: None,
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

    /// `hotkeys` are the saved tool hotkeys, shown on their tools' tabs;
    /// `picker` the colour picker's settings, changed on its tab.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        hotkeys: &[ToolHotkey],
        picker: &mut ColorPicker,
        actions: &mut Vec<Action>,
    ) {
        ui.horizontal(|ui| {
            ui.heading("Tools");
            ui.add_space(12.0);
            theme::segmented(ui, |ui| {
                ui.selectable_value(&mut self.tool, Tool::Qr, "QR code");
                ui.selectable_value(&mut self.tool, Tool::Color, crate::locale::spelled!("Colour picker"));
                ui.selectable_value(&mut self.tool, Tool::Pin, "Pin to screen");
            });
        });
        ui.add_space(4.0);
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| match self.tool {
                Tool::Qr => self.qr_ui(ui, hotkeys, actions),
                Tool::Color => self.color_ui(ui, hotkeys, picker, actions),
                Tool::Pin => self.pin_ui(ui, hotkeys, actions),
            });
    }

    fn pin_ui(&mut self, ui: &mut egui::Ui, hotkeys: &[ToolHotkey], actions: &mut Vec<Action>) {
        heading(ui, "Pin an image to the screen", None);
        card(ui, |ui| {
            block(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui.add(theme::primary("Pin region\u{2026}")).clicked() {
                        self.pin_error = None;
                        actions.push(Action::PinRegion);
                    }
                    ui.add(
                        egui::Label::new(
                            RichText::new(
                                "Drag a region (or click a window); it stays on top of everything, right where it was.",
                            )
                            .weak(),
                        )
                        .wrap(),
                    );
                });
                hotkey_line(ui, hotkeys, ToolAction::PinRegion, actions);
            });
            block(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .button("Pin from clipboard")
                        .on_hover_text("Pin the image on the clipboard")
                        .clicked()
                    {
                        match clipboard_image() {
                            Ok(image) => {
                                self.pin_error = None;
                                actions.push(Action::PinImage(image));
                            }
                            Err(e) => self.pin_error = Some(e),
                        }
                    }
                    if ui.button("Pin image file\u{2026}").clicked()
                        && let Some(path) = rfd::FileDialog::new()
                            .add_filter("Images", &["png", "jpg", "jpeg", "gif", "webp", "bmp"])
                            .pick_file()
                    {
                        self.pin_error = None;
                        actions.push(Action::PinFile(path));
                    }
                });
                if let Some(e) = &self.pin_error {
                    ui.colored_label(ERROR, e);
                }
                ui.add(
                    egui::Label::new(
                        RichText::new("Screenshots on the Recent page can be pinned too: right-click one.")
                            .weak(),
                    )
                    .wrap(),
                );
            });
        });

        heading(ui, "Pinned images", None);
        card(ui, |ui| {
            if self.pinned > 0 {
                let n = self.pinned;
                let pinned = if n == 1 {
                    "1 image is pinned".to_string()
                } else {
                    format!("{n} images are pinned")
                };
                row(ui, "pinned", pinned, None, false, |ui| {
                    if ui.button("Close all").clicked() {
                        actions.push(Action::ClosePins);
                    }
                });
            }
            for (what, how) in [
                ("Move it", "Drag"),
                ("Make it bigger or smaller", "Scroll, or drag a corner"),
                ("Back to its real size", "Double-click, or 0"),
                ("Keep it on top of other windows, or not", "T, or the pin button"),
                ("Copy it", "Ctrl+C"),
                ("Save it as a PNG", "Ctrl+S"),
                ("Close it", "Right-click, or Esc"),
            ] {
                row(ui, what, what, None, false, |ui| {
                    ui.label(RichText::new(how).weak());
                });
            }
        });
    }

    fn color_ui(
        &mut self,
        ui: &mut egui::Ui,
        hotkeys: &[ToolHotkey],
        picker: &mut ColorPicker,
        actions: &mut Vec<Action>,
    ) {
        heading(ui, crate::locale::spelled!("Pick a colour from the screen"), None);
        card(ui, |ui| {
            block(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui.add(theme::primary(crate::locale::spelled!("Pick colour\u{2026}"))).clicked() {
                        actions.push(Action::PickColor);
                    }
                    ui.add(
                        egui::Label::new(
                            RichText::new(crate::locale::spelled!(
                                "Click any pixel to copy its colour. Arrow keys move one pixel, Esc cancels."
                            ))
                            .weak(),
                        )
                        .wrap(),
                    );
                });
                hotkey_line(ui, hotkeys, ToolAction::PickColor, actions);
            });
            if let Some(rgb) = self.color {
                block(ui, |ui| color_formats_ui(ui, rgb, actions));
                block(ui, |ui| self.recent_ui(ui));
            }
        });

        heading(ui, "After picking", None);
        card(ui, |ui| {
            row(ui, "copy-as", "Copy as", None, false, |ui| {
                // Shown with the last colour picked, or snapr's blue.
                let example = self.color.unwrap_or([0x3d, 0x9b, 0xff]);
                theme::segmented_wrapped(ui, |ui| {
                    for format in ColorFormat::ALL {
                        ui.selectable_value(
                            &mut picker.copy_as,
                            format,
                            RichText::new(color_text(format, example)).monospace(),
                        );
                    }
                });
            });
            row(
                ui,
                "show-window",
                "Open this tab after picking with a hotkey or from the tray",
                Some(crate::locale::spelled!(
                    "Otherwise the colour is just copied, with a sound to say so"
                )),
                false,
                |ui| {
                    theme::switch(ui, &mut picker.show_window, "");
                },
            );
        });
    }

    /// The colours picked lately; clicking one shows it in full.
    fn recent_ui(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Recent").weak());
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
                    .on_hover_text(format!("{}  {}", hex(*c), crate::color_names::name(*c)))
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    self.color = Some(*c);
                }
            }
        });
    }

    fn qr_ui(&mut self, ui: &mut egui::Ui, hotkeys: &[ToolHotkey], actions: &mut Vec<Action>) {
        heading(ui, "Read a QR code", None);
        card(ui, |ui| {
            block(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui.add(theme::primary("Scan region\u{2026}")).clicked() {
                        actions.push(Action::ScanQr);
                    }
                    ui.add(
                        egui::Label::new(
                            RichText::new(
                                "Drag a region around a code on screen, or click to search a whole window.",
                            )
                            .weak(),
                        )
                        .wrap(),
                    );
                });
                hotkey_line(ui, hotkeys, ToolAction::ScanQr, actions);
            });
            if let Some(scan) = &mut self.scan {
                block(ui, |ui| scan_ui(ui, scan, actions));
            }
        });

        heading(ui, "Make a QR code", None);
        card(ui, |ui| {
            block(ui, |ui| self.make_ui(ui, actions));
        });
    }

    /// The text to encode, with what to do with its code, beside the code.
    fn make_ui(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        const GAP: f32 = 16.0;
        let p = theme::palette(ui);
        ui.horizontal_top(|ui| {
            let width = (ui.available_width() - CODE_PREVIEW - GAP).max(160.0);
            ui.allocate_ui_with_layout(vec2(width, 0.0), Layout::top_down(Align::Min), |ui| {
                ui.add(
                    TextEdit::multiline(&mut self.text)
                        .margin(theme::FIELD_MARGIN)
                        .hint_text("Text or link to encode")
                        .desired_rows(6)
                        .desired_width(f32::INFINITY),
                );
                // Before the code beside it is drawn, so it's never a frame behind.
                self.regenerate(ui.ctx());
                ui.add_space(4.0);
                let image = match self.generated.as_ref().map(|g| &g.code) {
                    None => {
                        ui.label(RichText::new("The code updates as you type.").weak());
                        None
                    }
                    Some(Err(e)) => {
                        ui.colored_label(ERROR, format!("Can't make a QR code: {e}"));
                        None
                    }
                    Some(Ok((image, _))) => {
                        let chars = self.text.chars().count();
                        ui.label(
                            RichText::new(format!(
                                "{chars} character{}  \u{00b7}  {} \u{00d7} {} px PNG",
                                if chars == 1 { "" } else { "s" },
                                image.width(),
                                image.height()
                            ))
                            .weak(),
                        );
                        Some(image)
                    }
                };
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let copy = ui.add_enabled(image.is_some(), theme::primary("Copy image"));
                    let save_as = ui.add_enabled(image.is_some(), egui::Button::new("Save as\u{2026}"));
                    if let Some(image) = image {
                        if copy.clicked() {
                            actions.push(Action::CopyImage(image.clone()));
                        }
                        if save_as.clicked() {
                            self.notice = save(image);
                        }
                    }
                });
                if let Some((msg, is_error)) = &self.notice {
                    let color = if *is_error { ERROR } else { p.text_weak };
                    ui.add(egui::Label::new(RichText::new(msg).color(color)).wrap());
                }
            });
            ui.add_space(GAP);
            let (rect, response) = ui.allocate_exact_size(vec2(CODE_PREVIEW, CODE_PREVIEW), Sense::click());
            match self.generated.as_ref().map(|g| &g.code) {
                Some(Ok((image, texture))) => {
                    paint_code(ui, rect, image, texture);
                    // Right-clicking the code copies it, without a menu in between.
                    if response.on_hover_text("Right-click to copy").secondary_clicked() {
                        actions.push(Action::CopyImage(image.clone()));
                    }
                }
                _ => {
                    ui.painter().rect(rect, 8.0, p.field, Stroke::new(1.0, p.divider), egui::StrokeKind::Inside);
                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "Your code shows here",
                        egui::FontId::proportional(12.0),
                        p.text_weak,
                    );
                }
            }
        });
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

/// A picked colour's swatch, beside its name and the ways it's written,
/// each with a button to copy it.
fn color_formats_ui(ui: &mut egui::Ui, rgb: [u8; 3], actions: &mut Vec<Action>) {
    let p = theme::palette(ui);
    ui.horizontal_top(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(REGION_PREVIEW, REGION_PREVIEW), Sense::hover());
        let [r, g, b] = rgb;
        ui.painter().rect(rect, 8.0, Color32::from_rgb(r, g, b), Stroke::new(1.0, p.divider), egui::StrokeKind::Inside);
        ui.add_space(12.0);
        egui::Grid::new("color-formats")
            .num_columns(3)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label(RichText::new("Name").weak())
                    .on_hover_text(crate::locale::spelled!(
                        "The closest named colour, from Name that Color (credited on Settings \u{2192} About)"
                    ));
                let name = crate::color_names::name(rgb);
                ui.label(name);
                if ui.button("Copy").clicked() {
                    actions.push(Action::CopyText(name.into()));
                }
                ui.end_row();
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
}

/// The tool's global hotkeys, with a link to add one.
fn hotkey_line(
    ui: &mut egui::Ui,
    hotkeys: &[ToolHotkey],
    action: ToolAction,
    actions: &mut Vec<Action>,
) {
    let keys: Vec<&str> = hotkeys
        .iter()
        .filter(|h| h.action == action)
        .map(|h| h.hotkey.as_str())
        .collect();
    ui.horizontal(|ui| {
        if keys.is_empty() {
            ui.label(RichText::new("No hotkey.").weak());
        } else {
            ui.label(RichText::new(format!("Hotkey: {}", keys.join(", "))).weak());
        }
        let link = if keys.is_empty() {
            "Add a hotkey\u{2026}"
        } else {
            "Add another\u{2026}"
        };
        if ui.link(link).clicked() {
            actions.push(Action::AddHotkey(action));
        }
    });
}

fn scan_ui(ui: &mut egui::Ui, scan: &mut Scan, actions: &mut Vec<Action>) {
    if let Some(region) = scan.region.take() {
        scan.texture = Some(
            ui.ctx()
                .load_texture("qr-region", region, TextureOptions::LINEAR),
        );
    }
    let p = theme::palette(ui);
    ui.horizontal_top(|ui| {
        // The region, fitted into a square so a wide one doesn't push the
        // results about.
        let (rect, _) = ui.allocate_exact_size(vec2(REGION_PREVIEW, REGION_PREVIEW), Sense::hover());
        ui.painter().rect_filled(rect, 8.0, p.field);
        if let Some(texture) = &scan.texture {
            let size = texture.size_vec2();
            let fit = rect.shrink(6.0);
            let scale = (fit.width() / size.x).min(fit.height() / size.y).min(1.0);
            let image = egui::Rect::from_center_size(rect.center(), size * scale);
            egui::Image::new(texture).corner_radius(4.0).paint_at(ui, image);
        }
        ui.painter().rect_stroke(rect, 8.0, Stroke::new(1.0, p.divider), egui::StrokeKind::Inside);
        ui.add_space(12.0);
        ui.vertical(|ui| {
            let found = match scan.texts.len() {
                0 => {
                    ui.label(RichText::new("No QR code found there").color(p.danger).strong());
                    ui.add(
                        egui::Label::new(
                            RichText::new(
                                "Try selecting just the code, with a little margin around it.",
                            )
                            .weak(),
                        )
                        .wrap(),
                    );
                    return;
                }
                1 => "Found a QR code".to_string(),
                n => format!("Found {n} QR codes"),
            };
            ui.label(RichText::new(found).strong());
            for (i, text) in scan.texts.iter().enumerate() {
                ui.push_id(i, |ui| {
                    ui.add_space(4.0);
                    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                        let link = qr::is_link(text);
                        if link
                            && ui
                                .add(theme::primary("Open link"))
                                .on_hover_text(text.trim())
                                .clicked()
                        {
                            actions.push(Action::OpenUrl(text.trim().to_string()));
                        }
                        let copy = if link {
                            ui.button("Copy")
                        } else {
                            ui.add(theme::primary("Copy"))
                        };
                        if copy.clicked() {
                            actions.push(Action::CopyText(text.clone()));
                        }
                        ui.add_space(4.0);
                        // What the code says, selectable, on a field.
                        ui.with_layout(Layout::top_down(Align::Min), |ui| {
                            egui::Frame::new()
                                .fill(p.field)
                                .stroke(Stroke::new(1.0, p.divider))
                                .corner_radius(8)
                                .inner_margin(theme::FIELD_MARGIN)
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.add(egui::Label::new(RichText::new(text).color(p.text)).selectable(true).wrap());
                                });
                        });
                    });
                });
            }
        });
    });
}

/// A generated code in `rect`, as big as it fits with every module the same
/// whole number of pixels, so it stays sharp.
fn paint_code(ui: &egui::Ui, rect: egui::Rect, image: &RgbaImage, texture: &TextureHandle) {
    let ppp = ui.ctx().pixels_per_point();
    let modules = (image.width() / qr::MODULE) as f32;
    let per_module = (rect.width() * ppp / modules).floor();
    let side = if per_module >= 1.0 { per_module * modules / ppp } else { rect.width() };
    let min = ((rect.center() - vec2(side, side) / 2.0) * ppp).round() / ppp;
    let code = egui::Rect::from_min_size(min, vec2(side, side));
    // The code's white quiet zone, out to the edge of the square.
    ui.painter().rect_filled(rect, 8.0, Color32::WHITE);
    egui::Image::new(texture).paint_at(ui, code);
}

/// A colour as `#RRGGBB`.
fn hex([r, g, b]: [u8; 3]) -> String {
    format!("#{r:02X}{g:02X}{b:02X}")
}

/// A colour written the ways it's usually pasted: hex, CSS rgb() and hsl().
fn formats(rgb: [u8; 3]) -> [(&'static str, String); 3] {
    [
        ("HEX", color_text(ColorFormat::Hex, rgb)),
        ("RGB", color_text(ColorFormat::Rgb, rgb)),
        ("HSL", color_text(ColorFormat::Hsl, rgb)),
    ]
}

/// A colour written as `format` says.
pub fn color_text(format: ColorFormat, rgb: [u8; 3]) -> String {
    let [r, g, b] = rgb;
    match format {
        ColorFormat::Hex => hex(rgb),
        ColorFormat::HexBare => format!("{r:02X}{g:02X}{b:02X}"),
        ColorFormat::Rgb => format!("rgb({r}, {g}, {b})"),
        ColorFormat::RgbBare => format!("{r}, {g}, {b}"),
        ColorFormat::Hsl => {
            let (h, s, l) = hsl(rgb);
            format!("hsl({h}, {s}%, {l}%)")
        }
    }
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

/// The image on the clipboard, if there is one.
fn clipboard_image() -> Result<RgbaImage, String> {
    let image = arboard::Clipboard::new()
        .and_then(|mut c| c.get_image())
        .map_err(|_| "There's no image on the clipboard.".to_string())?;
    RgbaImage::from_raw(
        image.width as u32,
        image.height as u32,
        image.bytes.into_owned(),
    )
    .ok_or_else(|| "Couldn't read the image on the clipboard.".to_string())
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
        let blue = [0x3d, 0x9b, 0xff];
        assert_eq!(color_text(ColorFormat::HexBare, blue), "3D9BFF");
        assert_eq!(color_text(ColorFormat::RgbBare, blue), "61, 155, 255");
        assert_eq!(hsl([255, 0, 0]), (0, 100, 50));
        assert_eq!(hsl([255, 0, 128]), (330, 100, 50));
        assert_eq!(hsl([0, 128, 0]), (120, 100, 25));
        assert_eq!(hsl([128, 128, 128]), (0, 0, 50));
        assert_eq!(hsl([0, 0, 0]), (0, 0, 0));
        assert_eq!(hsl([255, 255, 255]), (0, 0, 100));
    }

    /// Renders the colour picker after a few picks to
    /// `target/tools-color-preview.png`: `cargo test tools_color_preview -- --ignored`.
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
        crate::preview::render("tools-color-preview", [920, 680], 1.0, |root| {
            crate::theme::apply(root.ctx());
            let hotkeys = [crate::settings::ToolHotkey {
                action: ToolAction::PickColor,
                hotkey: "Ctrl + Shift + KeyC".into(),
            }];
            egui::CentralPanel::default().show(root, |ui| tools.ui(ui, &hotkeys, &mut ColorPicker::default(), &mut Vec::new()));
        });
    }

    /// Renders the pin tab with two images pinned to
    /// `target/tools-pin-preview.png`: `cargo test tools_pin_preview -- --ignored`.
    #[test]
    #[ignore]
    fn tools_pin_preview() {
        let mut tools = Tools::new();
        tools.tool = Tool::Pin;
        tools.pinned = 2;
        crate::preview::render("tools-pin-preview", [920, 720], 1.0, |root| {
            crate::theme::apply(root.ctx());
            egui::CentralPanel::default().show(root, |ui| tools.ui(ui, &[], &mut ColorPicker::default(), &mut Vec::new()));
        });
    }

    /// Renders the QR code tool, after a scan and with text to encode, to
    /// `target/tools-preview.png`, and after a scan that found nothing with
    /// no text yet to `target/tools-qr-empty-preview.png`:
    /// `cargo test tools_preview -- --ignored`.
    #[test]
    #[ignore]
    fn tools_preview() {
        let mut tools = Tools::new();
        let region = qr::encode("https://example.com/snapr").unwrap();
        let (preview, _) = crate::thumbnail::to_egui(region.clone().into(), (320, 320));
        let mut texts = qr::decode(&region);
        texts.push("WIFI:S:snapr office;T:WPA;P:a long passphrase that wraps onto a second line of the field;;".into());
        tools.scanned(texts, preview);
        tools.text = "Hello from snapr".into();
        crate::preview::render("tools-preview", [920, 640], 1.0, |root| {
            crate::theme::apply(root.ctx());
            egui::CentralPanel::default().show(root, |ui| tools.ui(ui, &[], &mut ColorPicker::default(), &mut Vec::new()));
        });

        let mut tools = Tools::new();
        let wide = RgbaImage::from_pixel(400, 120, image::Rgba([90, 120, 160, 255]));
        let (preview, _) = crate::thumbnail::to_egui(wide.into(), (320, 320));
        tools.scanned(Vec::new(), preview);
        crate::preview::render("tools-qr-empty-preview", [640, 560], 1.0, |root| {
            crate::theme::apply(root.ctx());
            egui::CentralPanel::default().show(root, |ui| tools.ui(ui, &[], &mut ColorPicker::default(), &mut Vec::new()));
        });
    }
}
