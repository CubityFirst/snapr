//! The look of snapr's own windows, matching the capture toolbar and the
//! recording bar: dark panels with thin outlines, 8 px corners, and the blue
//! accent kept for what matters (the main action, what's switched on). Light
//! mode follows the same roles. `apply` sets it up on a context, in the
//! theme picked in Settings (`follow` keeps it there); `switch`, `checkbox`,
//! `segmented`, `primary` and `slider` are the controls egui can't be styled
//! into.

use std::sync::atomic::{AtomicU8, Ordering};

use egui::{
    Color32, CornerRadius, Margin, Response, Sense, Shape, Stroke, StrokeKind, Ui, Vec2,
    WidgetInfo, WidgetText, WidgetType, pos2, vec2,
};

/// snapr's blue.
pub const ACCENT: Color32 = Color32::from_rgb(0x3d, 0x9b, 0xff);
/// The accent under the pointer.
pub const ACCENT_HOVER: Color32 = Color32::from_rgb(0x5c, 0xad, 0xff);
/// Text and marks on the accent: white on that blue is too faint to read.
pub const ON_ACCENT: Color32 = Color32::from_rgb(0x07, 0x13, 0x21);

/// Corner radius of buttons, fields and other controls.
const RADIUS: u8 = 8;
/// Inside text fields, so they stand as tall as the buttons beside them.
pub const FIELD_MARGIN: Margin = Margin::symmetric(8, 7);

/// The colours each role takes in one theme.
pub struct Palette {
    pub dark: bool,
    /// The icon bar down the side.
    pub nav: Color32,
    /// The page behind everything.
    pub panel: Color32,
    /// Menus and popups.
    pub surface: Color32,
    /// The cards a page's settings are grouped on.
    pub card: Color32,
    /// Buttons and other controls at rest.
    pub control: Color32,
    pub control_hover: Color32,
    pub control_pressed: Color32,
    /// Outlines of controls and cards.
    pub stroke: Color32,
    pub stroke_hover: Color32,
    /// Separators and the edges of panels.
    pub divider: Color32,
    /// Inside text fields.
    pub field: Color32,
    /// The track of a segmented control, and its picked option.
    pub track: Color32,
    pub track_picked: Color32,
    pub text: Color32,
    pub text_strong: Color32,
    pub text_weak: Color32,
    /// Something selected that isn't the main action: a tint of the accent.
    pub selected: Color32,
    pub selected_text: Color32,
    pub danger: Color32,
    /// The panels that float over the screen: the capture toolbar and its
    /// popups, see-through a little.
    pub floating: Color32,
    /// The bar over a pinned image, see-through more.
    pub hud: Color32,
    /// Icons and labels drawn on those panels.
    pub icon: Color32,
    pub icon_disabled: Color32,
    pub shadow: Color32,
    /// What shows past a borderless window's rounded corners.
    pub backdrop: Color32,
}

impl Palette {
    /// A wash over the panel, lighter in the dark theme and darker in the
    /// light one, for what's hovered or picked: stronger as `alpha` grows.
    pub fn wash(&self, alpha: u8) -> Color32 {
        if self.dark {
            Color32::from_white_alpha(alpha)
        } else {
            Color32::from_black_alpha(alpha)
        }
    }
}

pub const DARK: Palette = Palette {
    dark: true,
    nav: Color32::from_rgb(0x16, 0x17, 0x1a),
    panel: Color32::from_rgb(0x1a, 0x1b, 0x1e),
    surface: Color32::from_rgb(0x1c, 0x1d, 0x20),
    card: Color32::from_rgb(0x20, 0x21, 0x25),
    control: Color32::from_rgb(0x26, 0x27, 0x2b),
    control_hover: Color32::from_rgb(0x2e, 0x30, 0x35),
    control_pressed: Color32::from_rgb(0x1f, 0x20, 0x23),
    stroke: Color32::from_rgb(0x3a, 0x3c, 0x42),
    stroke_hover: Color32::from_rgb(0x4a, 0x4d, 0x55),
    divider: Color32::from_rgb(0x2c, 0x2e, 0x33),
    field: Color32::from_rgb(0x11, 0x12, 0x14),
    track: Color32::from_rgb(0x16, 0x17, 0x1a),
    track_picked: Color32::from_rgb(0x2e, 0x30, 0x35),
    text: Color32::from_rgb(0xc9, 0xcb, 0xd1),
    text_strong: Color32::from_rgb(0xff, 0xff, 0xff),
    text_weak: Color32::from_rgb(0x8b, 0x8e, 0x97),
    selected: Color32::from_rgb(0x1d, 0x35, 0x50),
    selected_text: Color32::from_rgb(0x7a, 0xb8, 0xff),
    danger: Color32::from_rgb(0xff, 0x8a, 0x8f),
    floating: Color32::from_rgba_premultiplied(28, 29, 32, 245),
    hud: Color32::from_rgba_unmultiplied_const(20, 21, 24, 220),
    icon: Color32::from_rgb(222, 223, 228),
    icon_disabled: Color32::from_rgb(96, 98, 104),
    shadow: Color32::from_black_alpha(110),
    backdrop: Color32::BLACK,
};

pub const LIGHT: Palette = Palette {
    dark: false,
    nav: Color32::from_rgb(0xe8, 0xea, 0xee),
    panel: Color32::from_rgb(0xf6, 0xf7, 0xf9),
    surface: Color32::from_rgb(0xff, 0xff, 0xff),
    card: Color32::from_rgb(0xff, 0xff, 0xff),
    control: Color32::from_rgb(0xff, 0xff, 0xff),
    control_hover: Color32::from_rgb(0xef, 0xf1, 0xf4),
    control_pressed: Color32::from_rgb(0xe4, 0xe6, 0xea),
    stroke: Color32::from_rgb(0xcf, 0xd2, 0xd8),
    stroke_hover: Color32::from_rgb(0xb3, 0xb7, 0xbf),
    divider: Color32::from_rgb(0xe0, 0xe2, 0xe7),
    field: Color32::from_rgb(0xff, 0xff, 0xff),
    track: Color32::from_rgb(0xe8, 0xea, 0xee),
    track_picked: Color32::from_rgb(0xff, 0xff, 0xff),
    text: Color32::from_rgb(0x2a, 0x2c, 0x31),
    text_strong: Color32::from_rgb(0x0e, 0x0f, 0x11),
    text_weak: Color32::from_rgb(0x5f, 0x63, 0x6d),
    selected: Color32::from_rgb(0xdc, 0xea, 0xff),
    selected_text: Color32::from_rgb(0x0d, 0x5f, 0xc4),
    danger: Color32::from_rgb(0xc8, 0x32, 0x3b),
    floating: Color32::from_rgba_unmultiplied_const(0xff, 0xff, 0xff, 245),
    hud: Color32::from_rgba_unmultiplied_const(0xff, 0xff, 0xff, 230),
    icon: Color32::from_rgb(0x33, 0x35, 0x3b),
    icon_disabled: Color32::from_rgb(0xb4, 0xb7, 0xbe),
    shadow: Color32::from_black_alpha(45),
    backdrop: Color32::from_rgb(0xcf, 0xd2, 0xd8),
};

/// The palette for `ui`'s theme.
pub fn palette(ui: &Ui) -> &'static Palette {
    palette_of(ui.visuals().dark_mode)
}

/// The palette for the dark theme, or the light one.
pub fn palette_of(dark: bool) -> &'static Palette {
    if dark { &DARK } else { &LIGHT }
}

/// The theme picked in Settings, for the windows that open without the
/// settings to hand: the capture overlay, pins, toasts, the recording bar.
static PICKED: AtomicU8 = AtomicU8::new(0);

/// Sets the theme every window follows.
pub fn pick(theme: crate::settings::Theme) {
    PICKED.store(theme as u8, Ordering::Relaxed);
}

pub fn picked() -> crate::settings::Theme {
    let n = PICKED.load(Ordering::Relaxed);
    crate::settings::Theme::ALL
        .into_iter()
        .find(|t| *t as u8 == n)
        .unwrap_or_default()
}

/// Puts a context in the theme picked in Settings. Called before each
/// frame, so a window takes a new pick the next time it's drawn. With
/// *System*, which theme that is comes from the window's own input.
pub fn follow(ctx: &egui::Context) {
    ctx.set_theme(picked().preference());
}

/// The colour to clear a window to before drawing its frame, for what
/// shows past its rounded corners.
pub fn backdrop(ctx: &egui::Context) -> [f32; 4] {
    let dark = ctx.theme() == egui::Theme::Dark;
    egui::Rgba::from(palette_of(dark).backdrop).to_array()
}

/// Styles a context's light and dark themes, and shows the one picked in
/// Settings.
pub fn apply(ctx: &egui::Context) {
    ctx.set_visuals_of(egui::Theme::Dark, visuals(egui::Visuals::dark(), &DARK));
    ctx.set_visuals_of(egui::Theme::Light, visuals(egui::Visuals::light(), &LIGHT));
    ctx.all_styles_mut(|style| {
        let s = &mut style.spacing;
        s.item_spacing = vec2(8.0, 8.0);
        s.button_padding = vec2(12.0, 6.0);
        s.interact_size.y = 30.0;
        s.icon_width = 18.0;
        s.icon_width_inner = 10.0;
        s.icon_spacing = 8.0;
        s.slider_rail_height = 4.0;
        s.menu_margin = Margin::same(4);
    });
    crisp_text(ctx);
    follow(ctx);
}

/// On macOS's 2× screens, glyphs drawn at quarter-pixel offsets
/// (subpixel binning) come out visibly soft and gain nothing in spacing; they
/// go on whole pixels there. Set after any `set_visuals`, which resets it.
/// The text there is also in the system font, see [`system_font`].
pub fn crisp_text(ctx: &egui::Context) {
    if cfg!(target_os = "macos") {
        ctx.all_styles_mut(|s| s.visuals.text_options.subpixel_binning = false);
        if let Some(font) = system_font() {
            ctx.add_font(egui::epaint::text::FontInsert::new(
                "system",
                font,
                vec![egui::epaint::text::InsertFontFamily {
                    family: egui::FontFamily::Proportional,
                    priority: egui::epaint::text::FontPriority::Highest,
                }],
            ));
        }
    }
}

/// macOS's own UI font (SF Pro), at regular weight, ahead of egui's Ubuntu
/// Light: Macs draw text without Windows' thickening, so the light weight
/// came out spindly, and it has no arrows. Ubuntu stays as the fallback.
/// `None` if the font isn't where it's been since macOS 10.15.
fn system_font() -> Option<egui::FontData> {
    static FONT: std::sync::OnceLock<Option<&'static [u8]>> = std::sync::OnceLock::new();
    let data = (*FONT.get_or_init(|| {
        let data = std::fs::read("/System/Library/Fonts/SFNS.ttf").ok()?;
        Some(Vec::leak(data))
    }))?;
    let mut font = egui::FontData::from_static(data);
    // A variable font: regular weight, and the optical size cut for small
    // text (the smallest it has).
    font.tweak.coords = egui::epaint::text::VariationCoords::new([(b"wght", 400.0), (b"opsz", 0.0)]);
    Some(font)
}

fn visuals(mut v: egui::Visuals, p: &Palette) -> egui::Visuals {
    let radius = CornerRadius::same(RADIUS);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.surface;
    w.noninteractive.weak_bg_fill = p.surface;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.divider);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = radius;
    for (state, fill, stroke, text) in [
        (&mut w.inactive, p.control, p.stroke, p.text),
        (
            &mut w.hovered,
            p.control_hover,
            p.stroke_hover,
            p.text_strong,
        ),
        (&mut w.active, p.control_pressed, p.stroke, p.text_strong),
        (&mut w.open, p.control_hover, p.stroke_hover, p.text_strong),
    ] {
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.bg_stroke = Stroke::new(1.0, stroke);
        state.fg_stroke = Stroke::new(1.0, text);
        state.corner_radius = radius;
        state.expansion = 0.0;
    }
    v.selection.bg_fill = p.selected;
    v.selection.stroke = Stroke::new(1.0, p.selected_text);
    v.hyperlink_color = p.selected_text;
    v.weak_text_color = Some(p.text_weak);
    v.panel_fill = p.panel;
    v.window_fill = p.surface;
    v.window_stroke = Stroke::new(1.0, p.stroke);
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(10);
    v.extreme_bg_color = p.field;
    v.text_edit_bg_color = Some(p.field);
    v.faint_bg_color = p.surface;
    v.code_bg_color = p.field;
    v.error_fg_color = p.danger;
    v.handle_shape = egui::style::HandleShape::Circle;
    v
}

/// A button for the main action: filled with the accent.
pub fn primary(text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(text).color(ON_ACCENT).strong()).fill(ACCENT)
}

/// A button that deletes or removes something: its text in red.
pub fn danger(ui: &Ui, text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(text).color(palette(ui).danger))
}

/// An on/off switch with a label, for settings that stand alone. The knob
/// has a tick when it's on and a cross when it's off.
pub fn switch(ui: &mut Ui, on: &mut bool, text: impl Into<WidgetText>) -> Response {
    let track = vec2(36.0, 20.0);
    let mut response = labelled(
        ui,
        track,
        text,
        |ui, rect, response, on| {
            let p = palette(ui);
            let enabled = ui.is_enabled();
            let t = ui.ctx().animate_bool_responsive(response.id, on);
            let painter = ui.painter();
            let (fill, stroke) = if on {
                (ACCENT, Stroke::NONE)
            } else if response.hovered() && enabled {
                (p.control_hover, Stroke::new(1.0, p.stroke_hover))
            } else {
                (p.control, Stroke::new(1.0, p.stroke))
            };
            painter.rect(
                rect,
                CornerRadius::same(10),
                fill,
                stroke,
                StrokeKind::Inside,
            );
            let radius = egui::lerp(7.0..=8.0, t);
            let x = egui::lerp((rect.left() + 10.0)..=(rect.right() - 10.0), t);
            let center = pos2(x, rect.center().y);
            let knob = if on { Color32::WHITE } else { p.text_weak };
            painter.circle_filled(center, radius, knob);
            if on {
                let mark = [vec2(-3.5, 0.0), vec2(-1.0, 2.5), vec2(3.5, -2.5)];
                painter.add(Shape::line(
                    mark.iter().map(|d| center + *d).collect(),
                    Stroke::new(1.8, ACCENT),
                ));
            } else {
                let s = Stroke::new(1.6, p.control);
                let d = 2.5;
                painter.line_segment([center + vec2(-d, -d), center + vec2(d, d)], s);
                painter.line_segment([center + vec2(d, -d), center + vec2(-d, d)], s);
            }
        },
        *on,
    );
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response
}

/// A checkbox, for one of several options that go together.
pub fn checkbox(ui: &mut Ui, checked: &mut bool, text: impl Into<WidgetText>) -> Response {
    let mut response = labelled(
        ui,
        Vec2::splat(18.0),
        text,
        |ui, rect, response, checked| {
            let p = palette(ui);
            let enabled = ui.is_enabled();
            let painter = ui.painter();
            let radius = CornerRadius::same(5);
            if checked {
                painter.rect_filled(rect, radius, ACCENT);
                let at = |x: f32, y: f32| {
                    pos2(
                        rect.left() + rect.width() * x,
                        rect.top() + rect.height() * y,
                    )
                };
                painter.add(Shape::line(
                    vec![at(0.26, 0.52), at(0.43, 0.69), at(0.75, 0.33)],
                    Stroke::new(2.0, ON_ACCENT),
                ));
            } else {
                let (fill, stroke) = if response.hovered() && enabled {
                    (p.control_hover, p.stroke_hover)
                } else {
                    (p.control, p.stroke_hover)
                };
                painter.rect(
                    rect,
                    radius,
                    fill,
                    Stroke::new(1.0, stroke),
                    StrokeKind::Inside,
                );
            }
        },
        *checked,
    );
    if response.clicked() {
        *checked = !*checked;
        response.mark_changed();
    }
    response
}

/// Lays out an icon of `icon` size and a label after it, clickable as one,
/// and has `paint` draw the icon.
fn labelled(
    ui: &mut Ui,
    icon: Vec2,
    text: impl Into<WidgetText>,
    paint: impl FnOnce(&Ui, egui::Rect, &Response, bool),
    value: bool,
) -> Response {
    let text: WidgetText = text.into();
    let label = text.text().to_owned();
    let gap = if label.is_empty() {
        0.0
    } else {
        ui.spacing().icon_spacing
    };
    let wrap = ui.available_width() - icon.x - gap;
    let galley = text.into_galley(ui, None, wrap, egui::TextStyle::Body);
    let size = vec2(
        icon.x + gap + galley.size().x,
        icon.y
            .max(galley.size().y)
            .max(ui.spacing().interact_size.y),
    );
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response
        .widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, ui.is_enabled(), value, &label));
    if ui.is_rect_visible(rect) {
        let icon_rect =
            egui::Rect::from_min_size(pos2(rect.left(), rect.center().y - icon.y / 2.0), icon);
        paint(ui, icon_rect, &response, value);
        let visuals = ui.style().interact(&response);
        let color = if ui.is_enabled() {
            visuals.text_color()
        } else {
            palette(ui).text_weak
        };
        let at = pos2(
            icon_rect.right() + gap,
            rect.center().y - galley.size().y / 2.0,
        );
        ui.painter().galley(at, galley, color);
    }
    response
}

/// Tightens a context menu's items: the window-wide spacing is sized for
/// controls, which leaves a list of menu entries looking sparse.
pub fn compact_menu(ui: &mut Ui) {
    let spacing = ui.spacing_mut();
    spacing.item_spacing.y = 1.0;
    spacing.button_padding = vec2(10.0, 3.0);
    spacing.interact_size.y = 24.0;
}

/// A row of options where one is picked, in a sunken track with the picked
/// one raised. `add` adds them with `selectable_value`. It stays on one line,
/// so it's as wide as its options wherever it's put.
pub fn segmented<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    segmented_in(ui, false, add)
}

/// [`segmented`], wrapping onto more lines if its options don't fit.
pub fn segmented_wrapped<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    segmented_in(ui, true, add)
}

fn segmented_in<R>(ui: &mut Ui, wrap: bool, add: impl FnOnce(&mut Ui) -> R) -> R {
    let p = palette(ui);
    egui::Frame::new()
        .fill(p.track)
        .stroke(Stroke::new(1.0, p.stroke))
        .corner_radius(CornerRadius::same(9))
        .inner_margin(Margin::same(3))
        .show(ui, |ui| {
            let spacing = ui.spacing_mut();
            spacing.item_spacing.x = 2.0;
            spacing.button_padding = vec2(12.0, 4.0);
            spacing.interact_size.y = 24.0;
            let v = ui.visuals_mut();
            v.selection.bg_fill = p.track_picked;
            v.selection.stroke = Stroke::new(1.0, p.text_strong);
            for state in [
                &mut v.widgets.inactive,
                &mut v.widgets.hovered,
                &mut v.widgets.active,
            ] {
                state.corner_radius = CornerRadius::same(6);
                state.bg_stroke = Stroke::NONE;
            }
            v.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
            v.widgets.inactive.fg_stroke = Stroke::new(1.0, p.text_weak);
            v.widgets.hovered.weak_bg_fill = p.control;
            if wrap {
                ui.horizontal_wrapped(add).inner
            } else {
                ui.horizontal(add).inner
            }
        })
        .inner
}

/// A slider whose filled part, up to the handle, is the accent. Its value's
/// box is as wide as `widest`, the longest the value gets, so the box doesn't
/// change width, and move the slider, as the value does.
pub fn slider(ui: &mut Ui, slider: egui::Slider<'_>, widest: &str) -> Response {
    ui.scope(|ui| {
        let v = ui.visuals_mut();
        v.slider_trailing_fill = true;
        v.selection.bg_fill = ACCENT;
        let text = WidgetText::from(widest).into_galley(ui, None, f32::INFINITY, egui::TextStyle::Button);
        let s = ui.spacing_mut();
        s.interact_size.x = s.interact_size.x.max(text.size().x + 2.0 * s.button_padding.x);
        ui.add(slider)
    })
    .inner
}
