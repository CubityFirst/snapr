//! One capture: the frozen overlays, annotating them, and dragging out the
//! region to save. Releasing the region drag takes the screenshot.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use egui::CursorIcon;
use egui_wgpu::wgpu;
use image::RgbaImage;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, ModifiersState, NamedKey, PhysicalKey};
use winit::window::WindowId;

use crate::annotate::{Annotation, Canvas, RedactImage, Style, Tool};
use crate::capture::{self, Rect};
use crate::gpu::Gpu;
use crate::naming::Context;
use crate::overlay::{Frame, Overlay};
use crate::settings::{CrosshairInfo, MagnifierScroll};
use crate::toolbar::{Action, CursorInfo};

pub enum Outcome {
    Continue,
    Cancel,
    Done {
        image: RgbaImage,
        save_file: bool,
        /// The region taken, for capturing it again.
        rect: Rect,
    },
    /// A region was picked for screen recording.
    Record(Rect),
    /// A region was picked to look for QR codes in.
    Scan(RgbaImage),
    /// A pixel's colour was picked (the colour picker).
    Color([u8; 3]),
    /// A region was picked to pin to the screen, where it was.
    Pin(RgbaImage, Rect),
}

/// What the region being picked is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Screenshot,
    Record,
    /// Reading QR codes (the QR code tool).
    Scan,
    /// Picking a pixel's colour: a click picks, there's no region.
    PickColor,
    /// Pinning a region to the screen.
    Pin,
}

impl Purpose {
    /// Shown at the top of the screen while picking, for anything but a
    /// screenshot.
    fn hint(self, scroll: MagnifierScroll) -> Option<&'static str> {
        match self {
            Purpose::Screenshot => None,
            Purpose::Record => Some("Select a region to record"),
            Purpose::Scan => Some("Select a QR code to read"),
            Purpose::PickColor => Some(match scroll {
                MagnifierScroll::Zoom => {
                    "Click to pick a colour \u{2014} arrow keys move one pixel, scroll zooms"
                }
                MagnifierScroll::Resize => {
                    "Click to pick a colour \u{2014} arrow keys move one pixel, \
                     scroll resizes the magnifier"
                }
            }),
            Purpose::Pin => Some("Select a region to pin to the screen"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Grab {
    None,
    /// Dragging out the region from `anchor`. Holding Ctrl moves the whole
    /// region with the mouse instead of resizing it.
    Region {
        anchor: (f64, f64),
    },
    Draw,
}

/// Pixels across the magnifier at first (odd, so one is in the middle).
pub const MAGNIFIER_PIXELS: u32 = 15;
/// The fewest and most pixels the mouse wheel zooms (or resizes) the
/// magnifier between. Resizing goes one step smaller still, to hide it.
const MAGNIFIER_ZOOM: (i32, i32) = (5, 45);

/// How close (in pixels) a press must be to a line's or an arrow's middle
/// node to grab it.
const NODE_RADIUS: f64 = 9.0;

fn node_hit(node: (f32, f32), p: (f64, f64)) -> bool {
    (node.0 as f64 - p.0).hypot(node.1 as f64 - p.1) <= NODE_RADIUS
}

/// Whether a region is too small to be a drag, so it was meant as a click.
fn is_click(r: &Rect) -> bool {
    r.w < 4 && r.h < 4
}

/// The region between `anchor` and the cursor; a square with `square` (Shift).
fn region_rect(anchor: (f64, f64), cursor: (f64, f64), square: bool) -> Rect {
    let mut end = cursor;
    if square {
        let (dx, dy) = (cursor.0 - anchor.0, cursor.1 - anchor.1);
        let side = dx.abs().max(dy.abs());
        end = (anchor.0 + side.copysign(dx), anchor.1 + side.copysign(dy));
    }
    Rect::from_points(anchor, end)
}

/// The GPU copy of the annotations, shared by all overlays.
struct Layer {
    texture: wgpu::Texture,
    size: (u32, u32),
}

/// State that decides which overlays need redrawing after a change.
struct Snap {
    cursor: Option<(f64, f64)>,
    selection: Option<Rect>,
    host: Option<WindowId>,
    canvas: Option<Rect>,
}

pub struct Session {
    overlays: HashMap<WindowId, Overlay>,
    pub context: Context,
    annotate: bool,
    pub purpose: Purpose,
    /// Custom frame-rate limit, if any.
    interval: Option<Duration>,
    /// What's shown beside the crosshair while selecting a region.
    crosshair_info: CrosshairInfo,
    /// Pixels across the magnifier; the mouse wheel zooms or resizes it. 0
    /// when resizing has hidden it.
    pub magnifier: u32,
    /// Mouse wheel movement not yet used up by a whole step (touchpads
    /// scroll in small amounts).
    wheel: f64,
    cursor: Option<(f64, f64)>,
    grab: Grab,
    /// Other apps' windows when the capture started, topmost first. Hovering
    /// one highlights it, and a click takes it.
    windows: Vec<Rect>,
    /// Annotations drawn on the frozen screen, if any.
    canvas: Option<Canvas>,
    layer: Option<Layer>,
    pub tool: Tool,
    pub style: Style,
    /// What the image redaction tool covers areas with; black boxes without.
    pub redact_image: Option<RedactImage>,
    modifiers: ModifiersState,
    /// A right-click that will close the capture when the button is released,
    /// so the release doesn't reach the window underneath (and, say, open
    /// its context menu).
    close_on_right_release: bool,
    /// Keys pressed inside the overlay, so their auto-repeat isn't mistaken
    /// for a key held from before it opened.
    keys_down: HashSet<PhysicalKey>,
    /// Keys already held when the overlay opened, e.g. the H of a Ctrl+H
    /// hotkey. They're ignored until released, or they'd pick a tool.
    stale_keys: HashSet<PhysicalKey>,
}

impl Session {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        event_loop: &ActiveEventLoop,
        gpu: &Gpu,
        annotate: bool,
        overlay_fps: u32,
        crosshair_info: CrosshairInfo,
        magnifier: u32,
        tool: Tool,
        style: Style,
        snap_to: &[&winit::window::Window],
    ) -> Result<Self, String> {
        // Grab every monitor first so the "frozen" frame is from the moment
        // the hotkey was pressed, before any of our windows appear.
        let time = chrono::Local::now();
        let shots = match capture::capture_all() {
            Ok(s) if !s.is_empty() => s,
            Ok(_) => return Err("no monitors found".into()),
            Err(e) => return Err(format!("screen capture failed: {e}")),
        };
        let (process, title) = capture::foreground_window();
        let windows = capture::window_rects(snap_to);
        let monitors: Vec<_> = event_loop.available_monitors().collect();
        let mut overlays = HashMap::new();
        for shot in shots {
            match Overlay::new(event_loop, gpu, shot, &monitors, overlay_fps == 0) {
                Ok(o) => {
                    overlays.insert(o.window.id(), o);
                }
                Err(e) => eprintln!("failed to create overlay: {e}"),
            }
        }
        if overlays.is_empty() {
            return Err("couldn't open any overlay windows".into());
        }
        let mut session = Self {
            overlays,
            context: Context {
                process,
                title,
                ..Context::new(time)
            },
            annotate,
            purpose: Purpose::Screenshot,
            interval: (overlay_fps > 0).then(|| Duration::from_secs_f64(1.0 / overlay_fps as f64)),
            crosshair_info,
            // Hidden by resizing, then switched to zooming: show it again.
            magnifier: match crosshair_info.scroll {
                MagnifierScroll::Zoom if magnifier == 0 => MAGNIFIER_PIXELS,
                _ => magnifier,
            },
            wheel: 0.0,
            cursor: capture::cursor_position(),
            grab: Grab::None,
            windows,
            canvas: None,
            layer: None,
            tool,
            style,
            redact_image: None,
            modifiers: ModifiersState::empty(),
            close_on_right_release: false,
            keys_down: HashSet::new(),
            stale_keys: HashSet::new(),
        };
        let ids: Vec<_> = session.overlays.keys().copied().collect();
        for id in ids {
            let frame = session.frame_for(id);
            session
                .overlays
                .get_mut(&id)
                .expect("exists")
                .show(gpu, &frame);
        }
        // Focus just one, for the keyboard shortcuts: focusing each in turn
        // makes them fight over it.
        if let Some(o) = session
            .focus_target()
            .and_then(|id| session.overlays.get(&id))
        {
            o.window.focus_window();
        }
        Ok(session)
    }

    /// The overlay to give keyboard focus: the toolbar's, else the one under
    /// the cursor.
    fn focus_target(&self) -> Option<WindowId> {
        self.toolbar_host().or_else(|| {
            let c = self.cursor?;
            self.overlays
                .iter()
                .find(|(_, o)| o.global_rect().contains(c))
                .map(|(id, _)| *id)
                .or_else(|| self.overlays.keys().next().copied())
        })
    }

    pub fn has_window(&self, id: WindowId) -> bool {
        self.overlays.contains_key(&id)
    }

    /// Takes focus back after a capture stacked on top of this one closed.
    pub fn resume(&mut self) {
        self.grab = Grab::None;
        // The cursor may have moved while the other capture was open.
        self.cursor = capture::cursor_position().or(self.cursor);
        let target = self.focus_target();
        for (id, o) in &mut self.overlays {
            o.request_redraw();
            if Some(*id) == target {
                o.window.focus_window();
            }
        }
    }

    /// The region being dragged out (not counting a click that hasn't moved
    /// far enough to be a drag).
    fn selection(&self) -> Option<Rect> {
        if self.purpose == Purpose::PickColor {
            return None;
        }
        match (self.grab, self.cursor) {
            (Grab::Region { anchor }, Some(c)) => {
                Some(region_rect(anchor, c, self.modifiers.shift_key())).filter(|r| !is_click(r))
            }
            _ => None,
        }
    }

    /// The window under the cursor while choosing a region, clipped to its
    /// monitor: what a click would take.
    fn hovered_window(&self) -> Option<Rect> {
        let region_tool = self.tool == Tool::Select || !self.annotate;
        let choosing = match self.grab {
            Grab::None => true,
            Grab::Region { .. } => self.selection().is_none(),
            Grab::Draw => false,
        };
        if !region_tool || !choosing || self.over_toolbar() || self.purpose == Purpose::PickColor {
            return None;
        }
        let c = self.cursor?;
        let monitor = self.monitor_at(c)?;
        self.windows
            .iter()
            .find(|w| w.contains(c))?
            .intersect(&monitor)
    }

    /// The dragged region, or else the window a click would take.
    fn shown_selection(&self) -> Option<Rect> {
        self.selection().or_else(|| self.hovered_window())
    }

    fn monitor_at(&self, p: (f64, f64)) -> Option<Rect> {
        self.overlays
            .values()
            .map(|o| o.global_rect())
            .find(|r| r.contains(p))
    }

    /// The overlay showing the toolbar: the one under the cursor.
    fn toolbar_host(&self) -> Option<WindowId> {
        if !self.annotate {
            return None;
        }
        let find = |p| {
            self.overlays
                .iter()
                .find(|(_, o)| o.global_rect().contains(p))
                .map(|(id, _)| *id)
        };
        self.cursor
            .and_then(find)
            .or_else(|| find((0.0, 0.0)))
            .or_else(|| self.overlays.keys().next().copied())
    }

    /// The whole monitor under the cursor (or hosting the toolbar).
    fn current_monitor(&self) -> Option<Rect> {
        self.cursor.and_then(|c| self.monitor_at(c)).or_else(|| {
            self.toolbar_host()
                .and_then(|id| self.overlays.get(&id))
                .map(|o| o.global_rect())
        })
    }

    fn frame_for(&self, id: WindowId) -> Frame {
        let dragging = self.selection().is_some();
        let selection = self.shown_selection();
        let region_tool = self.tool == Tool::Select || !self.annotate;
        let canvas = self.canvas.as_ref();
        let editing = canvas.is_some_and(Canvas::is_editing);
        let arrow_nodes = match canvas {
            Some(c)
                if matches!(self.tool, Tool::Line | Tool::Arrow)
                    && (editing || self.grab == Grab::None) =>
            {
                c.bend_nodes(self.tool)
            }
            _ => Vec::new(),
        };
        let over_node = self
            .cursor
            .is_some_and(|p| arrow_nodes.iter().any(|n| node_hit(*n, p)));
        let over_movable = self.grab == Grab::None
            && canvas
                .zip(self.cursor)
                .is_some_and(|(c, p)| c.movable_at((p.0 as f32, p.1 as f32), self.tool));
        let hovered = canvas
            .zip(self.cursor)
            .filter(|_| self.annotate && self.grab == Grab::None && !self.over_toolbar())
            .and_then(|(c, p)| c.deletable_rect((p.0 as f32, p.1 as f32)));
        let picking = self.purpose == Purpose::PickColor;
        let on_this_monitor = self
            .cursor
            .zip(self.overlays.get(&id))
            .is_some_and(|(c, o)| o.global_rect().contains(c));
        Frame {
            selection,
            // Not over the toolbar, where the cursor is a normal pointer.
            // Picking a colour shows the loupe instead.
            crosshair: if region_tool && !picking && self.grab == Grab::None && !self.over_toolbar()
            {
                self.cursor
            } else {
                None
            },
            layer: self.canvas.as_ref().map(|c| c.rect),
            // Dim while choosing a region; draw, or pick a colour, on an
            // undimmed screen.
            dim_all: region_tool && !picking,
            decorate: selection.is_some(),
            show_toolbar: !dragging && self.toolbar_host() == Some(id),
            tool: self.tool,
            style: self.style,
            can_undo: self.canvas.as_ref().is_some_and(|c| c.can_undo()),
            can_redo: self.canvas.as_ref().is_some_and(|c| c.can_redo()),
            cursor: if editing {
                CursorIcon::Grabbing
            } else if over_node || over_movable {
                CursorIcon::Grab
            } else {
                CursorIcon::Crosshair
            },
            hovered,
            arrow_nodes: arrow_nodes
                .into_iter()
                .map(|(x, y)| (x as f64, y as f64))
                .collect(),
            // On the monitor with the cursor, unless the region covers it.
            hint: self
                .purpose
                .hint(self.crosshair_info.scroll)
                .or_else(|| {
                    (self.annotate && self.tool == Tool::Image && self.redact_image.is_none())
                        .then_some(
                            "No redaction image chosen \u{2014} pick one in Settings. \
                             Drawing black boxes for now.",
                        )
                })
                .filter(|_| !dragging && on_this_monitor),
            cursor_info: self
                .cursor
                .filter(|_| on_this_monitor)
                .and_then(|c| Some((c, self.cursor_info(c)?))),
        }
    }

    /// The card beside the cursor at `c`: the colour picker's magnifier and
    /// colour, or while selecting a region, the crosshair info chosen in
    /// Settings.
    fn cursor_info(&self, c: (f64, f64)) -> Option<CursorInfo> {
        let show = self.crosshair_info;
        let (magnifier, color) = self.card()?;
        Some(CursorInfo {
            magnifier: (magnifier && self.magnifier > 0).then(|| self.loupe_pixels(c)),
            fixed_cells: show.scroll == MagnifierScroll::Resize,
            color: color.then(|| self.pixel_at(c)),
            position: show
                .position
                .then(|| [c.0.floor() as i32, c.1.floor() as i32]),
        })
    }

    /// Whether the card beside the cursor shows, and if so whether it has
    /// the magnifier (even one resized away) and the colour.
    fn card(&self) -> Option<(bool, bool)> {
        let show = self.crosshair_info;
        let selecting = (self.tool == Tool::Select || !self.annotate)
            && self.grab != Grab::Draw
            && !self.over_toolbar();
        match self.purpose {
            Purpose::PickColor => Some((true, true)),
            _ if selecting && show.any() => Some((show.magnifier, show.color)),
            _ => None,
        }
    }

    /// Whether the cursor is over the toolbar (or one of its popups).
    fn over_toolbar(&self) -> bool {
        let Some(c) = self.cursor else { return false };
        self.toolbar_host()
            .and_then(|id| self.overlays.get(&id))
            .is_some_and(|o| o.over_toolbar(c))
    }

    fn snap(&self) -> Snap {
        Snap {
            cursor: self.cursor,
            selection: self.shown_selection(),
            host: self.toolbar_host(),
            canvas: self.canvas.as_ref().map(|c| c.rect),
        }
    }

    /// Redraws overlays affected by a change: those holding the old or new
    /// cursor or touching the old or new selection, plus (with `everything`)
    /// the toolbar and annotations.
    fn invalidate(&mut self, before: Snap, everything: bool) {
        let after = self.snap();
        // Generous margin so the border and size label above are included.
        let grow = |r: Rect| Rect {
            x: r.x - 60,
            y: r.y - 60,
            w: r.w + 120,
            h: r.h + 120,
        };
        let mut rects: Vec<Rect> = [before.selection, after.selection]
            .into_iter()
            .flatten()
            .map(grow)
            .collect();
        if everything || self.grab == Grab::Draw {
            rects.extend([before.canvas, after.canvas].into_iter().flatten());
        }
        let cursors: Vec<(f64, f64)> = [before.cursor, after.cursor]
            .into_iter()
            .flatten()
            .collect();
        let mut hosts = Vec::new();
        if everything || before.host != after.host {
            hosts.extend([before.host, after.host].into_iter().flatten());
        }
        let interval = self.interval;
        for (id, o) in &mut self.overlays {
            let win = o.global_rect();
            let touched = hosts.contains(id)
                || cursors.iter().any(|&c| win.contains(c))
                || rects.iter().any(|r| r.intersect(&win).is_some());
            if touched {
                request_redraw(o, interval);
            }
        }
    }

    fn crop(&self, rect: Rect) -> RgbaImage {
        let shots: Vec<_> = self.overlays.values().map(|o| o.placed_shot()).collect();
        capture::crop(&shots, rect).unwrap_or_else(|| RgbaImage::new(rect.w.max(1), rect.h.max(1)))
    }

    /// Makes sure the annotation canvas covers the monitor at `p`.
    fn ensure_canvas_at(&mut self, p: (f64, f64)) -> bool {
        let Some(monitor) = self.monitor_at(p) else {
            return false;
        };
        let rect = match &self.canvas {
            None => monitor,
            Some(c) if c.rect.contains(p) => return true,
            Some(c) => c.rect.union(&monitor),
        };
        let base = self.crop(rect);
        match &mut self.canvas {
            None => self.canvas = Some(Canvas::new(rect, base)),
            Some(c) => c.set_region(rect, base),
        }
        true
    }

    /// Uploads the changed part of the canvas to the GPU layer.
    fn sync_layer(&mut self, gpu: &Gpu) {
        let Some(canvas) = &mut self.canvas else {
            return;
        };
        let Some((mut dirty, pixmap)) = canvas.take_dirty() else {
            return;
        };
        let (w, h) = (pixmap.width(), pixmap.height());
        let fits = self
            .layer
            .as_ref()
            .is_some_and(|l| l.size.0 >= w && l.size.1 >= h);
        if !fits {
            // Grow in steps so growing the canvas doesn't reallocate often.
            let round = |v: u32| v.div_ceil(256) * 256;
            let old = self.layer.as_ref().map_or((0, 0), |l| l.size);
            let size = (round(w).max(old.0), round(h).max(old.1));
            let format = self
                .overlays
                .values()
                .next()
                .map_or(wgpu::TextureFormat::Rgba8Unorm, |o| o.texture_format);
            let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("annotation layer"),
                size: wgpu::Extent3d {
                    width: size.0,
                    height: size.1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            for o in self.overlays.values_mut() {
                o.set_layer(gpu, &view);
            }
            self.layer = Some(Layer { texture, size });
            dirty = Rect { x: 0, y: 0, w, h };
        }
        let layer = self.layer.as_ref().expect("just created");
        // Send only the changed rectangle, not whole rows of the canvas.
        let packed: Vec<u8>;
        let (data, row_bytes) = if dirty.w == w {
            let offset = (dirty.y as u32 * w * 4) as usize;
            (&pixmap.data()[offset..], w * 4)
        } else {
            let (stride, x0, len) = (w as usize * 4, dirty.x as usize * 4, dirty.w as usize * 4);
            packed = (dirty.y as usize..dirty.bottom() as usize)
                .flat_map(|y| &pixmap.data()[y * stride + x0..y * stride + x0 + len])
                .copied()
                .collect();
            (&packed[..], dirty.w * 4)
        };
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &layer.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: dirty.x as u32,
                    y: dirty.y as u32,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row_bytes),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: dirty.w,
                height: dirty.h,
                depth_or_array_layers: 1,
            },
        );
    }

    pub fn window_event(&mut self, gpu: &Gpu, id: WindowId, event: &WindowEvent) -> Outcome {
        let Some(overlay) = self.overlays.get_mut(&id) else {
            return Outcome::Continue;
        };
        overlay.egui_event(event);
        match event {
            WindowEvent::RedrawRequested => return self.redraw(gpu, id),
            WindowEvent::Resized(_) => overlay.request_redraw(),
            WindowEvent::ModifiersChanged(m) => {
                // Shift changes the shape of the region being dragged.
                let before = self.snap();
                self.modifiers = m.state();
                self.invalidate(before, false);
            }
            WindowEvent::CursorMoved { position, .. } => {
                let p = overlay.to_global(*position);
                self.on_move(p);
            }
            WindowEvent::MouseWheel { delta, .. } => self.on_wheel(*delta),
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                let over_toolbar = self.cursor.is_some_and(|c| overlay.over_toolbar(c));
                match state {
                    ElementState::Pressed if !over_toolbar => self.on_press(),
                    ElementState::Released => return self.on_release(),
                    _ => {}
                }
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Right,
                ..
            } => match state {
                ElementState::Pressed => self.on_right_press(),
                ElementState::Released if self.close_on_right_release => return Outcome::Cancel,
                ElementState::Released => {}
            },
            WindowEvent::KeyboardInput {
                event,
                is_synthetic,
                ..
            } => {
                let key = event.physical_key;
                match event.state {
                    ElementState::Released => {
                        self.keys_down.remove(&key);
                        self.stale_keys.remove(&key);
                    }
                    // A key held as the overlay opened shows up as a synthetic
                    // press when it takes focus, or as auto-repeat.
                    ElementState::Pressed if *is_synthetic || event.repeat => {
                        if !self.keys_down.contains(&key) {
                            self.stale_keys.insert(key);
                        }
                    }
                    ElementState::Pressed if self.stale_keys.contains(&key) => {}
                    ElementState::Pressed => {
                        self.keys_down.insert(key);
                        return self.on_key(event);
                    }
                }
            }
            WindowEvent::CloseRequested => return Outcome::Cancel,
            _ => {}
        }
        Outcome::Continue
    }

    /// Draws the overlay Windows asked to paint, and any others waiting to
    /// be redrawn. Windows paints one window at a time, and only once no input
    /// is waiting: while the mouse moves, the overlay it's captured by gets
    /// every turn, and one it's dragged onto would hardly ever update.
    fn redraw(&mut self, gpu: &Gpu, id: WindowId) -> Outcome {
        self.sync_layer(gpu);
        let waiting: Vec<WindowId> = self
            .overlays
            .iter()
            .filter(|(other, o)| **other != id && o.redraw_pending)
            .map(|(other, _)| *other)
            .collect();
        for id in std::iter::once(id).chain(waiting) {
            match self.render(gpu, id) {
                Outcome::Continue => {}
                done => return done,
            }
        }
        Outcome::Continue
    }

    fn render(&mut self, gpu: &Gpu, id: WindowId) -> Outcome {
        let frame = self.frame_for(id);
        let Some(overlay) = self.overlays.get_mut(&id) else {
            return Outcome::Continue;
        };
        overlay.due = None;
        let actions = overlay.render(gpu, &frame);
        for action in actions {
            match self.apply(action) {
                Outcome::Continue => {}
                done => return done,
            }
        }
        Outcome::Continue
    }

    fn apply(&mut self, action: Action) -> Outcome {
        let before = self.snap();
        match action {
            Action::Tool(t) => self.tool = t,
            Action::Color(c) => self.style.color = c,
            Action::Size(s) => self.style.size = s,
            Action::Pixelate(o) => self.style.pixelate = o,
            Action::Arrow(o) => self.style.arrow = o,
            Action::ClipShadow(on) => self.style.clip_shadow = on,
            Action::Undo => self.canvas.iter_mut().for_each(Canvas::undo),
            Action::Redo => self.canvas.iter_mut().for_each(Canvas::redo),
        }
        self.invalidate(before, true);
        Outcome::Continue
    }

    /// The frozen pixels around `p` for the magnifier. Off-screen pixels
    /// are transparent.
    fn loupe_pixels(&self, p: (f64, f64)) -> RgbaImage {
        let n = self.magnifier;
        let half = n as i32 / 2;
        self.crop(Rect {
            x: p.0.floor() as i32 - half,
            y: p.1.floor() as i32 - half,
            w: n,
            h: n,
        })
    }

    /// Zooms the magnifier, if it's on: fewer, bigger pixels scrolling up,
    /// more scrolling down, its size staying the same. Or, if Settings says
    /// so, resizes it: bigger scrolling up, smaller scrolling down until it
    /// hides, its pixels staying the same size.
    fn on_wheel(&mut self, delta: MouseScrollDelta) {
        if self.cursor.is_none() || !self.card().is_some_and(|(magnifier, _)| magnifier) {
            return;
        }
        self.wheel += match delta {
            MouseScrollDelta::LineDelta(_, y) => y as f64,
            MouseScrollDelta::PixelDelta(p) => p.y / 60.0,
        };
        let steps = self.wheel.trunc();
        if steps == 0.0 {
            return;
        }
        self.wheel -= steps;
        let before = self.snap();
        let (min, max) = MAGNIFIER_ZOOM;
        let (n, steps) = (self.magnifier as i32, steps as i32);
        self.magnifier = match self.crosshair_info.scroll {
            MagnifierScroll::Zoom => (n - 2 * steps).clamp(min, max),
            MagnifierScroll::Resize => {
                // Hidden is one step below the smallest.
                let hidden = min - 2;
                let n = (n.max(hidden) + 2 * steps).clamp(hidden, max);
                if n == hidden { 0 } else { n }
            }
        } as u32;
        self.invalidate(before, false);
    }

    /// The frozen colour of the pixel at `p`.
    fn pixel_at(&self, p: (f64, f64)) -> [u8; 3] {
        let rect = Rect {
            x: p.0.floor() as i32,
            y: p.1.floor() as i32,
            w: 1,
            h: 1,
        };
        let [r, g, b, _] = self.crop(rect).get_pixel(0, 0).0;
        [r, g, b]
    }

    /// The colour of the pixel under the cursor.
    fn pick_color(&self) -> Outcome {
        match self.cursor {
            Some(c) => Outcome::Color(self.pixel_at(c)),
            None => Outcome::Continue,
        }
    }

    /// Moves the mouse pointer by whole pixels, for picking a colour exactly.
    fn nudge(&self, dx: f64, dy: f64) {
        let Some(c) = self.cursor else { return };
        let Some(o) = self.overlays.values().find(|o| o.global_rect().contains(c)) else {
            return;
        };
        let r = o.global_rect();
        let local = winit::dpi::PhysicalPosition::new(
            c.0.floor() + 0.5 + dx - r.x as f64,
            c.1.floor() + 0.5 + dy - r.y as f64,
        );
        let _ = o.window.set_cursor_position(local);
    }

    /// Takes the screenshot of `rect`: the frozen screen plus annotations.
    fn finish(&mut self, rect: Rect, save_file: bool) -> Outcome {
        match self.purpose {
            Purpose::Record => return Outcome::Record(rect),
            Purpose::Scan => return Outcome::Scan(self.crop(rect)),
            Purpose::PickColor => return self.pick_color(),
            Purpose::Pin => return Outcome::Pin(self.crop(rect), rect),
            Purpose::Screenshot => {}
        }
        let mut image = self.crop(rect);
        if let Some(canvas) = &mut self.canvas {
            canvas.commit();
            canvas.copy_into(&mut image, rect);
        }
        Outcome::Done {
            image,
            save_file,
            rect,
        }
    }

    /// Takes the whole monitor under the cursor.
    fn finish_monitor(&mut self, save_file: bool) -> Outcome {
        match self.current_monitor() {
            Some(rect) => self.finish(rect, save_file),
            None => Outcome::Continue,
        }
    }

    fn on_move(&mut self, p: (f64, f64)) {
        let before = self.snap();
        let previous = self.cursor.replace(p);
        match self.grab {
            Grab::Draw => {
                // Dragging (say, a clip) onto another monitor: grow the canvas
                // to cover it, or the drag disappears off its edge.
                self.ensure_canvas_at(p);
                let shift = self.modifiers.shift_key();
                if let Some(c) = &mut self.canvas {
                    match previous {
                        // Ctrl moves what's being drawn instead of reshaping it.
                        Some(prev) if self.modifiers.control_key() => {
                            c.shift_active(((p.0 - prev.0) as f32, (p.1 - prev.1) as f32))
                        }
                        _ => c.drag_to((p.0 as f32, p.1 as f32), shift),
                    }
                }
            }
            Grab::Region { anchor } if self.modifiers.control_key() => {
                // Ctrl moves the whole region.
                if let Some(prev) = previous {
                    self.grab = Grab::Region {
                        anchor: (anchor.0 + p.0 - prev.0, anchor.1 + p.1 - prev.1),
                    };
                }
            }
            Grab::Region { .. } | Grab::None => {}
        }
        self.invalidate(before, false);
    }

    fn on_press(&mut self) {
        let Some(p) = self.cursor else { return };
        let before = self.snap();
        if self.tool == Tool::Select || !self.annotate {
            self.grab = Grab::Region { anchor: p };
        } else if let Some(canvas) = &mut self.canvas
            && canvas.begin_bend((p.0 as f32, p.1 as f32), NODE_RADIUS as f32, self.tool)
        {
            // Grabbed a line's or an arrow's middle node: drag to curve it.
            self.grab = Grab::Draw;
        } else if let Some(canvas) = &mut self.canvas
            && canvas.begin_move((p.0 as f32, p.1 as f32), self.tool)
        {
            // Grabbed a clip, or a blurred or pixelated area: drag to move it.
            self.grab = Grab::Draw;
        } else if self.ensure_canvas_at(p)
            && let (Some(canvas), Some(a)) = (
                &mut self.canvas,
                Annotation::new(self.tool, self.style, (p.0 as f32, p.1 as f32))
                    .map(|a| a.with_image(self.redact_image.clone())),
            )
        {
            canvas.begin(a);
            self.grab = Grab::Draw;
        }
        self.invalidate(before, true);
    }

    fn on_release(&mut self) -> Outcome {
        let before = self.snap();
        match std::mem::replace(&mut self.grab, Grab::None) {
            Grab::Region { .. } if self.purpose == Purpose::PickColor => return self.pick_color(),
            Grab::Region { anchor } => {
                let Some(cursor) = self.cursor else {
                    return Outcome::Continue;
                };
                let rect = region_rect(anchor, cursor, self.modifiers.shift_key());
                // A plain click takes the window under it, or else the
                // whole monitor.
                let rect = if is_click(&rect) {
                    self.windows
                        .iter()
                        .find(|w| w.contains(cursor))
                        .zip(self.monitor_at(cursor))
                        .and_then(|(w, m)| w.intersect(&m))
                        .or_else(|| self.monitor_at(cursor))
                } else {
                    Some(rect)
                };
                if let Some(rect) = rect {
                    return self.finish(rect, true);
                }
            }
            Grab::Draw => {
                if let Some(c) = &mut self.canvas {
                    c.commit();
                }
            }
            Grab::None => {}
        }
        self.invalidate(before, true);
        Outcome::Continue
    }

    /// Right-click abandons the current drag, or closes the capture (on
    /// release) if there's none.
    fn on_right_press(&mut self) {
        let before = self.snap();
        // Right-clicking anything drawn deletes it; anywhere else it closes
        // the overlay.
        let deleted = self.grab == Grab::None
            && self.annotate
            && self
                .canvas
                .as_mut()
                .zip(self.cursor)
                .is_some_and(|(c, p)| c.delete_at((p.0 as f32, p.1 as f32)));
        self.close_on_right_release = !deleted && !self.cancel_grab();
        self.invalidate(before, true);
    }

    /// Abandons the current drag, if any. Returns whether there was one.
    fn cancel_grab(&mut self) -> bool {
        match std::mem::replace(&mut self.grab, Grab::None) {
            Grab::None => false,
            Grab::Draw => {
                if let Some(c) = &mut self.canvas {
                    c.cancel_active();
                }
                true
            }
            Grab::Region { .. } => true,
        }
    }

    fn on_key(&mut self, event: &KeyEvent) -> Outcome {
        let before = self.snap();
        let ctrl = self.modifiers.control_key() || self.modifiers.super_key();
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                if !self.cancel_grab() {
                    return Outcome::Cancel;
                }
            }
            Key::Named(NamedKey::Enter) if self.purpose == Purpose::PickColor => {
                return self.pick_color();
            }
            Key::Named(arrow) if self.purpose == Purpose::PickColor => {
                let step = if self.modifiers.shift_key() {
                    10.0
                } else {
                    1.0
                };
                match arrow {
                    NamedKey::ArrowLeft => self.nudge(-step, 0.0),
                    NamedKey::ArrowRight => self.nudge(step, 0.0),
                    NamedKey::ArrowUp => self.nudge(0.0, -step),
                    NamedKey::ArrowDown => self.nudge(0.0, step),
                    _ => {}
                }
            }
            Key::Named(NamedKey::Enter) if self.annotate => return self.finish_monitor(true),
            Key::Character(c) if self.annotate => {
                let c = c.to_ascii_uppercase();
                match (ctrl, c.as_str()) {
                    (true, "C") => return self.finish_monitor(false),
                    (true, "S") => return self.finish_monitor(true),
                    (true, "Z") if self.modifiers.shift_key() => return self.apply(Action::Redo),
                    (true, "Z") => return self.apply(Action::Undo),
                    (true, "Y") => return self.apply(Action::Redo),
                    (false, key) => {
                        let picks = |t: &Tool| {
                            key.starts_with(t.key()) || t.alias().is_some_and(|a| key.starts_with(a))
                        };
                        if let Some(t) = Tool::ALL.into_iter().find(picks) {
                            return self.apply(Action::Tool(t));
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        self.invalidate(before, true);
        Outcome::Continue
    }

    /// Fires postponed redraws that are due; returns when to wake next.
    pub fn poll(&mut self, now: Instant) -> Option<Instant> {
        let mut wake: Option<Instant> = None;
        for o in self.overlays.values_mut() {
            match o.due {
                Some(t) if t <= now => {
                    o.due = None;
                    o.request_redraw();
                }
                Some(t) => wake = Some(wake.map_or(t, |w| w.min(t))),
                None => {}
            }
            match o.repaint_at() {
                Some(t) if t <= now => o.request_redraw(),
                Some(t) => wake = Some(wake.map_or(t, |w| w.min(t))),
                None => {}
            }
        }
        wake
    }
}

/// Asks an overlay to redraw, respecting a custom frame-rate limit.
fn request_redraw(o: &mut Overlay, interval: Option<Duration>) {
    let next = interval.zip(o.last_frame).map(|(i, last)| last + i);
    match next {
        Some(t) if t > Instant::now() => o.due = Some(o.due.map_or(t, |d| d.min(t))),
        _ => o.request_redraw(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shift_makes_the_region_square() {
        let r = region_rect((100.0, 100.0), (180.0, 130.0), true);
        assert_eq!((r.x, r.y, r.w, r.h), (100, 100, 80, 80));
        // Dragging up-left keeps the square on that side of the anchor.
        let r = region_rect((100.0, 100.0), (70.0, 40.0), true);
        assert_eq!((r.x, r.y, r.w, r.h), (40, 40, 60, 60));
        let r = region_rect((100.0, 100.0), (180.0, 130.0), false);
        assert_eq!((r.w, r.h), (80, 30));
    }
}
