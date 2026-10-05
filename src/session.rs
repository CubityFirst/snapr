//! One capture: the frozen overlays, annotating them, and dragging out the
//! region to save. Releasing the region drag takes the screenshot.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use egui::CursorIcon;
use egui_wgpu::wgpu;
use image::RgbaImage;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, ModifiersState, NamedKey, PhysicalKey};
use winit::window::WindowId;

use crate::annotate::{Annotation, Canvas, Style, Tool};
use crate::capture::{self, Rect};
use crate::gpu::Gpu;
use crate::naming::Context;
use crate::overlay::{Frame, Overlay};
use crate::toolbar::Action;

pub enum Outcome {
    Continue,
    Cancel,
    Done {
        image: RgbaImage,
        save_file: bool,
    },
    /// A region was picked for screen recording.
    Record(Rect),
    /// A region was picked to look for QR codes in.
    Scan(RgbaImage),
    /// A pixel's colour was picked (the colour picker).
    Color([u8; 3]),
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
}

impl Purpose {
    /// Shown at the top of the screen while picking, for anything but a
    /// screenshot.
    fn hint(self) -> Option<&'static str> {
        match self {
            Purpose::Screenshot => None,
            Purpose::Record => Some("Select a region to record"),
            Purpose::Scan => Some("Select a QR code to read"),
            Purpose::PickColor => Some("Click to pick a colour \u{2014} arrow keys move one pixel"),
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

/// Side of the colour picker's loupe, in screen pixels (odd, so one is in
/// the middle).
pub const LOUPE_SIZE: u32 = 15;

/// How close (in pixels) a press must be to an arrow's middle node to grab it.
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
    pub fn new(
        event_loop: &ActiveEventLoop,
        gpu: &Gpu,
        annotate: bool,
        overlay_fps: u32,
        tool: Tool,
        style: Style,
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
        let windows = capture::window_rects();
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
            cursor: capture::cursor_position(),
            grab: Grab::None,
            windows,
            canvas: None,
            layer: None,
            tool,
            style,
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
            o.window.request_redraw();
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
            Some(c) if self.tool == Tool::Arrow && (editing || self.grab == Grab::None) => {
                c.arrow_nodes()
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
            arrow_nodes: arrow_nodes
                .into_iter()
                .map(|(x, y)| (x as f64, y as f64))
                .collect(),
            // On the monitor with the cursor, unless the region covers it.
            hint: self.purpose.hint().filter(|_| !dragging && on_this_monitor),
            loupe: self
                .cursor
                .filter(|_| picking && on_this_monitor)
                .map(|c| (c, self.loupe_pixels(c))),
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
            WindowEvent::Resized(_) => overlay.window.request_redraw(),
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

    fn redraw(&mut self, gpu: &Gpu, id: WindowId) -> Outcome {
        self.sync_layer(gpu);
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
            Action::Undo => self.canvas.iter_mut().for_each(Canvas::undo),
            Action::Redo => self.canvas.iter_mut().for_each(Canvas::redo),
        }
        self.invalidate(before, true);
        Outcome::Continue
    }

    /// The frozen pixels around `p`, `LOUPE_SIZE` square, for the colour
    /// picker's loupe. Off-screen pixels are transparent.
    fn loupe_pixels(&self, p: (f64, f64)) -> RgbaImage {
        let half = LOUPE_SIZE as i32 / 2;
        self.crop(Rect {
            x: p.0.floor() as i32 - half,
            y: p.1.floor() as i32 - half,
            w: LOUPE_SIZE,
            h: LOUPE_SIZE,
        })
    }

    /// The colour of the pixel under the cursor.
    fn pick_color(&self) -> Outcome {
        let Some(c) = self.cursor else {
            return Outcome::Continue;
        };
        let rect = Rect {
            x: c.0.floor() as i32,
            y: c.1.floor() as i32,
            w: 1,
            h: 1,
        };
        let [r, g, b, _] = self.crop(rect).get_pixel(0, 0).0;
        Outcome::Color([r, g, b])
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
            Purpose::Screenshot => {}
        }
        let mut image = self.crop(rect);
        if let Some(canvas) = &mut self.canvas {
            canvas.commit();
            canvas.copy_into(&mut image, rect);
        }
        Outcome::Done { image, save_file }
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
        } else if self.tool == Tool::Arrow
            && let Some(canvas) = &mut self.canvas
            && canvas.begin_bend((p.0 as f32, p.1 as f32), NODE_RADIUS as f32)
        {
            // Grabbed an arrow's middle node: drag to curve it.
            self.grab = Grab::Draw;
        } else if let Some(canvas) = &mut self.canvas
            && canvas.begin_move((p.0 as f32, p.1 as f32), self.tool)
        {
            // Grabbed a clip, or a blurred or pixelated area: drag to move it.
            self.grab = Grab::Draw;
        } else if self.ensure_canvas_at(p)
            && let (Some(canvas), Some(a)) = (
                &mut self.canvas,
                Annotation::new(self.tool, self.style, (p.0 as f32, p.1 as f32)),
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
        self.close_on_right_release = !self.cancel_grab();
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
                        if let Some(t) = Tool::ALL.into_iter().find(|t| key.starts_with(t.key())) {
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
                    o.window.request_redraw();
                }
                Some(t) => wake = Some(wake.map_or(t, |w| w.min(t))),
                None => {}
            }
            match o.repaint_at() {
                Some(t) if t <= now => o.window.request_redraw(),
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
        _ => o.window.request_redraw(),
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
