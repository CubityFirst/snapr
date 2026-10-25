//! The annotation toolbar and on-screen selection decorations, drawn with egui
//! over the overlay. Icons are painted as vector shapes. The toolbar and its
//! popups take the theme picked in Settings; what's drawn on the screen
//! itself (the size label, hints, the loupe) is dark whatever the theme, to
//! stand out on anything.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Pos2, Rect, Sense, Shape, Stroke,
    StrokeKind, Vec2, pos2, vec2,
};

use crate::annotate::{
    ArrowOptions, BENDS, COLORS, HighlighterInk, HighlighterShape, PIXEL_BLOCKS, PixelateOptions, STEP_SIZES, Size, StepOptions, Style, TEXT_SIZES, TextOptions, Tool,
};
use crate::session::MAGNIFIER_PIXELS;
use crate::theme::{self, ACCENT};

const BUTTON: f32 = 32.0;
/// Colours per row in the colour popup.
const PALETTE_COLUMNS: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    Tool(Tool),
    Color([u8; 3]),
    Size(Size),
    Pixelate(PixelateOptions),
    Arrow(ArrowOptions),
    Bends(u8),
    ClipShadow(bool),
    Step(StepOptions),
    Text(TextOptions),
    Highlighter(HighlighterShape),
    HighlighterInk(HighlighterInk),
    Undo,
    Redo,
}

/// The outline around the annotation under the cursor, in points.
pub enum Hover {
    /// A box around it.
    Box(Rect),
    /// A ring drawn around its strokes, shown over this area.
    Ring(egui::TextureId, Rect),
}

/// Everything the overlay UI needs for one frame, in window points.
pub struct View {
    pub tool: Tool,
    pub style: Style,
    pub can_undo: bool,
    pub can_redo: bool,
    pub show_toolbar: bool,
    /// The top of the screen to keep clear (the camera notch on MacBooks),
    /// in points.
    pub top_inset: f32,
    /// The selection on this monitor, and its size in pixels.
    pub selection: Option<(Rect, [u32; 2])>,
    pub cursor: CursorIcon,
    /// Handles for curving lines or arrows, or drawing steps out to a
    /// point, in points.
    pub arrow_nodes: Vec<egui::Pos2>,
    /// The annotation under the cursor, outlined because it can be
    /// right-clicked away.
    pub hovered: Option<Hover>,
    /// What to do, shown at the top of the screen.
    pub hint: Option<&'static str>,
    /// Details beside the cursor (in points): the colour picker's
    /// magnifier, or the crosshair info chosen in Settings.
    pub cursor_info: Option<(egui::Pos2, CursorInfo)>,
}

/// What's shown in the card beside the cursor.
#[derive(Debug, Clone, Default)]
pub struct CursorInfo {
    /// The screen pixels around the cursor, enlarged (an odd-sided square
    /// centred on it; off-screen pixels are transparent).
    pub magnifier: Option<image::RgbaImage>,
    /// Whether the magnifier's pixels are a set size, so it grows with how
    /// many there are (scrolling resizes it), rather than it being a set
    /// size (scrolling zooms it).
    pub fixed_cells: bool,
    /// The colour of the pixel under the cursor.
    pub color: Option<[u8; 3]>,
    /// That pixel's position on the screen.
    pub position: Option<[i32; 2]>,
}

/// Runs egui for one overlay frame. egui needs extra passes when the toolbar
/// first appears (to measure it); those run immediately rather than waiting
/// for another redraw, so the toolbar is there from the very first frame.
pub fn run(
    ctx: &egui::Context,
    mut input: egui::RawInput,
    view: &View,
) -> (Vec<Action>, Option<Rect>, egui::FullOutput) {
    const MAX_PASSES: usize = 3;
    let mut actions = Vec::new();
    let mut textures = egui::TexturesDelta::default();
    let mut platform = egui::PlatformOutput::default();
    for pass in 1..=MAX_PASSES {
        let mut toolbar = None;
        let mut output = ctx.run_ui(input.clone(), |root| {
            let (a, t) = ui(root, view);
            actions.extend(a);
            toolbar = t;
        });
        textures.append(std::mem::take(&mut output.textures_delta));
        platform.append(std::mem::take(&mut output.platform_output));
        let again = output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .is_some_and(|v| v.repaint_delay.is_zero());
        if !again || pass == MAX_PASSES {
            output.textures_delta = textures;
            output.platform_output = platform;
            return (actions, toolbar, output);
        }
        // Events were handled by the first pass.
        input.events.clear();
    }
    unreachable!("the last pass always returns")
}

/// Draws the overlay UI. Returns clicked actions and the toolbar's area.
fn ui(ui: &mut egui::Ui, view: &View) -> (Vec<Action>, Option<Rect>) {
    let ctx = ui.ctx().clone();
    ctx.set_cursor_icon(view.cursor);
    let mut actions = Vec::new();

    if let Some((sel, [w, h])) = view.selection {
        let painter = ui.painter();
        // Size label above the selection, or inside it at the top of the screen.
        let text = format!("{w} \u{00d7} {h}");
        let galley = painter.layout_no_wrap(text, FontId::proportional(12.0), Color32::WHITE);
        let size = galley.size() + vec2(12.0, 6.0);
        let mut at = pos2(sel.left(), sel.top() - size.y - 6.0);
        if at.y < 2.0 {
            at.y = sel.top() + 6.0;
            at.x += 6.0;
        }
        let bg = Rect::from_min_size(at, size);
        painter.rect_filled(bg, 4.0, Color32::from_black_alpha(190));
        painter.galley(bg.min + vec2(6.0, 3.0), galley, Color32::WHITE);
    }

    if let Some(hint) = view.hint {
        let painter = ui.painter();
        let galley =
            painter.layout_no_wrap(hint.into(), FontId::proportional(15.0), Color32::WHITE);
        let size = galley.size() + vec2(24.0, 14.0);
        let top = ctx.content_rect().center_top() + vec2(0.0, 14.0 + view.top_inset);
        let bg = Rect::from_min_size(top - vec2(size.x / 2.0, 0.0), size);
        painter.rect_filled(bg, 8.0, Color32::from_black_alpha(190));
        painter.galley(bg.min + vec2(12.0, 7.0), galley, Color32::WHITE);
    }

    if let Some((at, info)) = &view.cursor_info {
        draw_cursor_info(ui.painter(), *at, info);
    }

    match view.hovered {
        Some(Hover::Box(r)) => {
            // Dark under light, to show on any background.
            let r = r.expand(2.0);
            let painter = ui.painter();
            painter.rect_stroke(r, 3.0, Stroke::new(3.0, Color32::from_black_alpha(140)), StrokeKind::Middle);
            painter.rect_stroke(r, 3.0, Stroke::new(1.5, Color32::WHITE), StrokeKind::Middle);
        }
        Some(Hover::Ring(texture, r)) => {
            let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
            ui.painter().image(texture, r, uv, Color32::WHITE);
        }
        None => {}
    }

    for &node in &view.arrow_nodes {
        let painter = ui.painter();
        painter.circle_filled(node, 5.0, Color32::WHITE);
        painter.circle_stroke(node, 5.0, Stroke::new(1.5, Color32::from_black_alpha(200)));
    }

    if !view.show_toolbar {
        return (actions, None);
    }
    let p = theme::palette(ui);
    let area = egui::Area::new(egui::Id::new("toolbar"))
        .anchor(Align2::CENTER_TOP, vec2(0.0, 14.0 + view.top_inset))
        .order(egui::Order::Foreground)
        .fade_in(false)
        .show(&ctx, |ui| {
            egui::Frame::new()
                .fill(p.floating)
                .stroke(Stroke::new(1.0, p.stroke))
                .corner_radius(CornerRadius::same(10))
                .inner_margin(6)
                .shadow(egui::Shadow {
                    offset: [0, 4],
                    blur: 16,
                    spread: 0,
                    color: p.shadow,
                })
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing = vec2(2.0, 0.0);
                    ui.horizontal(|ui| toolbar_contents(ui, view, &mut actions))
                        .inner
                })
                .inner
        });
    let mut covered = area.response.rect;
    let mut hovered = area.response.hovered();
    if let Some(popup) = show_popup(&ctx, &area.inner, view, &mut actions) {
        covered = covered.union(popup.rect);
        hovered |= popup.hovered();
    }
    // Over the toolbar, use a normal pointer instead of the tool cursor.
    if hovered {
        ctx.set_cursor_icon(CursorIcon::Default);
    }
    (actions, Some(covered))
}

/// The card beside the cursor: the pixels around it enlarged (the middle one
/// outlined), then rows for its colour, the colour's name and position.
fn draw_cursor_info(painter: &egui::Painter, at: egui::Pos2, info: &CursorInfo) {
    const GAP: f32 = 22.0;
    /// The magnifier's smallest side, whatever its zoom; it fills the card's
    /// width. With fixed cells, its side at the starting zoom instead.
    const SIDE: f32 = 135.0;
    const ROW: f32 = 24.0;
    /// Room on the left of each row for its swatch or icon.
    const INDENT: f32 = 28.0;
    let font = FontId::monospace(12.0);
    let color = info.color.map(|[r, g, b]| {
        let text = format!("#{r:02X}{g:02X}{b:02X}   {r}, {g}, {b}");
        let label = painter.layout_no_wrap(text, font.clone(), Color32::WHITE);
        (Color32::from_rgb(r, g, b), label)
    });
    let name = info.color.map(|rgb| {
        let name = crate::color_names::name(rgb).to_string();
        painter.layout_no_wrap(name, FontId::proportional(12.0), Color32::from_white_alpha(200))
    });
    let position = info.position.map(|[x, y]| {
        painter.layout_no_wrap(format!("X {x}   Y {y}"), font.clone(), Color32::WHITE)
    });
    let labels = [color.as_ref().map(|c| &c.1), name.as_ref(), position.as_ref()];
    let rows_h = labels.iter().flatten().count() as f32 * ROW;
    // Room for the widest colour (the font is monospace), so the card doesn't
    // change width as the colour does.
    let widest_color = info.color.map_or(0.0, |_| {
        let text = "#FFFFFF   255, 255, 255".to_string();
        painter.layout_no_wrap(text, font.clone(), Color32::WHITE).size().x
    });
    let text_w = labels
        .iter()
        .flatten()
        .map(|g| g.size().x)
        .fold(widest_color, f32::max);
    let width = text_w + INDENT + 8.0;
    let side = match &info.magnifier {
        Some(p) if info.fixed_cells => p.width() as f32 * SIDE / MAGNIFIER_PIXELS as f32,
        Some(_) => width.max(SIDE),
        None => 0.0,
    };
    let size = vec2(width.max(side), side + rows_h);
    if size.y == 0.0 {
        return;
    }
    // Below right of the cursor, flipped where it would leave the screen.
    let screen = painter.clip_rect();
    let mut min = at + vec2(GAP, GAP);
    if min.x + size.x > screen.right() {
        min.x = at.x - GAP - size.x;
    }
    if min.y + size.y > screen.bottom() {
        min.y = at.y - GAP - size.y;
    }
    let card = Rect::from_min_size(min, size);
    painter.rect_filled(card.expand(3.0), 8.0, Color32::from_black_alpha(200));

    let mut top = card.top();
    if let Some(pixels) = &info.magnifier {
        let n = pixels.width();
        let grid = Rect::from_min_size(pos2(card.center().x - side / 2.0, top), vec2(side, side));
        // Cell edges on whole screen pixels, so the cells are crisp and evenly
        // sized at any zoom. One mesh, so there are no seams between them.
        let ppp = painter.pixels_per_point();
        let edge = |from: f32, i: u32| (((from + i as f32 * side / n as f32) * ppp).round()) / ppp;
        let cell = |x: u32, y: u32| {
            Rect::from_min_max(
                pos2(edge(grid.left(), x), edge(grid.top(), y)),
                pos2(edge(grid.left(), x + 1), edge(grid.top(), y + 1)),
            )
        };
        let mut mesh = egui::Mesh::default();
        for (x, y, p) in pixels.enumerate_pixels() {
            let [r, g, b, a] = p.0;
            let color = if a == 0 {
                Color32::from_gray(30) // off screen
            } else {
                Color32::from_rgb(r, g, b)
            };
            mesh.add_colored_rect(cell(x, y), color);
        }
        painter.add(mesh);
        let middle = cell(n / 2, n / 2);
        painter.rect_stroke(
            middle,
            0.0,
            Stroke::new(1.0, Color32::BLACK),
            StrokeKind::Outside,
        );
        painter.rect_stroke(
            middle.expand(1.0),
            0.0,
            Stroke::new(1.0, Color32::WHITE),
            StrokeKind::Outside,
        );
        painter.rect_stroke(
            grid,
            0.0,
            Stroke::new(1.0, Color32::from_white_alpha(90)),
            StrokeKind::Outside,
        );
        top = grid.bottom();
    }
    // Lays out the next row's text; returns where its swatch or icon goes.
    let mut row = |label: std::sync::Arc<egui::Galley>| {
        let r = Rect::from_min_size(pos2(card.left(), top), vec2(size.x, ROW));
        painter.galley(
            pos2(r.left() + INDENT, r.center().y - label.size().y / 2.0),
            label,
            Color32::WHITE,
        );
        top = r.bottom();
        Rect::from_center_size(pos2(r.left() + 14.0, r.center().y), vec2(14.0, 14.0))
    };
    if let Some((fill, label)) = color {
        let swatch = row(label);
        painter.rect_filled(swatch, 3.0, fill);
        painter.rect_stroke(
            swatch,
            3.0,
            Stroke::new(1.0, Color32::from_white_alpha(140)),
            StrokeKind::Inside,
        );
    }
    if let Some(label) = name {
        row(label);
    }
    if let Some(label) = position {
        let icon = row(label).shrink(1.0);
        let stroke = Stroke::new(1.5, Color32::from_white_alpha(200));
        painter.line_segment([icon.center_top(), icon.center_bottom()], stroke);
        painter.line_segment([icon.left_center(), icon.right_center()], stroke);
    }
}

/// A toolbar button that opens a popup of choices underneath it.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Popup {
    Color,
    Size,
    /// The current tool's settings.
    Options,
}

/// Where the popup buttons are, for hanging their popups from.
struct PopupButtons {
    color: Rect,
    size: Rect,
    options: Rect,
}

fn popup_id() -> egui::Id {
    egui::Id::new("toolbar-popup")
}

fn open_popup(ctx: &egui::Context) -> Option<Popup> {
    ctx.data(|d| d.get_temp::<Option<Popup>>(popup_id()).flatten())
}

fn set_popup(ctx: &egui::Context, popup: Option<Popup>) {
    ctx.data_mut(|d| d.insert_temp(popup_id(), popup));
}

/// The open popup, if any. Picking a choice, or clicking anywhere else,
/// closes it.
fn show_popup(
    ctx: &egui::Context,
    buttons: &PopupButtons,
    view: &View,
    actions: &mut Vec<Action>,
) -> Option<egui::Response> {
    let popup = open_popup(ctx)?;
    let button = match popup {
        Popup::Color => buttons.color,
        Popup::Size => buttons.size,
        Popup::Options => buttons.options,
    };
    let mut picked = false;
    let p = theme::palette_of(ctx.theme() == egui::Theme::Dark);
    let area = egui::Area::new(egui::Id::new("toolbar-popup-area"))
        .fixed_pos(button.left_bottom() + vec2(-6.0, 10.0))
        .order(egui::Order::Foreground)
        .fade_in(false)
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(p.floating)
                .stroke(Stroke::new(1.0, p.stroke))
                .corner_radius(CornerRadius::same(10))
                .inner_margin(6)
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing = vec2(2.0, 2.0);
                    match popup {
                        Popup::Color => {
                            let current = view.style.color_for(view.tool);
                            for row in COLORS.chunks(PALETTE_COLUMNS) {
                                ui.horizontal(|ui| {
                                    for &color in row {
                                        let selected = current == color;
                                        if color_dot(ui, color, selected).clicked() {
                                            actions.push(Action::Color(color));
                                            picked = true;
                                        }
                                    }
                                });
                            }
                            // Any colour; stays open while it's being chosen.
                            ui.add_space(4.0);
                            if let Some(color) = custom_color(ui, current) {
                                actions.push(Action::Color(color));
                            }
                        }
                        Popup::Size => {
                            for size in Size::ALL {
                                if size_option(ui, size, view.style.size == size).clicked() {
                                    actions.push(Action::Size(size));
                                    picked = true;
                                }
                            }
                        }
                        // Stays open, to change more than one setting.
                        Popup::Options => tool_options(ui, view, actions),
                    }
                });
        });
    let clicked_away = ctx.input(|i| i.pointer.any_pressed())
        && ctx
            .pointer_interact_pos()
            .is_some_and(|p| !area.response.rect.contains(p) && !button.contains(p));
    if picked || clicked_away {
        set_popup(ctx, None);
    }
    Some(area.response)
}

/// A picker for any colour: saturation and brightness in the square, hue
/// in the strip under it. Returns the colour while it's being changed.
fn custom_color(ui: &mut egui::Ui, current: [u8; 3]) -> Option<[u8; 3]> {
    use egui::ecolor::Hsva;
    // The hue is kept while the colour stays the same, so it isn't lost at
    // grey, black or white.
    let id = egui::Id::new("custom-colour");
    let mut hsv = ui
        .data(|d| d.get_temp::<([f32; 3], [u8; 3])>(id))
        .filter(|(_, c)| *c == current)
        .map_or_else(
            || {
                let [r, g, b] = current;
                let h = Hsva::from(Color32::from_rgb(r, g, b));
                [h.h, h.s, h.v]
            },
            |(hsv, _)| hsv,
        );
    let color = |h: f32, s: f32, v: f32| Color32::from(Hsva::new(h, s, v, 1.0));
    // As wide as the palette's rows of dots.
    let columns = PALETTE_COLUMNS as f32;
    let width = columns * 26.0 + (columns - 1.0) * ui.spacing().item_spacing.x;
    let (square, square_resp) = ui.allocate_exact_size(vec2(width, 80.0), Sense::drag());
    let (strip, strip_resp) = ui.allocate_exact_size(vec2(width, 14.0), Sense::drag());
    let p = ui.painter();

    // A grid of cells, coloured exactly at their corners.
    const CELLS: usize = 12;
    let mut mesh = egui::Mesh::default();
    for row in 0..=CELLS {
        for col in 0..=CELLS {
            let (s, v) = (col as f32 / CELLS as f32, 1.0 - row as f32 / CELLS as f32);
            let at = pos2(square.left() + s * square.width(), square.top() + (1.0 - v) * square.height());
            mesh.colored_vertex(at, color(hsv[0], s, v));
        }
    }
    let mut hue = egui::Mesh::default();
    for i in 0..=CELLS {
        let h = i as f32 / CELLS as f32;
        let x = strip.left() + h * strip.width();
        hue.colored_vertex(pos2(x, strip.top()), color(h, 1.0, 1.0));
        hue.colored_vertex(pos2(x, strip.bottom()), color(h, 1.0, 1.0));
    }
    let n = CELLS as u32 + 1;
    for row in 0..CELLS as u32 {
        for col in 0..CELLS as u32 {
            let i = row * n + col;
            mesh.add_triangle(i, i + 1, i + n);
            mesh.add_triangle(i + 1, i + n + 1, i + n);
        }
    }
    for i in 0..CELLS as u32 {
        hue.add_triangle(2 * i, 2 * i + 1, 2 * i + 2);
        hue.add_triangle(2 * i + 1, 2 * i + 3, 2 * i + 2);
    }
    p.add(mesh);
    p.add(hue);

    let mut changed = false;
    if let Some(at) = square_resp.interact_pointer_pos() {
        hsv[1] = ((at.x - square.left()) / square.width()).clamp(0.0, 1.0);
        hsv[2] = (1.0 - (at.y - square.top()) / square.height()).clamp(0.0, 1.0);
        changed = true;
    }
    if let Some(at) = strip_resp.interact_pointer_pos() {
        hsv[0] = ((at.x - strip.left()) / strip.width()).clamp(0.0, 1.0);
        changed = true;
    }
    // Where the colour is, ringed dark under light.
    let dot = pos2(square.left() + hsv[1] * square.width(), square.top() + (1.0 - hsv[2]) * square.height());
    p.circle_stroke(dot, 5.0, Stroke::new(3.0, Color32::from_black_alpha(140)));
    p.circle_stroke(dot, 5.0, Stroke::new(1.5, Color32::WHITE));
    let x = strip.left() + hsv[0] * strip.width();
    let mark = Rect::from_min_max(pos2(x - 2.0, strip.top() - 1.0), pos2(x + 2.0, strip.bottom() + 1.0));
    p.rect_stroke(mark, 1.0, Stroke::new(1.5, Color32::WHITE), StrokeKind::Middle);
    for r in [square_resp, strip_resp] {
        r.on_hover_cursor(CursorIcon::Crosshair);
    }

    let picked = color(hsv[0], hsv[1], hsv[2]);
    let rgb = [picked.r(), picked.g(), picked.b()];
    ui.data_mut(|d| d.insert_temp(id, (hsv, rgb)));
    (changed && rgb != current).then_some(rgb)
}

/// One colour to pick: a dot, ringed when selected or hovered.
fn color_dot(ui: &mut egui::Ui, color: [u8; 3], selected: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::click());
    let palette = theme::palette(ui);
    let p = ui.painter();
    if selected {
        p.circle_stroke(rect.center(), 10.5, Stroke::new(2.0, palette.text_strong));
    } else if resp.hovered() {
        p.circle_stroke(rect.center(), 10.5, Stroke::new(1.0, palette.wash(90)));
    }
    p.circle_filled(
        rect.center(),
        8.0,
        Color32::from_rgb(color[0], color[1], color[2]),
    );
    p.circle_stroke(rect.center(), 8.0, Stroke::new(1.0, palette.wash(40)));
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// Dot radius and name shown for a size.
fn size_look(size: Size) -> (f32, &'static str) {
    match size {
        Size::Small => (2.5, "Thin"),
        Size::Medium => (4.0, "Medium"),
        Size::Large => (6.0, "Thick"),
    }
}

/// One size to pick: its dot and name.
fn size_option(ui: &mut egui::Ui, size: Size, selected: bool) -> egui::Response {
    let (radius, label) = size_look(size);
    let (rect, resp) = ui.allocate_exact_size(vec2(96.0, 28.0), Sense::click());
    let palette = theme::palette(ui);
    let p = ui.painter();
    if selected || resp.hovered() {
        p.rect_filled(rect, 6.0, palette.wash(if selected { 28 } else { 14 }));
    }
    p.circle_filled(pos2(rect.left() + 16.0, rect.center().y), radius, palette.icon);
    p.text(
        pos2(rect.left() + 32.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(13.0),
        palette.icon,
    );
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// A toolbar button that toggles `popup`, with a chevron to show it opens.
/// `paint` draws the current choice, in the given colour, in the space left
/// of the chevron. Disabled, it can't be opened (and closes if it was).
fn popup_button(
    ui: &mut egui::Ui,
    popup: Popup,
    enabled: bool,
    tip: &str,
    paint: impl FnOnce(&egui::Painter, Pos2, Color32),
) -> Rect {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(vec2(40.0, BUTTON), sense);
    let mut open = open_popup(ui.ctx()) == Some(popup);
    if open && !enabled {
        set_popup(ui.ctx(), None);
        open = false;
    }
    let palette = theme::palette(ui);
    let color = if enabled { palette.icon } else { palette.icon_disabled };
    let p = ui.painter();
    if open || (enabled && resp.hovered()) {
        let bg = palette.wash(if open { 28 } else { 14 });
        p.rect_filled(rect.shrink2(vec2(1.0, 3.0)), 6.0, bg);
    }
    paint(p, pos2(rect.left() + 14.0, rect.center().y), color);
    let c = pos2(rect.right() - 10.0, rect.center().y);
    p.add(Shape::line(
        vec![
            c + vec2(-3.5, -1.75),
            c + vec2(0.0, 1.75),
            c + vec2(3.5, -1.75),
        ],
        Stroke::new(1.5, color),
    ));
    let resp = resp.on_hover_text(tip);
    if enabled && resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
        set_popup(ui.ctx(), if open { None } else { Some(popup) });
    }
    rect
}

/// Draws the toolbar's buttons. Returns where the popup buttons are.
fn toolbar_contents(ui: &mut egui::Ui, view: &View, actions: &mut Vec<Action>) -> PopupButtons {
    for tool in Tool::ALL {
        let tip = match tool.alias() {
            Some(alias) => format!("{}  ({} or {alias})", tool.name(), tool.key()),
            None => format!("{}  ({})", tool.name(), tool.key()),
        };
        if icon_button(ui, Icon::Tool(tool), view.tool == tool, true, &tip).clicked() {
            actions.push(Action::Tool(tool));
        }
    }
    divider(ui);
    let palette = theme::palette(ui);
    let [r, g, b] = view.style.color_for(view.tool);
    let color = popup_button(ui, Popup::Color, true, crate::locale::spelled!("Colour"), |p, at, _| {
        p.circle_filled(at, 8.0, Color32::from_rgb(r, g, b));
        p.circle_stroke(at, 8.0, Stroke::new(1.0, palette.wash(60)));
    });
    let (radius, label) = size_look(view.style.size);
    let sized = uses_size(view.tool);
    let tip = if sized {
        format!("Thickness: {label}")
    } else if view.tool == Tool::Text {
        "Text size is in the tool settings".into()
    } else if view.tool == Tool::Step {
        "Step sizes are in the tool settings".into()
    } else {
        "This tool has no thickness".into()
    };
    let size = popup_button(ui, Popup::Size, sized, &tip, |p, at, color| {
        p.circle_filled(at, radius, color);
    });
    let has_options = has_options(view.tool);
    let tip = if has_options {
        "Tool settings"
    } else {
        "No settings for this tool"
    };
    let options = popup_button(ui, Popup::Options, has_options, tip, |p, at, color| {
        // Two sliders.
        for (dy, knob) in [(-3.5, -2.5), (3.5, 2.5)] {
            let y = at.y + dy;
            p.line_segment(
                [pos2(at.x - 7.0, y), pos2(at.x + 7.0, y)],
                Stroke::new(1.5, color),
            );
            p.circle_filled(pos2(at.x + knob, y), 2.6, palette.floating);
            p.circle_stroke(pos2(at.x + knob, y), 2.6, Stroke::new(1.5, color));
        }
    });
    divider(ui);
    if icon_button(ui, Icon::Undo, false, view.can_undo, "Undo  (Ctrl+Z)").clicked() {
        actions.push(Action::Undo);
    }
    if icon_button(ui, Icon::Redo, false, view.can_redo, "Redo  (Ctrl+Y)").clicked() {
        actions.push(Action::Redo);
    }
    PopupButtons {
        color,
        size,
        options,
    }
}

/// Whether the thickness setting changes what `tool` draws.
fn uses_size(tool: Tool) -> bool {
    !matches!(
        tool,
        Tool::Select
            | Tool::Pixelate
            | Tool::Image
            | Tool::Eraser
            | Tool::Clip
            | Tool::Text
            | Tool::Step
    )
}

/// Whether `tool` has settings of its own.
fn has_options(tool: Tool) -> bool {
    matches!(
        tool,
        Tool::Pixelate
            | Tool::Line
            | Tool::Arrow
            | Tool::Clip
            | Tool::Text
            | Tool::Highlighter
            | Tool::Step
    )
}

/// The current tool's name, over its settings.
fn options_title(tool: Tool) -> &'static str {
    match tool {
        Tool::Pixelate => "Pixelate",
        Tool::Line => "Line",
        Tool::Arrow => "Arrow",
        Tool::Clip => "Clip",
        Tool::Text => "Text",
        Tool::Highlighter => "Highlighter",
        Tool::Step => "Step",
        _ => "Settings",
    }
}

/// What puts `tool`'s settings back to how they start: nothing if they
/// already are.
fn reset_options(tool: Tool, style: &Style) -> Vec<Action> {
    let d = Style::default();
    let mut actions = Vec::new();
    match tool {
        Tool::Pixelate if style.pixelate != d.pixelate => actions.push(Action::Pixelate(d.pixelate)),
        Tool::Line | Tool::Arrow => {
            if tool == Tool::Arrow && style.arrow != d.arrow {
                actions.push(Action::Arrow(d.arrow));
            }
            if style.bends != d.bends {
                actions.push(Action::Bends(d.bends));
            }
        }
        Tool::Clip if style.clip_shadow != d.clip_shadow => {
            actions.push(Action::ClipShadow(d.clip_shadow))
        }
        Tool::Text if style.text != d.text => actions.push(Action::Text(d.text)),
        Tool::Highlighter => {
            if style.highlighter != d.highlighter {
                actions.push(Action::Highlighter(d.highlighter));
            }
            if style.highlighter_ink != d.highlighter_ink {
                actions.push(Action::HighlighterInk(d.highlighter_ink));
            }
        }
        Tool::Step if style.step != d.step => actions.push(Action::Step(d.step)),
        _ => {}
    }
    actions
}

/// The current tool's settings, in the options popup, under its name and
/// a button to reset them.
fn tool_options(ui: &mut egui::Ui, view: &View, actions: &mut Vec<Action>) {
    // As wide as the settings under it, with the button at the right.
    let (header, _) = ui.allocate_exact_size(vec2(230.0, 24.0), Sense::hover());
    ui.painter().text(
        header.left_center(),
        Align2::LEFT_CENTER,
        options_title(view.tool),
        FontId::proportional(13.0),
        theme::palette(ui).icon,
    );
    let reset = reset_options(view.tool, &view.style);
    let button = Rect::from_min_size(header.right_top() - vec2(24.0, 0.0), vec2(24.0, 24.0));
    if reset_button(ui, button, !reset.is_empty()).clicked() {
        actions.extend(reset);
    }
    match view.tool {
        Tool::Pixelate => {
            let opts = view.style.pixelate;
            option_heading(ui, "Pixel size");
            ui.horizontal(|ui| {
                for block in PIXEL_BLOCKS {
                    let label = format!("{block} px");
                    if option(ui, &label, 56.0, opts.block == block, true).clicked() {
                        actions.push(Action::Pixelate(PixelateOptions { block, ..opts }));
                    }
                }
            });
            option_heading(ui, "Pixelation");
            for (secure, label, tip) in [
                (
                    false,
                    "Average of what\u{2019}s behind",
                    crate::locale::spelled!("Each block is the average colour of the pixels under it"),
                ),
                (
                    true,
                    "Secure: shuffled noise",
                    crate::locale::spelled!(
                        "The blocks\u{2019} colours are shuffled and noised, so nothing \
                         under them can be recovered"
                    ),
                ),
            ] {
                if option(ui, label, 230.0, opts.secure == secure, true)
                    .on_hover_text(tip)
                    .clicked()
                {
                    actions.push(Action::Pixelate(PixelateOptions { secure, ..opts }));
                }
            }
        }
        Tool::Arrow => {
            let opts = view.style.arrow;
            option_heading(ui, "Heads");
            ui.horizontal(|ui| {
                for (double, label) in [(false, "One"), (true, "Two")] {
                    if option(ui, label, 114.0, opts.double == double, true).clicked() {
                        actions.push(Action::Arrow(ArrowOptions { double, ..opts }));
                    }
                }
            });
            option_heading(ui, "Point the head at");
            for (head_at_start, label) in [
                (false, "Where you let go"),
                (true, "Where you start dragging"),
            ] {
                let selected = !opts.double && opts.head_at_start == head_at_start;
                if option(ui, label, 230.0, selected, !opts.double).clicked() {
                    actions.push(Action::Arrow(ArrowOptions {
                        head_at_start,
                        ..opts
                    }));
                }
            }
            bends_options(ui, view.style.bends, actions);
        }
        Tool::Line => bends_options(ui, view.style.bends, actions),
        Tool::Text => {
            let opts = view.style.text;
            let fonts = crate::fonts::installed();
            let current = opts
                .font
                .and_then(|i| fonts?.get(i as usize))
                .map_or("Default (Ubuntu)", |f| f.family.as_str());
            // The popup is as wide as its other settings; long names wrap.
            ui.set_max_width(230.0);
            option_heading(ui, &format!("Font: {current}"));
            let id = egui::Id::new("font-search");
            let mut search: String = ui.data(|d| d.get_temp(id)).unwrap_or_default();
            let field = ui.add(
                egui::TextEdit::singleline(&mut search)
                    .hint_text("Search fonts")
                    .desired_width(230.0),
            );
            if field.changed() {
                ui.data_mut(|d| d.insert_temp(id, search.clone()));
            }
            let query = search.trim().to_lowercase();
            let matches = |name: &str| name.to_lowercase().contains(&query);
            // Enter in the search picks the first font found.
            let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let mut first = None;
            ui.add_space(2.0);
            egui::ScrollArea::vertical()
                .max_height(180.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.set_width(230.0);
                    if matches("Default (Ubuntu)") {
                        first = Some(None);
                        if option(ui, "Default (Ubuntu)", 230.0, opts.font.is_none(), true).clicked() {
                            actions.push(Action::Text(TextOptions { font: None, ..opts }));
                        }
                    }
                    let Some(fonts) = fonts else {
                        ui.label(egui::RichText::new("Finding fonts\u{2026}").weak());
                        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
                        return;
                    };
                    for (i, font) in fonts.iter().enumerate() {
                        if !matches(&font.family) {
                            continue;
                        }
                        let i = Some(i as u16);
                        first.get_or_insert(i);
                        let selected = opts.font == i;
                        if option(ui, &font.family, 230.0, selected, true).clicked() {
                            actions.push(Action::Text(TextOptions { font: i, ..opts }));
                        }
                    }
                    if first.is_none() {
                        ui.label(egui::RichText::new("No fonts found").weak());
                    }
                });
            if enter && let Some(font) = first {
                actions.push(Action::Text(TextOptions { font, ..opts }));
            }
            option_heading(ui, "Size");
            let mut size = opts.size;
            if px_slider(ui, &mut size, TEXT_SIZES).changed() {
                actions.push(Action::Text(TextOptions { size, ..opts }));
            }
            option_heading(ui, "Outline");
            ui.horizontal(|ui| {
                for (outline, label) in [(true, "On"), (false, "Off")] {
                    if option(ui, label, 114.0, opts.outline == outline, true)
                        .on_hover_text(crate::locale::spelled!(
                            "A black or white edge, whichever stands out from the colour"
                        ))
                        .clicked()
                    {
                        actions.push(Action::Text(TextOptions { outline, ..opts }));
                    }
                }
            });
        }
        Tool::Highlighter => {
            option_heading(ui, "Shape");
            ui.horizontal(|ui| {
                for (shape, label, tip) in [
                    (HighlighterShape::Freehand, "Freehand", "Follows the mouse"),
                    (
                        HighlighterShape::Straight,
                        "Straight",
                        "A straight stroke; Shift snaps it to 45\u{b0}",
                    ),
                    (HighlighterShape::Box, "Box", "Fills the dragged area; Shift makes a square"),
                ] {
                    if option(ui, label, 76.0, view.style.highlighter == shape, true)
                        .on_hover_text(tip)
                        .clicked()
                    {
                        actions.push(Action::Highlighter(shape));
                    }
                }
            });
            option_heading(ui, "Ink");
            ui.horizontal(|ui| {
                for (ink, label, tip) in [
                    (
                        HighlighterInk::Realistic,
                        "Realistic",
                        "Like a real marker: light areas take the colour, dark ones stay dark",
                    ),
                    (HighlighterInk::Tint, "Tint", "A see-through wash of the colour"),
                ] {
                    if option(ui, label, 114.0, view.style.highlighter_ink == ink, true)
                        .on_hover_text(tip)
                        .clicked()
                    {
                        actions.push(Action::HighlighterInk(ink));
                    }
                }
            });
        }
        Tool::Clip => {
            option_heading(ui, "Drop shadow");
            ui.horizontal(|ui| {
                for (shadow, label) in [(true, "On"), (false, "Off")] {
                    let selected = view.style.clip_shadow == shadow;
                    if option(ui, label, 114.0, selected, true).clicked() {
                        actions.push(Action::ClipShadow(shadow));
                    }
                }
            });
        }
        Tool::Step => {
            let opts = view.style.step;
            option_heading(ui, "Circle size");
            let mut size = opts.size;
            if px_slider(ui, &mut size, STEP_SIZES).changed() {
                // A smaller circle takes the number down with it.
                let opts = StepOptions { size, ..opts };
                let number = opts.number.min(*opts.number_sizes().end());
                actions.push(Action::Step(StepOptions { number, ..opts }));
            }
            option_heading(ui, "Number size");
            let mut number = opts.number;
            if px_slider(ui, &mut number, opts.number_sizes())
                .on_hover_text("How tall the digits are, up to what fits in the circle")
                .changed()
            {
                actions.push(Action::Step(StepOptions { number, ..opts }));
            }
            option_heading(ui, "Border");
            ui.horizontal(|ui| {
                for (border, label) in [(true, "On"), (false, "Off")] {
                    if option(ui, label, 114.0, opts.border == border, true)
                        .on_hover_text(crate::locale::spelled!(
                            "A thin white edge, so steps stand out on their own colour"
                        ))
                        .clicked()
                    {
                        actions.push(Action::Step(StepOptions { border, ..opts }));
                    }
                }
            });
        }
        _ => {}
    }
}

/// How many nodes lines and arrows get to bend them by.
fn bends_options(ui: &mut egui::Ui, current: u8, actions: &mut Vec<Action>) {
    option_heading(ui, "Bend handles");
    ui.horizontal(|ui| {
        for bends in BENDS {
            if option(ui, &bends.to_string(), 44.0, current == bends, true)
                .on_hover_text("Nodes along it to drag, to curve it through them")
                .clicked()
            {
                actions.push(Action::Bends(bends));
            }
        }
    });
}

/// A small heading over a group of settings.
fn option_heading(ui: &mut egui::Ui, text: &str) {
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(text)
            .size(11.5)
            .color(theme::palette(ui).text_weak),
    );
    ui.add_space(1.0);
}

/// A size in pixels to pick by dragging along a track, with the size shown
/// beside it. As wide as two choices side by side.
fn px_slider(
    ui: &mut egui::Ui,
    value: &mut u16,
    range: std::ops::RangeInclusive<u16>,
) -> egui::Response {
    const LABEL: f32 = 50.0;
    const KNOB: f32 = 7.0;
    let (rect, mut resp) = ui.allocate_exact_size(vec2(230.0, 28.0), Sense::click_and_drag());
    let track = Rect::from_min_max(
        pos2(rect.left() + KNOB + 2.0, rect.center().y - 2.0),
        pos2(rect.right() - LABEL - KNOB - 6.0, rect.center().y + 2.0),
    );
    let (lo, hi) = (*range.start() as f32, *range.end() as f32);
    if let Some(at) = resp.interact_pointer_pos() {
        let t = ((at.x - track.left()) / track.width()).clamp(0.0, 1.0);
        let picked = (lo + t * (hi - lo)).round() as u16;
        if picked != *value {
            *value = picked;
            resp.mark_changed();
        }
    }
    let t = if hi > lo {
        ((*value as f32 - lo) / (hi - lo)).clamp(0.0, 1.0)
    } else {
        1.0
    };
    let knob = pos2(track.left() + t * track.width(), track.center().y);
    let palette = theme::palette(ui);
    let p = ui.painter();
    p.rect_filled(track, 2.0, palette.wash(30));
    p.rect_filled(
        Rect::from_min_max(track.min, pos2(knob.x, track.max.y)),
        2.0,
        ACCENT,
    );
    let active = resp.hovered() || resp.dragged();
    p.circle_filled(knob + vec2(0.0, 1.0), KNOB + 1.0, Color32::from_black_alpha(80));
    p.circle_filled(knob, if active { KNOB + 1.0 } else { KNOB }, Color32::WHITE);
    let label = Rect::from_min_max(
        pos2(rect.right() - LABEL, rect.top() + 2.0),
        pos2(rect.right(), rect.bottom() - 2.0),
    );
    p.rect_filled(label, 6.0, palette.wash(14));
    p.text(
        label.center(),
        Align2::CENTER_CENTER,
        format!("{value} px"),
        FontId::proportional(12.5),
        palette.icon,
    );
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// A small button at `rect` putting a tool's settings back to how they
/// start: a grey circling arrow, lighter when hovered and dimmed when
/// there's nothing to reset.
fn reset_button(ui: &mut egui::Ui, rect: Rect, enabled: bool) -> egui::Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let resp = ui.interact(rect, egui::Id::new("reset-tool-options"), sense);
    let palette = theme::palette(ui);
    let p = ui.painter();
    let color = if !enabled {
        palette.icon_disabled
    } else if resp.hovered() {
        p.rect_filled(rect, 6.0, palette.wash(14));
        palette.icon
    } else {
        palette.text_weak
    };
    // Most of a circle, open at the top left, the arrow on its end
    // pointing back round anticlockwise.
    let (c, rad) = (rect.center(), 6.0);
    let angle = |i: usize| (-100.0f32 + 300.0 * i as f32 / 24.0).to_radians();
    let pts: Vec<Pos2> = (0..=24)
        .map(|i| c + vec2(angle(i).cos(), angle(i).sin()) * rad)
        .collect();
    let end = pts[0];
    p.add(Shape::line(pts, Stroke::new(1.6, color)));
    p.add(Shape::convex_polygon(
        vec![end + vec2(-4.0, -0.8), end + vec2(1.6, -3.6), end + vec2(1.2, 2.6)],
        color,
        Stroke::NONE,
    ));
    let resp = resp.on_hover_text(if enabled {
        "Reset to defaults"
    } else {
        "These are the defaults"
    });
    if enabled {
        resp.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        resp
    }
}

/// One choice in the settings popup: its label, highlighted when selected.
fn option(
    ui: &mut egui::Ui,
    label: &str,
    width: f32,
    selected: bool,
    enabled: bool,
) -> egui::Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 28.0), sense);
    let palette = theme::palette(ui);
    let p = ui.painter();
    if selected {
        p.rect_filled(rect, 6.0, ACCENT);
    } else if enabled && resp.hovered() {
        p.rect_filled(rect, 6.0, palette.wash(14));
    }
    let color = if !enabled {
        palette.icon_disabled
    } else if selected {
        Color32::WHITE
    } else {
        palette.icon
    };
    p.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(13.0),
        color,
    );
    if enabled {
        resp.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        resp
    }
}

fn divider(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(11.0, BUTTON), Sense::hover());
    ui.painter().line_segment(
        [
            pos2(rect.center().x, rect.top() + 7.0),
            pos2(rect.center().x, rect.bottom() - 7.0),
        ],
        Stroke::new(1.0, theme::palette(ui).stroke),
    );
}

#[derive(Clone, Copy)]
enum Icon {
    Tool(Tool),
    Undo,
    Redo,
}

fn icon_button(
    ui: &mut egui::Ui,
    icon: Icon,
    selected: bool,
    enabled: bool,
    tip: &str,
) -> egui::Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(BUTTON), sense);
    let palette = theme::palette(ui);
    let p = ui.painter();
    let color = if !enabled {
        palette.icon_disabled
    } else if selected {
        Color32::WHITE
    } else {
        palette.icon
    };
    let hover = palette.wash(22);
    let bg = if selected {
        p.rect_filled(rect, 7.0, ACCENT);
        ACCENT
    } else if enabled && resp.hovered() {
        p.rect_filled(rect, 7.0, hover);
        palette.floating.blend(hover)
    } else {
        palette.floating
    };
    draw_icon(p, rect.shrink(8.0), icon, color, bg);
    let resp = resp.on_hover_text(tip);
    if enabled {
        resp.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        resp
    }
}

/// `bg` is the colour behind the icon, for parts that cut into others.
fn draw_icon(p: &egui::Painter, r: Rect, icon: Icon, color: Color32, bg: Color32) {
    let s = Stroke::new(1.7, color);
    let at = |x: f32, y: f32| pos2(r.left() + r.width() * x, r.top() + r.height() * y);
    match icon {
        Icon::Tool(Tool::Select) => {
            // Region: a dashed selection box with solid corners.
            let b = Rect::from_min_max(at(0.05, 0.12), at(0.95, 0.88));
            let dashed = Shape::dashed_line(
                &[
                    b.left_top(),
                    b.right_top(),
                    b.right_bottom(),
                    b.left_bottom(),
                    b.left_top(),
                ],
                Stroke::new(1.2, color),
                2.5,
                2.5,
            );
            p.extend(dashed);
            let arm = r.width() * 0.28;
            let corner = Stroke::new(2.2, color);
            for (c, dx, dy) in [
                (b.left_top(), 1.0, 1.0),
                (b.right_top(), -1.0, 1.0),
                (b.right_bottom(), -1.0, -1.0),
                (b.left_bottom(), 1.0, -1.0),
            ] {
                p.add(Shape::line(
                    vec![c + vec2(0.0, dy * arm), c, c + vec2(dx * arm, 0.0)],
                    corner,
                ));
            }
        }
        Icon::Tool(Tool::Pen) => {
            let pts: Vec<Pos2> = (0..=24)
                .map(|i| {
                    let t = i as f32 / 24.0;
                    at(
                        0.05 + 0.9 * t,
                        0.55 - 0.3 * (t * std::f32::consts::TAU * 1.2).sin() * (1.0 - t * 0.3),
                    )
                })
                .collect();
            p.add(Shape::line(pts, Stroke::new(2.0, color)));
        }
        Icon::Tool(Tool::Line) => {
            p.line_segment([at(0.1, 0.9), at(0.9, 0.1)], Stroke::new(2.0, color));
        }
        Icon::Tool(Tool::Arrow) => {
            p.line_segment([at(0.1, 0.9), at(0.78, 0.22)], Stroke::new(2.0, color));
            p.add(Shape::convex_polygon(
                vec![at(0.95, 0.05), at(0.5, 0.18), at(0.82, 0.5)],
                color,
                Stroke::NONE,
            ));
        }
        Icon::Tool(Tool::Rect) => {
            p.rect_stroke(
                Rect::from_min_max(at(0.08, 0.18), at(0.92, 0.82)),
                1.0,
                s,
                StrokeKind::Middle,
            );
        }
        Icon::Tool(Tool::Ellipse) => {
            p.add(Shape::ellipse_stroke(
                r.center(),
                vec2(r.width() * 0.45, r.height() * 0.36),
                s,
            ));
        }
        Icon::Tool(Tool::Highlighter) => {
            p.line_segment(
                [at(0.1, 0.62), at(0.9, 0.62)],
                Stroke::new(7.0, Color32::from_rgba_unmultiplied(255, 214, 10, 150)),
            );
            p.add(Shape::convex_polygon(
                vec![
                    at(0.55, 0.45),
                    at(0.85, 0.05),
                    at(1.0, 0.18),
                    at(0.72, 0.55),
                ],
                color,
                Stroke::NONE,
            ));
        }
        Icon::Tool(Tool::Blur) => {
            for (i, alpha) in [(0, 50u8), (1, 110), (2, 255)] {
                let rad = r.width() * (0.48 - 0.13 * i as f32);
                let c = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha);
                p.circle_filled(r.center(), rad, c);
            }
        }
        Icon::Tool(Tool::Text) => {
            // A capital T.
            p.line_segment([at(0.15, 0.18), at(0.85, 0.18)], Stroke::new(2.2, color));
            p.line_segment([at(0.5, 0.18), at(0.5, 0.86)], Stroke::new(2.2, color));
        }
        Icon::Tool(Tool::Step) => {
            // A filled marker with a 1 cut out of it, like the ones it places.
            // Measured in sixteenths of the icon.
            let u = r.width() / 16.0;
            p.circle_filled(r.center(), 7.6 * u, color);
            let one = vec![
                at(6.1 / 16.0, 5.7 / 16.0),
                at(8.6 / 16.0, 4.1 / 16.0),
                at(8.6 / 16.0, 11.9 / 16.0),
            ];
            let width = 1.9 * u;
            // Round the ends, as egui's lines stop square.
            for end in [one[0], one[2]] {
                p.circle_filled(end, width / 2.0, bg);
            }
            p.add(Shape::line(one, Stroke::new(width, bg)));
        }
        Icon::Tool(Tool::Eraser) => {
            // A tilted eraser, rubber end down-left, with a sparkle cut into it.
            // Measured in sixteenths of the icon.
            let u = r.width() / 16.0;
            let centre = at(0.6, 0.5875);
            let (along, across) = (vec2(1.0, -1.0).normalized(), vec2(1.0, 1.0).normalized());
            let place = |x: f32, y: f32| centre + (along * x + across * y) * u;
            // The eraser from `x0` to `x1` along its length, corners rounded by `rad`.
            let body = |x0: f32, x1: f32, rad: f32| -> Vec<Pos2> {
                let h = 3.3;
                let mut pts = Vec::new();
                for (cx, cy, start) in [
                    (x1 - rad, h - rad, 0.0f32),
                    (x0 + rad, h - rad, 90.0),
                    (x0 + rad, -h + rad, 180.0),
                    (x1 - rad, -h + rad, 270.0),
                ] {
                    for i in 0..=4 {
                        let a = (start + 22.5 * i as f32).to_radians();
                        pts.push(place(cx + rad * a.cos(), cy + rad * a.sin()));
                    }
                }
                pts
            };
            p.add(Shape::convex_polygon(body(-6.25, -1.25, 1.0), color, Stroke::NONE));
            p.add(Shape::closed_line(body(-6.25, 6.25, 1.2), Stroke::new(1.5, color)));

            // Four points joined by curves that bow in towards the middle.
            let mid = at(0.325, 0.325);
            let rad = 5.2 * u;
            let tips = [vec2(0.0, -rad), vec2(rad, 0.0), vec2(0.0, rad), vec2(-rad, 0.0)];
            let q = rad * 0.12;
            let bends = [vec2(q, -q), vec2(q, q), vec2(-q, q), vec2(-q, -q)];
            let mut sparkle = Vec::new();
            for i in 0..4 {
                let (a, b, bend) = (tips[i], tips[(i + 1) % 4], bends[i]);
                for s in 0..8 {
                    let t = s as f32 / 8.0;
                    let off = a * (1.0 - t) * (1.0 - t) + bend * 2.0 * t * (1.0 - t) + b * t * t;
                    sparkle.push(mid + off);
                }
            }
            // Not convex, so filled as a fan from the middle, with an edge
            // stroke to smooth it. The first pass, wider and in the
            // background colour, cuts the gap into the eraser.
            for (fill, edge) in [(bg, 2.6), (color, 0.8)] {
                let mut mesh = egui::Mesh::default();
                mesh.colored_vertex(mid, fill);
                for &pt in &sparkle {
                    mesh.colored_vertex(pt, fill);
                }
                let n = sparkle.len() as u32;
                for i in 0..n {
                    mesh.add_triangle(0, 1 + i, 1 + (i + 1) % n);
                }
                p.add(mesh);
                p.add(Shape::closed_line(sparkle.clone(), Stroke::new(edge, fill)));
            }
        }
        Icon::Tool(Tool::Clip) => {
            // A dashed area with a copy of it lifted off, down and right.
            let back = Rect::from_min_max(at(0.0, 0.0), at(0.62, 0.62));
            let dashed = Shape::dashed_line(
                &[
                    back.left_top(),
                    back.right_top(),
                    back.right_bottom(),
                    back.left_bottom(),
                    back.left_top(),
                ],
                Stroke::new(1.2, color),
                2.5,
                2.0,
            );
            p.extend(dashed);
            let front = Rect::from_min_max(at(0.36, 0.36), at(1.0, 1.0));
            p.rect_filled(front, 1.5, bg);
            p.rect_stroke(front, 1.5, Stroke::new(1.7, color), StrokeKind::Inside);
        }
        Icon::Tool(Tool::Pixelate) => {
            let n = 4;
            let cell = r.width() / n as f32;
            for y in 0..n {
                for x in 0..n {
                    let alpha = [255u8, 90, 170, 40][(x + y * 3) % 4];
                    let c = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha);
                    let min = r.min + vec2(x as f32 * cell, y as f32 * cell);
                    p.rect_filled(Rect::from_min_size(min, Vec2::splat(cell - 1.0)), 0.5, c);
                }
            }
        }
        Icon::Tool(Tool::Image) => {
            // A picture: a frame with a sun and a mountain.
            let frame = Rect::from_min_max(at(0.0, 0.1), at(1.0, 0.9));
            p.rect_stroke(frame, 2.0, Stroke::new(1.7, color), StrokeKind::Inside);
            p.circle_filled(at(0.7, 0.33), r.width() * 0.1, color);
            p.add(Shape::convex_polygon(
                vec![at(0.12, 0.78), at(0.4, 0.42), at(0.68, 0.78)],
                color,
                Stroke::NONE,
            ));
            p.add(Shape::convex_polygon(
                vec![at(0.5, 0.78), at(0.7, 0.55), at(0.88, 0.78)],
                color.gamma_multiply(0.6),
                Stroke::NONE,
            ));
        }
        Icon::Undo | Icon::Redo => {
            // An arc over the top, ending in a downward arrowhead: like a
            // counter-clockwise arrow for undo, mirrored for redo.
            let flip = if matches!(icon, Icon::Redo) {
                -1.0
            } else {
                1.0
            };
            let c = pos2(r.center().x, r.center().y + r.height() * 0.12);
            let rad = r.width() * 0.4;
            let pts: Vec<Pos2> = (0..=24)
                .map(|i| {
                    let a = 0.5 - (std::f32::consts::PI + 0.5) * i as f32 / 24.0;
                    pos2(c.x + flip * rad * a.cos(), c.y + rad * a.sin())
                })
                .collect();
            let end = *pts.last().expect("non-empty");
            p.add(Shape::line(pts, s));
            p.add(Shape::convex_polygon(
                vec![
                    end + vec2(0.0, 5.5),
                    end + vec2(-4.5, -1.5),
                    end + vec2(4.5, -1.5),
                ],
                color,
                Stroke::NONE,
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Renders the toolbar offscreen to `target/toolbar-preview.png` for a
    /// visual check: `cargo test toolbar_preview -- --ignored`.
    #[test]
    fn reset_puts_back_only_that_tools_settings() {
        let d = Style::default();
        assert!(reset_options(Tool::Step, &d).is_empty());
        let mut style = d;
        style.step.size = 60;
        style.step.border = false;
        style.arrow.double = true;
        style.highlighter_ink = HighlighterInk::Tint;
        assert!(matches!(
            reset_options(Tool::Step, &style)[..],
            [Action::Step(s)] if s == d.step
        ));
        assert!(matches!(
            reset_options(Tool::Highlighter, &style)[..],
            [Action::HighlighterInk(HighlighterInk::Realistic)]
        ));
        assert_eq!(reset_options(Tool::Arrow, &style).len(), 1);
        assert!(reset_options(Tool::Clip, &style).is_empty());
    }

    #[test]
    #[ignore]
    fn toolbar_preview() {
        let view = View {
            tool: Tool::Select,
            style: Style::default(),
            can_undo: true,
            can_redo: false,
            show_toolbar: true,
            top_inset: 0.0,
            selection: Some((
                Rect::from_min_size(pos2(120.0, 90.0), vec2(500.0, 60.0)),
                [750, 90],
            )),
            cursor: CursorIcon::Crosshair,
            arrow_nodes: vec![pos2(900.0, 200.0)],
            hovered: None,
            hint: None,
            cursor_info: None,
        };
        for (name, dark) in [("toolbar-preview", true), ("toolbar-light-preview", false)] {
            let mut laid_out = false;
            crate::preview::render(name, [1200, 260], 1.5, |root| {
                crate::preview::themed(root, dark);
                laid_out |= ui(root, &view).1.is_some();
            });
            assert!(laid_out, "toolbar should be laid out");
        }
    }

    /// Renders the toolbar with each tool's settings popup open, to
    /// `target/options-preview-*.png`: `cargo test options_preview -- --ignored`.
    #[test]
    #[ignore]
    fn options_preview() {
        // The text tool's font list, once it's been found.
        while crate::fonts::installed().is_none() {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        for tool in [
            Tool::Pixelate,
            Tool::Line,
            Tool::Arrow,
            Tool::Clip,
            Tool::Text,
            Tool::Highlighter,
            Tool::Step,
        ] {
            let mut style = Style::default();
            style.arrow.double = true;
            style.bends = 3;
            style.step.size = 44;
            let view = View {
                tool,
                style,
                can_undo: true,
                can_redo: false,
                show_toolbar: true,
                top_inset: 0.0,
                selection: None,
                cursor: CursorIcon::Crosshair,
                arrow_nodes: Vec::new(),
                hovered: None,
                hint: None,
                cursor_info: None,
            };
            for dark in [true, false] {
                let theme = if dark { "" } else { "-light" };
                let name = format!("options-preview-{tool:?}{theme}").to_lowercase();
                crate::preview::render(&name, [1200, 560], 1.5, |root| {
                    crate::preview::themed(root, dark);
                    set_popup(root.ctx(), Some(Popup::Options));
                    ui(root, &view);
                });
            }
            if tool == Tool::Text {
                crate::preview::render("options-preview-font-search", [2400, 560], 1.5, |root| {
                    crate::preview::themed(root, true);
                    set_popup(root.ctx(), Some(Popup::Options));
                    root.ctx()
                        .data_mut(|d| d.insert_temp(egui::Id::new("font-search"), "seg".to_string()));
                    ui(root, &view);
                });
            }
        }
        let view = View {
            tool: Tool::Pen,
            style: Style::default(),
            can_undo: true,
            can_redo: false,
            show_toolbar: true,
            top_inset: 0.0,
            selection: None,
            cursor: CursorIcon::Crosshair,
            arrow_nodes: Vec::new(),
            hovered: None,
            hint: None,
            cursor_info: None,
        };
        for (name, dark) in [("options-preview-colour", true), ("options-preview-colour-light", false)] {
            crate::preview::render(name, [1200, 560], 1.5, |root| {
                crate::preview::themed(root, dark);
                set_popup(root.ctx(), Some(Popup::Color));
                ui(root, &view);
            });
        }
    }

    /// Renders the colour picker's hint and loupe, near the bottom-right
    /// corner so it flips, to `target/loupe-preview.png`:
    /// `cargo test loupe_preview -- --ignored`.
    #[test]
    #[ignore]
    fn loupe_preview() {
        let n = crate::session::MAGNIFIER_PIXELS;
        let pixels = image::RgbaImage::from_fn(n, n, |x, y| {
            if x + 3 < y {
                image::Rgba([0, 0, 0, 0]) // off screen
            } else if (x / 3 + y / 3) % 2 == 0 {
                image::Rgba([0x3d, 0x9b, 0xff, 255])
            } else {
                image::Rgba([240, 240, 240, 255])
            }
        });
        let view = View {
            tool: Tool::Select,
            style: Style::default(),
            can_undo: false,
            can_redo: false,
            show_toolbar: false,
            top_inset: 0.0,
            selection: None,
            cursor: CursorIcon::Crosshair,
            arrow_nodes: Vec::new(),
            hovered: None,
            hint: Some("Click to pick a colour \u{2014} arrow keys move one pixel, scroll zooms"),
            cursor_info: Some((
                pos2(560.0, 300.0),
                CursorInfo {
                    magnifier: Some(pixels),
                    fixed_cells: false,
                    color: Some([0x3d, 0x9b, 0xff]),
                    position: Some([1847, -212]),
                },
            )),
        };
        crate::preview::render("loupe-preview", [640, 360], 1.0, |root| {
            root.painter()
                .rect_filled(root.max_rect(), 0.0, Color32::from_rgb(200, 120, 60));
            ui(root, &view);
        });
    }

    /// Renders the crosshair info without the magnifier, to
    /// `target/crosshair-info-preview.png`:
    /// `cargo test crosshair_info_preview -- --ignored`.
    #[test]
    #[ignore]
    fn crosshair_info_preview() {
        let view = View {
            tool: Tool::Select,
            style: Style::default(),
            can_undo: false,
            can_redo: false,
            show_toolbar: false,
            top_inset: 0.0,
            selection: None,
            cursor: CursorIcon::Crosshair,
            arrow_nodes: Vec::new(),
            hovered: None,
            hint: None,
            cursor_info: Some((
                pos2(60.0, 30.0),
                CursorInfo {
                    magnifier: None,
                    fixed_cells: false,
                    color: Some([200, 120, 60]),
                    position: Some([640, 1080]),
                },
            )),
        };
        crate::preview::render("crosshair-info-preview", [600, 240], 1.5, |root| {
            root.painter()
                .rect_filled(root.max_rect(), 0.0, Color32::from_rgb(200, 120, 60));
            ui(root, &view);
        });
    }
}
