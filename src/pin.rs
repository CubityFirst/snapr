//! Images pinned to the screen: borderless, always-on-top windows showing a
//! screenshot (or any image) at its real size. Drag it to move it, scroll or
//! drag a corner to make it bigger or smaller (it keeps its shape),
//! double-click or `0` for its real size again. It stays on top of other
//! windows unless that's turned off (`T`, or its button). Ctrl+C copies it,
//! Ctrl+S saves it, right-click or Esc closes it. Hovering shows the zoom
//! level and buttons for those.

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Color32, FontId, Pos2, Rect, Stroke, pos2, vec2};
use egui_wgpu::winit::Painter;
use image::RgbaImage;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::monitor::MonitorHandle;
use winit::window::{CursorIcon, Window, WindowLevel};

use crate::gpu::Gpu;

const ACCENT: Color32 = Color32::from_rgb(0x3d, 0x9b, 0xff);
/// The smallest a pin gets: its longer side, in pixels.
const MIN_SIDE: f64 = 24.0;
const MAX_ZOOM: f64 = 8.0;
/// Each notch of the mouse wheel zooms by this much.
const WHEEL_STEP: f64 = 1.1;
/// How close to a corner (in points) grabs it to resize.
const GRIP: f32 = 14.0;
/// A pin placed in the middle of a screen covers at most this much of it.
const FIT_SCREEN: f64 = 0.8;
/// Hover buttons, in points.
const BUTTON: f32 = 24.0;
const ZOOM_LABEL: f32 = 46.0;
const BAR_MARGIN: f32 = 6.0;
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

pub enum Action {
    Close,
    Copy(RgbaImage),
}

/// Where a new pin goes.
pub enum Place {
    /// At its real size with its top-left corner here (screen pixels): over
    /// the region it was captured from.
    At(PhysicalPosition<i32>),
    /// In the middle of this screen (or the main one), shrunk to fit.
    Center(Option<MonitorHandle>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Button {
    /// The zoom level; clicking it goes back to 100%.
    Zoom,
    /// Keep it on top of other windows, or not.
    OnTop,
    Copy,
    Close,
}

/// Where the window goes and how big it is, in screen pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Geometry {
    x: i32,
    y: i32,
    w: u32,
    h: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Corner {
    fn cursor(self) -> CursorIcon {
        match self {
            Corner::TopLeft | Corner::BottomRight => CursorIcon::NwseResize,
            Corner::TopRight | Corner::BottomLeft => CursorIcon::NeswResize,
        }
    }

    fn left(self) -> bool {
        matches!(self, Corner::TopLeft | Corner::BottomLeft)
    }

    fn top(self) -> bool {
        matches!(self, Corner::TopLeft | Corner::TopRight)
    }
}

/// A corner being dragged, and the opposite corner that stays put (screen
/// pixels).
#[derive(Debug, Clone, Copy)]
struct Resize {
    corner: Corner,
    anchor: (f64, f64),
}

pub struct Pin {
    pub window: Arc<Window>,
    state: egui_winit::State,
    painter: Painter,
    texture: egui::TextureHandle,
    image: RgbaImage,
    /// Window pixels per image pixel.
    zoom: f64,
    /// Longest side the window may have, from the GPU's limits.
    max_side: f64,
    /// The mouse, in window pixels, while it's over the pin (or dragging).
    cursor: Option<PhysicalPosition<f64>>,
    modifiers: ModifiersState,
    resize: Option<Resize>,
    /// The hover button pressed, clicked if it's released over it.
    pressed: Option<Button>,
    /// For telling a double-click.
    last_press: Option<Instant>,
    /// Hover buttons from the last paint, in points.
    buttons: Vec<(Rect, Button)>,
    /// Shown under the hover buttons for a moment, e.g. "Copied".
    notice: Option<&'static str>,
    /// Kept above other windows.
    on_top: bool,
    /// Where zooming or resizing wants the window, applied once per frame:
    /// the mouse reports far more often than the window can be resized and
    /// redrawn, and resizing on every report falls behind it.
    pending: Option<Geometry>,
    /// The size the drawing surface was last set up for.
    surface: PhysicalSize<u32>,
}

impl Pin {
    pub fn open(
        event_loop: &ActiveEventLoop,
        gpu: &Gpu,
        image: RgbaImage,
        place: Place,
    ) -> Result<Self, String> {
        let (w, h) = (image.width().max(1) as f64, image.height().max(1) as f64);
        let (zoom, position) = match place {
            Place::At(p) => (1.0, Some(p)),
            Place::Center(monitor) => {
                match monitor
                    .or_else(|| event_loop.primary_monitor())
                    .or_else(|| event_loop.available_monitors().next())
                {
                    Some(m) => {
                        let (mp, ms) = (m.position(), m.size());
                        let zoom = (FIT_SCREEN * ms.width as f64 / w)
                            .min(FIT_SCREEN * ms.height as f64 / h)
                            .min(1.0);
                        let (pw, ph) = ((w * zoom).round(), (h * zoom).round());
                        let x = mp.x + ((ms.width as f64 - pw) / 2.0) as i32;
                        let y = mp.y + ((ms.height as f64 - ph) / 2.0) as i32;
                        (zoom, Some(PhysicalPosition::new(x, y)))
                    }
                    None => (1.0, None),
                }
            }
        };
        let size = PhysicalSize::new(
            ((w * zoom).round() as u32).max(1),
            ((h * zoom).round() as u32).max(1),
        );

        #[allow(unused_mut)]
        let mut attrs = Window::default_attributes()
            .with_title("snapr pin")
            .with_decorations(false)
            .with_resizable(false)
            .with_visible(false)
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_inner_size(size);
        #[cfg(windows)]
        {
            use winit::platform::windows::WindowAttributesExtWindows;
            attrs = attrs.with_skip_taskbar(true);
        }
        let window = Arc::new(event_loop.create_window(attrs).map_err(|e| e.to_string())?);
        if let Some(p) = position {
            window.set_outer_position(p);
        }

        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        let config = egui_wgpu::WgpuConfiguration {
            wgpu_setup: gpu.egui_setup(),
            ..Default::default()
        };
        let mut painter =
            pollster::block_on(Painter::new(ctx.clone(), config, false, Default::default()));
        pollster::block_on(painter.set_window(egui::ViewportId::ROOT, Some(window.clone())))
            .map_err(|e| format!("couldn't pin the image: {e}"))?;
        let max_side = painter.max_texture_side().unwrap_or(8192);
        let state = egui_winit::State::new(
            ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            window.theme(),
            Some(max_side),
        );
        let texture = ctx.load_texture("pin", texture_image(&image, max_side), TEXTURE);
        let mut pin = Self {
            window,
            state,
            painter,
            texture,
            image,
            zoom,
            max_side: max_side as f64,
            cursor: None,
            modifiers: ModifiersState::default(),
            resize: None,
            pressed: None,
            last_press: None,
            buttons: Vec::new(),
            notice: None,
            on_top: true,
            pending: None,
            surface: PhysicalSize::new(0, 0),
        };
        // Window sizes are clamped like zooming, e.g. for huge images.
        if pin.zoom != pin.clamp_zoom(pin.zoom) {
            pin.set_zoom(pin.zoom, None);
        }
        // Paint before showing it, so it doesn't flash white.
        pin.paint();
        pin.window.set_visible(true);
        pin.window.focus_window();
        Ok(pin)
    }

    pub fn on_event(&mut self, event: &WindowEvent) -> Option<Action> {
        // Kept up to date for egui's sizes; the pin draws its own buttons.
        let _ = self.state.on_window_event(&self.window, event);
        match event {
            WindowEvent::CloseRequested => return Some(Action::Close),
            WindowEvent::RedrawRequested => self.paint(),
            // Resizes the pin made itself were drawn straight away.
            WindowEvent::Resized(size) => {
                if self.fit_surface(*size) {
                    self.window.request_redraw();
                }
            }
            // Moving to a screen with another scale keeps it the same number
            // of pixels: a pinned screenshot stays sharp at 100%.
            WindowEvent::ScaleFactorChanged {
                inner_size_writer, ..
            } => {
                let mut writer = inner_size_writer.clone();
                let _ = writer.request_inner_size(self.window.inner_size());
            }
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::CursorMoved { position, .. } => {
                let before = self.button_at();
                self.cursor = Some(*position);
                if self.resize.is_some() {
                    self.resize_to_cursor();
                } else {
                    self.update_cursor_icon();
                }
                if self.button_at() != before {
                    self.window.request_redraw();
                }
            }
            WindowEvent::CursorEntered { .. } => self.window.request_redraw(),
            WindowEvent::CursorLeft { .. } => {
                if self.resize.is_none() {
                    self.cursor = None;
                    self.notice = None;
                    self.window.request_redraw();
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => self.on_press(),
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                self.resize = None;
                if let Some(b) = self.pressed.take()
                    && self.button_at() == Some(b)
                {
                    return self.click(b);
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Right,
                ..
            } => return Some(Action::Close),
            WindowEvent::MouseWheel { delta, .. } => {
                let steps = match delta {
                    MouseScrollDelta::LineDelta(_, y) => *y as f64,
                    MouseScrollDelta::PixelDelta(p) => p.y / 60.0,
                };
                let zoom = snap_to_one(self.zoom, self.zoom * WHEEL_STEP.powf(steps));
                self.set_zoom(zoom, self.cursor.map(|c| (c.x, c.y)));
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                let ctrl = self.modifiers.control_key() || self.modifiers.super_key();
                match event.logical_key.as_ref() {
                    Key::Named(NamedKey::Escape) => return Some(Action::Close),
                    Key::Character(c) if ctrl && c.eq_ignore_ascii_case("c") => {
                        return self.click(Button::Copy);
                    }
                    Key::Character(c) if ctrl && c.eq_ignore_ascii_case("s") => self.save_as(),
                    Key::Character("+" | "=") => self.set_zoom(self.zoom * WHEEL_STEP, None),
                    Key::Character("-" | "_") => self.set_zoom(self.zoom / WHEEL_STEP, None),
                    Key::Character("0") => self.set_zoom(1.0, None),
                    Key::Character(c) if !ctrl && c.eq_ignore_ascii_case("t") => {
                        self.set_on_top(!self.on_top);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        None
    }

    fn click(&mut self, button: Button) -> Option<Action> {
        match button {
            Button::Zoom => {
                self.set_zoom(1.0, None);
                None
            }
            Button::OnTop => {
                self.set_on_top(!self.on_top);
                None
            }
            Button::Copy => {
                self.notice = Some("Copied");
                self.window.request_redraw();
                Some(Action::Copy(self.image.clone()))
            }
            Button::Close => Some(Action::Close),
        }
    }

    fn on_press(&mut self) {
        let now = Instant::now();
        let double = self
            .last_press
            .replace(now)
            .is_some_and(|t| now - t < DOUBLE_CLICK);
        if let Some(b) = self.button_at() {
            self.pressed = Some(b);
        } else if let Some(corner) = self.corner_at() {
            let Some(g) = self.geometry() else {
                return;
            };
            let x = if corner.left() { g.w } else { 0 };
            let y = if corner.top() { g.h } else { 0 };
            self.resize = Some(Resize {
                corner,
                anchor: ((g.x + x as i32) as f64, (g.y + y as i32) as f64),
            });
        } else if double {
            self.last_press = None;
            self.set_zoom(1.0, self.cursor.map(|c| (c.x, c.y)));
        } else {
            // The system moves it, so it follows the mouse smoothly and
            // snaps like any other window.
            let _ = self.window.drag_window();
        }
    }

    /// Zooms to `zoom`, keeping the point `anchor` (window pixels; default
    /// the middle) where it is on the screen.
    fn set_zoom(&mut self, zoom: f64, anchor: Option<(f64, f64)>) {
        self.zoom = self.clamp_zoom(zoom);
        self.notice = None;
        self.window.request_redraw();
        let Some(g) = self.geometry() else { return };
        let new = self.size_at(self.zoom);
        let (gw, gh) = (g.w.max(1) as f64, g.h.max(1) as f64);
        // The mouse is reported against where the window is now, which may
        // be behind where it's going.
        let (sx, sy) = match (anchor, self.window.outer_position()) {
            (Some((ax, ay)), Ok(pos)) => (pos.x as f64 + ax, pos.y as f64 + ay),
            _ => (g.x as f64 + gw / 2.0, g.y as f64 + gh / 2.0),
        };
        let fx = ((sx - g.x as f64) / gw).clamp(0.0, 1.0);
        let fy = ((sy - g.y as f64) / gh).clamp(0.0, 1.0);
        self.move_to(Geometry {
            x: (sx - fx * new.width as f64).round() as i32,
            y: (sy - fy * new.height as f64).round() as i32,
            w: new.width,
            h: new.height,
        });
    }

    /// Follows a corner being dragged, keeping the image's shape.
    fn resize_to_cursor(&mut self) {
        let (Some(r), Some(c), Ok(pos)) = (self.resize, self.cursor, self.window.outer_position())
        else {
            return;
        };
        let (w, h) = self.image_size();
        let (sx, sy) = (pos.x as f64 + c.x, pos.y as f64 + c.y);
        let zoom = ((sx - r.anchor.0).abs() / w).max((sy - r.anchor.1).abs() / h);
        let zoom = snap_to_one(self.zoom, zoom);
        self.zoom = self.clamp_zoom(zoom);
        self.notice = None;
        let new = self.size_at(self.zoom);
        let x = if r.corner.left() {
            r.anchor.0 - new.width as f64
        } else {
            r.anchor.0
        };
        let y = if r.corner.top() {
            r.anchor.1 - new.height as f64
        } else {
            r.anchor.1
        };
        self.move_to(Geometry {
            x: x as i32,
            y: y as i32,
            w: new.width,
            h: new.height,
        });
    }

    /// Where the window is going (or is).
    fn geometry(&self) -> Option<Geometry> {
        if self.pending.is_some() {
            return self.pending;
        }
        let pos = self.window.outer_position().ok()?;
        let size = self.window.inner_size();
        Some(Geometry {
            x: pos.x,
            y: pos.y,
            w: size.width,
            h: size.height,
        })
    }

    /// Moves and resizes the window on the next frame.
    fn move_to(&mut self, g: Geometry) {
        self.pending = Some(g);
        self.window.request_redraw();
    }

    /// Moves and resizes the window in one step, so it doesn't jump to the
    /// new place and then grow.
    fn apply(&mut self, g: Geometry) {
        #[cfg(windows)]
        if let Some(hwnd) = hwnd(&self.window) {
            use windows_sys::Win32::UI::WindowsAndMessaging::{
                SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_NOZORDER, SetWindowPos,
            };
            // The window's frame, if it has any, on top of the picture.
            let (outer, inner) = (self.window.outer_size(), self.window.inner_size());
            let w = g.w + outer.width.saturating_sub(inner.width);
            let h = g.h + outer.height.saturating_sub(inner.height);
            let flags = SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_NOACTIVATE;
            unsafe { SetWindowPos(hwnd, std::ptr::null_mut(), g.x, g.y, w as i32, h as i32, flags) };
            return;
        }
        self.window
            .set_outer_position(PhysicalPosition::new(g.x, g.y));
        let _ = self.window.request_inner_size(PhysicalSize::new(g.w, g.h));
    }

    /// Sets the drawing surface up for a new window size (slow-ish, so only
    /// when it changed). Returns whether it did.
    fn fit_surface(&mut self, size: PhysicalSize<u32>) -> bool {
        if size == self.surface {
            return false;
        }
        let (Some(w), Some(h)) = (
            std::num::NonZeroU32::new(size.width),
            std::num::NonZeroU32::new(size.height),
        ) else {
            return false;
        };
        self.painter.on_window_resized(egui::ViewportId::ROOT, w, h);
        self.surface = size;
        true
    }

    fn set_on_top(&mut self, on_top: bool) {
        self.on_top = on_top;
        self.window.set_window_level(if on_top {
            WindowLevel::AlwaysOnTop
        } else {
            WindowLevel::Normal
        });
        // Behind other windows it needs a way back: the taskbar and Alt+Tab.
        #[cfg(windows)]
        {
            use winit::platform::windows::WindowExtWindows;
            self.window.set_skip_taskbar(on_top);
        }
        self.notice = Some(if on_top {
            "Always on top"
        } else {
            "Not on top"
        });
        self.window.request_redraw();
    }

    fn image_size(&self) -> (f64, f64) {
        (
            self.image.width().max(1) as f64,
            self.image.height().max(1) as f64,
        )
    }

    fn clamp_zoom(&self, zoom: f64) -> f64 {
        let (w, h) = self.image_size();
        let long = w.max(h);
        let min = (MIN_SIDE / long).min(1.0);
        let max = MAX_ZOOM.min(self.max_side / long).max(min);
        zoom.clamp(min, max)
    }

    fn size_at(&self, zoom: f64) -> PhysicalSize<u32> {
        let (w, h) = self.image_size();
        PhysicalSize::new(
            ((w * zoom).round() as u32).max(1),
            ((h * zoom).round() as u32).max(1),
        )
    }

    fn points_per_pixel(&self) -> f32 {
        1.0 / self.window.scale_factor() as f32
    }

    /// The mouse, in points.
    fn pointer(&self) -> Option<Pos2> {
        let s = self.points_per_pixel();
        self.cursor.map(|c| pos2(c.x as f32 * s, c.y as f32 * s))
    }

    fn button_at(&self) -> Option<Button> {
        let p = self.pointer()?;
        self.buttons
            .iter()
            .find(|(r, _)| r.contains(p))
            .map(|(_, b)| *b)
    }

    fn corner_at(&self) -> Option<Corner> {
        let p = self.pointer()?;
        let size = self.window.inner_size();
        let s = self.points_per_pixel();
        let (w, h) = (size.width as f32 * s, size.height as f32 * s);
        // Small pins keep most of their middle for moving them.
        let grip = GRIP.min(w.min(h) / 4.0);
        let left = p.x <= grip;
        let right = p.x >= w - grip;
        let top = p.y <= grip;
        let bottom = p.y >= h - grip;
        match (left, right, top, bottom) {
            (true, _, true, _) => Some(Corner::TopLeft),
            (_, true, true, _) => Some(Corner::TopRight),
            (true, _, _, true) => Some(Corner::BottomLeft),
            (_, true, _, true) => Some(Corner::BottomRight),
            _ => None,
        }
    }

    fn update_cursor_icon(&self) {
        let icon = if self.button_at().is_some() {
            CursorIcon::Pointer
        } else if let Some(c) = self.corner_at() {
            c.cursor()
        } else {
            CursorIcon::Move
        };
        self.window.set_cursor(icon);
    }

    fn save_as(&mut self) {
        let Some(mut path) = rfd::FileDialog::new()
            .add_filter("PNG image", &["png"])
            .set_file_name("pinned.png")
            .save_file()
        else {
            return;
        };
        if path.extension().is_none() {
            path.set_extension("png");
        }
        self.notice = Some(match self.image.save(&path) {
            Ok(()) => "Saved",
            Err(e) => {
                eprintln!("couldn't save {}: {e}", path.display());
                "Couldn't save"
            }
        });
        self.window.request_redraw();
    }

    fn paint(&mut self) {
        // Resize and redraw together, so the picture keeps up.
        if let Some(g) = self.pending.take() {
            self.apply(g);
        }
        self.fit_surface(self.window.inner_size());
        let input = self.state.take_egui_input(&self.window);
        let ctx = self.state.egui_ctx().clone();
        let texture = self.texture.id();
        let pointer = self.pointer();
        let zoom = self.zoom;
        let notice = self.notice;
        let on_top = self.on_top;
        let mut buttons = Vec::new();
        let mut output = ctx.run_ui(input, |ui| {
            buttons = draw(ui, texture, pointer, zoom, on_top, notice);
        });
        self.buttons = buttons;
        self.state
            .handle_platform_output(&self.window, std::mem::take(&mut output.platform_output));
        let primitives =
            ctx.tessellate(std::mem::take(&mut output.shapes), output.pixels_per_point);
        self.painter.paint_and_update_textures(
            egui::ViewportId::ROOT,
            output.pixels_per_point,
            [0.0, 0.0, 0.0, 1.0],
            &primitives,
            &mut output.textures_delta,
            Vec::new(),
            &self.window,
        );
        output.textures_delta.clear();
    }
}

/// The window's handle, for moving and resizing it in one call.
#[cfg(windows)]
fn hwnd(window: &Window) -> Option<windows_sys::Win32::Foundation::HWND> {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get() as _),
        _ => None,
    }
}

/// Smooth when zoomed, mipmapped so shrunk text doesn't shimmer; exact at
/// 100%, where pixels line up with the screen's.
const TEXTURE: egui::TextureOptions = egui::TextureOptions {
    magnification: egui::TextureFilter::Linear,
    minification: egui::TextureFilter::Linear,
    wrap_mode: egui::TextureWrapMode::ClampToEdge,
    mipmap_mode: Some(egui::TextureFilter::Linear),
};

/// The image as a texture, shrunk if the GPU can't hold it whole.
fn texture_image(image: &RgbaImage, max_side: usize) -> egui::ColorImage {
    let (w, h) = image.dimensions();
    let long = w.max(h) as usize;
    let shrunk;
    let image = if long > max_side {
        let scale = max_side as f64 / long as f64;
        shrunk = image::imageops::resize(
            image,
            ((w as f64 * scale) as u32).max(1),
            ((h as f64 * scale) as u32).max(1),
            image::imageops::FilterType::Triangle,
        );
        &shrunk
    } else {
        image
    };
    egui::ColorImage::from_rgba_unmultiplied(
        [image.width() as usize, image.height() as usize],
        image.as_raw(),
    )
}

/// Stops at 100% when zooming past it, so it's easy to get back to.
fn snap_to_one(old: f64, new: f64) -> f64 {
    if (old < 1.0 && new > 1.0) || (old > 1.0 && new < 1.0) {
        1.0
    } else {
        new
    }
}

/// The image filling the window, a thin outline so it stands apart from
/// what's behind it, and, while the mouse is over it, the zoom level and
/// On top, Copy and Close buttons. Returns where the buttons are.
fn draw(
    ui: &mut egui::Ui,
    texture: egui::TextureId,
    pointer: Option<Pos2>,
    zoom: f64,
    on_top: bool,
    notice: Option<&str>,
) -> Vec<(Rect, Button)> {
    let rect = ui.max_rect();
    let painter = ui.painter();
    painter.image(
        texture,
        rect,
        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
        Color32::WHITE,
    );
    let hovered = pointer.is_some_and(|p| rect.contains(p));
    let outline = if hovered {
        ACCENT
    } else {
        Color32::from_rgba_unmultiplied(0x3d, 0x9b, 0xff, 140)
    };
    painter.rect_stroke(rect, 0.0, Stroke::new(1.0, outline), egui::StrokeKind::Inside);

    let bar_width = ZOOM_LABEL + 3.0 * BUTTON;
    if !hovered || rect.width() < bar_width + 2.0 * BAR_MARGIN + 2.0 * GRIP
        || rect.height() < BUTTON + 2.0 * BAR_MARGIN
    {
        return Vec::new();
    }
    // Top-right, clear of the corner grip.
    let bar = Rect::from_min_size(
        pos2(rect.right() - BAR_MARGIN - GRIP - bar_width, rect.top() + BAR_MARGIN),
        vec2(bar_width, BUTTON),
    );
    painter.rect_filled(bar, 6.0, Color32::from_rgba_unmultiplied(20, 21, 24, 220));
    let zoom_rect = Rect::from_min_size(bar.min, vec2(ZOOM_LABEL, BUTTON));
    let top_rect = Rect::from_min_size(zoom_rect.right_top(), vec2(BUTTON, BUTTON));
    let copy_rect = Rect::from_min_size(top_rect.right_top(), vec2(BUTTON, BUTTON));
    let close_rect = Rect::from_min_size(copy_rect.right_top(), vec2(BUTTON, BUTTON));
    let buttons = vec![
        (zoom_rect, Button::Zoom),
        (top_rect, Button::OnTop),
        (copy_rect, Button::Copy),
        (close_rect, Button::Close),
    ];
    for (r, b) in &buttons {
        if pointer.is_some_and(|p| r.contains(p)) {
            let fill = if *b == Button::Close {
                Color32::from_rgb(196, 43, 28)
            } else {
                Color32::from_white_alpha(28)
            };
            painter.rect_filled(r.shrink(2.0), 4.0, fill);
        }
    }
    let ink = Color32::from_gray(235);
    painter.text(
        zoom_rect.center(),
        egui::Align2::CENTER_CENTER,
        format!("{:.0}%", zoom * 100.0),
        FontId::proportional(12.0),
        ink,
    );
    let stroke = Stroke::new(1.3, ink);
    // On top: a push pin, filled in while it's on.
    let c = top_rect.center();
    let head = c + vec2(2.0, -2.0);
    let tip = c + vec2(-5.0, 5.0);
    if on_top {
        painter.line_segment([head, tip], Stroke::new(1.5, ACCENT));
        painter.circle_filled(head, 4.0, ACCENT);
    } else {
        painter.line_segment([head, tip], stroke);
        painter.circle(head, 3.5, Color32::from_rgb(20, 21, 24), stroke);
    }
    // Copy: two overlapping sheets.
    let c = copy_rect.center();
    let back = Rect::from_center_size(c + vec2(-1.5, -1.5), vec2(8.0, 9.0));
    let front = Rect::from_center_size(c + vec2(1.5, 1.5), vec2(8.0, 9.0));
    painter.rect_stroke(back, 1.5, stroke, egui::StrokeKind::Middle);
    painter.rect_filled(front, 1.5, Color32::from_rgb(20, 21, 24));
    painter.rect_stroke(front, 1.5, stroke, egui::StrokeKind::Middle);
    // Close: a cross.
    let c = close_rect.center();
    let d = 4.5;
    painter.line_segment([c + vec2(-d, -d), c + vec2(d, d)], stroke);
    painter.line_segment([c + vec2(-d, d), c + vec2(d, -d)], stroke);
    if let Some(text) = notice {
        let at = pos2(bar.right(), bar.bottom() + 4.0);
        let galley = painter.layout_no_wrap(text.to_string(), FontId::proportional(12.0), ink);
        let bg = Rect::from_min_size(
            at - vec2(galley.size().x + 12.0, 0.0),
            galley.size() + vec2(12.0, 6.0),
        );
        painter.rect_filled(bg, 4.0, Color32::from_rgba_unmultiplied(20, 21, 24, 220));
        painter.galley(bg.min + vec2(6.0, 3.0), galley, ink);
    }
    buttons
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_stops_at_100_percent() {
        assert_eq!(snap_to_one(0.9, 1.05), 1.0);
        assert_eq!(snap_to_one(1.2, 0.95), 1.0);
        assert_eq!(snap_to_one(1.0, 1.1), 1.1);
        assert_eq!(snap_to_one(1.0, 0.9), 0.9);
        assert_eq!(snap_to_one(2.0, 2.2), 2.2);
    }

    #[test]
    fn huge_images_fit_the_gpu() {
        let image = RgbaImage::new(10000, 50);
        let t = texture_image(&image, 4096);
        assert_eq!(t.size, [4096, 20]);
        let t = texture_image(&RgbaImage::new(300, 200), 4096);
        assert_eq!(t.size, [300, 200]);
    }

    /// Renders a hovered pin to `target/pin-preview.png`:
    /// `cargo test pin_preview -- --ignored`.
    #[test]
    #[ignore]
    fn pin_preview() {
        let pixels: Vec<u8> = (0..480 * 270)
            .flat_map(|i| [(i % 480 / 2) as u8, 90, (i / 480) as u8, 255])
            .collect();
        let image = egui::ColorImage::from_rgba_unmultiplied([480, 270], &pixels);
        let mut texture = None;
        crate::preview::render("pin-preview", [480, 270], 1.0, |root| {
            let t = texture.get_or_insert_with(|| {
                root.ctx().load_texture("pin", image.clone(), TEXTURE)
            });
            draw(root, t.id(), Some(pos2(400.0, 18.0)), 1.25, true, Some("Copied"));
        });
    }
}
