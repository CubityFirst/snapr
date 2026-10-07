//! Annotations drawn over the selected region, rasterized with tiny-skia.
//!
//! The canvas keeps the selected region's pixels (`base`), the finished
//! annotations drawn on top (`committed`), and the shown frame (`frame` =
//! committed + the annotation being drawn). The frame is exactly what gets
//! saved, and only the area that changed is redrawn while drawing.

use std::sync::{Arc, LazyLock};

use image::RgbaImage;
use tiny_skia::{
    FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, PixmapPaint, Stroke, StrokeDash,
    Transform,
};

use crate::capture::Rect;

type Point = (f32, f32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Select,
    Pen,
    Line,
    Arrow,
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
    pub const ALL: [Tool; 13] = [
        Tool::Select,
        Tool::Pen,
        Tool::Line,
        Tool::Arrow,
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
            Tool::Step => "Step: click to place numbered markers 1, 2, 3\u{2026}",
            Tool::Rect => "Box",
            Tool::Ellipse => "Ellipse",
            Tool::Highlighter => "Highlighter",
            Tool::Blur => "Blur",
            Tool::Pixelate => "Pixelate",
            Tool::Image => "Image redaction: cover an area with your chosen image (set in Settings)",
            Tool::Eraser => {
                "Smart eraser: drag a box filled with the colour under where you start (hold Ctrl to move it)"
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArrowOptions {
    /// A head at both ends.
    pub double: bool,
    /// The head goes where the drag started, pointing back at it, rather
    /// than where it ends.
    pub head_at_start: bool,
}

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
    pub size: Size,
    pub pixelate: PixelateOptions,
    pub arrow: ArrowOptions,
    /// Clips get a soft drop shadow, lifting them off the screen under them.
    pub clip_shadow: bool,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            color: COLORS[0],
            size: Size::Medium,
            pixelate: PixelateOptions {
                block: 14,
                secure: false,
            },
            arrow: ArrowOptions {
                double: false,
                head_at_start: false,
            },
            clip_shadow: true,
        }
    }
}

#[derive(Debug, Clone)]
enum Shape {
    Pen(Vec<Point>),
    Highlighter(Vec<Point>),
    /// From, to, and the node in the middle the curve passes through, once
    /// it's been moved off the straight line.
    Line(Point, Point, Option<Point>),
    /// Like a line, with a head at the end.
    Arrow(Point, Point, Option<Point>),
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
    /// A numbered marker: its centre and number.
    Step(Point, u32),
    /// A box filled with the colour sampled where it was started
    /// (premultiplied RGBA, as stored in the pixmap).
    Erase(Point, Point, [u8; 4]),
}

/// How close (in pixels) the mouse has to be to a line or stroke to pick it.
const HIT_TOLERANCE: i32 = 5;

/// Shadow under a clip, in pixels.
const CLIP_SHADOW: i32 = 5;

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
    pub fn new(tool: Tool, style: Style, p: Point) -> Option<Self> {
        let shape = match tool {
            Tool::Select => return None,
            Tool::Pen => Shape::Pen(vec![p]),
            Tool::Highlighter => Shape::Highlighter(vec![p]),
            Tool::Line => Shape::Line(p, p, None),
            Tool::Arrow => Shape::Arrow(p, p, None),
            Tool::Rect => Shape::Rect(p, p),
            Tool::Ellipse => Shape::Ellipse(p, p),
            Tool::Blur => Shape::Blur(p, p),
            Tool::Pixelate => Shape::Pixelate(p, p),
            // Its picture is added with `with_image`.
            Tool::Image => Shape::Image(p, p, None),
            Tool::Clip => Shape::ClipSelect(p, p),
            // Numbered when it's added to the canvas.
            Tool::Step => Shape::Step(p, 0),
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
            Shape::Pen(pts) | Shape::Highlighter(pts) => {
                let last = *pts.last().expect("never empty");
                if (p.0 - last.0).hypot(p.1 - last.1) >= 1.0 {
                    pts.push(p);
                }
            }
            Shape::Line(a, b, _) | Shape::Arrow(a, b, _) => {
                *b = if constrain { snap_45(*a, p) } else { p }
            }
            Shape::Rect(a, b) | Shape::Ellipse(a, b) => {
                *b = if constrain { square(*a, p) } else { p }
            }
            Shape::Blur(_, b)
            | Shape::Pixelate(_, b)
            | Shape::Image(_, b, _)
            | Shape::ClipSelect(_, b)
            | Shape::Erase(_, b, _) => *b = p,
            Shape::Clip(..) => {}
            // It follows the mouse until it's dropped.
            Shape::Step(c, _) => *c = p,
        }
    }

    /// Shifts the whole annotation by `d`, e.g. while drawing with Ctrl held.
    fn translate(&mut self, d: Point) {
        let mv = |p: &mut Point| *p = (p.0 + d.0, p.1 + d.1);
        match &mut self.shape {
            Shape::Pen(pts) | Shape::Highlighter(pts) => pts.iter_mut().for_each(mv),
            Shape::Line(a, b, mid) | Shape::Arrow(a, b, mid) => {
                mv(a);
                mv(b);
                if let Some(m) = mid {
                    mv(m);
                }
            }
            Shape::Rect(a, b)
            | Shape::Ellipse(a, b)
            | Shape::Blur(a, b)
            | Shape::Pixelate(a, b)
            | Shape::Image(a, b, _)
            | Shape::ClipSelect(a, b)
            | Shape::Erase(a, b, _) => {
                mv(a);
                mv(b);
            }
            Shape::Step(c, _) => mv(c),
            Shape::Clip(_, at) => *at = (at.0 + d.0.round() as i32, at.1 + d.1.round() as i32),
        }
    }

    /// Whether it's too small to keep, e.g. a click with the box tool.
    fn is_empty(&self) -> bool {
        match &self.shape {
            Shape::Pen(_) => false,
            Shape::Highlighter(p) => p.len() < 2,
            Shape::Line(a, b, _) | Shape::Arrow(a, b, _) => (b.0 - a.0).hypot(b.1 - a.1) < 2.0,
            Shape::Rect(a, b)
            | Shape::Ellipse(a, b)
            | Shape::Blur(a, b)
            | Shape::Pixelate(a, b)
            | Shape::Image(a, b, _)
            | Shape::Erase(a, b, _) => (b.0 - a.0).abs() < 2.0 || (b.1 - a.1).abs() < 2.0,
            Shape::ClipSelect(a, b) => (b.0 - a.0).abs() < 4.0 || (b.1 - a.1).abs() < 4.0,
            Shape::Clip(..) | Shape::Step(..) => false,
        }
    }

    fn step_radius(&self) -> f32 {
        match self.style.size {
            Size::Small => 11.0,
            Size::Medium => 15.0,
            Size::Large => 21.0,
        }
    }

    /// Where it is on screen (global pixels), if `tool` can move it: clips
    /// with the clip tool, blurred and pixelated areas with their own tools.
    fn movable_rect(&self, tool: Tool) -> Option<Rect> {
        match (&self.shape, tool) {
            (Shape::Clip(img, (x, y)), Tool::Clip) => Some(Rect {
                x: *x,
                y: *y,
                w: img.width(),
                h: img.height(),
            }),
            (Shape::Blur(a, b), Tool::Blur)
            | (Shape::Pixelate(a, b), Tool::Pixelate)
            | (Shape::Image(a, b, _), Tool::Image)
            | (Shape::Erase(a, b, _), Tool::Eraser) => Some(Rect::from_points(
                (a.0 as f64, a.1 as f64),
                (b.0 as f64, b.1 as f64),
            )),
            (Shape::Step(c, _), Tool::Step) => {
                let r = self.step_radius() as f64;
                let c = (c.0 as f64, c.1 as f64);
                Some(Rect::from_points((c.0 - r, c.1 - r), (c.0 + r, c.1 + r)))
            }
            _ => None,
        }
    }

    /// The tool that moves it, for the ones that can be picked up.
    fn mover(&self) -> Option<Tool> {
        match self.shape {
            Shape::Clip(..) => Some(Tool::Clip),
            Shape::Blur(..) => Some(Tool::Blur),
            Shape::Pixelate(..) => Some(Tool::Pixelate),
            Shape::Image(..) => Some(Tool::Image),
            Shape::Erase(..) => Some(Tool::Eraser),
            Shape::Step(..) => Some(Tool::Step),
            _ => None,
        }
    }

    /// Whether `p` is on it, and if so the area to outline (global pixels):
    /// anywhere inside a clip or area; within a few pixels of what's drawn
    /// for lines, shapes and strokes.
    fn hit(&self, p: Point) -> Option<Rect> {
        if let Some(tool) = self.mover() {
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

    /// Moves a clip or area so its top-left corner is at `p`.
    fn move_to(&mut self, p: Point) {
        let p = (p.0.round(), p.1.round());
        let radius = self.step_radius();
        match &mut self.shape {
            Shape::Clip(_, at) => *at = (p.0 as i32, p.1 as i32),
            Shape::Blur(a, b)
            | Shape::Pixelate(a, b)
            | Shape::Image(a, b, _)
            | Shape::Erase(a, b, _) => {
                let size = ((b.0 - a.0).abs(), (b.1 - a.1).abs());
                *a = p;
                *b = (p.0 + size.0, p.1 + size.1);
            }
            // `p` is the top-left of its square; the centre is a radius in.
            Shape::Step(c, _) => *c = (p.0 + radius, p.1 + radius),
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

    /// The middle node of a line drawn with `tool` (a line or an arrow),
    /// which can be dragged to curve it.
    fn bend_node(&self, tool: Tool) -> Option<Point> {
        match (&self.shape, tool) {
            (Shape::Line(a, b, mid), Tool::Line) | (Shape::Arrow(a, b, mid), Tool::Arrow) => {
                Some(mid.unwrap_or(midpoint(*a, *b)))
            }
            _ => None,
        }
    }

    /// Moves a line's or an arrow's middle node to `p`. Close to the
    /// straight line, it snaps back to straight.
    fn bend_to(&mut self, p: Point) {
        if let Shape::Line(a, b, mid) | Shape::Arrow(a, b, mid) = &mut self.shape {
            let m = midpoint(*a, *b);
            *mid = ((p.0 - m.0).hypot(p.1 - m.1) >= 4.0).then_some(p);
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
        if let Shape::Step(c, _) = self.shape {
            let r = self.step_radius() + 2.0;
            return Rect::from_points(
                ((c.0 - r) as f64, (c.1 - r) as f64),
                ((c.0 + r) as f64, (c.1 + r) as f64),
            );
        }
        let pts: Vec<Point> = match &self.shape {
            Shape::Pen(p) | Shape::Highlighter(p) => p.clone(),
            // The curve stays inside the triangle of its control points.
            Shape::Line(a, b, mid) | Shape::Arrow(a, b, mid) => {
                vec![*a, *b, control(*a, *b, *mid)]
            }
            Shape::Rect(a, b)
            | Shape::Ellipse(a, b)
            | Shape::Blur(a, b)
            | Shape::Pixelate(a, b)
            | Shape::Image(a, b, _)
            | Shape::ClipSelect(a, b)
            | Shape::Erase(a, b, _) => vec![*a, *b],
            Shape::Clip(..) | Shape::Step(..) => unreachable!("handled above"),
        };
        let pad = match self.shape {
            Shape::Arrow(..) => arrow_head(self.stroke_width()).0 + 2.0,
            Shape::Blur(..) | Shape::Pixelate(..) => 1.0,
            Shape::ClipSelect(..) | Shape::Erase(..) | Shape::Image(..) => 3.0,
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
        paint.set_color_rgba8(
            r,
            g,
            b,
            if matches!(self.shape, Shape::Highlighter(_)) {
                110
            } else {
                255
            },
        );
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
                let mut pb = PathBuilder::new();
                pb.move_to(pts[0].0, pts[0].1);
                // Smooth through the midpoints of successive samples.
                for w in pts.windows(2).skip(1) {
                    let mid = ((w[0].0 + w[1].0) / 2.0, (w[0].1 + w[1].1) / 2.0);
                    pb.quad_to(w[0].0, w[0].1, mid.0, mid.1);
                }
                let last = pts[pts.len() - 1];
                pb.line_to(last.0, last.1);
                if let Some(path) = pb.finish() {
                    pm.stroke_path(&path, &paint, &stroke, id, None);
                }
            }
            Shape::Line(a, b, mid) => {
                let c = local(&control(*a, *b, *mid));
                let (a, b) = (local(a), local(b));
                let mut pb = PathBuilder::new();
                pb.move_to(a.0, a.1);
                if mid.is_some() {
                    pb.quad_to(c.0, c.1, b.0, b.1);
                } else {
                    pb.line_to(b.0, b.1);
                }
                if let Some(path) = pb.finish() {
                    pm.stroke_path(&path, &paint, &stroke, id, None);
                }
            }
            Shape::Arrow(a, b, mid) => {
                let c = local(&control(*a, *b, *mid));
                let (a, b) = (local(a), local(b));
                let len = (b.0 - a.0).hypot(b.1 - a.1);
                if len < 1.0 {
                    return;
                }
                let opts = self.style.arrow;
                let (head_at_start, head_at_end) = if opts.double {
                    (true, true)
                } else {
                    (opts.head_at_start, !opts.head_at_start)
                };
                let (head_len, head_w) = arrow_head(stroke.width);
                // Two heads get at most half the line each.
                let head_len = head_len.min(if opts.double { len / 2.0 } else { len });
                // The shaft stops inside each head so the round cap doesn't
                // poke out: the part of the curve from `t0` to `t1`, whose
                // control points come from blossoming the curve.
                let t0 = if head_at_start {
                    1.0 - t_at_distance(b, c, a, head_len * 0.7)
                } else {
                    0.0
                };
                let t1 = if head_at_end {
                    t_at_distance(a, c, b, head_len * 0.7)
                } else {
                    1.0
                };
                let blossom = |u: f32, v: f32| {
                    let (wa, wb) = ((1.0 - u) * (1.0 - v), u * v);
                    let wc = 1.0 - wa - wb;
                    (
                        wa * a.0 + wc * c.0 + wb * b.0,
                        wa * a.1 + wc * c.1 + wb * b.1,
                    )
                };
                if t0 < t1 {
                    let (start, ctrl, end) = (blossom(t0, t0), blossom(t0, t1), blossom(t1, t1));
                    let mut shaft = PathBuilder::new();
                    shaft.move_to(start.0, start.1);
                    if mid.is_some() {
                        shaft.quad_to(ctrl.0, ctrl.1, end.0, end.1);
                    } else {
                        shaft.line_to(end.0, end.1);
                    }
                    if let Some(path) = shaft.finish() {
                        pm.stroke_path(&path, &paint, &stroke, id, None);
                    }
                }
                if head_at_end {
                    fill_head(pm, &paint, (a, c, b), head_len, head_w);
                }
                if head_at_start {
                    fill_head(pm, &paint, (b, c, a), head_len, head_w);
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
            Shape::ClipSelect(a, b) => dashed_outline(pm, local(a), local(b)),
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
            Shape::Step(c, n) => {
                let c = local(c);
                let r = self.step_radius();
                if let Some(circle) = PathBuilder::from_circle(c.0, c.1, r) {
                    pm.fill_path(&circle, &paint, FillRule::Winding, id, None);
                    // A thin ring keeps it visible on its own colour.
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
                draw_number(pm, *n, c, r, &text);
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
fn draw_number(pm: &mut Pixmap, n: u32, c: Point, r: f32, paint: &Paint) {
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
    // Digits about as tall as the circle is wide, narrowed to fit 2+ digits.
    let scale = (r * 1.1 / cap).min(r * 1.45 / advance.max(1.0));
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
        width: (r * 0.09).max(1.0),
        line_join: LineJoin::Round,
        ..Stroke::default()
    };
    pm.stroke_path(&path, paint, &stroke, id, None);
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

fn midpoint(a: Point, b: Point) -> Point {
    ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0)
}

/// The quadratic BÃ©zier control point that makes the curve from `a` to `b`
/// pass through `mid` halfway along.
fn control(a: Point, b: Point, mid: Option<Point>) -> Point {
    let m = midpoint(a, b);
    match mid {
        Some(p) => (2.0 * p.0 - m.0, 2.0 * p.1 - m.1),
        None => m,
    }
}

/// A point on the quadratic BÃ©zier curve from `a` to `b` with control `c`.
fn bezier(a: Point, c: Point, b: Point, t: f32) -> Point {
    let u = 1.0 - t;
    (
        u * u * a.0 + 2.0 * u * t * c.0 + t * t * b.0,
        u * u * a.1 + 2.0 * u * t * c.1 + t * t * b.1,
    )
}

/// Where along the curve (as `t`) the point `dist` short of the end `b` is.
fn t_at_distance(a: Point, c: Point, b: Point, dist: f32) -> f32 {
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..24 {
        let t = (lo + hi) / 2.0;
        let p = bezier(a, c, b, t);
        if (b.0 - p.0).hypot(b.1 - p.1) > dist {
            lo = t;
        } else {
            hi = t;
        }
    }
    lo
}

fn arrow_head(stroke: f32) -> (f32, f32) {
    let len = stroke * 3.0 + 10.0;
    (len, len * 0.85)
}

/// Fills an arrowhead `len` long and `width` wide at `b`, the end of the
/// curve from `a` with control `c`. It sits on the curve, pointing from
/// where it meets the shaft to the tip.
fn fill_head(pm: &mut Pixmap, paint: &Paint, (a, c, b): (Point, Point, Point), len: f32, width: f32) {
    let meets = bezier(a, c, b, t_at_distance(a, c, b, len));
    let (dx, dy) = (b.0 - meets.0, b.1 - meets.1);
    let d = dx.hypot(dy).max(f32::EPSILON);
    let (dir, perp) = ((dx / d, dy / d), (-dy / d, dx / d));
    let base = (b.0 - dir.0 * len, b.1 - dir.1 * len);
    let mut head = PathBuilder::new();
    head.move_to(b.0, b.1);
    head.line_to(base.0 + perp.0 * width / 2.0, base.1 + perp.1 * width / 2.0);
    head.line_to(base.0 - perp.0 * width / 2.0, base.1 - perp.1 * width / 2.0);
    head.close();
    if let Some(path) = head.finish() {
        pm.fill_path(&path, paint, FillRule::Winding, Transform::identity(), None);
    }
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
    /// Moving an arrow's middle node.
    Bend,
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
            }
            self.dirty = Some(union(self.dirty, area));
        }
        self.active_area = new_area;
    }

    pub fn begin(&mut self, mut annotation: Annotation) {
        match &mut annotation.shape {
            Shape::Step(_, n) => {
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

    /// Picks up the topmost line or arrow drawn with `tool` whose middle node
    /// is within `radius` of `p`, to bend it by dragging. Returns whether
    /// there was one.
    pub fn begin_bend(&mut self, p: Point, radius: f32, tool: Tool) -> bool {
        let near = |a: &Annotation| {
            a.bend_node(tool)
                .is_some_and(|n| (n.0 - p.0).hypot(n.1 - p.1) <= radius)
        };
        let Some(i) = self.annotations.iter().rposition(near) else {
            return false;
        };
        self.begin_edit(i, EditKind::Bend);
        true
    }

    /// The topmost annotation under `p` that `tool` can move, if any.
    fn movable_index_at(&self, p: Point, tool: Tool) -> Option<usize> {
        self.annotations.iter().rposition(|a| {
            a.movable_rect(tool)
                .is_some_and(|r| r.contains((p.0 as f64, p.1 as f64)))
        })
    }

    /// Whether `tool` can pick something up at `p`.
    pub fn movable_at(&self, p: Point, tool: Tool) -> bool {
        self.movable_index_at(p, tool).is_some()
    }

    /// Picks up the topmost annotation under `p` that `tool` can move (a
    /// clip, or a blurred or pixelated area), to move it by dragging.
    /// Returns whether there was one.
    pub fn begin_move(&mut self, p: Point, tool: Tool) -> bool {
        let Some(i) = self.movable_index_at(p, tool) else {
            return false;
        };
        let r = self.annotations[i].movable_rect(tool).expect("movable");
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

    /// Where the annotation a right-click at `p` would delete is (global
    /// pixels), to outline it.
    pub fn deletable_rect(&self, p: Point) -> Option<Rect> {
        self.deletable_at(p).map(|(_, r)| r)
    }

    /// Removes the annotation [`Self::deletable_rect`] finds at `p`.
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

    /// Whether an existing annotation is being bent or moved.
    pub fn is_editing(&self) -> bool {
        self.editing.is_some()
    }

    /// Middle nodes of the lines or arrows drawn with `tool`, which can be
    /// dragged to curve them.
    pub fn bend_nodes(&self, tool: Tool) -> Vec<Point> {
        let edited = self.active.as_ref().filter(|_| self.editing.is_some());
        self.annotations
            .iter()
            .chain(edited)
            .filter_map(|a| a.bend_node(tool))
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
                Some(EditKind::Bend) => a.bend_to(p),
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
        let Some(a) = self.active.take() else { return };
        if let Some(edit) = self.editing.take() {
            let mut before = self.annotations.clone();
            before.insert(edit.index, edit.original);
            self.undo.push(before);
            self.redo.clear();
            self.annotations.insert(edit.index, a);
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
        let below_clip = |clip_shadow| {
            let mut c = canvas();
            let style = Style {
                clip_shadow,
                ..Style::default()
            };
            c.begin(Annotation::new(Tool::Clip, style, (1005.0, 505.0)).unwrap());
            c.drag_to((1035.0, 535.0), false);
            c.commit();
            c.image().get_pixel(20, 37).0
        };
        let untouched = [200, 200, 200, 255];
        assert_ne!(below_clip(true), untouched);
        assert_eq!(below_clip(false), untouched);
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
        assert!(c.deletable_rect((80.0, 45.0)).is_none());
        assert!(c.deletable_rect((95.0, 45.0)).is_some());
        assert!(c.delete_at((50.0, 20.0)));
        assert!(!c.delete_at((50.0, 20.0)));
        assert_eq!(c.annotations.len(), 1);
        c.undo();
        assert_eq!(c.image().as_raw(), erased.as_raw());

        // Lines and arrows go too, from near their stroke.
        draw(&mut c, Tool::Arrow, (10.0, 50.0), (90.0, 50.0));
        assert!(c.deletable_rect((30.0, 40.0)).is_none());
        assert!(c.delete_at((30.0, 52.0)));
        assert_eq!(c.annotations.len(), 2);
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
                    Shape::Step(_, n) => Some(n),
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
        let mut style = Style::default();
        style.pixelate = PixelateOptions {
            block: 8,
            secure: true,
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
            let mut style = Style::default();
            style.arrow = arrow;
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
