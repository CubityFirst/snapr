//! Annotations drawn over the selected region, rasterized with tiny-skia.
//!
//! The canvas keeps the selected region's pixels (`base`), the finished
//! annotations drawn on top (`committed`), and the shown frame (`frame` =
//! committed + the annotation being drawn). The frame is exactly what gets
//! saved, and only the area that changed is redrawn while drawing.

use std::hash::{Hash, Hasher};
use std::sync::{Arc, LazyLock, Mutex};

use image::RgbaImage;
use tiny_skia::{
    BlendMode, FillRule, LineCap, LineJoin, Paint, Path, PathBuilder, Pixmap, PixmapPaint,
    Stroke, StrokeDash, Transform,
};

use crate::capture::Rect;

type Point = (f32, f32);

/// A ring drawn around a hovered annotation: what it was for, where its
/// top-left corner goes and the ring itself.
type Ring = (u64, (i32, i32), Arc<Pixmap>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Select,
    Pen,
    Line,
    Arrow,
    Text,
    Step,
    Rect,
    Ellipse,
    Highlighter,
    Blur,
    Pixelate,
    Image,
    Eraser,
    Clip,
}

impl Tool {
    pub const ALL: [Tool; 14] = [
        Tool::Select,
        Tool::Pen,
        Tool::Line,
        Tool::Arrow,
        Tool::Text,
        Tool::Step,
        Tool::Rect,
        Tool::Ellipse,
        Tool::Highlighter,
        Tool::Blur,
        Tool::Pixelate,
        Tool::Image,
        Tool::Eraser,
        Tool::Clip,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Tool::Select => "Region: drag to capture (Shift: square, Ctrl: move)",
            Tool::Pen => "Pen",
            Tool::Line => "Line",
            Tool::Arrow => "Arrow",
            Tool::Text => {
                "Text: click and type; Enter finishes, Shift+Enter starts a new line. \
                 Drag text to move it, click it to change it"
            }
            Tool::Step => "Step: click to place numbered markers 1, 2, 3\u{2026}",
            Tool::Rect => "Box",
            Tool::Ellipse => "Ellipse",
            Tool::Highlighter => "Highlighter",
            Tool::Blur => "Blur",
            Tool::Pixelate => "Pixelate",
            Tool::Image => "Image redaction: cover an area with your chosen image (set in Settings)",
            Tool::Eraser => {
                crate::locale::spelled!(
                    "Smart eraser: drag a box filled with the colour under where you start (hold Ctrl to move it)"
                )
            }
            Tool::Clip => {
                "Clip: drag over part of the screen to copy it, then drag the copy to move it"
            }
        }
    }

    /// Keyboard shortcut.
    pub fn key(self) -> char {
        match self {
            Tool::Select => 'S',
            Tool::Pen => 'P',
            Tool::Line => 'L',
            Tool::Arrow => 'A',
            Tool::Text => 'T',
            Tool::Step => 'N',
            Tool::Rect => 'R',
            Tool::Ellipse => 'E',
            Tool::Highlighter => 'H',
            Tool::Blur => 'B',
            Tool::Pixelate => 'X',
            Tool::Image => 'I',
            Tool::Eraser => 'D',
            Tool::Clip => 'C',
        }
    }

    /// A second shortcut, for tools with one.
    pub fn alias(self) -> Option<char> {
        match self {
            Tool::Pen => Some('F'), // freehand
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    Small,
    Medium,
    Large,
}

impl Size {
    pub const ALL: [Size; 3] = [Size::Small, Size::Medium, Size::Large];

    fn stroke(self) -> f32 {
        match self {
            Size::Small => 3.0,
            Size::Medium => 5.0,
            Size::Large => 9.0,
        }
    }

    fn highlighter(self) -> f32 {
        match self {
            Size::Small => 14.0,
            Size::Medium => 22.0,
            Size::Large => 36.0,
        }
    }

    fn blur_radius(self) -> usize {
        match self {
            Size::Small => 5,
            Size::Medium => 10,
            Size::Large => 20,
        }
    }

}

/// What the highlighter draws.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HighlighterShape {
    /// Follows the mouse.
    #[default]
    Freehand,
    /// A straight stroke from where the drag starts; Shift snaps it to 45°.
    Straight,
    /// A filled box over the dragged area.
    Box,
}

/// How the highlighter's colour goes on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HighlighterInk {
    /// Like a real marker: each channel keeps the darker of the picture and
    /// the colour, so light areas take the colour, dark ones stay dark, and
    /// strokes that overlap don't build up.
    #[default]
    Realistic,
    /// A see-through wash of the colour over everything.
    Tint,
}

/// How see-through a tint is (alpha, of 255).
const TINT_ALPHA: u8 = 110;

/// Block sizes the pixelate tool offers, in pixels.
pub const PIXEL_BLOCKS: [u32; 4] = [8, 14, 24, 40];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelateOptions {
    /// Side of each block, in pixels.
    pub block: u32,
    /// Shuffle the blocks and add noise, so nothing under them can be
    /// pieced back together, rather than averaging each block in place.
    pub secure: bool,
}

/// The picture the image redaction tool covers areas with.
#[derive(Debug, Clone)]
pub struct RedactImage {
    pixmap: Arc<Pixmap>,
    /// Squash or stretch it to the area's shape, rather than keeping its
    /// proportions and cropping what overhangs.
    pub stretch: bool,
}

impl RedactImage {
    pub fn load(path: &std::path::Path, stretch: bool) -> Result<Self, String> {
        let img = image::open(path)
            .map_err(|e| format!("couldn't open {}: {e}", path.display()))?
            .into_rgba8();
        Ok(Self {
            pixmap: Arc::new(premultiplied_pixmap(&img).ok_or("the image is empty")?),
            stretch,
        })
    }
}

/// Premultiplies an image (which may be partly transparent) into a pixmap.
fn premultiplied_pixmap(img: &RgbaImage) -> Option<Pixmap> {
    let mut pm = Pixmap::new(img.width(), img.height())?;
    for (dst, src) in pm.pixels_mut().iter_mut().zip(img.pixels()) {
        let [r, g, b, a] = src.0;
        *dst = tiny_skia::ColorU8::from_rgba(r, g, b, a).premultiply();
    }
    Some(pm)
}

/// How the text tool writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextOptions {
    /// An installed font (its place in `fonts::installed`), or `None` for
    /// the bundled one.
    pub font: Option<u16>,
    /// Pixels per em.
    pub size: u16,
    /// Edge it in black or white, whichever stands out from its colour.
    pub outline: bool,
}

/// The text sizes offered, in pixels per em.
pub const TEXT_SIZES: std::ops::RangeInclusive<u16> = 10..=160;

/// How the step tool's markers look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StepOptions {
    /// Across the circle, in pixels.
    pub size: u16,
    /// How tall the digits are, in pixels; at most what fits the circle
    /// (see [`Self::number_sizes`]). Numbers of two digits or more are
    /// narrowed to fit it.
    pub number: u16,
    /// A thin white border, to show on their own colour.
    pub border: bool,
}

impl StepOptions {
    /// The number sizes that fit inside the circle, in pixels tall.
    pub fn number_sizes(&self) -> std::ops::RangeInclusive<u16> {
        8..=(self.size as f32 * STEP_NUMBER_FIT) as u16
    }
}

/// The circle sizes offered, in pixels across.
pub const STEP_SIZES: std::ops::RangeInclusive<u16> = 16..=120;

/// How tall a number can be, for each pixel across its circle.
const STEP_NUMBER_FIT: f32 = 0.55;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArrowOptions {
    /// A head at both ends.
    pub double: bool,
    /// The head goes where the drag started, pointing back at it, rather
    /// than where it ends.
    pub head_at_start: bool,
}

/// How many nodes lines and arrows offer to bend them by.
pub const BENDS: std::ops::RangeInclusive<u8> = 1..=5;

pub const COLORS: [[u8; 3]; 8] = [
    [0xff, 0x3b, 0x30], // red
    [0xff, 0x95, 0x00], // orange
    [0xff, 0xd6, 0x0a], // yellow
    [0x34, 0xc7, 0x59], // green
    [0x3d, 0x9b, 0xff], // blue
    [0xbf, 0x5a, 0xf2], // purple
    [0xff, 0xff, 0xff], // white
    [0x1c, 0x1c, 0x1e], // black
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    pub color: [u8; 3],
    /// The highlighter's own colour, so marking text doesn't change the
    /// other tools' (or theirs it).
    pub highlighter_color: [u8; 3],
    pub size: Size,
    pub pixelate: PixelateOptions,
    pub arrow: ArrowOptions,
    /// Nodes along a line or an arrow, evenly spaced, to bend it by (see
    /// [`BENDS`]).
    pub bends: u8,
    /// Clips get a soft drop shadow, lifting them off the screen under them.
    pub clip_shadow: bool,
    pub step: StepOptions,
    pub text: TextOptions,
    pub highlighter: HighlighterShape,
    pub highlighter_ink: HighlighterInk,
}

impl Style {
    /// The colour `tool` draws in.
    pub fn color_for(&self, tool: Tool) -> [u8; 3] {
        if tool == Tool::Highlighter { self.highlighter_color } else { self.color }
    }

    /// Changes the colour `tool` draws in.
    pub fn set_color(&mut self, tool: Tool, color: [u8; 3]) {
        if tool == Tool::Highlighter {
            self.highlighter_color = color;
        } else {
            self.color = color;
        }
    }
}

impl Default for Style {
    fn default() -> Self {
        Self {
            color: COLORS[0],
            highlighter_color: COLORS[2],
            size: Size::Medium,
            pixelate: PixelateOptions {
                block: 14,
                secure: false,
            },
            arrow: ArrowOptions {
                double: false,
                head_at_start: false,
            },
            bends: 1,
            clip_shadow: true,
            step: StepOptions {
                size: 30,
                number: 16,
                border: true,
            },
            highlighter: HighlighterShape::Freehand,
            highlighter_ink: HighlighterInk::Realistic,
            text: TextOptions {
                font: None,
                size: 28,
                outline: true,
            },
        }
    }
}

#[derive(Debug, Clone)]
enum Shape {
    Pen(Vec<Point>),
    /// Freehand, or (drawn straight) just its two ends.
    Highlighter(Vec<Point>),
    /// A highlighted box.
    Mark(Point, Point),
    /// From, to, and the nodes the curve passes through, once one's been
    /// moved off the straight line (none while it's straight).
    Line(Point, Point, Vec<Point>),
    /// Like a line, with a head at the end.
    Arrow(Point, Point, Vec<Point>),
    Rect(Point, Point),
    Ellipse(Point, Point),
    Blur(Point, Point),
    Pixelate(Point, Point),
    /// An area covered with a picture; solid black without one.
    Image(Point, Point, Option<RedactImage>),
    /// The area being dragged out with the clip tool.
    ClipSelect(Point, Point),
    /// A copied piece of the screen, with its top-left corner.
    Clip(Arc<Pixmap>, (i32, i32)),
    /// A numbered marker: its centre and number, and the tip of the point
    /// it's drawn out to, once its node has been dragged.
    Step(Point, u32, Option<Point>),
    /// Typed text, by its top-left corner; lines split at `\n`. In a box
    /// dragged out for it, the box's size: the text wraps to its width.
    Text(Point, String, Option<(f32, f32)>),
    /// The box being dragged out for text: where the drag started, and the
    /// mouse.
    TextSelect(Point, Point),
    /// A box filled with the colour sampled where it was started
    /// (premultiplied RGBA, as stored in the pixmap).
    Erase(Point, Point, [u8; 4]),
}

/// How close (in pixels) the mouse has to be to a line or stroke to pick it.
const HIT_TOLERANCE: i32 = 5;

/// Room (in pixels) between a hovered stroke's edge and the ring around it.
const RING_GAP: f32 = 4.0;

/// What to outline around the annotation under the cursor (global pixels).
#[derive(Debug, Clone)]
pub enum Highlight {
    /// A box around it.
    Box(Rect),
    /// A ring that follows its strokes, drawn with its top-left corner at
    /// `at`. `id` changes when the picture does.
    Ring {
        id: u64,
        at: (i32, i32),
        image: Arc<Pixmap>,
    },
}

/// Shadow under a clip, in pixels.
const CLIP_SHADOW: i32 = 5;

/// Room (in pixels) around text for its outline and, while it's typed, the
/// dashed box.
const TEXT_PAD: i32 = 5;

/// One annotation, in global (virtual desktop) pixel coordinates.
#[derive(Debug, Clone)]
pub struct Annotation {
    shape: Shape,
    style: Style,
    /// Randomness of its own (for secure pixelation), so it looks the same
    /// every time it's redrawn.
    seed: u64,
}

impl Annotation {
    /// Starts an annotation at `p`; `None` for tools that don't draw.
    pub fn new(tool: Tool, mut style: Style, p: Point) -> Option<Self> {
        style.color = style.color_for(tool);
        let shape = match tool {
            Tool::Select => return None,
            Tool::Pen => Shape::Pen(vec![p]),
            Tool::Highlighter => match style.highlighter {
                HighlighterShape::Freehand => Shape::Highlighter(vec![p]),
                HighlighterShape::Straight => Shape::Highlighter(vec![p, p]),
                HighlighterShape::Box => Shape::Mark(p, p),
            },
            Tool::Line => Shape::Line(p, p, Vec::new()),
            Tool::Arrow => Shape::Arrow(p, p, Vec::new()),
            Tool::Rect => Shape::Rect(p, p),
            Tool::Ellipse => Shape::Ellipse(p, p),
            Tool::Blur => Shape::Blur(p, p),
            Tool::Pixelate => Shape::Pixelate(p, p),
            // Its picture is added with `with_image`.
            Tool::Image => Shape::Image(p, p, None),
            Tool::Clip => Shape::ClipSelect(p, p),
            // Numbered when it's added to the canvas.
            Tool::Step => Shape::Step(p, 0, None),
            // Typed into once it's let go: in the box it's dragged out to,
            // or, clicked, with the click in the middle of its first line.
            Tool::Text => Shape::TextSelect(p, p),
            // Its colour is sampled when it's added to the canvas.
            Tool::Eraser => Shape::Erase(p, p, [0, 0, 0, 255]),
        };
        Some(Self {
            shape,
            style,
            seed: fastrand::u64(..),
        })
    }

    /// Gives an image redaction its picture.
    pub fn with_image(mut self, image: Option<RedactImage>) -> Self {
        if let Shape::Image(_, _, img) = &mut self.shape {
            *img = image;
        }
        self
    }

    /// Extends the annotation to `p` while dragging. With `constrain` (Shift),
    /// lines snap to 45Â° and boxes/ellipses become squares/circles.
    pub fn drag_to(&mut self, p: Point, constrain: bool) {
        match &mut self.shape {
            Shape::Highlighter(pts) if self.style.highlighter == HighlighterShape::Straight => {
                pts[1] = if constrain { snap_45(pts[0], p) } else { p };
            }
            Shape::Pen(pts) | Shape::Highlighter(pts) => {
                let last = *pts.last().expect("never empty");
                if (p.0 - last.0).hypot(p.1 - last.1) >= 1.0 {
                    pts.push(p);
                }
            }
            Shape::Line(a, b, _) | Shape::Arrow(a, b, _) => {
                *b = if constrain { snap_45(*a, p) } else { p }
            }
            Shape::Rect(a, b) | Shape::Ellipse(a, b) | Shape::Mark(a, b) => {
                *b = if constrain { square(*a, p) } else { p }
            }
            Shape::Blur(_, b)
            | Shape::Pixelate(_, b)
            | Shape::Image(_, b, _)
            | Shape::ClipSelect(_, b)
            | Shape::Erase(_, b, _) => *b = p,
            Shape::TextSelect(_, b) => *b = p,
            Shape::Clip(..) | Shape::Text(..) => {}
            // It follows the mouse until it's dropped.
            Shape::Step(c, _, _) => *c = p,
        }
    }

    /// Shifts the whole annotation by `d`, e.g. while drawing with Ctrl held.
    fn translate(&mut self, d: Point) {
        let mv = |p: &mut Point| *p = (p.0 + d.0, p.1 + d.1);
        match &mut self.shape {
            Shape::Pen(pts) | Shape::Highlighter(pts) => pts.iter_mut().for_each(mv),
            Shape::Line(a, b, nodes) | Shape::Arrow(a, b, nodes) => {
                mv(a);
                mv(b);
                nodes.iter_mut().for_each(mv);
            }
            Shape::Rect(a, b)
            | Shape::Ellipse(a, b)
            | Shape::Mark(a, b)
            | Shape::Blur(a, b)
            | Shape::Pixelate(a, b)
            | Shape::Image(a, b, _)
            | Shape::ClipSelect(a, b)
            | Shape::TextSelect(a, b)
            | Shape::Erase(a, b, _) => {
                mv(a);
                mv(b);
            }
            Shape::Step(c, _, tip) => {
                mv(c);
                if let Some(t) = tip {
                    mv(t);
                }
            }
            Shape::Text(c, _, _) => mv(c),
            Shape::Clip(_, at) => *at = (at.0 + d.0.round() as i32, at.1 + d.1.round() as i32),
        }
    }

    /// Whether it's too small to keep, e.g. a click with the box tool.
    fn is_empty(&self) -> bool {
        match &self.shape {
            Shape::Pen(_) => false,
            Shape::Highlighter(p) => match p[..] {
                [a, b] => (b.0 - a.0).hypot(b.1 - a.1) < 2.0,
                _ => p.len() < 2,
            },
            Shape::Line(a, b, _) | Shape::Arrow(a, b, _) => (b.0 - a.0).hypot(b.1 - a.1) < 2.0,
            Shape::Rect(a, b)
            | Shape::Ellipse(a, b)
            | Shape::Mark(a, b)
            | Shape::Blur(a, b)
            | Shape::Pixelate(a, b)
            | Shape::Image(a, b, _)
            | Shape::Erase(a, b, _) => (b.0 - a.0).abs() < 2.0 || (b.1 - a.1).abs() < 2.0,
            Shape::ClipSelect(a, b) => (b.0 - a.0).abs() < 4.0 || (b.1 - a.1).abs() < 4.0,
            Shape::Clip(..) | Shape::Step(..) => false,
            Shape::Text(_, text, _) => text.trim().is_empty(),
            // Typing starts in it when it's let go.
            Shape::TextSelect(..) => true,
        }
    }

    fn step_radius(&self) -> f32 {
        self.style.step.size as f32 / 2.0
    }

    /// Where it is on screen (global pixels), if `tool` can move it: clips
    /// with the clip tool, areas, pen strokes, lines and arrows with their
    /// own tools.
    fn movable_rect(&self, tool: Tool) -> Option<Rect> {
        match (&self.shape, tool) {
            (Shape::Pen(_), Tool::Pen) | (Shape::Highlighter(_), Tool::Highlighter) => {
                self.stroke_box()
            }
            (Shape::Line(..), Tool::Line) | (Shape::Arrow(..), Tool::Arrow) => self.line_box(),
            (Shape::Text(..), Tool::Text) => self.text_box(),
            (Shape::Clip(img, (x, y)), Tool::Clip) => Some(Rect {
                x: *x,
                y: *y,
                w: img.width(),
                h: img.height(),
            }),
            (Shape::Blur(a, b), Tool::Blur)
            | (Shape::Mark(a, b), Tool::Highlighter)
            | (Shape::Pixelate(a, b), Tool::Pixelate)
            | (Shape::Image(a, b, _), Tool::Image)
            | (Shape::Erase(a, b, _), Tool::Eraser) => Some(Rect::from_points(
                (a.0 as f64, a.1 as f64),
                (b.0 as f64, b.1 as f64),
            )),
            (Shape::Step(c, _, _), Tool::Step) => {
                let r = self.step_radius() as f64;
                let c = (c.0 as f64, c.1 as f64);
                Some(Rect::from_points((c.0 - r, c.1 - r), (c.0 + r, c.1 + r)))
            }
            _ => None,
        }
    }

    /// The box text takes up (global pixels), with room for the caret
    /// when there's nothing typed yet.
    fn text_box(&self) -> Option<Rect> {
        let Shape::Text(at, text, boxed) = &self.shape else {
            return None;
        };
        let (w, h) = layout_text(self.style.text, text, *at, boxed.map(|b| b.0)).size;
        let (w, h) = match boxed {
            Some((bw, bh)) => (*bw, h.max(*bh)),
            None => (w.max(self.style.text.size as f32 * 0.3), h),
        };
        Some(Rect::from_points(
            (at.0 as f64, at.1 as f64),
            ((at.0 + w) as f64, (at.1 + h) as f64),
        ))
    }

    /// While text is being typed: a dashed box around it and the caret at
    /// its end. Not part of the screenshot.
    fn draw_typing(&self, pm: &mut Pixmap, origin: (i32, i32)) {
        let (Shape::Text(at, text, boxed), Some(r)) = (&self.shape, self.text_box()) else {
            return;
        };
        let local = |p: Point| (p.0 - origin.0 as f32, p.1 - origin.1 as f32);
        let pad = TEXT_PAD as f32;
        dashed_outline(
            pm,
            local((r.x as f32 - pad, r.y as f32 - pad)),
            local((r.right() as f32 + pad, r.bottom() as f32 + pad)),
        );
        let px = self.style.text.size as f32;
        let (top, height) = layout_text(self.style.text, text, *at, boxed.map(|b| b.0)).caret;
        let top = local(top);
        let [r, g, b] = self.style.color;
        let mut paint = Paint::default();
        paint.set_color_rgba8(r, g, b, 255);
        let width = (px * 0.07).max(2.0);
        if let Some(caret) = tiny_skia::Rect::from_xywh(top.0 + 1.0, top.1, width, height) {
            pm.fill_rect(caret, &paint, Transform::identity(), None);
        }
    }

    /// The box around a pen or highlighter stroke's points.
    fn stroke_box(&self) -> Option<Rect> {
        let (Shape::Pen(pts) | Shape::Highlighter(pts)) = &self.shape else {
            return None;
        };
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for &(x, y) in pts {
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
        (x0 <= x1).then(|| Rect::from_points((x0 as f64, y0 as f64), (x1 as f64, y1 as f64)))
    }

    /// The box between a line's or an arrow's ends.
    fn line_box(&self) -> Option<Rect> {
        let (Shape::Line(a, b, _) | Shape::Arrow(a, b, _)) = &self.shape else {
            return None;
        };
        Some(Rect::from_points((a.0 as f64, a.1 as f64), (b.0 as f64, b.1 as f64)))
    }

    /// Where it is if `tool` can pick it up at `p`: anywhere in a clip or
    /// area, but on the line itself for a stroke, a line or an arrow.
    fn movable_under(&self, p: Point, tool: Tool) -> Option<Rect> {
        let r = self.movable_rect(tool)?;
        match self.shape {
            Shape::Pen(_) | Shape::Highlighter(_) | Shape::Line(..) | Shape::Arrow(..) => {
                self.hit(p).map(|_| r)
            }
            _ => r.contains((p.0 as f64, p.1 as f64)).then_some(r),
        }
    }

    /// An arrow's heads, for a shaft `len` long.
    fn heads(&self, len: f32) -> Heads {
        let opts = self.style.arrow;
        let (at_start, at_end) = if opts.double {
            (true, true)
        } else {
            (opts.head_at_start, !opts.head_at_start)
        };
        let (head_len, width) = arrow_head(self.stroke_width());
        Heads {
            at_start,
            at_end,
            // Two heads get at most half the line each.
            len: head_len.min(if opts.double { len / 2.0 } else { len }),
            width,
        }
    }

    /// What the ring around it follows when it's hovered (global pixels):
    /// lines stroked at its width, and filled areas. `None` for the shapes
    /// outlined with a box instead.
    fn ring_shape(&self) -> Option<(Vec<Path>, Vec<Path>)> {
        match &self.shape {
            Shape::Pen(pts) | Shape::Highlighter(pts) if pts.len() == 1 => {
                let dot = PathBuilder::from_circle(pts[0].0, pts[0].1, self.stroke_width() / 2.0)?;
                Some((Vec::new(), vec![dot]))
            }
            Shape::Pen(pts) | Shape::Highlighter(pts) => Some((vec![pen_path(pts)?], Vec::new())),
            Shape::Line(a, b, nodes) => Some((vec![Curve::new(*a, *b, nodes).path()?], Vec::new())),
            Shape::Arrow(a, b, nodes) => {
                let curve = Curve::new(*a, *b, nodes);
                let heads = self.heads((b.0 - a.0).hypot(b.1 - a.1));
                let fills = [(heads.at_end, curve.clone()), (heads.at_start, curve.reversed())]
                    .into_iter()
                    .filter(|(on, _)| *on)
                    .filter_map(|(_, curve)| head_path(&curve, heads.len, heads.width))
                    .collect();
                Some((vec![curve.path()?], fills))
            }
            Shape::Step(c, _, tip) => {
                Some((Vec::new(), vec![step_path(*c, self.step_radius(), *tip)?]))
            }
            _ => None,
        }
    }

    /// The ring around it when it's hovered, and where its top-left corner
    /// goes (global pixels): white, with a dark edge either side to show on
    /// any background. `None` for the shapes outlined with a box.
    fn ring(&self) -> Option<((i32, i32), Pixmap)> {
        let (lines, fills) = self.ring_shape()?;
        let width = self.stroke_width();
        let bounds = self.bounds();
        let margin = (RING_GAP + 4.0).ceil() as i32;
        let origin = (bounds.x - margin, bounds.y - margin);
        let size = |n: u32| n + 2 * margin as u32;
        let to_local = Transform::from_translate(-origin.0 as f32, -origin.1 as f32);
        // Everything within `out` of the shape: painted, or with no colour,
        // cleared.
        let cover = |pm: &mut Pixmap, color: Option<[u8; 4]>, out: f32| {
            let mut paint = Paint {
                anti_alias: true,
                ..Paint::default()
            };
            match color {
                Some([r, g, b, a]) => paint.set_color_rgba8(r, g, b, a),
                None => paint.blend_mode = BlendMode::Clear,
            }
            let stroke = |width: f32| Stroke {
                width,
                line_cap: LineCap::Round,
                line_join: LineJoin::Round,
                ..Stroke::default()
            };
            for path in &lines {
                pm.stroke_path(path, &paint, &stroke(width + 2.0 * out), to_local, None);
            }
            for path in &fills {
                pm.fill_path(path, &paint, FillRule::Winding, to_local, None);
                pm.stroke_path(path, &paint, &stroke(2.0 * out), to_local, None);
            }
        };
        let mut ring = Pixmap::new(size(bounds.w), size(bounds.h))?;
        cover(&mut ring, Some([0, 0, 0, 140]), RING_GAP + 2.25);
        cover(&mut ring, None, RING_GAP - 0.75);
        let mut light = Pixmap::new(ring.width(), ring.height())?;
        cover(&mut light, Some([255, 255, 255, 255]), RING_GAP + 1.5);
        cover(&mut light, None, RING_GAP);
        ring.draw_pixmap(0, 0, light.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
        Some((origin, ring))
    }

    /// The tool that moves it, for the ones that can be picked up.
    fn mover(&self) -> Option<Tool> {
        match self.shape {
            Shape::Clip(..) => Some(Tool::Clip),
            Shape::Blur(..) => Some(Tool::Blur),
            Shape::Mark(..) => Some(Tool::Highlighter),
            Shape::Pixelate(..) => Some(Tool::Pixelate),
            Shape::Image(..) => Some(Tool::Image),
            Shape::Erase(..) => Some(Tool::Eraser),
            Shape::Step(..) => Some(Tool::Step),
            Shape::Text(..) => Some(Tool::Text),
            _ => None,
        }
    }

    /// Whether `p` is on it, and if so the area to outline (global pixels):
    /// anywhere inside a clip or area; within a few pixels of what's drawn
    /// for lines, shapes and strokes.
    fn hit(&self, p: Point) -> Option<Rect> {
        // A step's point sticks out of the box it's moved by.
        if let Some(tool) = self.mover()
            && !matches!(self.shape, Shape::Step(..))
        {
            return self
                .movable_rect(tool)
                .filter(|r| r.contains((p.0 as f64, p.1 as f64)));
        }
        let bounds = self.bounds();
        let reach = HIT_TOLERANCE as f64;
        let near = (p.0 as f64) >= bounds.x as f64 - reach
            && (p.1 as f64) >= bounds.y as f64 - reach
            && (p.0 as f64) < bounds.right() as f64 + reach
            && (p.1 as f64) < bounds.bottom() as f64 + reach;
        if !near {
            return None;
        }
        // Draw it into a little square around `p` and look for paint.
        let side = 2 * HIT_TOLERANCE as u32 + 1;
        let mut pm = Pixmap::new(side, side)?;
        let origin = (
            p.0.round() as i32 - HIT_TOLERANCE,
            p.1.round() as i32 - HIT_TOLERANCE,
        );
        self.draw(&mut pm, origin);
        pm.pixels()
            .iter()
            .any(|px| px.alpha() > 0)
            .then_some(bounds)
    }

    /// Moves a clip, area, stroke, line or arrow so its top-left corner is
    /// at `p`.
    fn move_to(&mut self, p: Point) {
        let p = (p.0.round(), p.1.round());
        if let Some(r) = self.stroke_box().or_else(|| self.line_box()) {
            return self.translate((p.0 - r.x as f32, p.1 - r.y as f32));
        }
        let radius = self.step_radius();
        match &mut self.shape {
            Shape::Clip(_, at) => *at = (p.0 as i32, p.1 as i32),
            Shape::Blur(a, b)
            | Shape::Mark(a, b)
            | Shape::Pixelate(a, b)
            | Shape::Image(a, b, _)
            | Shape::Erase(a, b, _) => {
                let size = ((b.0 - a.0).abs(), (b.1 - a.1).abs());
                *a = p;
                *b = (p.0 + size.0, p.1 + size.1);
            }
            // `p` is the top-left of its square; the centre is a radius in.
            // Its point goes with it.
            Shape::Step(c, _, tip) => {
                let to = (p.0 + radius, p.1 + radius);
                if let Some(t) = tip {
                    *t = (t.0 + to.0 - c.0, t.1 + to.1 - c.1);
                }
                *c = to;
            }
            Shape::Text(at, _, _) => *at = p,
            _ => {}
        }
    }

    /// Draws it while it's being dragged out or moved. Blurred and pixelated
    /// areas show just their outline until they're dropped.
    fn draw_active(&self, pm: &mut Pixmap, origin: (i32, i32)) {
        match &self.shape {
            Shape::Blur(a, b) | Shape::Pixelate(a, b) => {
                let local = |p: &Point| (p.0 - origin.0 as f32, p.1 - origin.1 as f32);
                dashed_outline(pm, local(a), local(b));
            }
            _ => self.draw(pm, origin),
        }
    }

    /// The nodes of a shape drawn with `tool` that can be dragged to reshape
    /// it: a line's or an arrow's, to curve it, or the tip of a step's point
    /// (on its edge, down and right, until it has one).
    fn bend_nodes(&self, tool: Tool) -> Vec<Point> {
        match (&self.shape, tool) {
            (Shape::Line(a, b, nodes), Tool::Line) | (Shape::Arrow(a, b, nodes), Tool::Arrow) => {
                if nodes.is_empty() {
                    straight_nodes(*a, *b, self.style.bends)
                } else {
                    nodes.clone()
                }
            }
            (Shape::Step(c, _, tip), Tool::Step) => {
                let edge = self.step_radius() * std::f32::consts::FRAC_1_SQRT_2;
                vec![tip.unwrap_or((c.0 + edge, c.1 + edge))]
            }
            _ => Vec::new(),
        }
    }

    /// Moves a shape's node `i` to `p`. With all its nodes close to the
    /// straight line, a line or an arrow snaps back to straight; close to
    /// its circle, a step loses its point.
    fn bend_to(&mut self, i: usize, p: Point) {
        let radius = self.step_radius();
        let bends = self.style.bends;
        match &mut self.shape {
            Shape::Line(a, b, nodes) | Shape::Arrow(a, b, nodes) => {
                let straight = straight_nodes(*a, *b, bends);
                if nodes.is_empty() {
                    *nodes = straight.clone();
                }
                if let Some(node) = nodes.get_mut(i) {
                    *node = p;
                }
                if nodes.iter().zip(&straight).all(|(n, s)| (n.0 - s.0).hypot(n.1 - s.1) < 4.0) {
                    nodes.clear();
                }
            }
            Shape::Step(c, _, tip) => {
                *tip = ((p.0 - c.0).hypot(p.1 - c.1) >= radius + 6.0).then_some(p);
            }
            _ => {}
        }
    }

    fn stroke_width(&self) -> f32 {
        match self.shape {
            Shape::Highlighter(_) => self.style.size.highlighter(),
            _ => self.style.size.stroke(),
        }
    }

    /// Area the annotation can touch, in global pixels.
    fn bounds(&self) -> Rect {
        if let Some(r) = self.movable_rect(Tool::Clip) {
            return Rect {
                x: r.x - CLIP_SHADOW,
                y: r.y - CLIP_SHADOW,
                w: r.w + 2 * CLIP_SHADOW as u32,
                h: r.h + 2 * CLIP_SHADOW as u32 + 2,
            };
        }
        if let Some(r) = self.text_box() {
            // The outline, and the dashed box and caret while typing.
            let pad = TEXT_PAD + 3;
            return Rect {
                x: r.x - pad,
                y: r.y - pad,
                w: r.w + 2 * pad as u32,
                h: r.h + 2 * pad as u32,
            };
        }
        if let Shape::Step(c, _, tip) = self.shape {
            let r = self.step_radius() + 2.0;
            let (mut x0, mut y0, mut x1, mut y1) = (c.0 - r, c.1 - r, c.0 + r, c.1 + r);
            if let Some(t) = tip {
                // Room for its outline, wider at a sharp point.
                let pad = 4.0;
                (x0, y0) = (x0.min(t.0 - pad), y0.min(t.1 - pad));
                (x1, y1) = (x1.max(t.0 + pad), y1.max(t.1 + pad));
            }
            return Rect::from_points((x0 as f64, y0 as f64), (x1 as f64, y1 as f64));
        }
        let pts: Vec<Point> = match &self.shape {
            Shape::Pen(p) | Shape::Highlighter(p) => p.clone(),
            // The curve stays inside the hull of its control points.
            Shape::Line(a, b, nodes) | Shape::Arrow(a, b, nodes) => {
                Curve::new(*a, *b, nodes).pieces.into_iter().flatten().collect()
            }
            Shape::Rect(a, b)
            | Shape::Ellipse(a, b)
            | Shape::Mark(a, b)
            | Shape::Blur(a, b)
            | Shape::Pixelate(a, b)
            | Shape::Image(a, b, _)
            | Shape::ClipSelect(a, b)
            | Shape::TextSelect(a, b)
            | Shape::Erase(a, b, _) => vec![*a, *b],
            Shape::Clip(..) | Shape::Step(..) | Shape::Text(..) => unreachable!("handled above"),
        };
        let pad = match self.shape {
            Shape::Arrow(..) => arrow_head(self.stroke_width()).0 + 2.0,
            Shape::Blur(..) | Shape::Pixelate(..) => 1.0,
            Shape::ClipSelect(..) | Shape::TextSelect(..) | Shape::Erase(..) | Shape::Image(..) => {
                3.0
            }
            _ => self.stroke_width() / 2.0 + 2.0,
        };
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for (x, y) in pts {
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
        Rect::from_points(
            ((x0 - pad) as f64, (y0 - pad) as f64),
            ((x1 + pad) as f64, (y1 + pad) as f64),
        )
    }

    fn draw(&self, pm: &mut Pixmap, origin: (i32, i32)) {
        let local = |p: &Point| (p.0 - origin.0 as f32, p.1 - origin.1 as f32);
        let [r, g, b] = self.style.color;
        let (r_, g_, b_) = (r, g, b);
        let mut paint = Paint {
            anti_alias: true,
            ..Paint::default()
        };
        paint.set_color_rgba8(r, g, b, 255);
        if matches!(self.shape, Shape::Highlighter(_) | Shape::Mark(..)) {
            match self.style.highlighter_ink {
                HighlighterInk::Realistic => paint.blend_mode = BlendMode::Darken,
                HighlighterInk::Tint => paint.set_color_rgba8(r, g, b, TINT_ALPHA),
            }
        }
        let stroke = Stroke {
            width: self.stroke_width(),
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            ..Stroke::default()
        };
        let id = Transform::identity();
        match &self.shape {
            Shape::Pen(pts) | Shape::Highlighter(pts) => {
                let pts: Vec<Point> = pts.iter().map(local).collect();
                if pts.len() == 1 {
                    // A click: a dot.
                    if let Some(path) =
                        PathBuilder::from_circle(pts[0].0, pts[0].1, stroke.width / 2.0)
                    {
                        pm.fill_path(&path, &paint, FillRule::Winding, id, None);
                    }
                    return;
                }
                if let Some(path) = pen_path(&pts) {
                    pm.stroke_path(&path, &paint, &stroke, id, None);
                }
            }
            Shape::Line(a, b, nodes) => {
                let nodes: Vec<Point> = nodes.iter().map(local).collect();
                if let Some(path) = Curve::new(local(a), local(b), &nodes).path() {
                    pm.stroke_path(&path, &paint, &stroke, id, None);
                }
            }
            Shape::Mark(a, b) => {
                let (a, b) = (local(a), local(b));
                let (x, y) = (a.0.min(b.0), a.1.min(b.1));
                let (w, h) = ((b.0 - a.0).abs(), (b.1 - a.1).abs());
                if let Some(rect) = tiny_skia::Rect::from_xywh(x, y, w, h) {
                    pm.fill_rect(rect, &paint, id, None);
                }
            }
            Shape::Arrow(a, b, nodes) => {
                let nodes: Vec<Point> = nodes.iter().map(local).collect();
                let (a, b) = (local(a), local(b));
                let len = (b.0 - a.0).hypot(b.1 - a.1);
                if len < 1.0 {
                    return;
                }
                let Heads {
                    at_start: head_at_start,
                    at_end: head_at_end,
                    len: head_len,
                    width: head_w,
                } = self.heads(len);
                let curve = Curve::new(a, b, &nodes);
                let last = curve.pieces.len() - 1;
                // The shaft stops inside each head so the round cap doesn't
                // poke out.
                let from = if head_at_start {
                    let (i, t) = curve.reversed().back_from_end(head_len * 0.7);
                    (last - i, 1.0 - t)
                } else {
                    (0, 0.0)
                };
                let to = if head_at_end {
                    curve.back_from_end(head_len * 0.7)
                } else {
                    (last, 1.0)
                };
                if let Some(path) = curve.between(from, to).and_then(|c| c.path()) {
                    pm.stroke_path(&path, &paint, &stroke, id, None);
                }
                let heads = [(head_at_end, curve.clone()), (head_at_start, curve.reversed())];
                for (_, curve) in heads.into_iter().filter(|(on, _)| *on) {
                    if let Some(path) = head_path(&curve, head_len, head_w) {
                        pm.fill_path(&path, &paint, FillRule::Winding, id, None);
                    }
                }
            }
            Shape::Rect(a, b) | Shape::Ellipse(a, b) => {
                let (a, b) = (local(a), local(b));
                let Some(rect) = tiny_skia::Rect::from_ltrb(
                    a.0.min(b.0),
                    a.1.min(b.1),
                    a.0.max(b.0),
                    a.1.max(b.1),
                ) else {
                    return;
                };
                let path = if matches!(self.shape, Shape::Rect(..)) {
                    Some(PathBuilder::from_rect(rect))
                } else {
                    PathBuilder::from_oval(rect)
                };
                if let Some(path) = path {
                    let stroke = Stroke {
                        line_join: LineJoin::Miter,
                        ..stroke
                    };
                    pm.stroke_path(&path, &paint, &stroke, id, None);
                }
            }
            Shape::Blur(a, b) | Shape::Pixelate(a, b) => {
                let r = Rect::from_points(
                    (
                        (a.0 - origin.0 as f32) as f64,
                        (a.1 - origin.1 as f32) as f64,
                    ),
                    (
                        (b.0 - origin.0 as f32) as f64,
                        (b.1 - origin.1 as f32) as f64,
                    ),
                );
                let Some(r) = r.intersect(&Rect {
                    x: 0,
                    y: 0,
                    w: pm.width(),
                    h: pm.height(),
                }) else {
                    return;
                };
                if matches!(self.shape, Shape::Blur(..)) {
                    blur(pm, r, self.style.size.blur_radius());
                } else {
                    let opts = self.style.pixelate;
                    pixelate(pm, r, opts.block, opts.secure.then_some(self.seed));
                }
            }
            Shape::ClipSelect(a, b) | Shape::TextSelect(a, b) => {
                dashed_outline(pm, local(a), local(b))
            }
            Shape::Image(a, b, img) => {
                let (a, b) = (local(a), local(b));
                // Whole pixels, so no soft edge lets anything show through.
                let Some(rect) = tiny_skia::Rect::from_ltrb(
                    a.0.min(b.0).floor(),
                    a.1.min(b.1).floor(),
                    a.0.max(b.0).ceil(),
                    a.1.max(b.1).ceil(),
                ) else {
                    return;
                };
                // Black first, so a picture with transparent parts hides
                // what's under it too.
                let mut fill = Paint::default();
                fill.set_color_rgba8(0, 0, 0, 255);
                pm.fill_rect(rect, &fill, id, None);
                let Some(img) = img else { return };
                let (iw, ih) = (img.pixmap.width() as f32, img.pixmap.height() as f32);
                let (sx, sy) = if img.stretch {
                    (rect.width() / iw, rect.height() / ih)
                } else {
                    // Big enough to cover it, centred, cropping the overhang.
                    let s = (rect.width() / iw).max(rect.height() / ih);
                    (s, s)
                };
                let at = (
                    rect.x() + (rect.width() - iw * sx) / 2.0,
                    rect.y() + (rect.height() - ih * sy) / 2.0,
                );
                fill.shader = tiny_skia::Pattern::new(
                    img.pixmap.as_ref().as_ref(),
                    tiny_skia::SpreadMode::Pad,
                    tiny_skia::FilterQuality::Bicubic,
                    1.0,
                    Transform::from_row(sx, 0.0, 0.0, sy, at.0, at.1),
                );
                pm.fill_rect(rect, &fill, id, None);
            }
            Shape::Erase(a, b, [r, g, b_, alpha]) => {
                let (a, b) = (local(a), local(b));
                // Whole pixels, so no soft edge lets the text show through.
                let Some(rect) = tiny_skia::Rect::from_ltrb(
                    a.0.min(b.0).floor(),
                    a.1.min(b.1).floor(),
                    a.0.max(b.0).ceil(),
                    a.1.max(b.1).ceil(),
                ) else {
                    return;
                };
                let color = tiny_skia::PremultipliedColorU8::from_rgba(*r, *g, *b_, *alpha).map_or(
                    tiny_skia::Color::BLACK,
                    |c| {
                        let c = c.demultiply();
                        tiny_skia::Color::from_rgba8(c.red(), c.green(), c.blue(), c.alpha())
                    },
                );
                let mut fill = Paint::default();
                fill.set_color(color);
                pm.fill_rect(rect, &fill, id, None);
            }
            Shape::Text(at, text, boxed) => {
                let wrap = boxed.map(|b| b.0);
                draw_text(pm, self.style.text, text, local(at), wrap, self.style.color);
            }
            Shape::Step(c, n, tip) => {
                let c = local(c);
                let r = self.step_radius();
                if let Some(circle) = step_path(c, r, tip.as_ref().map(local)) {
                    pm.fill_path(&circle, &paint, FillRule::Winding, id, None);
                    // A thin ring keeps it visible on its own colour.
                    if self.style.step.border {
                        let mut ring = Paint {
                            anti_alias: true,
                            ..Paint::default()
                        };
                        ring.set_color_rgba8(255, 255, 255, 200);
                        let stroke = Stroke {
                            width: 1.5,
                            ..Stroke::default()
                        };
                        pm.stroke_path(&circle, &ring, &stroke, id, None);
                    }
                }
                // Dark digits on light colours, white ones otherwise.
                let luma = 0.299 * r_ as f32 + 0.587 * g_ as f32 + 0.114 * b_ as f32;
                let mut text = Paint {
                    anti_alias: true,
                    ..Paint::default()
                };
                if luma > 170.0 {
                    text.set_color_rgba8(0x1c, 0x1c, 0x1e, 255);
                } else {
                    text.set_color_rgba8(255, 255, 255, 255);
                }
                draw_number(pm, *n, c, r, self.style.step.number as f32, &text);
            }
            Shape::Clip(img, (x, y)) => {
                let (x, y) = (x - origin.0, y - origin.1);
                let (w, h) = (img.width() as f32, img.height() as f32);
                // A soft shadow: rings of faint black, a little lower down.
                let shadow = if self.style.clip_shadow { CLIP_SHADOW } else { 0 };
                for i in 1..=shadow {
                    let grow = i as f32;
                    if let Some(r) = tiny_skia::Rect::from_xywh(
                        x as f32 - grow,
                        y as f32 - grow + 2.0,
                        w + 2.0 * grow,
                        h + 2.0 * grow,
                    ) {
                        let mut paint = Paint::default();
                        paint.set_color_rgba8(0, 0, 0, 14);
                        pm.fill_rect(r, &paint, id, None);
                    }
                }
                pm.draw_pixmap(
                    x,
                    y,
                    img.as_ref().as_ref(),
                    &PixmapPaint::default(),
                    id,
                    None,
                );
                // A faint edge goes with the shadow; without it the clip is
                // just its pixels.
                if shadow == 0 {
                    return;
                }
                if let Some(r) =
                    tiny_skia::Rect::from_xywh(x as f32 + 0.5, y as f32 + 0.5, w - 1.0, h - 1.0)
                {
                    let mut paint = Paint::default();
                    paint.set_color_rgba8(0, 0, 0, 90);
                    let stroke = Stroke {
                        width: 1.0,
                        ..Stroke::default()
                    };
                    pm.stroke_path(&PathBuilder::from_rect(r), &paint, &stroke, id, None);
                }
            }
        }
    }
}

/// A step's marker: the circle around `c`, drawn out to a point at `tip`
/// if it has one, with straight sides that meet the circle at a tangent.
fn step_path(c: Point, r: f32, tip: Option<Point>) -> Option<Path> {
    let Some(t) = tip.filter(|t| (t.0 - c.0).hypot(t.1 - c.1) > r) else {
        return PathBuilder::from_circle(c.0, c.1, r);
    };
    let toward = (t.1 - c.1).atan2(t.0 - c.0);
    // Either side of the line to the tip, where its sides touch the circle.
    let spread = (r / (t.0 - c.0).hypot(t.1 - c.1)).acos();
    let at = |a: f32| (c.0 + r * a.cos(), c.1 + r * a.sin());
    let mut pb = PathBuilder::new();
    pb.move_to(t.0, t.1);
    let start = toward + spread;
    let first = at(start);
    pb.line_to(first.0, first.1);
    // Round the far side back to the other tangent, in Bézier curves of
    // at most a quarter turn each.
    let sweep = std::f32::consts::TAU - 2.0 * spread;
    let n = (sweep / std::f32::consts::FRAC_PI_2).ceil() as usize;
    let turn = sweep / n as f32;
    let k = r * 4.0 / 3.0 * (turn / 4.0).tan();
    for i in 0..n {
        let (a0, a1) = (start + turn * i as f32, start + turn * (i + 1) as f32);
        let (p0, p1) = (at(a0), at(a1));
        pb.cubic_to(
            p0.0 - k * a0.sin(),
            p0.1 + k * a0.cos(),
            p1.0 + k * a1.sin(),
            p1.1 - k * a1.cos(),
            p1.0,
            p1.1,
        );
    }
    pb.close();
    pb.finish()
}

/// The font for step numbers: egui's bundled Ubuntu.
static FONT: LazyLock<Option<ttf_parser::Face<'static>>> =
    LazyLock::new(|| ttf_parser::Face::parse(epaint_default_fonts::UBUNTU_LIGHT, 0).ok());

/// Turns glyph outlines (font units, y up) into a tiny-skia path.
struct GlyphPath<'a> {
    pb: &'a mut PathBuilder,
    scale: f32,
    origin: Point,
}

impl GlyphPath<'_> {
    fn at(&self, x: f32, y: f32) -> Point {
        (
            self.origin.0 + x * self.scale,
            self.origin.1 - y * self.scale,
        )
    }
}

impl ttf_parser::OutlineBuilder for GlyphPath<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        let p = self.at(x, y);
        self.pb.move_to(p.0, p.1);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.at(x, y);
        self.pb.line_to(p.0, p.1);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let (c, p) = (self.at(x1, y1), self.at(x, y));
        self.pb.quad_to(c.0, c.1, p.0, p.1);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (c1, c2, p) = (self.at(x1, y1), self.at(x2, y2), self.at(x, y));
        self.pb.cubic_to(c1.0, c1.1, c2.0, c2.1, p.0, p.1);
    }
    fn close(&mut self) {
        self.pb.close();
    }
}

/// Draws `n` centred on `c`, sized to fit a circle of radius `r`.
/// Writes `n` centred on `c`, its digits `height` pixels tall, or less to
/// fit inside a circle of radius `r`.
fn draw_number(pm: &mut Pixmap, n: u32, c: Point, r: f32, height: f32, paint: &Paint) {
    let Some(face) = FONT.as_ref() else { return };
    let digits = n.to_string();
    let glyphs: Vec<_> = digits
        .chars()
        .filter_map(|ch| face.glyph_index(ch))
        .collect();
    let units = face.units_per_em() as f32;
    let advance: f32 = glyphs
        .iter()
        .map(|&g| face.glyph_hor_advance(g).unwrap_or(0) as f32)
        .sum();
    let cap = face.capital_height().map_or(units * 0.7, |h| h as f32);
    let height = height.min(r * 2.0 * STEP_NUMBER_FIT);
    let scale = (height / cap).min(r * 1.45 / advance.max(1.0));
    let mut pb = PathBuilder::new();
    let mut x = c.0 - advance * scale / 2.0;
    for g in glyphs {
        let mut out = GlyphPath {
            pb: &mut pb,
            scale,
            origin: (x, c.1 + cap * scale / 2.0),
        };
        face.outline_glyph(g, &mut out);
        x += face.glyph_hor_advance(g).unwrap_or(0) as f32 * scale;
    }
    let Some(path) = pb.finish() else { return };
    let id = Transform::identity();
    pm.fill_path(&path, paint, FillRule::Winding, id, None);
    // The bundled face is light; a thin stroke makes it read as bold.
    let stroke = Stroke {
        width: (cap * scale * 0.08).max(1.0),
        line_join: LineJoin::Round,
        ..Stroke::default()
    };
    pm.stroke_path(&path, paint, &stroke, id, None);
}

/// Runs `f` with the face of `font`: an installed font, or the bundled one
/// (also when an installed one can't be read). `None` with neither.
fn with_face<R>(font: Option<u16>, f: impl FnOnce(&ttf_parser::Face) -> R) -> Option<R> {
    if let Some((data, index)) = font.and_then(crate::fonts::data)
        && let Ok(face) = ttf_parser::Face::parse(&data, index)
    {
        return Some(f(&face));
    }
    FONT.as_ref().map(f)
}

/// The height of a line of text in `face` at `px` pixels per em.
fn line_height(face: &ttf_parser::Face, px: f32) -> f32 {
    let units = face.units_per_em() as f32;
    let line = face.ascender() as f32 - face.descender() as f32 + face.line_gap() as f32;
    (line / units * px).ceil()
}

/// Text laid out in its font.
struct TextLayout {
    /// Every glyph's outline.
    path: Option<Path>,
    /// Width and height of the lines.
    size: (f32, f32),
    /// The caret after the last character: its top, and height.
    caret: (Point, f32),
}

/// The height of a line of text.
fn text_line(opts: TextOptions) -> f32 {
    layout_text(opts, "", (0.0, 0.0), None).caret.1
}

/// Where text clicked at `p` goes: with the click in the middle of its first
/// line.
fn text_at_click(opts: TextOptions, p: Point) -> Point {
    (p.0, (p.1 - text_line(opts) / 2.0).round())
}

/// Text in the box dragged from `p` to `q`, or (if that's too narrow for
/// it) clicked at `p`.
fn text_in_box(opts: TextOptions, p: Point, q: Point) -> Shape {
    let pad = TEXT_PAD as f32;
    let (x0, y0) = (p.0.min(q.0), p.1.min(q.1));
    let (w, h) = ((q.0 - p.0).abs() - 2.0 * pad, (q.1 - p.1).abs() - 2.0 * pad);
    if w < opts.size as f32 {
        return Shape::Text(text_at_click(opts, p), String::new(), None);
    }
    Shape::Text(((x0 + pad).round(), (y0 + pad).round()), String::new(), Some((w, h.max(0.0))))
}

/// Splits a line of text where it gets wider than `width`: after the last
/// space that fits, or mid-word for a word wider than the line. Spaces at a
/// break stay at the end of the line before it.
fn wrap_line(line: &str, width: f32, advance: impl Fn(char) -> f32) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut x = 0.0;
    let mut space_end = None;
    for (i, ch) in line.char_indices() {
        let w = advance(ch);
        if ch != ' ' && x + w > width && i > start {
            let end = space_end.filter(|&e| e > start).unwrap_or(i);
            lines.push(&line[start..end]);
            start = end;
            x = line[start..i].chars().map(&advance).sum();
            space_end = None;
        }
        x += w;
        if ch == ' ' {
            space_end = Some(i + 1);
        }
    }
    lines.push(&line[start..]);
    lines
}

/// Lays `text` out in the font and size of `opts` with its top-left at
/// `at`, one line per `\n`, and wrapped to `wrap` pixels wide if given.
fn layout_text(opts: TextOptions, text: &str, at: Point, wrap: Option<f32>) -> TextLayout {
    let px = opts.size as f32;
    let laid_out = with_face(opts.font, |face| {
        let line = line_height(face, px);
        let scale = px / face.units_per_em() as f32;
        // Characters the font lacks show as its empty box.
        let glyph = |ch: char| face.glyph_index(ch).unwrap_or(ttf_parser::GlyphId(0));
        let advance = |ch: char| face.glyph_hor_advance(glyph(ch)).unwrap_or(0) as f32 * scale;
        let lines: Vec<&str> = match wrap {
            Some(width) => text.split('\n').flat_map(|l| wrap_line(l, width, advance)).collect(),
            None => text.split('\n').collect(),
        };
        // Lines share the leading evenly above and below.
        let ascent = face.ascender() as f32 * scale + face.line_gap() as f32 * scale / 2.0;
        let mut pb = PathBuilder::new();
        let (mut width, mut x) = (0.0f32, 0.0);
        for (i, text) in lines.iter().enumerate() {
            x = 0.0;
            let baseline = at.1 + i as f32 * line + ascent;
            for ch in text.chars() {
                let mut out = GlyphPath {
                    pb: &mut pb,
                    scale,
                    origin: (at.0 + x, baseline),
                };
                face.outline_glyph(glyph(ch), &mut out);
                x += advance(ch);
            }
            width = width.max(x);
        }
        let last_top = at.1 + (lines.len() - 1) as f32 * line;
        TextLayout {
            path: pb.finish(),
            size: (width.ceil(), line * lines.len() as f32),
            caret: ((at.0 + x, last_top), line),
        }
    });
    laid_out.unwrap_or(TextLayout {
        path: None,
        size: (0.0, px * 1.2),
        caret: (at, px * 1.2),
    })
}

/// Draws `text` with its top-left at `at` in `color`, edged (if `opts` says
/// so) in black or white, whichever stands out from it, to read over anything.
fn draw_text(
    pm: &mut Pixmap,
    opts: TextOptions,
    text: &str,
    at: Point,
    wrap: Option<f32>,
    [r, g, b]: [u8; 3],
) {
    let Some(path) = layout_text(opts, text, at, wrap).path else {
        return;
    };
    let px = opts.size as f32;
    let id = Transform::identity();
    let round = |width: f32| Stroke {
        width,
        line_join: LineJoin::Round,
        ..Stroke::default()
    };
    if opts.outline {
        let luma = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
        let mut edge = Paint {
            anti_alias: true,
            ..Paint::default()
        };
        if luma > 150.0 {
            edge.set_color_rgba8(0x1c, 0x1c, 0x1e, 220);
        } else {
            edge.set_color_rgba8(255, 255, 255, 230);
        }
        pm.stroke_path(&path, &edge, &round((px * 0.16).max(3.0)), id, None);
    }
    let mut paint = Paint {
        anti_alias: true,
        ..Paint::default()
    };
    paint.set_color_rgba8(r, g, b, 255);
    pm.fill_path(&path, &paint, FillRule::Winding, id, None);
    // The bundled face is light; a thin stroke gives it a regular weight.
    if opts.font.is_none() {
        pm.stroke_path(&path, &paint, &round(px * 0.035), id, None);
    }
}

/// A dashed box from `a` to `b` (local pixels) that shows on light and dark
/// screens alike.
fn dashed_outline(pm: &mut Pixmap, a: Point, b: Point) {
    let Some(rect) = tiny_skia::Rect::from_ltrb(
        a.0.min(b.0).round() + 0.5,
        a.1.min(b.1).round() + 0.5,
        a.0.max(b.0).round() - 0.5,
        a.1.max(b.1).round() - 0.5,
    ) else {
        return;
    };
    let path = PathBuilder::from_rect(rect);
    for (color, offset) in [(0u8, 0.0), (255u8, 4.0)] {
        let mut paint = Paint::default();
        paint.set_color_rgba8(color, color, color, 230);
        let stroke = Stroke {
            width: 1.0,
            dash: StrokeDash::new(vec![4.0, 4.0], offset),
            ..Stroke::default()
        };
        pm.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    }
}

fn lerp(a: Point, b: Point, t: f32) -> Point {
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
}

/// `n` nodes spaced evenly along the straight line from `a` to `b`.
fn straight_nodes(a: Point, b: Point, n: u8) -> Vec<Point> {
    (1..=n).map(|i| lerp(a, b, i as f32 / (n as f32 + 1.0))).collect()
}

/// A line's or an arrow's course: cubic Bézier pieces end to end, each
/// its start, two controls and its end.
#[derive(Debug, Clone)]
struct Curve {
    pieces: Vec<[Point; 4]>,
    /// Drawn as curves, rather than one straight line.
    bent: bool,
}

impl Curve {
    /// From `a` to `b` through `nodes`: one in the middle makes a single
    /// arc through it, more a smooth (Catmull-Rom) curve through them all.
    fn new(a: Point, b: Point, nodes: &[Point]) -> Self {
        let pieces = match nodes {
            [] => vec![[a, lerp(a, b, 1.0 / 3.0), lerp(a, b, 2.0 / 3.0), b]],
            // The quadratic through it halfway along, as a cubic.
            [mid] => {
                let m = lerp(a, b, 0.5);
                let c = (2.0 * mid.0 - m.0, 2.0 * mid.1 - m.1);
                vec![[a, lerp(a, c, 2.0 / 3.0), lerp(b, c, 2.0 / 3.0), b]]
            }
            _ => {
                let pts: Vec<Point> = [a].into_iter().chain(nodes.iter().copied()).chain([b]).collect();
                let at = |i: usize| pts[i.min(pts.len() - 1)];
                (0..pts.len() - 1)
                    .map(|i| {
                        let (p0, p1, p2, p3) = (at(i.saturating_sub(1)), at(i), at(i + 1), at(i + 2));
                        [
                            p1,
                            (p1.0 + (p2.0 - p0.0) / 6.0, p1.1 + (p2.1 - p0.1) / 6.0),
                            (p2.0 - (p3.0 - p1.0) / 6.0, p2.1 - (p3.1 - p1.1) / 6.0),
                            p2,
                        ]
                    })
                    .collect()
            }
        };
        Self {
            pieces,
            bent: !nodes.is_empty(),
        }
    }

    fn end(&self) -> Point {
        self.pieces[self.pieces.len() - 1][3]
    }

    /// The same course, from the other end.
    fn reversed(&self) -> Self {
        Self {
            pieces: self.pieces.iter().rev().map(|&[a, b, c, d]| [d, c, b, a]).collect(),
            bent: self.bent,
        }
    }

    /// Where the point `dist` short of the end is: which piece, and how far
    /// along it (as `t`).
    fn back_from_end(&self, dist: f32) -> (usize, f32) {
        let tip = self.end();
        let away = |p: Point| (tip.0 - p.0).hypot(tip.1 - p.1) > dist;
        for (i, piece) in self.pieces.iter().enumerate().rev() {
            if away(piece[0]) {
                let (mut lo, mut hi) = (0.0f32, 1.0f32);
                for _ in 0..24 {
                    let t = (lo + hi) / 2.0;
                    if away(cubic(piece, t)) {
                        lo = t;
                    } else {
                        hi = t;
                    }
                }
                return (i, lo);
            }
        }
        (0, 0.0)
    }

    fn at(&self, (i, t): (usize, f32)) -> Point {
        cubic(&self.pieces[i], t)
    }

    /// The part of it between two places along it, if there's any.
    fn between(&self, from: (usize, f32), to: (usize, f32)) -> Option<Self> {
        if from >= to {
            return None;
        }
        let pieces = (from.0..=to.0)
            .filter_map(|i| {
                let lo = if i == from.0 { from.1 } else { 0.0 };
                let hi = if i == to.0 { to.1 } else { 1.0 };
                (lo < hi).then(|| {
                    let left = split(self.pieces[i], hi).0;
                    split(left, lo / hi).1
                })
            })
            .collect();
        Some(Self {
            pieces,
            bent: self.bent,
        })
    }

    fn path(&self) -> Option<Path> {
        let mut pb = PathBuilder::new();
        let start = self.pieces.first()?[0];
        pb.move_to(start.0, start.1);
        if self.bent {
            for [_, c1, c2, end] in &self.pieces {
                pb.cubic_to(c1.0, c1.1, c2.0, c2.1, end.0, end.1);
            }
        } else {
            let end = self.end();
            pb.line_to(end.0, end.1);
        }
        pb.finish()
    }
}

/// A point on a cubic Bézier piece.
fn cubic(&[a, b, c, d]: &[Point; 4], t: f32) -> Point {
    let u = 1.0 - t;
    let (wa, wb, wc, wd) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    (
        wa * a.0 + wb * b.0 + wc * c.0 + wd * d.0,
        wa * a.1 + wb * b.1 + wc * c.1 + wd * d.1,
    )
}

/// A cubic Bézier piece cut in two at `t`.
fn split([a, b, c, d]: [Point; 4], t: f32) -> ([Point; 4], [Point; 4]) {
    let (ab, bc, cd) = (lerp(a, b, t), lerp(b, c, t), lerp(c, d, t));
    let (abc, bcd) = (lerp(ab, bc, t), lerp(bc, cd, t));
    let m = lerp(abc, bcd, t);
    ([a, ab, abc, m], [m, bcd, cd, d])
}

fn arrow_head(stroke: f32) -> (f32, f32) {
    let len = stroke * 3.0 + 10.0;
    (len, len * 0.85)
}

/// An arrowhead `len` long and `width` wide at the end of `curve`. It sits
/// on the curve, pointing from where it meets the shaft to the tip.
fn head_path(curve: &Curve, len: f32, width: f32) -> Option<Path> {
    let b = curve.end();
    let meets = curve.at(curve.back_from_end(len));
    let (dx, dy) = (b.0 - meets.0, b.1 - meets.1);
    let d = dx.hypot(dy).max(f32::EPSILON);
    let (dir, perp) = ((dx / d, dy / d), (-dy / d, dx / d));
    let base = (b.0 - dir.0 * len, b.1 - dir.1 * len);
    let mut head = PathBuilder::new();
    head.move_to(b.0, b.1);
    head.line_to(base.0 + perp.0 * width / 2.0, base.1 + perp.1 * width / 2.0);
    head.line_to(base.0 - perp.0 * width / 2.0, base.1 - perp.1 * width / 2.0);
    head.close();
    head.finish()
}

/// A freehand stroke through `pts`, smoothed through the midpoints of
/// successive samples.
fn pen_path(pts: &[Point]) -> Option<Path> {
    let mut pb = PathBuilder::new();
    pb.move_to(pts.first()?.0, pts[0].1);
    for w in pts.windows(2).skip(1) {
        let mid = ((w[0].0 + w[1].0) / 2.0, (w[0].1 + w[1].1) / 2.0);
        pb.quad_to(w[0].0, w[0].1, mid.0, mid.1);
    }
    let last = pts[pts.len() - 1];
    pb.line_to(last.0, last.1);
    pb.finish()
}

/// Which ends of an arrow have heads, and their size.
struct Heads {
    at_start: bool,
    at_end: bool,
    len: f32,
    width: f32,
}

fn snap_45(a: Point, p: Point) -> Point {
    let (dx, dy) = (p.0 - a.0, p.1 - a.1);
    let len = dx.hypot(dy);
    let angle = (dy.atan2(dx) / std::f32::consts::FRAC_PI_4).round() * std::f32::consts::FRAC_PI_4;
    (a.0 + len * angle.cos(), a.1 + len * angle.sin())
}

fn square(a: Point, p: Point) -> Point {
    let side = (p.0 - a.0).abs().max((p.1 - a.1).abs());
    (
        a.0 + side.copysign(p.0 - a.0),
        a.1 + side.copysign(p.1 - a.1),
    )
}

/// Approximates a gaussian blur with three box blurs.
fn blur(pm: &mut Pixmap, r: Rect, radius: usize) {
    let (w, h) = (r.w as usize, r.h as usize);
    if w == 0 || h == 0 {
        return;
    }
    let stride = pm.width() as usize * 4;
    let x0 = r.x as usize * 4;
    let data = pm.data_mut();
    // Channels as u32, row by row, so every pass runs through memory in order.
    let mut a: Vec<u32> = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        let row = (r.y as usize + y) * stride + x0;
        a.extend(data[row..row + w * 4].iter().map(|&v| v as u32));
    }
    let mut b = vec![0u32; w * h * 4];
    for _ in 0..3 {
        box_rows(&a, &mut b, w, radius);
        box_columns(&b, &mut a, w, h, radius);
    }
    for y in 0..h {
        let row = (r.y as usize + y) * stride + x0;
        for (d, &s) in data[row..row + w * 4]
            .iter_mut()
            .zip(&a[y * w * 4..(y + 1) * w * 4])
        {
            *d = s as u8;
        }
    }
}

/// Multiplier for dividing a box sum by `n` with a shift: `(sum * m) >> 16`.
fn reciprocal(n: usize) -> u32 {
    ((65536 + n / 2) / n) as u32
}

/// Runs `f(first_row, rows)` on bands of `dst`'s rows across the CPU cores.
fn in_bands(dst: &mut [u32], row_len: usize, f: impl Fn(usize, &mut [u32]) + Sync) {
    let h = dst.len() / row_len;
    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(8);
    // Not worth starting threads for small areas.
    if threads == 1 || dst.len() < 64 * 1024 {
        return f(0, dst);
    }
    let band = h.div_ceil(threads);
    std::thread::scope(|scope| {
        for (i, rows) in dst.chunks_mut(band * row_len).enumerate() {
            let f = &f;
            scope.spawn(move || f(i * band, rows));
        }
    });
}

/// Horizontal box blur of each row, with clamped edges.
fn box_rows(src: &[u32], dst: &mut [u32], w: usize, radius: usize) {
    let r = radius.min(w - 1);
    let m = reciprocal(2 * r + 1);
    let px = |i: isize| i.clamp(0, w as isize - 1) as usize;
    in_bands(dst, w * 4, |y0, band| {
        let src = &src[y0 * w * 4..y0 * w * 4 + band.len()];
        for (s, d) in src.chunks_exact(w * 4).zip(band.chunks_exact_mut(w * 4)) {
            let at =
                |x: usize| -> [u32; 4] { [s[x * 4], s[x * 4 + 1], s[x * 4 + 2], s[x * 4 + 3]] };
            let mut sum = [0u32; 4];
            for i in -(r as isize)..=r as isize {
                let p = at(px(i));
                for c in 0..4 {
                    sum[c] += p[c];
                }
            }
            for (x, out) in d.chunks_exact_mut(4).enumerate() {
                for c in 0..4 {
                    out[c] = (sum[c] * m + 32768) >> 16;
                }
                // In the middle of the row neither end needs clamping.
                let (add, sub) = if x >= r && x + r + 1 < w {
                    (at(x + r + 1), at(x - r))
                } else {
                    (
                        at(px((x + r + 1) as isize)),
                        at(px(x as isize - r as isize)),
                    )
                };
                for c in 0..4 {
                    sum[c] = sum[c] + add[c] - sub[c];
                }
            }
        }
    });
}

/// Vertical box blur, done a whole row at a time with a running sum per
/// column.
fn box_columns(src: &[u32], dst: &mut [u32], w: usize, h: usize, radius: usize) {
    let r = radius.min(h - 1) as isize;
    let m = reciprocal(2 * r as usize + 1);
    let row_len = w * 4;
    let row = |y: isize| {
        let y = y.clamp(0, h as isize - 1) as usize;
        &src[y * row_len..(y + 1) * row_len]
    };
    in_bands(dst, row_len, |y0, band| {
        let y0 = y0 as isize;
        let mut sums = vec![0u32; row_len];
        for i in y0 - r..=y0 + r {
            for (s, &v) in sums.iter_mut().zip(row(i)) {
                *s += v;
            }
        }
        for (y, out) in (y0..).zip(band.chunks_exact_mut(row_len)) {
            for (d, &s) in out.iter_mut().zip(&sums) {
                *d = (s * m + 32768) >> 16;
            }
            for ((s, &a), &b) in sums.iter_mut().zip(row(y + r + 1)).zip(row(y - r)) {
                *s = *s + a - b;
            }
        }
    });
}

/// Replaces each block with its average colour. With a `seed` (secure), the
/// averages are shuffled between the blocks and each is nudged by a little
/// noise, so the area keeps its colours but no block says anything about
/// what was under it.
fn pixelate(pm: &mut Pixmap, r: Rect, block: u32, seed: Option<u64>) {
    let stride = pm.width() as usize;
    let data = pm.data_mut();
    // Each block's pixel span and average colour.
    let mut blocks = Vec::new();
    for by in (0..r.h).step_by(block as usize) {
        for bx in (0..r.w).step_by(block as usize) {
            let (bw, bh) = (block.min(r.w - bx) as usize, block.min(r.h - by) as usize);
            let (x0, y0) = (r.x as usize + bx as usize, r.y as usize + by as usize);
            let mut sum = [0u32; 4];
            for y in y0..y0 + bh {
                for x in x0..x0 + bw {
                    let i = (y * stride + x) * 4;
                    (0..4).for_each(|c| sum[c] += data[i + c] as u32);
                }
            }
            let n = (bw * bh) as u32;
            blocks.push(((x0, y0, bw, bh), sum.map(|s| (s / n) as u8)));
        }
    }
    let mut colors: Vec<[u8; 4]> = blocks.iter().map(|b| b.1).collect();
    if let Some(seed) = seed {
        let mut rng = fastrand::Rng::with_seed(seed);
        rng.shuffle(&mut colors);
        for c in &mut colors {
            // Premultiplied: colour can't exceed alpha.
            let alpha = c[3] as i32;
            for v in &mut c[..3] {
                *v = (*v as i32 + rng.i32(-14..=14)).clamp(0, alpha) as u8;
            }
        }
    }
    for (((x0, y0, bw, bh), _), color) in blocks.into_iter().zip(colors) {
        for y in y0..y0 + bh {
            for x in x0..x0 + bw {
                let i = (y * stride + x) * 4;
                data[i..i + 4].copy_from_slice(&color);
            }
        }
    }
}

/// Copies `r` (local pixels) from one same-sized pixmap to another.
fn copy_rect(src: &Pixmap, dst: &mut Pixmap, r: Rect) {
    let stride = src.width() as usize * 4;
    let (x0, x1) = (r.x as usize * 4, r.right() as usize * 4);
    let (s, d) = (src.data(), dst.data_mut());
    for y in r.y as usize..r.bottom() as usize {
        d[y * stride + x0..y * stride + x1].copy_from_slice(&s[y * stride + x0..y * stride + x1]);
    }
}

fn union(a: Option<Rect>, b: Rect) -> Rect {
    let Some(a) = a else { return b };
    let (x0, y0) = (a.x.min(b.x), a.y.min(b.y));
    let (x1, y1) = (a.right().max(b.right()), a.bottom().max(b.bottom()));
    Rect {
        x: x0,
        y: y0,
        w: (x1 - x0) as u32,
        h: (y1 - y0) as u32,
    }
}

/// An annotation lifted out to be changed by dragging.
struct Edit {
    /// Where it was in the list, so it goes back at the same depth.
    index: usize,
    /// How it was, for undo and cancel.
    original: Annotation,
    kind: EditKind,
}

#[derive(Clone, Copy)]
enum EditKind {
    /// Moving a line's or an arrow's node (which one), or a step's point.
    Bend(usize),
    /// Moving a clip, held at `grab` from its top-left corner.
    Move { grab: Point },
}

pub struct Canvas {
    /// The selected region, in global pixels.
    pub rect: Rect,
    base: Pixmap,
    committed: Pixmap,
    frame: Pixmap,
    annotations: Vec<Annotation>,
    /// Earlier and undone states of `annotations`.
    undo: Vec<Vec<Annotation>>,
    redo: Vec<Vec<Annotation>>,
    active: Option<Annotation>,
    /// The annotation being bent or moved, if any.
    editing: Option<Edit>,
    /// Where the active annotation was last drawn (local pixels).
    active_area: Option<Rect>,
    /// Area of `frame` changed since the last `take_dirty` (local pixels).
    dirty: Option<Rect>,
    /// The active annotation moved but `frame` doesn't show it yet. Drags
    /// only update the shape; it's redrawn once per frame in `take_dirty`,
    /// however many mouse events arrive in between.
    stale: bool,
    /// The last ring drawn around a hovered annotation, with what it was for.
    ring: Mutex<Option<Ring>>,
    /// The active annotation is text being typed.
    typing: bool,
}

fn to_pixmap(img: RgbaImage) -> Pixmap {
    let (w, h) = img.dimensions();
    let size = tiny_skia::IntSize::from_wh(w.max(1), h.max(1)).expect("non-zero size");
    // Captured pixels are opaque or fully transparent, so they're already
    // valid premultiplied RGBA.
    Pixmap::from_vec(img.into_raw(), size)
        .unwrap_or_else(|| Pixmap::new(w.max(1), h.max(1)).expect("size"))
}

impl Canvas {
    pub fn new(rect: Rect, base: RgbaImage) -> Self {
        let base = to_pixmap(base);
        Self {
            rect,
            committed: base.clone(),
            frame: base.clone(),
            base,
            annotations: Vec::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            active: None,
            editing: None,
            stale: false,
            ring: Mutex::new(None),
            typing: false,
            active_area: None,
            dirty: Some(Rect {
                x: 0,
                y: 0,
                w: rect.w,
                h: rect.h,
            }),
        }
    }

    /// Moves or resizes the region, keeping annotations where they are on screen.
    pub fn set_region(&mut self, rect: Rect, base: RgbaImage) {
        self.rect = rect;
        self.base = to_pixmap(base);
        self.replay();
    }

    fn origin(&self) -> (i32, i32) {
        (self.rect.x, self.rect.y)
    }

    fn bounds(&self) -> Rect {
        Rect {
            x: 0,
            y: 0,
            w: self.frame.width(),
            h: self.frame.height(),
        }
    }

    fn local(&self, r: Rect) -> Option<Rect> {
        Rect {
            x: r.x - self.rect.x,
            y: r.y - self.rect.y,
            ..r
        }
        .intersect(&self.bounds())
    }

    /// Redraws everything from the base image.
    fn replay(&mut self) {
        let origin = self.origin();
        self.committed = self.base.clone();
        for a in &self.annotations {
            a.draw(&mut self.committed, origin);
        }
        self.frame = self.committed.clone();
        self.active_area = None;
        if let Some(a) = &self.active {
            a.draw_active(&mut self.frame, origin);
            if self.typing {
                a.draw_typing(&mut self.frame, origin);
            }
            self.active_area = self.local(a.bounds());
        }
        self.dirty = Some(self.bounds());
    }

    /// Redraws the active annotation over the committed image.
    fn refresh_active(&mut self) {
        self.stale = false;
        let new_area = self.active.as_ref().and_then(|a| self.local(a.bounds()));
        let area = match (self.active_area, new_area) {
            (Some(old), Some(new)) => Some(union(Some(old), new)),
            (a, b) => a.or(b),
        };
        if let Some(area) = area {
            copy_rect(&self.committed, &mut self.frame, area);
            if let Some(a) = &self.active {
                a.draw_active(&mut self.frame, (self.rect.x, self.rect.y));
                if self.typing {
                    a.draw_typing(&mut self.frame, (self.rect.x, self.rect.y));
                }
            }
            self.dirty = Some(union(self.dirty, area));
        }
        self.active_area = new_area;
    }

    pub fn begin(&mut self, mut annotation: Annotation) {
        match &mut annotation.shape {
            Shape::Step(_, n, _) => {
                let steps = self
                    .annotations
                    .iter()
                    .filter(|a| matches!(a.shape, Shape::Step(..)))
                    .count();
                *n = steps as u32 + 1;
            }
            // The colour showing where the drag starts, annotations included.
            Shape::Erase(start, _, color) => {
                let (x, y) = (
                    start.0.floor() as i32 - self.rect.x,
                    start.1.floor() as i32 - self.rect.y,
                );
                if let Some(p) = self.committed.pixel(x.max(0) as u32, y.max(0) as u32) {
                    *color = [p.red(), p.green(), p.blue(), p.alpha()];
                }
            }
            _ => {}
        }
        self.active = Some(annotation);
        self.refresh_active();
    }

    /// Lifts annotation `i` out of the committed image to edit it in place.
    fn begin_edit(&mut self, i: usize, kind: EditKind) {
        let a = self.annotations.remove(i);
        self.editing = Some(Edit {
            index: i,
            original: a.clone(),
            kind,
        });
        self.active = Some(a);
        self.replay();
    }

    /// Picks up the topmost shape drawn with `tool` with a node within
    /// `radius` of `p`, by its nearest one, to reshape it by dragging.
    /// Returns whether there was one.
    pub fn begin_bend(&mut self, p: Point, radius: f32, tool: Tool) -> bool {
        let nearest = |a: &Annotation| {
            a.bend_nodes(tool)
                .into_iter()
                .map(|n| (n.0 - p.0).hypot(n.1 - p.1))
                .enumerate()
                .filter(|&(_, d)| d <= radius)
                .min_by(|x, y| x.1.total_cmp(&y.1))
                .map(|(node, _)| node)
        };
        let Some((i, node)) = self
            .annotations
            .iter()
            .enumerate()
            .rev()
            .find_map(|(i, a)| nearest(a).map(|node| (i, node)))
        else {
            return false;
        };
        self.begin_edit(i, EditKind::Bend(node));
        true
    }

    /// The topmost annotation under `p` that `tool` can move, if any.
    fn movable_index_at(&self, p: Point, tool: Tool) -> Option<usize> {
        self.annotations
            .iter()
            .rposition(|a| a.movable_under(p, tool).is_some())
    }

    /// Whether `tool` can pick something up at `p`.
    pub fn movable_at(&self, p: Point, tool: Tool) -> bool {
        self.movable_index_at(p, tool).is_some()
    }

    /// Picks up the topmost annotation under `p` that `tool` can move (a
    /// clip, an area, or a pen stroke), to move it by dragging. Returns
    /// whether there was one.
    pub fn begin_move(&mut self, p: Point, tool: Tool) -> bool {
        let Some(i) = self.movable_index_at(p, tool) else {
            return false;
        };
        let r = self.annotations[i].movable_under(p, tool).expect("movable");
        let grab = (p.0 - r.x as f32, p.1 - r.y as f32);
        self.begin_edit(i, EditKind::Move { grab });
        true
    }

    /// The topmost annotation under `p`, whatever the tool, and where it is
    /// on screen.
    fn deletable_at(&self, p: Point) -> Option<(usize, Rect)> {
        self.annotations
            .iter()
            .enumerate()
            .rev()
            .find_map(|(i, a)| a.hit(p).map(|r| (i, r)))
    }

    /// What to outline around the annotation a right-click at `p` would
    /// delete: a ring that follows a stroke, line or arrow, or a box.
    pub fn deletable_highlight(&self, p: Point) -> Option<Highlight> {
        let (i, rect) = self.deletable_at(p)?;
        let a = &self.annotations[i];
        // The same picture while it's the same annotation in the same place.
        let mut hasher = std::hash::DefaultHasher::new();
        (a.seed, rect.x, rect.y, rect.w, rect.h).hash(&mut hasher);
        let id = hasher.finish();
        let mut cached = self.ring.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((key, at, image)) = &*cached
            && *key == id
        {
            return Some(Highlight::Ring {
                id,
                at: *at,
                image: image.clone(),
            });
        }
        let Some((at, image)) = a.ring() else {
            return Some(Highlight::Box(rect));
        };
        let image = Arc::new(image);
        *cached = Some((id, at, image.clone()));
        Some(Highlight::Ring { id, at, image })
    }

    /// Removes the annotation [`Self::deletable_highlight`] finds at `p`.
    /// Returns whether there was one.
    pub fn delete_at(&mut self, p: Point) -> bool {
        let Some((i, _)) = self.deletable_at(p) else {
            return false;
        };
        self.undo.push(self.annotations.clone());
        self.redo.clear();
        self.annotations.remove(i);
        self.replay();
        true
    }

    /// Starts typing into the active annotation, if it's text (just placed,
    /// or picked up to change). Returns whether it is.
    pub fn begin_typing(&mut self) -> bool {
        if let Some(a) = &mut self.active
            && let Shape::TextSelect(p, q) = a.shape
        {
            a.shape = text_in_box(a.style.text, p, q);
        }
        self.typing = matches!(self.active.as_ref().map(|a| &a.shape), Some(Shape::Text(..)));
        self.stale = true;
        self.typing
    }

    /// Types `s` into the text being typed; `\n` starts a new line.
    pub fn type_text(&mut self, s: &str) {
        if let (true, Some(Annotation { shape: Shape::Text(_, text, _), .. })) =
            (self.typing, &mut self.active)
        {
            text.push_str(s);
            self.stale = true;
        }
    }

    /// Deletes the last character typed.
    pub fn backspace(&mut self) {
        if let (true, Some(Annotation { shape: Shape::Text(_, text, _), .. })) =
            (self.typing, &mut self.active)
        {
            text.pop();
            self.stale = true;
        }
    }

    /// Recolours or resizes the text being typed.
    pub fn restyle_typing(&mut self, style: Style) {
        if let (true, Some(a)) = (self.typing, &mut self.active) {
            a.style = style;
            self.stale = true;
        }
    }

    /// Whether an existing annotation is being bent or moved.
    pub fn is_editing(&self) -> bool {
        self.editing.is_some()
    }

    /// Nodes of the shapes drawn with `tool` that can be dragged to reshape
    /// them: lines' and arrows' nodes, and steps' points.
    pub fn bend_nodes(&self, tool: Tool) -> Vec<Point> {
        let edited = self.active.as_ref().filter(|_| self.editing.is_some());
        self.annotations
            .iter()
            .chain(edited)
            .flat_map(|a| a.bend_nodes(tool))
            .collect()
    }

    /// Moves the annotation being drawn by `d` without reshaping it (Ctrl).
    /// The smart eraser keeps the colour it sampled where it started.
    pub fn shift_active(&mut self, d: Point) {
        if self.editing.is_none()
            && let Some(a) = &mut self.active
        {
            a.translate(d);
            self.stale = true;
        }
    }

    pub fn drag_to(&mut self, p: Point, constrain: bool) {
        if let Some(a) = &mut self.active {
            match self.editing.as_ref().map(|e| e.kind) {
                Some(EditKind::Bend(node)) => a.bend_to(node, p),
                Some(EditKind::Move { grab }) => a.move_to((p.0 - grab.0, p.1 - grab.1)),
                None => a.drag_to(p, constrain),
            }
            self.stale = true;
        }
    }

    /// Turns a dragged-out clip area into a copy of those pixels.
    fn take_clip(&self, a: Annotation) -> Option<Annotation> {
        let Shape::ClipSelect(p, q) = a.shape else {
            return Some(a);
        };
        let global = Rect::from_points(
            (p.0.round() as f64, p.1.round() as f64),
            (q.0.round() as f64, q.1.round() as f64),
        );
        let local = self.local(global)?;
        let mut img = Pixmap::new(local.w, local.h)?;
        img.draw_pixmap(
            -local.x,
            -local.y,
            self.committed.as_ref(),
            &PixmapPaint::default(),
            Transform::identity(),
            None,
        );
        let at = (local.x + self.rect.x, local.y + self.rect.y);
        Some(Annotation {
            shape: Shape::Clip(Arc::new(img), at),
            style: a.style,
            seed: a.seed,
        })
    }

    pub fn commit(&mut self) {
        self.typing = false;
        let Some(a) = self.active.take() else { return };
        if let Some(edit) = self.editing.take() {
            let mut before = self.annotations.clone();
            before.insert(edit.index, edit.original);
            self.undo.push(before);
            self.redo.clear();
            // Text typed away to nothing is gone.
            if !a.is_empty() {
                self.annotations.insert(edit.index, a);
            }
            self.replay();
            return;
        }
        self.refresh_active();
        if a.is_empty() {
            return;
        }
        let Some(a) = self.take_clip(a) else { return };
        if a.is_empty() {
            return;
        }
        a.draw(&mut self.committed, (self.rect.x, self.rect.y));
        if let Some(area) = self.local(a.bounds()) {
            copy_rect(&self.committed, &mut self.frame, area);
            self.dirty = Some(union(self.dirty, area));
        }
        self.undo.push(self.annotations.clone());
        self.redo.clear();
        self.annotations.push(a);
    }

    pub fn cancel_active(&mut self) {
        self.typing = false;
        self.active = None;
        if let Some(edit) = self.editing.take() {
            self.annotations.insert(edit.index, edit.original);
            self.replay();
        } else {
            self.refresh_active();
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo(&mut self) {
        if let Some(before) = self.undo.pop() {
            self.redo
                .push(std::mem::replace(&mut self.annotations, before));
            self.replay();
        }
    }

    pub fn redo(&mut self) {
        if let Some(after) = self.redo.pop() {
            self.undo
                .push(std::mem::replace(&mut self.annotations, after));
            self.replay();
        }
    }

    /// The changed area since the last call (local pixels), plus the frame.
    pub fn take_dirty(&mut self) -> Option<(Rect, &Pixmap)> {
        if self.stale {
            self.refresh_active();
        }
        let r = self.dirty.take()?;
        Some((r, &self.frame))
    }

    /// Copies the annotated pixels over `dst`, an image of the global region
    /// `dst_rect`, where the two overlap.
    pub fn copy_into(&self, dst: &mut RgbaImage, dst_rect: Rect) {
        let Some(overlap) = self.rect.intersect(&dst_rect) else {
            return;
        };
        let stride = self.frame.width() as usize;
        let pixels = self.frame.pixels();
        for gy in overlap.y..overlap.bottom() {
            for gx in overlap.x..overlap.right() {
                let src =
                    pixels[(gy - self.rect.y) as usize * stride + (gx - self.rect.x) as usize];
                let c = src.demultiply();
                dst.put_pixel(
                    (gx - dst_rect.x) as u32,
                    (gy - dst_rect.y) as u32,
                    image::Rgba([c.red(), c.green(), c.blue(), c.alpha()]),
                );
            }
        }
    }

    /// The finished screenshot.
    #[cfg(test)]
    pub fn image(&mut self) -> RgbaImage {
        if self.stale {
            self.refresh_active();
        }
        let mut out = RgbaImage::new(self.frame.width(), self.frame.height());
        self.copy_into(&mut out, self.rect);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas() -> Canvas {
        let img = RgbaImage::from_pixel(100, 60, image::Rgba([200, 200, 200, 255]));
        Canvas::new(
            Rect {
                x: 1000,
                y: 500,
                w: 100,
                h: 60,
            },
            img,
        )
    }

    fn draw(c: &mut Canvas, tool: Tool, from: Point, to: Point) {
        c.begin(Annotation::new(tool, Style::default(), from).unwrap());
        c.drag_to(to, false);
        c.commit();
    }

    #[test]
    fn highlighter_has_its_own_colour() {
        let mut style = Style::default();
        style.set_color(Tool::Highlighter, COLORS[3]);
        style.set_color(Tool::Pen, COLORS[4]);
        assert_eq!(style.color_for(Tool::Highlighter), COLORS[3]);
        assert_eq!(style.color_for(Tool::Arrow), COLORS[4]);
        let drawn = |tool| Annotation::new(tool, style, (0.0, 0.0)).unwrap().style.color;
        assert_eq!(drawn(Tool::Highlighter), COLORS[3]);
        assert_eq!(drawn(Tool::Pen), COLORS[4]);
    }

    #[test]
    fn highlighter_draws_straight_or_as_a_box() {
        let style = |highlighter| Style {
            highlighter,
            ..Style::default()
        };
        // Straight: just its ends, however the mouse wanders on the way.
        let mut c = canvas();
        c.begin(Annotation::new(Tool::Highlighter, style(HighlighterShape::Straight), (1010.0, 520.0)).unwrap());
        c.drag_to((1030.0, 540.0), false);
        c.drag_to((1060.0, 521.0), true);
        let Some(Shape::Highlighter(pts)) = c.active.as_ref().map(|a| &a.shape) else {
            panic!("highlighter");
        };
        // Shift snapped it level.
        assert_eq!(pts.len(), 2);
        assert_eq!(pts[0], (1010.0, 520.0));
        assert!((pts[1].0 - 1060.0).abs() < 0.1 && pts[1].1 == 520.0);
        c.commit();
        assert_eq!(c.annotations.len(), 1);

        // Box: fills the area, darkening like a marker, and drags with the
        // highlighter.
        let mut c = canvas();
        let before = c.image().get_pixel(30, 20).0;
        c.begin(Annotation::new(Tool::Highlighter, style(HighlighterShape::Box), (1020.0, 510.0)).unwrap());
        c.drag_to((1060.0, 540.0), false);
        c.commit();
        let inside = c.image().get_pixel(30, 20).0;
        let ink = Style::default().highlighter_color;
        let darker: Vec<u8> = (0..3).map(|i| before[i].min(ink[i])).collect();
        assert_eq!(inside[..3], darker[..], "the darker of each channel");
        assert_ne!(inside[..3], ink[..], "see-through, not solid");
        // A tint washes the colour over it instead.
        let mut t = canvas();
        let tint = Style {
            highlighter_ink: HighlighterInk::Tint,
            ..style(HighlighterShape::Box)
        };
        t.begin(Annotation::new(Tool::Highlighter, tint, (1020.0, 510.0)).unwrap());
        t.drag_to((1060.0, 540.0), false);
        t.commit();
        let tinted = t.image().get_pixel(30, 20).0;
        assert_ne!(tinted[..3], darker[..]);
        assert!(tinted[1] > darker[1], "the picture shows through");
        assert!(c.begin_move((1040.0, 525.0), Tool::Highlighter));
        c.drag_to((1050.0, 525.0), false);
        c.commit();
        assert_eq!(c.image().get_pixel(25, 20).0, before);

        // A click draws nothing.
        let mut c = canvas();
        c.begin(Annotation::new(Tool::Highlighter, style(HighlighterShape::Straight), (1010.0, 510.0)).unwrap());
        c.commit();
        assert!(c.annotations.is_empty());
    }

    #[test]
    fn box_draws_in_global_coordinates() {
        let mut c = canvas();
        draw(&mut c, Tool::Rect, (1010.0, 510.0), (1050.0, 540.0));
        let img = c.image();
        assert_eq!(img.get_pixel(10, 20).0, [0xff, 0x3b, 0x30, 255]); // left edge
        assert_eq!(img.get_pixel(30, 25).0, [200, 200, 200, 255]); // inside, untouched
    }

    #[test]
    fn undo_redo_restore_pixels() {
        let mut c = canvas();
        draw(&mut c, Tool::Line, (1000.0, 530.0), (1100.0, 530.0));
        assert_ne!(c.image().get_pixel(50, 30).0, [200, 200, 200, 255]);
        c.undo();
        assert_eq!(c.image().get_pixel(50, 30).0, [200, 200, 200, 255]);
        assert!(c.can_redo());
        c.redo();
        assert_ne!(c.image().get_pixel(50, 30).0, [200, 200, 200, 255]);
    }

    #[test]
    fn clicks_with_shape_tools_are_dropped() {
        let mut c = canvas();
        draw(&mut c, Tool::Rect, (1010.0, 510.0), (1010.0, 510.0));
        assert!(!c.can_undo());
        draw(&mut c, Tool::Pen, (1010.0, 510.0), (1010.0, 510.0));
        assert!(c.can_undo()); // a pen click leaves a dot
    }

    #[test]
    fn arrows_bend_through_their_node_and_undo() {
        let mut c = canvas();
        draw(&mut c, Tool::Arrow, (1005.0, 530.0), (1095.0, 530.0));
        assert_eq!(c.bend_nodes(Tool::Arrow), vec![(1050.0, 530.0)]);
        let untouched = [200, 200, 200, 255];
        assert_eq!(c.image().get_pixel(50, 10).0, untouched);

        assert!(!c.begin_bend((1050.0, 500.0), 10.0, Tool::Arrow)); // too far from the node
        assert!(c.begin_bend((1052.0, 531.0), 10.0, Tool::Arrow));
        c.drag_to((1050.0, 510.0), false);
        c.commit();
        assert_eq!(c.bend_nodes(Tool::Arrow), vec![(1050.0, 510.0)]);
        assert_ne!(c.image().get_pixel(50, 10).0, untouched); // the curve's peak
        assert_eq!(c.image().get_pixel(50, 30).0, untouched); // no straight shaft

        c.undo();
        assert_eq!(c.bend_nodes(Tool::Arrow), vec![(1050.0, 530.0)]);
        assert_eq!(c.image().get_pixel(50, 10).0, untouched);
        c.undo();
        assert!(c.bend_nodes(Tool::Arrow).is_empty());
    }

    #[test]
    fn lines_bend_through_their_node() {
        let mut c = canvas();
        draw(&mut c, Tool::Line, (1005.0, 530.0), (1095.0, 530.0));
        // Each tool only offers its own shapes' nodes.
        assert!(c.bend_nodes(Tool::Arrow).is_empty());
        assert!(!c.begin_bend((1050.0, 530.0), 10.0, Tool::Arrow));
        assert_eq!(c.bend_nodes(Tool::Line), vec![(1050.0, 530.0)]);
        let untouched = [200, 200, 200, 255];

        assert!(c.begin_bend((1050.0, 530.0), 10.0, Tool::Line));
        c.drag_to((1050.0, 510.0), false);
        c.commit();
        assert_eq!(c.bend_nodes(Tool::Line), vec![(1050.0, 510.0)]);
        assert_ne!(c.image().get_pixel(50, 10).0, untouched); // the curve's peak
        assert_eq!(c.image().get_pixel(50, 30).0, untouched); // no straight line
    }

    #[test]
    fn clips_copy_pixels_and_move() {
        let mut c = canvas();
        // A red box to copy, at local (10..30, 10..30).
        draw(&mut c, Tool::Rect, (1010.0, 510.0), (1030.0, 530.0));
        draw(&mut c, Tool::Clip, (1005.0, 505.0), (1035.0, 535.0));
        assert!(c.movable_at((1020.0, 520.0), Tool::Clip));
        assert!(!c.movable_at((1050.0, 520.0), Tool::Clip));
        assert!(!c.movable_at((1020.0, 520.0), Tool::Rect));

        assert!(c.begin_move((1020.0, 520.0), Tool::Clip));
        c.drag_to((1080.0, 530.0), false); // 60 right, 10 down
        c.commit();
        let red = [0xff, 0x3b, 0x30, 255];
        let img = c.image();
        assert_eq!(img.get_pixel(10, 20).0, red); // the original box stays
        assert_eq!(img.get_pixel(70, 30).0, red); // the copy's left edge
        assert!(!c.movable_at((1020.0, 520.0), Tool::Clip));
        assert!(c.movable_at((1080.0, 530.0), Tool::Clip));

        c.undo(); // back where it was copied
        assert!(c.movable_at((1020.0, 520.0), Tool::Clip));
        assert_ne!(c.image().get_pixel(70, 30).0, red);
    }

    #[test]
    fn clip_shadow_can_be_turned_off() {
        // The pixels just below the clip and on its top edge.
        let around_clip = |clip_shadow| {
            let mut c = canvas();
            let style = Style {
                clip_shadow,
                ..Style::default()
            };
            c.begin(Annotation::new(Tool::Clip, style, (1005.0, 505.0)).unwrap());
            c.drag_to((1035.0, 535.0), false);
            c.commit();
            let img = c.image();
            [img.get_pixel(20, 37).0, img.get_pixel(20, 5).0]
        };
        let untouched = [200, 200, 200, 255];
        let [below, edge] = around_clip(true);
        assert_ne!(below, untouched);
        assert_ne!(edge, untouched);
        // No shadow, and no dark edge either.
        assert_eq!(around_clip(false), [untouched, untouched]);
    }

    #[test]
    fn step_circle_and_number_sizes_are_separate() {
        // The image after placing a step, and how many of its pixels are
        // the white of its number.
        let place = |size, number| {
            let mut c = canvas();
            let style = Style {
                step: StepOptions {
                    size,
                    number,
                    border: false,
                },
                ..Default::default()
            };
            c.begin(Annotation::new(Tool::Step, style, (1050.0, 530.0)).unwrap());
            c.commit();
            let img = c.image();
            let white = img.pixels().filter(|p| p.0 == [255, 255, 255, 255]).count();
            (img, white)
        };
        let red = [0xff, 0x3b, 0x30, 255];
        let (small, digits) = place(30, 16);
        let (big, same_digits) = place(50, 16);
        assert_ne!(small.get_pixel(72, 30).0, red);
        assert_eq!(big.get_pixel(72, 30).0, red);
        assert_eq!(digits, same_digits);
        let (_, bigger_digits) = place(50, 26);
        assert!(bigger_digits > digits * 2, "{digits} -> {bigger_digits}");
        // However big it's asked to be, the number stays inside its circle.
        let (huge, huge_digits) = place(30, 90);
        assert!(huge_digits < digits + digits / 5, "{digits} -> {huge_digits}");
        assert_eq!(huge.get_pixel(50, 13).0, [200, 200, 200, 255]);
    }

    #[test]
    fn step_border_can_be_turned_off() {
        let just_outside = |border| {
            let mut c = canvas();
            let mut style = Style::default();
            style.step.border = border;
            c.begin(Annotation::new(Tool::Step, style, (1020.0, 530.0)).unwrap());
            c.commit();
            c.image().get_pixel(35, 30).0
        };
        let untouched = [200, 200, 200, 255];
        assert_ne!(just_outside(true), untouched);
        assert_eq!(just_outside(false), untouched);
    }

    #[test]
    fn right_click_deletes_anything_whatever_the_tool() {
        let img = RgbaImage::from_pixel(100, 60, image::Rgba([255, 255, 255, 255]));
        let rect = Rect {
            x: 0,
            y: 0,
            w: 100,
            h: 60,
        };
        let mut c = Canvas::new(rect, img);
        c.begin(Annotation::new(Tool::Rect, Style::default(), (5.0, 5.0)).unwrap());
        c.drag_to((95.0, 55.0), false);
        c.commit();
        c.begin(Annotation::new(Tool::Eraser, Style::default(), (40.0, 10.0)).unwrap());
        c.drag_to((60.0, 30.0), false);
        c.commit();
        let erased = c.image();

        // A rectangle is picked by its outline, not its empty inside.
        assert!(c.deletable_highlight((80.0, 45.0)).is_none());
        assert!(c.deletable_highlight((95.0, 45.0)).is_some());
        assert!(c.delete_at((50.0, 20.0)));
        assert!(!c.delete_at((50.0, 20.0)));
        assert_eq!(c.annotations.len(), 1);
        c.undo();
        assert_eq!(c.image().as_raw(), erased.as_raw());

        // Lines and arrows go too, from near their stroke.
        draw(&mut c, Tool::Arrow, (10.0, 50.0), (90.0, 50.0));
        assert!(c.deletable_highlight((30.0, 40.0)).is_none());
        assert!(c.delete_at((30.0, 52.0)));
        assert_eq!(c.annotations.len(), 2);
    }

    /// Places text at `at` and types `text` into it, without finishing.
    fn type_at(c: &mut Canvas, at: Point, text: &str) {
        c.begin(Annotation::new(Tool::Text, Style::default(), at).unwrap());
        assert!(c.begin_typing());
        c.type_text(text);
    }

    #[test]
    fn typed_text_is_kept_without_the_caret() {
        let mut c = canvas();
        type_at(&mut c, (1010.0, 520.0), "Hix");
        c.backspace();
        c.type_text("\nyo");
        c.commit();
        assert_eq!(c.annotations.len(), 1);
        let Shape::Text(_, text, _) = &c.annotations[0].shape else {
            panic!("text");
        };
        assert_eq!(text, "Hi\nyo");
        let typed = c.image();
        assert_ne!(typed.get_pixel(15, 20).0, [200, 200, 200, 255], "something drawn");

        // The same text placed without typing looks the same: no caret or
        // dashed box left behind.
        let mut d = canvas();
        d.begin(Annotation {
            shape: c.annotations[0].shape.clone(),
            ..Annotation::new(Tool::Text, Style::default(), (0.0, 0.0)).unwrap()
        });
        d.commit();
        assert_eq!(d.image().as_raw(), typed.as_raw());
    }

    #[test]
    fn empty_text_is_dropped_and_text_typed_away_is_deleted() {
        let mut c = canvas();
        type_at(&mut c, (1010.0, 520.0), "  ");
        c.commit();
        assert!(c.annotations.is_empty());
        assert!(!c.can_undo());

        type_at(&mut c, (1010.0, 520.0), "ab");
        c.commit();
        let before = c.image();
        // Pick it up again (as a click with the text tool does) and delete it all.
        assert!(c.begin_move((1012.0, 520.0), Tool::Text));
        assert!(c.begin_typing());
        c.backspace();
        c.backspace();
        c.commit();
        assert!(c.annotations.is_empty());
        c.undo();
        assert_eq!(c.image().as_raw(), before.as_raw());
    }

    #[test]
    fn text_drags_with_the_text_tool_and_restyles_while_typed() {
        let mut c = canvas();
        type_at(&mut c, (1010.0, 520.0), "a");
        let big = TextOptions {
            size: 60,
            outline: false,
            ..Style::default().text
        };
        c.restyle_typing(Style {
            text: big,
            ..Style::default()
        });
        c.commit();
        assert_eq!(c.annotations[0].style.text, big);
        let Shape::Text(at, _, _) = c.annotations[0].shape else {
            panic!("text");
        };
        assert!(!c.movable_at((1012.0, 520.0), Tool::Pen));
        assert!(c.begin_move((1012.0, 520.0), Tool::Text));
        c.drag_to((1032.0, 530.0), false);
        c.commit();
        let Shape::Text(moved, _, _) = c.annotations[0].shape else {
            panic!("text");
        };
        assert_eq!((moved.0 - at.0, moved.1 - at.1), (20.0, 10.0));
    }

    #[test]
    fn lines_wrap_after_spaces_or_mid_word() {
        let wrap = |s, width| wrap_line(s, width, |_| 1.0);
        assert_eq!(wrap("one two three", 8.0), ["one two ", "three"]);
        assert_eq!(wrap("one two", 3.0), ["one ", "two"]);
        assert_eq!(wrap("abcdefgh", 3.0), ["abc", "def", "gh"]);
        assert_eq!(wrap("ab  ", 2.0), ["ab  "]);
        assert_eq!(wrap("", 2.0), [""]);
    }

    #[test]
    fn text_typed_in_a_dragged_out_box_wraps_to_it() {
        let mut c = canvas();
        c.begin(Annotation::new(Tool::Text, Style::default(), (1010.0, 505.0)).unwrap());
        c.drag_to((1090.0, 520.0), false);
        assert!(c.begin_typing());
        c.type_text("wrapped words in a box");
        let a = c.active.clone().unwrap();
        let Shape::Text(at, _, Some((w, _))) = a.shape else {
            panic!("boxed text");
        };
        assert_eq!(at, (1010.0 + TEXT_PAD as f32, 505.0 + TEXT_PAD as f32));
        assert_eq!(w, 80.0 - 2.0 * TEXT_PAD as f32);
        let r = a.text_box().unwrap();
        assert_eq!(r.w as f32, w);
        // Taller than one line once it wraps.
        assert!(r.h as f32 > 2.0 * text_line(a.style.text));
        c.commit();
        assert_eq!(c.annotations.len(), 1);

        // Too narrow for the text: placed where the drag started, unwrapped.
        c.begin(Annotation::new(Tool::Text, Style::default(), (1010.0, 530.0)).unwrap());
        c.drag_to((1014.0, 550.0), false);
        assert!(c.begin_typing());
        let Some(Shape::Text(at, _, None)) = c.active.as_ref().map(|a| &a.shape) else {
            panic!("unboxed text");
        };
        assert_eq!(*at, text_at_click(Style::default().text, (1010.0, 530.0)));
    }

    #[test]
    fn text_outline_can_be_turned_off() {
        let dark_pixels = |outline: bool| {
            let mut c = canvas();
            let style = Style {
                color: COLORS[6], // white: edged in black
                text: TextOptions {
                    outline,
                    ..Style::default().text
                },
                ..Style::default()
            };
            c.begin(Annotation::new(Tool::Text, style, (1010.0, 530.0)).unwrap());
            c.begin_typing();
            c.type_text("Hello");
            c.commit();
            c.image().pixels().filter(|p| p.0[0] < 150).count()
        };
        assert!(dark_pixels(true) > 50);
        assert_eq!(dark_pixels(false), 0);
    }

    #[test]
    fn text_uses_an_installed_font() {
        while crate::fonts::installed().is_none() {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if crate::fonts::installed().unwrap().is_empty() {
            return; // none to try
        }
        let typed = |font: Option<u16>| {
            let mut c = canvas();
            let style = Style {
                text: TextOptions {
                    font,
                    ..Style::default().text
                },
                ..Style::default()
            };
            c.begin(Annotation::new(Tool::Text, style, (1010.0, 530.0)).unwrap());
            c.begin_typing();
            c.type_text("Hello");
            c.commit();
            c.image()
        };
        let installed = typed(Some(0));
        assert_ne!(installed.as_raw(), typed(None).as_raw());
        assert_ne!(installed.as_raw(), canvas().image().as_raw(), "something drawn");
    }

    /// Renders text in each colour, sizes, a font and with no outline to
    /// `target/text-preview.png`,
    /// with one still being typed: `cargo test text_preview -- --ignored`.
    #[test]
    #[ignore]
    fn text_preview() {
        let img = RgbaImage::from_fn(520, 360, |x, _| {
            // Light on the left, dark on the right, to see the edging on both.
            let v = if x < 260 { 235 } else { 40 };
            image::Rgba([v, v, v, 255])
        });
        let rect = Rect {
            x: 0,
            y: 0,
            w: 520,
            h: 360,
        };
        let mut c = Canvas::new(rect, img);
        while crate::fonts::installed().is_none() {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let arial = crate::fonts::installed()
            .unwrap()
            .iter()
            .position(|f| f.family == "Arial")
            .map(|i| i as u16);
        // Rows: small bundled; bigger, in Arial; outline off.
        let rows = [
            TextOptions {
                size: 20,
                ..Style::default().text
            },
            TextOptions {
                size: 34,
                font: arial,
                ..Style::default().text
            },
            TextOptions {
                size: 34,
                outline: false,
                ..Style::default().text
            },
        ];
        for (i, text) in rows.into_iter().enumerate() {
            for (j, color) in [COLORS[0], COLORS[4], COLORS[6], COLORS[7]].into_iter().enumerate() {
                let style = Style {
                    color,
                    text,
                    ..Style::default()
                };
                let at = (10.0 + j as f32 * 130.0, 30.0 + i as f32 * 60.0);
                c.begin(Annotation::new(Tool::Text, style, at).unwrap());
                c.begin_typing();
                c.type_text("Text 12");
                c.commit();
            }
        }
        c.begin(Annotation::new(Tool::Text, Style::default(), (10.0, 250.0)).unwrap());
        c.drag_to((250.0, 350.0), false);
        c.begin_typing();
        c.type_text("Wrapped in a box dragged out for it");
        c.commit();
        type_at(&mut c, (20.0, 215.0), "Typing\u{2026}");
        c.commit();
        c.begin(Annotation::new(Tool::Text, Style::default(), (270.0, 250.0)).unwrap());
        c.drag_to((510.0, 350.0), false);
        c.begin_typing();
        c.type_text("Still typing in a box");
        c.take_dirty();
        c.frame
            .save_png(format!("{}/target/text-preview.png", env!("CARGO_MANIFEST_DIR")))
            .unwrap();
    }

    #[test]
    fn pen_strokes_drag_with_their_own_tool() {
        let mut c = canvas();
        draw(&mut c, Tool::Pen, (1010.0, 510.0), (1050.0, 530.0));
        let before = c.image();
        // Off the line, or with another tool: nothing to pick up.
        assert!(!c.movable_at((1030.0, 540.0), Tool::Pen));
        assert!(!c.movable_at((1030.0, 520.0), Tool::Highlighter));
        assert!(c.begin_move((1030.0, 520.0), Tool::Pen));
        c.drag_to((1060.0, 540.0), false);
        c.commit();
        assert_eq!(c.annotations.len(), 1);
        let Shape::Pen(pts) = &c.annotations[0].shape else {
            panic!("still a pen stroke");
        };
        assert_eq!(pts.first(), Some(&(1040.0, 530.0)));
        assert_eq!(pts.last(), Some(&(1080.0, 550.0)));
        // Where it was is clear again, and undo puts it back.
        assert_eq!(c.image().get_pixel(10, 10).0, [200, 200, 200, 255]);
        c.undo();
        assert_eq!(c.image().as_raw(), before.as_raw());
    }

    #[test]
    fn arrows_drag_with_their_own_tool_and_keep_their_curve() {
        let mut c = canvas();
        draw(&mut c, Tool::Arrow, (1010.0, 530.0), (1090.0, 530.0));
        assert!(c.begin_bend((1050.0, 530.0), 10.0, Tool::Arrow));
        c.drag_to((1050.0, 520.0), false);
        c.commit();
        let before = c.image();
        // Off the arrow, or with another tool: nothing to pick up.
        assert!(!c.movable_at((1030.0, 550.0), Tool::Arrow));
        assert!(!c.movable_at((1020.0, 529.0), Tool::Line));
        assert!(c.begin_move((1020.0, 529.0), Tool::Arrow));
        c.drag_to((1030.0, 539.0), false);
        c.commit();
        assert_eq!(c.annotations.len(), 1);
        let Shape::Arrow(a, b, nodes) = &c.annotations[0].shape else {
            panic!("still an arrow");
        };
        assert_eq!((*a, *b), ((1020.0, 540.0), (1100.0, 540.0)));
        assert_eq!(nodes, &[(1060.0, 530.0)]);
        c.undo();
        assert_eq!(c.image().as_raw(), before.as_raw());
    }

    #[test]
    fn lines_and_arrows_bend_through_each_of_their_nodes() {
        let style = Style {
            bends: 3,
            ..Style::default()
        };
        let untouched = [200, 200, 200, 255];
        for tool in [Tool::Line, Tool::Arrow] {
            let mut c = canvas();
            c.begin(Annotation::new(tool, style, (1010.0, 530.0)).unwrap());
            c.drag_to((1090.0, 530.0), false);
            c.commit();
            assert_eq!(c.bend_nodes(tool), vec![(1030.0, 530.0), (1050.0, 530.0), (1070.0, 530.0)]);

            // The nearest node is picked up; the others stay where they are.
            assert!(c.begin_bend((1033.0, 531.0), 16.0, tool));
            c.drag_to((1030.0, 512.0), false);
            c.commit();
            assert_eq!(c.bend_nodes(tool), vec![(1030.0, 512.0), (1050.0, 530.0), (1070.0, 530.0)]);
            // It curves through them all.
            assert_ne!(c.image().get_pixel(30, 12).0, untouched);
            assert_ne!(c.image().get_pixel(50, 30).0, untouched);
            assert_eq!(c.image().get_pixel(30, 30).0, untouched);

            // Put back on the line, it's straight again.
            assert!(c.begin_bend((1030.0, 512.0), 16.0, tool));
            c.drag_to((1031.0, 531.0), false);
            c.commit();
            assert_eq!(c.bend_nodes(tool), vec![(1030.0, 530.0), (1050.0, 530.0), (1070.0, 530.0)]);
            assert_ne!(c.image().get_pixel(30, 30).0, untouched);
        }
    }

    #[test]
    fn curves_split_into_the_same_course() {
        let curve = Curve::new((0.0, 0.0), (90.0, 0.0), &[(30.0, -20.0), (60.0, 20.0)]);
        assert_eq!(curve.pieces.len(), 3);
        // Each piece runs through the next node.
        assert_eq!(curve.pieces[0][3], (30.0, -20.0));
        assert_eq!(curve.pieces[1][3], (60.0, 20.0));
        // The middle third of the middle piece starts and ends on it.
        let part = curve.between((1, 1.0 / 3.0), (1, 2.0 / 3.0)).unwrap();
        let near = |p: Point, q: Point| (p.0 - q.0).hypot(p.1 - q.1) < 1e-3;
        assert!(near(part.pieces[0][0], curve.at((1, 1.0 / 3.0))));
        assert!(near(part.pieces[0][3], curve.at((1, 2.0 / 3.0))));
        assert!(near(cubic(&part.pieces[0], 0.5), curve.at((1, 0.5))));
        // Across pieces: the rest of the first, all of the second, and the
        // start of the third.
        assert_eq!(curve.between((0, 0.5), (2, 0.5)).unwrap().pieces.len(), 3);
        assert!(curve.between((2, 0.5), (0, 0.5)).is_none());
        // 10 short of the end is 10 from the tip, on the last piece.
        let (i, t) = curve.back_from_end(10.0);
        assert_eq!(i, 2);
        let p = curve.at((i, t));
        assert!(((90.0 - p.0).hypot(p.1) - 10.0).abs() < 1e-2);
    }

    #[test]
    fn hovered_strokes_get_a_ring_that_follows_them() {
        let img = RgbaImage::from_pixel(200, 120, image::Rgba([90, 90, 90, 255]));
        let rect = Rect {
            x: 0,
            y: 0,
            w: 200,
            h: 120,
        };
        let mut c = Canvas::new(rect, img);
        // A diagonal pen stroke: its bounding box is mostly empty.
        let mut pen = Annotation::new(Tool::Pen, Style::default(), (20.0, 20.0)).unwrap();
        if let Shape::Pen(pts) = &mut pen.shape {
            pts.extend((1..=40).map(|i| (20.0 + i as f32 * 4.0, 20.0 + i as f32 * 2.0)));
        }
        c.begin(pen);
        c.commit();
        draw(&mut c, Tool::Rect, (30.0, 70.0), (80.0, 110.0));

        let Some(Highlight::Ring { at, image, .. }) = c.deletable_highlight((100.0, 60.0)) else {
            panic!("a pen stroke should get a ring");
        };
        let alpha = |x: f32, y: f32| {
            let (x, y) = (x as i32 - at.0, y as i32 - at.1);
            image.pixel(x as u32, y as u32).map_or(0, |p| p.alpha())
        };
        let white = Style::default().size.stroke() / 2.0 + RING_GAP + 0.75;
        // Clear on the stroke and in the box's empty corners; white a little
        // way out from the stroke, across it (perpendicular to (2, 1)).
        assert_eq!(alpha(100.0, 60.0), 0);
        assert_eq!(alpha(170.0, 30.0), 0);
        assert_eq!(alpha(30.0, 55.0), 0);
        let (nx, ny) = (-1.0 / 5f32.sqrt(), 2.0 / 5f32.sqrt());
        assert_eq!(alpha(100.0 + nx * white, 60.0 + ny * white), 255);
        assert_eq!(alpha(100.0 - nx * white, 60.0 - ny * white), 255);
        // The same picture again while nothing changes.
        let Some(Highlight::Ring { image: again, .. }) = c.deletable_highlight((100.0, 60.0)) else {
            panic!();
        };
        assert!(Arc::ptr_eq(&image, &again));
        // Boxy shapes keep a box.
        assert!(matches!(c.deletable_highlight((30.0, 90.0)), Some(Highlight::Box(_))));
    }

    /// Renders a pen stroke and an arrow with their hover rings to
    /// `target/hover-ring-preview.png`: `cargo test hover_ring_preview -- --ignored`.
    #[test]
    #[ignore]
    fn hover_ring_preview() {
        let img = RgbaImage::from_pixel(360, 200, image::Rgba([235, 235, 235, 255]));
        let rect = Rect {
            x: 0,
            y: 0,
            w: 360,
            h: 200,
        };
        let mut c = Canvas::new(rect, img);
        let mut pen = Annotation::new(Tool::Pen, Style::default(), (30.0, 150.0)).unwrap();
        if let Shape::Pen(pts) = &mut pen.shape {
            pts.extend((1..=60).map(|i| {
                let t = i as f32 / 60.0;
                (30.0 + t * 150.0, 150.0 - t * 90.0 + (t * 12.0).sin() * 25.0)
            }));
        }
        c.begin(pen);
        c.commit();
        draw(&mut c, Tool::Arrow, (220.0, 160.0), (330.0, 40.0));
        let mut out = c.frame.clone();
        for p in [(105.0, 105.0), (275.0, 100.0)] {
            if let Some(Highlight::Ring { at, image, .. }) = c.deletable_highlight(p) {
                out.draw_pixmap(at.0, at.1, image.as_ref().as_ref(), &PixmapPaint::default(), Transform::identity(), None);
            }
        }
        out.save_png(format!("{}/target/hover-ring-preview.png", env!("CARGO_MANIFEST_DIR")))
            .unwrap();
    }

    #[test]
    fn pixelated_areas_preview_as_outlines_and_move() {
        let mut img = RgbaImage::from_pixel(100, 60, image::Rgba([0, 0, 0, 255]));
        for x in 0..50 {
            for y in 0..60 {
                img.put_pixel(x, y, image::Rgba([255, 255, 255, 255]));
            }
        }
        let mut c = Canvas::new(
            Rect {
                x: 0,
                y: 0,
                w: 100,
                h: 60,
            },
            img,
        );
        // Straddling the white/black edge at x = 50.
        c.begin(Annotation::new(Tool::Pixelate, Style::default(), (40.0, 10.0)).unwrap());
        c.drag_to((60.0, 30.0), false);
        // While dragging: only an outline, the pixels inside are untouched.
        assert_eq!(c.image().get_pixel(45, 20).0, [255, 255, 255, 255]);
        c.commit();
        let mixed = c.image().get_pixel(45, 20).0;
        assert!(mixed[0] > 0 && mixed[0] < 255, "{mixed:?}");

        assert!(c.movable_at((50.0, 20.0), Tool::Pixelate));
        assert!(!c.movable_at((50.0, 20.0), Tool::Blur));
        assert!(c.begin_move((50.0, 20.0), Tool::Pixelate));
        c.drag_to((20.0, 20.0), false); // 30 left: all white now
        c.commit();
        // The edge it left is sharp again; the area it covers now is all white.
        assert_eq!(c.image().get_pixel(45, 20).0, [255, 255, 255, 255]);
        assert_eq!(c.image().get_pixel(55, 20).0, [0, 0, 0, 255]);
        assert!(c.movable_at((20.0, 20.0), Tool::Pixelate));
        c.undo();
        assert_eq!(c.image().get_pixel(45, 20).0, mixed);
    }

    #[test]
    fn steps_count_up_and_undo_gives_the_number_back() {
        let mut c = canvas();
        let step = |c: &mut Canvas, x: f32| {
            c.begin(Annotation::new(Tool::Step, Style::default(), (x, 530.0)).unwrap());
            c.commit();
        };
        let numbers = |c: &Canvas| -> Vec<u32> {
            c.annotations
                .iter()
                .filter_map(|a| match a.shape {
                    Shape::Step(_, n, _) => Some(n),
                    _ => None,
                })
                .collect()
        };
        step(&mut c, 1020.0);
        step(&mut c, 1050.0);
        assert_eq!(numbers(&c), [1, 2]);
        // The circle is filled, with the number in a contrasting colour.
        let img = c.image();
        assert_eq!(img.get_pixel(20, 18).0, [0xff, 0x3b, 0x30, 255]);
        let center = (16..=24)
            .flat_map(|x| (25..=35).map(move |y| (x, y)))
            .filter(|&(x, y)| img.get_pixel(x, y).0[1] > 200)
            .count();
        assert!(center > 5, "white digit pixels: {center}");
        c.undo();
        step(&mut c, 1080.0);
        assert_eq!(numbers(&c), [1, 2]);
        assert!(c.movable_at((1080.0, 530.0), Tool::Step));
    }

    #[test]
    fn steps_draw_out_to_a_point_from_their_node() {
        let mut c = canvas();
        c.begin(Annotation::new(Tool::Step, Style::default(), (1020.0, 530.0)).unwrap());
        c.commit();
        // A plain circle, with its node on the edge, down and right.
        let [node] = c.bend_nodes(Tool::Step)[..] else {
            panic!("one node")
        };
        assert!((node.0 - 1030.6).abs() < 0.1 && (node.1 - 540.6).abs() < 0.1);
        assert!(c.bend_nodes(Tool::Line).is_empty());
        let untouched = [200, 200, 200, 255];
        let red = [0xff, 0x3b, 0x30, 255];
        assert_eq!(c.image().get_pixel(55, 30).0, untouched);

        assert!(c.begin_bend(node, 10.0, Tool::Step));
        c.drag_to((1070.0, 530.0), false);
        c.commit();
        assert_eq!(c.bend_nodes(Tool::Step), vec![(1070.0, 530.0)]);
        assert_eq!(c.image().get_pixel(55, 30).0, red); // on the point
        assert_eq!(c.image().get_pixel(55, 40).0, untouched); // beside it
        // The point can be picked to delete it.
        assert!(c.deletable_highlight((1060.0, 530.0)).is_some());

        // It goes along when the step is moved.
        assert!(c.begin_move((1020.0, 530.0), Tool::Step));
        c.drag_to((1025.0, 530.0), false);
        c.commit();
        assert_eq!(c.bend_nodes(Tool::Step), vec![(1075.0, 530.0)]);

        // Dragged back to the circle, it's gone.
        assert!(c.begin_bend((1075.0, 530.0), 10.0, Tool::Step));
        c.drag_to((1030.0, 535.0), false);
        c.commit();
        assert_eq!(c.image().get_pixel(60, 30).0, untouched);
        c.undo();
        assert_eq!(c.bend_nodes(Tool::Step), vec![(1075.0, 530.0)]);
    }

    #[test]
    fn smart_eraser_fills_with_the_colour_under_the_start() {
        let mut img = RgbaImage::from_pixel(100, 60, image::Rgba([240, 235, 220, 255]));
        // Some "text" to cover.
        for x in 30..70 {
            img.put_pixel(x, 30, image::Rgba([20, 20, 20, 255]));
        }
        let mut c = Canvas::new(
            Rect {
                x: 0,
                y: 0,
                w: 100,
                h: 60,
            },
            img,
        );
        draw(&mut c, Tool::Eraser, (25.0, 25.0), (75.0, 35.0));
        let img = c.image();
        assert_eq!(img.get_pixel(50, 30).0, [240, 235, 220, 255]);
        assert_eq!(img.get_pixel(25, 25).0, [240, 235, 220, 255]);
        assert!(c.movable_at((50.0, 30.0), Tool::Eraser));
    }

    #[test]
    fn ctrl_drag_moves_the_eraser_box_keeping_its_colour() {
        let mut img = RgbaImage::from_pixel(100, 60, image::Rgba([10, 120, 200, 255]));
        for x in 60..90 {
            img.put_pixel(x, 30, image::Rgba([255, 255, 255, 255]));
        }
        let mut c = Canvas::new(
            Rect {
                x: 0,
                y: 0,
                w: 100,
                h: 60,
            },
            img,
        );
        c.begin(Annotation::new(Tool::Eraser, Style::default(), (5.0, 25.0)).unwrap());
        c.drag_to((35.0, 35.0), false);
        c.shift_active((55.0, 0.0)); // Ctrl-drag 55 right: now 60..90
        c.commit();
        let img = c.image();
        assert_eq!(img.get_pixel(75, 30).0, [10, 120, 200, 255]);
        assert!(c.movable_at((75.0, 30.0), Tool::Eraser));
        assert!(!c.movable_at((10.0, 30.0), Tool::Eraser));
    }

    /// Covers (1010, 510)-(1030, 530), a 20 px square, with `image`.
    fn redact(image: Option<RedactImage>) -> RgbaImage {
        let mut c = canvas();
        c.begin(
            Annotation::new(Tool::Image, Style::default(), (1010.0, 510.0))
                .unwrap()
                .with_image(image),
        );
        c.drag_to((1030.0, 530.0), false);
        c.commit();
        c.image()
    }

    /// A 5x1 picture: red, three greens, blue.
    fn stripes(stretch: bool) -> RedactImage {
        let mut img = RgbaImage::from_pixel(5, 1, image::Rgba([0, 255, 0, 255]));
        img.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        img.put_pixel(4, 0, image::Rgba([0, 0, 255, 255]));
        RedactImage {
            pixmap: Arc::new(premultiplied_pixmap(&img).unwrap()),
            stretch,
        }
    }

    #[test]
    fn image_redaction_stretches_to_fill() {
        let img = redact(Some(stripes(true)));
        let [r, g, _, a] = img.get_pixel(10, 20).0;
        assert!(r > g && a == 255, "left edge should be red: {r} {g}");
        let [_, g, b, _] = img.get_pixel(29, 20).0;
        assert!(b > g, "right edge should be blue: {g} {b}");
        assert_eq!(img.get_pixel(9, 20).0, [200, 200, 200, 255]); // outside
        assert_eq!(img.get_pixel(30, 20).0, [200, 200, 200, 255]);
    }

    #[test]
    fn image_redaction_keeps_proportions_by_cropping() {
        // Scaled up to cover the square, only the middle green is left.
        let img = redact(Some(stripes(false)));
        for x in [10, 20, 29] {
            let [r, g, b, a] = img.get_pixel(x, 20).0;
            assert!(g > r && g > b && a == 255, "{x}: {r} {g} {b}");
        }
    }

    #[test]
    fn image_redaction_never_shows_through() {
        // No picture: a black box.
        assert_eq!(redact(None).get_pixel(20, 20).0, [0, 0, 0, 255]);
        // A see-through one goes over black.
        let clear = RgbaImage::from_pixel(4, 4, image::Rgba([255, 255, 255, 0]));
        let img = RedactImage {
            pixmap: Arc::new(premultiplied_pixmap(&clear).unwrap()),
            stretch: true,
        };
        assert_eq!(redact(Some(img)).get_pixel(20, 20).0, [0, 0, 0, 255]);
    }

    #[test]
    fn pixelate_averages_blocks() {
        let mut img = RgbaImage::from_pixel(28, 28, image::Rgba([0, 0, 0, 255]));
        for x in 0..14 {
            img.put_pixel(x, 0, image::Rgba([255, 255, 255, 255]));
        }
        let mut c = Canvas::new(
            Rect {
                x: 0,
                y: 0,
                w: 28,
                h: 28,
            },
            img,
        );
        draw(&mut c, Tool::Pixelate, (0.0, 0.0), (28.0, 28.0));
        let block = c.image().get_pixel(5, 5).0;
        assert_eq!(block, [18, 18, 18, 255]); // 14 white of 196 pixels
    }

    #[test]
    fn secure_pixelate_shuffles_blocks_the_same_way_every_redraw() {
        // Blocks alternately black and white along the top row.
        let img = RgbaImage::from_fn(80, 8, |x, _| {
            let v = if (x / 8) % 2 == 0 { 0 } else { 255 };
            image::Rgba([v, v, v, 255])
        });
        let rect = Rect {
            x: 0,
            y: 0,
            w: 80,
            h: 8,
        };
        let mut c = Canvas::new(rect, img.clone());
        let style = Style {
            pixelate: PixelateOptions {
                block: 8,
                secure: true,
            },
            ..Default::default()
        };
        c.begin(Annotation::new(Tool::Pixelate, style, (0.0, 0.0)).unwrap());
        c.drag_to((80.0, 8.0), false);
        c.commit();
        let out = c.image();
        let tops: Vec<u8> = (0..10).map(|i| out.get_pixel(i * 8, 0).0[0]).collect();
        // Still five dark and five light blocks, just not where they were.
        assert_eq!(tops.iter().filter(|&&v| v < 128).count(), 5);
        assert_ne!(
            tops,
            (0..10).map(|i| if i % 2 == 0 { 0 } else { 255 }).collect::<Vec<u8>>(),
            "blocks should be shuffled (1 in 252 chance of a false failure)"
        );
        // Undo and redo redraws it identically.
        c.undo();
        c.redo();
        assert_eq!(c.image(), out);
    }

    #[test]
    fn arrow_heads_go_where_the_options_say() {
        let head_at = |arrow: ArrowOptions| {
            let mut c = canvas();
            let style = Style {
                arrow,
                ..Default::default()
            };
            c.begin(Annotation::new(Tool::Arrow, style, (1010.0, 530.0)).unwrap());
            c.drag_to((1090.0, 530.0), false);
            c.commit();
            let img = c.image();
            let untouched = [200, 200, 200, 255];
            // Off the shaft, but inside a head's width.
            (
                img.get_pixel(30, 36).0 != untouched,
                img.get_pixel(70, 36).0 != untouched,
            )
        };
        let opts = |double, head_at_start| ArrowOptions {
            double,
            head_at_start,
        };
        assert_eq!(head_at(opts(false, false)), (false, true));
        assert_eq!(head_at(opts(false, true)), (true, false));
        assert_eq!(head_at(opts(true, false)), (true, true));
    }

    #[test]
    fn blur_keeps_flat_areas_flat() {
        let mut c = canvas();
        draw(&mut c, Tool::Blur, (1000.0, 500.0), (1100.0, 560.0));
        assert_eq!(c.image().get_pixel(50, 30).0, [200, 200, 200, 255]);
    }

    #[test]
    fn moving_region_keeps_annotations_on_screen() {
        let mut c = canvas();
        draw(&mut c, Tool::Rect, (1010.0, 510.0), (1050.0, 540.0));
        let img = RgbaImage::from_pixel(100, 60, image::Rgba([200, 200, 200, 255]));
        c.set_region(
            Rect {
                x: 990,
                y: 500,
                w: 100,
                h: 60,
            },
            img,
        );
        assert_eq!(c.image().get_pixel(20, 25).0, [0xff, 0x3b, 0x30, 255]); // left edge now at x=20
    }
}

#[cfg(test)]
mod bench {
    use super::*;

    /// Times a large blur: `cargo test --release blur_timing -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn blur_timing() {
        let mut pm = Pixmap::new(1920, 1080).unwrap();
        for (i, p) in pm.data_mut().iter_mut().enumerate() {
            *p = (i * 31 % 251) as u8;
        }
        let r = Rect {
            x: 100,
            y: 100,
            w: 1500,
            h: 800,
        };
        let start = std::time::Instant::now();
        for _ in 0..10 {
            blur(&mut pm, r, Size::Large.blur_radius());
        }
        println!("blur 1500x800: {:?} each", start.elapsed() / 10);
    }
}
