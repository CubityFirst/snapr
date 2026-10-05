//! The annotation toolbar and on-screen selection decorations, drawn with egui
//! over the overlay. Icons are painted as vector shapes.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Pos2, Rect, Sense, Shape, Stroke,
    StrokeKind, Vec2, pos2, vec2,
};

use crate::annotate::{COLORS, Size, Style, Tool};

const ACCENT: Color32 = Color32::from_rgb(0x3d, 0x9b, 0xff);
const PANEL: Color32 = Color32::from_rgba_premultiplied(28, 29, 32, 245);
const PANEL_STROKE: Color32 = Color32::from_rgb(58, 60, 66);
const ICON: Color32 = Color32::from_rgb(222, 223, 228);
const ICON_DISABLED: Color32 = Color32::from_rgb(96, 98, 104);
const BUTTON: f32 = 32.0;
/// Colours per row in the colour popup.
const PALETTE_COLUMNS: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    Tool(Tool),
    Color([u8; 3]),
    Size(Size),
    Undo,
    Redo,
}

/// Everything the overlay UI needs for one frame, in window points.
pub struct View {
    pub tool: Tool,
    pub style: Style,
    pub can_undo: bool,
    pub can_redo: bool,
    pub show_toolbar: bool,
    /// The selection on this monitor, and its size in pixels.
    pub selection: Option<(Rect, [u32; 2])>,
    pub cursor: CursorIcon,
    /// Handles for curving arrows, in points.
    pub arrow_nodes: Vec<egui::Pos2>,
    /// What to do, shown at the top of the screen.
    pub hint: Option<&'static str>,
    /// The colour picker's magnifier: the cursor, and the screen pixels
    /// around it (an odd-sided square centred on it).
    pub loupe: Option<(egui::Pos2, image::RgbaImage)>,
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
        let top = ctx.content_rect().center_top() + vec2(0.0, 14.0);
        let bg = Rect::from_min_size(top - vec2(size.x / 2.0, 0.0), size);
        painter.rect_filled(bg, 8.0, Color32::from_black_alpha(190));
        painter.galley(bg.min + vec2(12.0, 7.0), galley, Color32::WHITE);
    }

    if let Some((at, pixels)) = &view.loupe {
        draw_loupe(ui.painter(), *at, pixels);
    }

    for &node in &view.arrow_nodes {
        let painter = ui.painter();
        painter.circle_filled(node, 5.0, Color32::WHITE);
        painter.circle_stroke(node, 5.0, Stroke::new(1.5, Color32::from_black_alpha(200)));
    }

    if !view.show_toolbar {
        return (actions, None);
    }
    let area = egui::Area::new(egui::Id::new("toolbar"))
        .anchor(Align2::CENTER_TOP, vec2(0.0, 14.0))
        .order(egui::Order::Foreground)
        .fade_in(false)
        .show(&ctx, |ui| {
            egui::Frame::new()
                .fill(PANEL)
                .stroke(Stroke::new(1.0, PANEL_STROKE))
                .corner_radius(CornerRadius::same(10))
                .inner_margin(6)
                .shadow(egui::Shadow {
                    offset: [0, 4],
                    blur: 16,
                    spread: 0,
                    color: Color32::from_black_alpha(110),
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

/// The colour picker's magnifier beside the cursor: the pixels around it
/// enlarged, the middle one outlined, and its colour underneath.
fn draw_loupe(painter: &egui::Painter, at: egui::Pos2, pixels: &image::RgbaImage) {
    const CELL: f32 = 9.0;
    const GAP: f32 = 22.0;
    let n = pixels.width();
    let side = n as f32 * CELL;
    let [r, g, b, _] = pixels.get_pixel(n / 2, n / 2).0;
    let hex = format!("#{r:02X}{g:02X}{b:02X}");
    let label = painter.layout_no_wrap(
        format!("{hex}   {r}, {g}, {b}"),
        FontId::monospace(12.0),
        Color32::WHITE,
    );
    let label_h = label.size().y + 10.0;
    let size = vec2(side.max(label.size().x + 34.0), side + label_h);
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
    let grid = Rect::from_min_size(
        pos2(card.center().x - side / 2.0, card.top()),
        vec2(side, side),
    );
    for (x, y, p) in pixels.enumerate_pixels() {
        let [r, g, b, a] = p.0;
        let color = if a == 0 {
            Color32::from_gray(30) // off screen
        } else {
            Color32::from_rgb(r, g, b)
        };
        let cell = Rect::from_min_size(
            grid.min + vec2(x as f32 * CELL, y as f32 * CELL),
            vec2(CELL, CELL),
        );
        painter.rect_filled(cell, 0.0, color);
    }
    let middle = Rect::from_min_size(
        grid.min + vec2((n / 2) as f32 * CELL, (n / 2) as f32 * CELL),
        vec2(CELL, CELL),
    );
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
    // The colour's swatch and value.
    let row = Rect::from_min_max(pos2(card.left(), grid.bottom()), card.max);
    let swatch = Rect::from_center_size(pos2(row.left() + 14.0, row.center().y), vec2(14.0, 14.0));
    painter.rect_filled(swatch, 3.0, Color32::from_rgb(r, g, b));
    painter.rect_stroke(
        swatch,
        3.0,
        Stroke::new(1.0, Color32::from_white_alpha(140)),
        StrokeKind::Inside,
    );
    painter.galley(
        pos2(swatch.right() + 8.0, row.center().y - label.size().y / 2.0),
        label,
        Color32::WHITE,
    );
}

/// A toolbar button that opens a popup of choices underneath it.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Popup {
    Color,
    Size,
}

/// Where the popup buttons are, for hanging their popups from.
struct PopupButtons {
    color: Rect,
    size: Rect,
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
    };
    let mut picked = false;
    let area = egui::Area::new(egui::Id::new("toolbar-popup-area"))
        .fixed_pos(button.left_bottom() + vec2(-6.0, 10.0))
        .order(egui::Order::Foreground)
        .fade_in(false)
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(PANEL)
                .stroke(Stroke::new(1.0, PANEL_STROKE))
                .corner_radius(CornerRadius::same(10))
                .inner_margin(6)
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing = vec2(2.0, 2.0);
                    match popup {
                        Popup::Color => {
                            for row in COLORS.chunks(PALETTE_COLUMNS) {
                                ui.horizontal(|ui| {
                                    for &color in row {
                                        let selected = view.style.color == color;
                                        if color_dot(ui, color, selected).clicked() {
                                            actions.push(Action::Color(color));
                                            picked = true;
                                        }
                                    }
                                });
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

/// One colour to pick: a dot, ringed when selected or hovered.
fn color_dot(ui: &mut egui::Ui, color: [u8; 3], selected: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::click());
    let p = ui.painter();
    if selected {
        p.circle_stroke(rect.center(), 10.5, Stroke::new(2.0, Color32::WHITE));
    } else if resp.hovered() {
        p.circle_stroke(
            rect.center(),
            10.5,
            Stroke::new(1.0, Color32::from_white_alpha(90)),
        );
    }
    p.circle_filled(
        rect.center(),
        8.0,
        Color32::from_rgb(color[0], color[1], color[2]),
    );
    p.circle_stroke(
        rect.center(),
        8.0,
        Stroke::new(1.0, Color32::from_white_alpha(40)),
    );
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
    let p = ui.painter();
    if selected || resp.hovered() {
        let bg = if selected {
            Color32::from_white_alpha(28)
        } else {
            Color32::from_white_alpha(14)
        };
        p.rect_filled(rect, 6.0, bg);
    }
    p.circle_filled(pos2(rect.left() + 16.0, rect.center().y), radius, ICON);
    p.text(
        pos2(rect.left() + 32.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(13.0),
        ICON,
    );
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// A toolbar button that toggles `popup`, with a chevron to show it opens.
/// `paint` draws the current choice in the space left of the chevron.
fn popup_button(
    ui: &mut egui::Ui,
    popup: Popup,
    tip: &str,
    paint: impl FnOnce(&egui::Painter, Pos2),
) -> Rect {
    let (rect, resp) = ui.allocate_exact_size(vec2(40.0, BUTTON), Sense::click());
    let open = open_popup(ui.ctx()) == Some(popup);
    let p = ui.painter();
    if open || resp.hovered() {
        let bg = if open {
            Color32::from_white_alpha(28)
        } else {
            Color32::from_white_alpha(14)
        };
        p.rect_filled(rect.shrink2(vec2(1.0, 3.0)), 6.0, bg);
    }
    paint(p, pos2(rect.left() + 14.0, rect.center().y));
    let c = pos2(rect.right() - 10.0, rect.center().y);
    p.add(Shape::line(
        vec![
            c + vec2(-3.5, -1.75),
            c + vec2(0.0, 1.75),
            c + vec2(3.5, -1.75),
        ],
        Stroke::new(1.5, ICON),
    ));
    if resp
        .on_hover_text(tip)
        .on_hover_cursor(CursorIcon::PointingHand)
        .clicked()
    {
        set_popup(ui.ctx(), if open { None } else { Some(popup) });
    }
    rect
}

/// Draws the toolbar's buttons. Returns where the popup buttons are.
fn toolbar_contents(ui: &mut egui::Ui, view: &View, actions: &mut Vec<Action>) -> PopupButtons {
    for tool in Tool::ALL {
        let tip = format!("{}  ({})", tool.name(), tool.key());
        if icon_button(ui, Icon::Tool(tool), view.tool == tool, true, &tip).clicked() {
            actions.push(Action::Tool(tool));
        }
    }
    divider(ui);
    let [r, g, b] = view.style.color;
    let color = popup_button(ui, Popup::Color, "Colour", |p, at| {
        p.circle_filled(at, 8.0, Color32::from_rgb(r, g, b));
        p.circle_stroke(at, 8.0, Stroke::new(1.0, Color32::from_white_alpha(60)));
    });
    let (radius, label) = size_look(view.style.size);
    let size = popup_button(ui, Popup::Size, &format!("Thickness: {label}"), |p, at| {
        p.circle_filled(at, radius, ICON);
    });
    divider(ui);
    if icon_button(ui, Icon::Undo, false, view.can_undo, "Undo  (Ctrl+Z)").clicked() {
        actions.push(Action::Undo);
    }
    if icon_button(ui, Icon::Redo, false, view.can_redo, "Redo  (Ctrl+Y)").clicked() {
        actions.push(Action::Redo);
    }
    PopupButtons { color, size }
}

fn divider(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(11.0, BUTTON), Sense::hover());
    ui.painter().line_segment(
        [
            pos2(rect.center().x, rect.top() + 7.0),
            pos2(rect.center().x, rect.bottom() - 7.0),
        ],
        Stroke::new(1.0, PANEL_STROKE),
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
    let p = ui.painter();
    let color = if !enabled {
        ICON_DISABLED
    } else if selected {
        Color32::WHITE
    } else {
        ICON
    };
    if selected {
        p.rect_filled(rect, 7.0, ACCENT);
    } else if enabled && resp.hovered() {
        p.rect_filled(rect, 7.0, Color32::from_white_alpha(22));
    }
    draw_icon(p, rect.shrink(8.0), icon, color);
    let resp = resp.on_hover_text(tip);
    if enabled {
        resp.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        resp
    }
}

fn draw_icon(p: &egui::Painter, r: Rect, icon: Icon, color: Color32) {
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
        Icon::Tool(Tool::Step) => {
            p.circle_stroke(r.center(), r.width() * 0.48, Stroke::new(1.7, color));
            p.text(
                r.center() + vec2(0.0, 0.5),
                Align2::CENTER_CENTER,
                "1",
                FontId::proportional(r.height() * 0.72),
                color,
            );
        }
        Icon::Tool(Tool::Eraser) => {
            // An eyedropper over a filled box.
            let bx = Rect::from_min_max(at(0.0, 0.42), at(0.78, 1.0));
            p.rect_filled(bx, 2.0, color.gamma_multiply(0.55));
            p.line_segment([at(0.3, 0.7), at(0.82, 0.18)], Stroke::new(2.2, color));
            p.circle_filled(at(0.86, 0.14), r.width() * 0.13, color);
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
            p.rect_filled(front, 1.5, PANEL);
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
    #[ignore]
    fn toolbar_preview() {
        let view = View {
            tool: Tool::Select,
            style: Style::default(),
            can_undo: true,
            can_redo: false,
            show_toolbar: true,
            selection: Some((
                Rect::from_min_size(pos2(120.0, 90.0), vec2(500.0, 60.0)),
                [750, 90],
            )),
            cursor: CursorIcon::Crosshair,
            arrow_nodes: vec![pos2(900.0, 200.0)],
            hint: None,
            loupe: None,
        };
        let mut laid_out = false;
        crate::preview::render("toolbar-preview", [1200, 260], 1.5, |root| {
            laid_out |= ui(root, &view).1.is_some();
        });
        assert!(laid_out, "toolbar should be laid out");
    }

    /// Renders the colour picker's hint and loupe, near the bottom-right
    /// corner so it flips, to `target/loupe-preview.png`:
    /// `cargo test loupe_preview -- --ignored`.
    #[test]
    #[ignore]
    fn loupe_preview() {
        let n = crate::session::LOUPE_SIZE;
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
            selection: None,
            cursor: CursorIcon::Crosshair,
            arrow_nodes: Vec::new(),
            hint: Some("Click to pick a colour \u{2014} arrow keys move one pixel"),
            loupe: Some((pos2(560.0, 300.0), pixels)),
        };
        crate::preview::render("loupe-preview", [640, 360], 1.0, |root| {
            root.painter()
                .rect_filled(root.max_rect(), 0.0, Color32::from_rgb(200, 120, 60));
            ui(root, &view);
        });
    }
}
