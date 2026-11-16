//! The preview that pops up in the corner of the screen after a capture.
//! Left-click opens the link (or the image, before it's uploaded), middle-click
//! copies the image, right-click dismisses it, and dragging it drops the file
//! into another app. It goes away by itself after a few seconds, unless the
//! mouse is over it. A finger has no middle or right button, so a tap shows
//! Open, Copy and Close buttons, and a second tap picks one; swiping it left
//! or right dismisses it.

// `Backend` only has its X11 variant on Linux.
#![cfg_attr(not(target_os = "linux"), allow(irrefutable_let_patterns))]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::RichText;
use egui_wgpu::winit::Painter;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, MouseButton, Touch, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowLevel};

use crate::gpu::Gpu;
use crate::settings::ToastSize;
use crate::touch::Finger;

#[cfg(target_os = "linux")]
use crate::x11::{self, X11Event};

/// Largest preview, in points; the window fits the image's shape.
const MAX_PREVIEW: (f32, f32) = (320.0, 200.0);
const CAPTION_HEIGHT: f32 = 26.0;
/// Between the preview and the caption under it.
const CAPTION_GAP: f32 = 8.0;
const MARGIN: f32 = 6.0;
/// Between a mouse icon and what its button does.
const ICON_GAP: f32 = 4.0;
/// The compact toast, and the thumbnail box inside it, in points.
const COMPACT_WIDTH: f32 = 332.0;
const COMPACT_THUMB: (f32, f32) = (128.0, 72.0);
/// Gap between the toast and the screen's corner, in points.
const SCREEN_GAP: f64 = 16.0;
/// How long it stays up, and how long after the mouse leaves it.
const SHOWN_FOR: Duration = Duration::from_secs(6);
const AFTER_HOVER: Duration = Duration::from_secs(2);
/// How far (in points) the mouse moves with the button down before it's a
/// drag rather than a click.
const DRAG_THRESHOLD: f64 = 6.0;
/// How far of the toast's width a swipe goes to dismiss it, or, for a
/// quick flick, how far (in points) and how fast (points per second).
const SWIPE_AWAY: f64 = 0.3;
const FLICK_DISTANCE: f64 = 24.0;
const FLICK_SPEED: f64 = 600.0;
/// The buttons a tap shows: their tallest, and the gap between them, in points.
const BUTTON_HEIGHT: f32 = 44.0;
const BUTTON_GAP: f32 = 6.0;
/// Pixels decoded for the preview (twice the size, for high-DPI screens).
pub const DECODE_SIZE: (u32, u32) = (640, 400);

#[derive(Clone, Copy)]
pub enum Action {
    /// Open the link, or the image if there's no link yet.
    Open,
    CopyImage,
    Dismiss,
}

/// The buttons a tap shows, left to right.
const BUTTONS: [Action; 3] = [Action::Open, Action::CopyImage, Action::Dismiss];

/// Where the buttons a tap shows go, in a toast of `size` points.
fn button_rects(size: egui::Vec2) -> [(Action, egui::Rect); 3] {
    let inner = egui::Rect::from_min_size(egui::Pos2::ZERO, size).shrink(MARGIN);
    let height = inner.height().min(BUTTON_HEIGHT);
    let width = (inner.width() - 2.0 * BUTTON_GAP) / 3.0;
    let top = inner.center().y - height / 2.0;
    std::array::from_fn(|i| {
        let left = inner.left() + i as f32 * (width + BUTTON_GAP);
        let rect = egui::Rect::from_min_size(egui::pos2(left, top), egui::vec2(width, height));
        (BUTTONS[i], rect)
    })
}

/// What the mouse did over the toast (or another X11 window).
#[derive(Debug)]
pub enum Pointer {
    Pressed(MouseButton),
    Released(MouseButton),
    Moved(PhysicalPosition<f64>),
    Entered,
    Left,
}

/// The toast's window and what draws it. There's one toast at a time.
#[allow(clippy::large_enum_variant)]
enum Backend {
    Winit {
        window: Arc<Window>,
        state: egui_winit::State,
        painter: Painter,
    },
    /// Under Wayland, which won't let the toast go in the corner.
    #[cfg(target_os = "linux")]
    X11(x11::X11Window),
}

pub struct Toast {
    backend: Backend,
    ctx: egui::Context,
    /// The preview; `None` shows a file icon.
    texture: Option<egui::TextureHandle>,
    pub path: PathBuf,
    pub link: Option<String>,
    /// A recording rather than a screenshot: the preview is its first frame.
    pub video: bool,
    size: ToastSize,
    /// Mouse buttons drawn rather than named.
    icons: bool,
    /// When it goes away; `None` while the mouse is over it.
    pub expires_at: Option<Instant>,
    cursor: PhysicalPosition<f64>,
    /// Where the left button went down, until it's released or a drag starts.
    pressed_at: Option<PhysicalPosition<f64>>,
    /// A touchscreen finger, played as the mouse.
    finger: Finger,
    /// The Open, Copy and Close buttons are up, after a tap.
    buttons: bool,
    /// The window's size, in points.
    points: egui::Vec2,
    /// A finger that's down, which may swipe the toast away.
    swipe: Option<Swipe>,
}

/// A finger on the toast, which moves it sideways once it's dragged.
struct Swipe {
    /// Where the finger went down, across the screen, in pixels.
    finger_x: f64,
    /// Where the window was then; `None` where it can't be moved (Wayland).
    window: Option<PhysicalPosition<i32>>,
    /// When it became a swipe rather than a tap.
    since: Option<Instant>,
}

impl Toast {
    /// `send_x11` passes on an X11 toast's input, from its own thread.
    #[cfg_attr(target_os = "linux", allow(clippy::too_many_arguments))]
    pub fn open(
        event_loop: &ActiveEventLoop,
        gpu: &Gpu,
        path: PathBuf,
        image: Option<egui::ColorImage>,
        link: Option<String>,
        size: ToastSize,
        icons: bool,
        #[cfg(target_os = "linux")] send_x11: impl Fn(X11Event) + Send + 'static,
    ) -> Result<Self, String> {
        let window_size = match size {
            ToastSize::Large => {
                // A file without a picture gets a 16:10 icon area.
                let [w, h] = image.as_ref().map_or([320, 200], |i| i.size);
                let scale = (MAX_PREVIEW.0 / w as f32)
                    .min(MAX_PREVIEW.1 / h as f32)
                    .min(1.0);
                let preview = egui::vec2(w as f32 * scale, h as f32 * scale);
                // Wide enough for the caption even for tall, narrow images.
                LogicalSize::new(
                    (preview.x.max(220.0) + 2.0 * MARGIN) as f64,
                    (preview.y + CAPTION_GAP + CAPTION_HEIGHT + 2.0 * MARGIN) as f64,
                )
            }
            ToastSize::Compact => LogicalSize::new(
                COMPACT_WIDTH as f64,
                (COMPACT_THUMB.1 + 2.0 * MARGIN) as f64,
            ),
        };
        let ctx = egui::Context::default();
        crate::theme::apply(&ctx);

        #[cfg(target_os = "linux")]
        let backend = if x11::wanted(event_loop) {
            Self::open_x11(gpu, &ctx, window_size, send_x11)
                .map(Backend::X11)
                .inspect_err(|e| eprintln!("couldn't show the preview through XWayland: {e}"))
                .ok()
        } else {
            None
        };
        #[cfg(not(target_os = "linux"))]
        let backend = None;
        let backend = match backend {
            Some(b) => b,
            None => Self::open_winit(event_loop, gpu, &ctx, window_size)?,
        };

        let texture = image.map(|i| ctx.load_texture("toast", i, egui::TextureOptions::LINEAR));
        let video = crate::thumbnail::kind(&path) == crate::thumbnail::Kind::Video;
        let mut toast = Self {
            backend,
            ctx,
            texture,
            path,
            link,
            video,
            size,
            icons,
            expires_at: Some(Instant::now() + SHOWN_FOR),
            cursor: PhysicalPosition::new(0.0, 0.0),
            pressed_at: None,
            finger: Finger::default(),
            buttons: false,
            points: egui::vec2(window_size.width as f32, window_size.height as f32),
            swipe: None,
        };
        // Paint before showing it, so it doesn't flash white.
        toast.paint();
        if let Backend::Winit { window, .. } = &toast.backend {
            window.set_visible(true);
        }
        Ok(toast)
    }

    /// In the primary screen's bottom-right corner, above any panel or dock.
    #[cfg(target_os = "linux")]
    fn open_x11(
        gpu: &Gpu,
        ctx: &egui::Context,
        size: LogicalSize<f64>,
        send: impl Fn(X11Event) + Send + 'static,
    ) -> Result<x11::X11Window, String> {
        let screen = x11::Screen::connect()?;
        let size = size.to_physical::<u32>(screen.scale_factor);
        let (right, bottom) = screen.corner();
        let gap = (SCREEN_GAP * screen.scale_factor) as i32;
        let rect = crate::capture::Rect {
            x: right - size.width as i32 - gap,
            y: bottom - size.height as i32 - gap,
            w: size.width,
            h: size.height,
        };
        // The toast's colour, until it's first drawn.
        crate::theme::follow(ctx);
        let surface = crate::theme::palette_of(ctx.theme() == egui::Theme::Dark).surface;
        let background = u32::from_be_bytes([0, surface.r(), surface.g(), surface.b()]);
        x11::X11Window::open(screen, gpu, rect, background, b"_NET_WM_WINDOW_TYPE_NOTIFICATION", send)
    }

    fn open_winit(
        event_loop: &ActiveEventLoop,
        gpu: &Gpu,
        ctx: &egui::Context,
        size: LogicalSize<f64>,
    ) -> Result<Backend, String> {
        #[allow(unused_mut)]
        let mut attrs = crate::app_id::window_attributes()
            .with_title("snapr")
            .with_decorations(false)
            .with_resizable(false)
            .with_visible(false)
            .with_active(false)
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_inner_size(size);
        #[cfg(windows)]
        {
            use winit::platform::windows::WindowAttributesExtWindows;
            attrs = attrs.with_skip_taskbar(true);
        }
        let window = Arc::new(event_loop.create_window(attrs).map_err(|e| e.to_string())?);
        // So it isn't in the next screenshot or recording.
        crate::record_ui::exclude_from_capture(&window);
        #[cfg(target_os = "macos")]
        not_shared(&window);
        if let Some(pos) = corner_position(event_loop, &window) {
            window.set_outer_position(pos);
        }

        let config = egui_wgpu::WgpuConfiguration {
            wgpu_setup: gpu.egui_setup(),
            ..Default::default()
        };
        let mut painter =
            pollster::block_on(Painter::new(ctx.clone(), config, false, Default::default()));
        pollster::block_on(painter.set_window(egui::ViewportId::ROOT, Some(window.clone())))
            .map_err(|e| format!("couldn't set up the preview: {e}"))?;
        let state = egui_winit::State::new(
            ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            window.theme(),
            painter.max_texture_side(),
        );
        Ok(Backend::Winit {
            window,
            state,
            painter,
        })
    }

    /// Whether this is the toast's window.
    pub fn is_window(&self, id: winit::window::WindowId) -> bool {
        matches!(&self.backend, Backend::Winit { window, .. } if window.id() == id)
    }

    pub fn set_link(&mut self, link: String) {
        self.link = Some(link);
        self.redraw();
    }

    fn redraw(&mut self) {
        match &self.backend {
            Backend::Winit { window, .. } => window.request_redraw(),
            #[cfg(target_os = "linux")]
            Backend::X11(_) => self.paint(),
        }
    }

    pub fn on_event(&mut self, event: &WindowEvent) -> Option<Action> {
        if let WindowEvent::Touch(touch) = event {
            return self.on_touch(touch);
        }
        let pointer = match event {
            WindowEvent::MouseInput { state, button, .. } => Some(match state {
                ElementState::Pressed => Pointer::Pressed(*button),
                ElementState::Released => Pointer::Released(*button),
            }),
            WindowEvent::CursorMoved { position, .. } => Some(Pointer::Moved(*position)),
            WindowEvent::CursorEntered { .. } => Some(Pointer::Entered),
            WindowEvent::CursorLeft { .. } => Some(Pointer::Left),
            _ => None,
        };
        let action = pointer.and_then(|p| self.on_pointer(p));
        if action.is_some() {
            return action;
        }
        // Not passed on to egui, which asks to repaint after every redraw:
        // the toast would paint non-stop, and on Windows that starves the
        // main window of redraws, so it stops responding.
        if let WindowEvent::RedrawRequested = event {
            self.paint();
            return None;
        }
        let Backend::Winit {
            window,
            state,
            painter,
        } = &mut self.backend
        else {
            return None;
        };
        if let WindowEvent::Resized(size) = event
            && let (Some(w), Some(h)) = (
                std::num::NonZeroU32::new(size.width),
                std::num::NonZeroU32::new(size.height),
            )
        {
            painter.on_window_resized(egui::ViewportId::ROOT, w, h);
        }
        if state.on_window_event(window, event).repaint {
            window.request_redraw();
        }
        None
    }

    /// Input for an X11 toast; ignored if it's for an earlier one.
    #[cfg(target_os = "linux")]
    pub fn on_x11_event(&mut self, event: X11Event) -> Option<Action> {
        match &self.backend {
            Backend::X11(w) if w.id() == event.window => {}
            _ => return None,
        }
        match event.pointer {
            Some(p) => self.on_pointer(p),
            None => {
                self.paint();
                None
            }
        }
    }

    /// A finger works the toast like the mouse, except that a tap shows the
    /// buttons (a finger has only the one button), and moving it swipes the
    /// toast rather than dragging the file out: the system's drag follows
    /// only the mouse. The events are kept from egui, which doesn't need them.
    fn on_touch(&mut self, touch: &Touch) -> Option<Action> {
        let mut action = None;
        for event in self.finger.mouse_events(touch) {
            match event {
                WindowEvent::CursorMoved { position, .. } => {
                    self.cursor = position;
                    if self.pressed_at.is_some_and(|from| self.dragged(from)) {
                        self.pressed_at = None;
                        if let Some(s) = &mut self.swipe {
                            s.since = Some(Instant::now());
                        }
                    }
                    self.follow_finger();
                }
                WindowEvent::MouseInput { state, .. } => match state {
                    ElementState::Pressed => {
                        self.pressed_at = Some(self.cursor);
                        self.swipe = Some(Swipe {
                            finger_x: self.finger_x(),
                            window: self.window().and_then(|w| w.outer_position().ok()),
                            since: None,
                        });
                    }
                    ElementState::Released if self.pressed_at.take().is_some() => {
                        self.swipe = None;
                        action = action.or(self.click(true));
                    }
                    ElementState::Released => action = action.or(self.end_swipe()),
                },
                WindowEvent::CursorEntered { .. } => self.expires_at = None,
                // Long enough to pick a button, once they're up.
                WindowEvent::CursorLeft { .. } => {
                    let wait = if self.buttons { SHOWN_FOR } else { AFTER_HOVER };
                    self.expires_at = Some(Instant::now() + wait);
                }
                _ => {}
            }
        }
        action
    }

    fn window(&self) -> Option<&Window> {
        match &self.backend {
            Backend::Winit { window, .. } => Some(window),
            #[cfg(target_os = "linux")]
            Backend::X11(_) => None,
        }
    }

    /// The finger across the screen, in pixels: the window may be moving
    /// under it. Just across the window where it can't be placed.
    fn finger_x(&self) -> f64 {
        let left = self.window().and_then(|w| w.inner_position().ok());
        left.map_or(0.0, |p| p.x as f64) + self.cursor.x
    }

    /// How far the finger has swiped, in pixels; `None` if it isn't swiping.
    fn swiped(&self) -> Option<f64> {
        let s = self.swipe.as_ref().filter(|s| s.since.is_some())?;
        Some(self.finger_x() - s.finger_x)
    }

    /// Moves the toast sideways with a swiping finger.
    fn follow_finger(&self) {
        let from = self.swipe.as_ref().and_then(|s| s.window);
        if let (Some(dx), Some(window), Some(from)) = (self.swiped(), self.window(), from) {
            window.set_outer_position(PhysicalPosition::new(from.x + dx.round() as i32, from.y));
        }
    }

    /// The finger's lifted after a swipe: far or fast enough dismisses the
    /// toast, else it goes back.
    fn end_swipe(&mut self) -> Option<Action> {
        let dx = self.swiped();
        let swipe = self.swipe.take()?;
        let (dx, since) = (dx?, swipe.since?);
        let points = dx.abs() / self.scale_factor();
        let speed = points / since.elapsed().as_secs_f64().max(0.001);
        let far = points >= self.points.x as f64 * SWIPE_AWAY;
        let flicked = points >= FLICK_DISTANCE && speed >= FLICK_SPEED;
        if far || flicked {
            return Some(Action::Dismiss);
        }
        if let (Some(window), Some(from)) = (self.window(), swipe.window) {
            window.set_outer_position(from);
        }
        None
    }

    /// Whether the pointer has gone far enough from `from` to be a drag.
    fn dragged(&self, from: PhysicalPosition<f64>) -> bool {
        let (dx, dy) = (self.cursor.x - from.x, self.cursor.y - from.y);
        let threshold = DRAG_THRESHOLD * self.scale_factor();
        dx * dx + dy * dy >= threshold * threshold
    }

    /// A left-click or a tap: the button under it if the buttons are up,
    /// else opens, or shows the buttons for a tap. Missing the buttons puts
    /// them away.
    fn click(&mut self, touch: bool) -> Option<Action> {
        if self.buttons {
            let points =
                egui::pos2(self.cursor.x as f32, self.cursor.y as f32) / self.scale_factor() as f32;
            let hit = button_rects(self.points)
                .into_iter()
                .find(|(_, rect)| rect.contains(points));
            if hit.is_none() {
                self.buttons = false;
                self.redraw();
            }
            return hit.map(|(action, _)| action);
        }
        if touch {
            self.buttons = true;
            self.redraw();
            return None;
        }
        Some(Action::Open)
    }

    fn on_pointer(&mut self, pointer: Pointer) -> Option<Action> {
        match pointer {
            Pointer::Pressed(MouseButton::Left) => self.pressed_at = Some(self.cursor),
            Pointer::Pressed(_) => {}
            Pointer::Released(button) => match button {
                // Not if the press turned into a drag.
                MouseButton::Left if self.pressed_at.take().is_some() => return self.click(false),
                MouseButton::Middle => return Some(Action::CopyImage),
                MouseButton::Right => return Some(Action::Dismiss),
                _ => {}
            },
            Pointer::Moved(position) => {
                self.cursor = position;
                // Also after a drag, which swallows the mouse's comings and goings.
                self.expires_at = None;
                if self.pressed_at.is_some_and(|from| self.dragged(from)) {
                    self.pressed_at = None;
                    if let Backend::Winit { window, .. } = &self.backend {
                        crate::output::drag_out(window, std::slice::from_ref(&self.path));
                    }
                    self.expires_at = Some(Instant::now() + AFTER_HOVER);
                }
            }
            Pointer::Entered => self.expires_at = None,
            Pointer::Left => self.expires_at = Some(Instant::now() + AFTER_HOVER),
        }
        None
    }

    fn scale_factor(&self) -> f64 {
        match &self.backend {
            Backend::Winit { window, .. } => window.scale_factor(),
            #[cfg(target_os = "linux")]
            Backend::X11(w) => w.scale_factor,
        }
    }

    fn paint(&mut self) {
        let input = match &mut self.backend {
            Backend::Winit { window, state, .. } => state.take_egui_input(window),
            #[cfg(target_os = "linux")]
            Backend::X11(w) => w.egui_input(),
        };
        let ctx = self.ctx.clone();
        crate::theme::follow(&ctx);
        let texture = self.texture.clone();
        let toast = Contents {
            texture: texture.as_ref(),
            path: &self.path,
            link: self.link.as_deref(),
            video: self.video,
            size: self.size,
            icons: self.icons,
            buttons: self.buttons,
        };
        let mut output = ctx.run_ui(input, |ui| toast.show(ui));
        let (window, state, painter) = match &mut self.backend {
            Backend::Winit {
                window,
                state,
                painter,
            } => (window, state, painter),
            #[cfg(target_os = "linux")]
            Backend::X11(w) => return w.paint(&ctx, output),
        };
        state.handle_platform_output(window, std::mem::take(&mut output.platform_output));
        let primitives =
            ctx.tessellate(std::mem::take(&mut output.shapes), output.pixels_per_point);
        painter.paint_and_update_textures(
            egui::ViewportId::ROOT,
            output.pixels_per_point,
            crate::theme::backdrop(&ctx),
            &primitives,
            &mut output.textures_delta,
            Vec::new(),
            window,
        );
        output.textures_delta.clear();
    }
}

/// What a toast shows.
struct Contents<'a> {
    texture: Option<&'a egui::TextureHandle>,
    path: &'a std::path::Path,
    link: Option<&'a str>,
    video: bool,
    size: ToastSize,
    icons: bool,
    /// Draws the buttons a tap shows over it.
    buttons: bool,
}

impl Contents<'_> {
    fn show(&self, ui: &mut egui::Ui) {
        let window = ui.max_rect();
        egui::CentralPanel::default()
            .frame({
                let p = crate::theme::palette(ui);
                egui::Frame::new()
                    .fill(p.surface)
                    .stroke(egui::Stroke::new(1.0, p.stroke))
                    .inner_margin(MARGIN)
            })
            .show(ui, |ui| match self.size {
                ToastSize::Large => self.large(ui),
                ToastSize::Compact => self.compact(ui),
            });
        if self.buttons {
            self.touch_buttons(ui, window);
        }
    }

    /// Open, Copy and Close, over a dimmed toast, for a finger to pick.
    fn touch_buttons(&self, ui: &egui::Ui, window: egui::Rect) {
        let p = crate::theme::palette(ui);
        let painter = ui.painter();
        painter.rect_filled(window.shrink(1.0), 0.0, p.surface.gamma_multiply(0.8));
        let open = match (self.link.is_some(), self.video) {
            (true, _) => "Open link",
            (false, true) => "Play",
            (false, false) => "Open",
        };
        for (action, rect) in button_rects(window.size()) {
            let rect = rect.translate(window.min.to_vec2());
            let (label, fill, text) = match action {
                Action::Open => (open, crate::theme::ACCENT, egui::Color32::WHITE),
                Action::CopyImage => ("Copy", p.control, p.text_strong),
                Action::Dismiss => ("Close", p.control, p.text_strong),
            };
            let stroke = egui::Stroke::new(1.0, p.stroke);
            painter.rect(rect, 6.0, fill, stroke, egui::StrokeKind::Inside);
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                label,
                egui::FontId::proportional(13.0),
                text,
            );
        }
    }

    /// The preview image and the caption under it.
    fn large(&self, ui: &mut egui::Ui) {
        ui.spacing_mut().item_spacing.y = CAPTION_GAP;
        let image_area = egui::vec2(
            ui.available_width(),
            ui.available_height() - CAPTION_HEIGHT - CAPTION_GAP,
        );
        let (rect, _) = ui.allocate_exact_size(image_area, egui::Sense::hover());
        self.picture(ui, rect, 20.0);
        // Centred in its own strip so it clears the bottom edge.
        let caption_row = egui::vec2(ui.available_width(), CAPTION_HEIGHT);
        let row = egui::Layout::left_to_right(egui::Align::Center);
        ui.allocate_ui_with_layout(caption_row, row, |ui| {
            ui.set_min_height(CAPTION_HEIGHT);
            ui.spacing_mut().item_spacing.x = ICON_GAP;
            self.caption(ui);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                self.hints(ui);
            });
        });
    }

    /// A thumbnail, with the caption, the link (or the file's name) and the
    /// hints beside it.
    fn compact(&self, ui: &mut egui::Ui) {
        let thumb = egui::vec2(COMPACT_THUMB.0, COMPACT_THUMB.1);
        let (rect, _) = ui.allocate_exact_size(thumb, egui::Sense::hover());
        self.picture(ui, rect, 14.0);
        let p = crate::theme::palette(ui);
        let (second, color) = match self.link {
            Some(link) => (link.trim_start_matches("https://").to_owned(), p.selected_text),
            None => {
                let name = self.path.file_name().unwrap_or_default();
                (name.to_string_lossy().into_owned(), p.text_weak)
            }
        };
        let text = egui::Rect::from_min_max(
            egui::pos2(rect.right() + 10.0, rect.top()),
            ui.max_rect().right_bottom(),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(text), |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(ICON_GAP, 3.0);
            // Rows only as tall as their text.
            ui.spacing_mut().interact_size.y = 0.0;
            // Centred against the thumbnail.
            ui.add_space(7.0);
            ui.horizontal(|ui| self.caption(ui));
            let second = RichText::new(second).color(color).size(11.0);
            ui.add(egui::Label::new(second).truncate());
            ui.horizontal(|ui| self.hints(ui));
        });
    }

    /// What a left-click does.
    fn caption(&self, ui: &mut egui::Ui) {
        let (click, button) = match (self.link.is_some(), self.video) {
            (true, _) => ("Click to open the link", "Open the link"),
            (false, true) => ("Click to play", "Play"),
            (false, false) => ("Click to open", "Open"),
        };
        let text = if self.icons {
            mouse_icon(ui, MouseButton::Left);
            button
        } else {
            click
        };
        ui.add(egui::Label::new(RichText::new(text).strong().size(12.0)).truncate());
    }

    /// What the middle and right buttons do; from the right in a
    /// right-to-left layout.
    fn hints(&self, ui: &mut egui::Ui) {
        let copies = "copy";
        if !self.icons {
            let text = format!("middle: {copies} \u{00b7} right: close");
            ui.add(egui::Label::new(RichText::new(text).weak().size(11.0)).truncate());
            return;
        }
        let mut hints = [(MouseButton::Middle, copies), (MouseButton::Right, "close")];
        let backwards = ui.layout().main_dir() == egui::Direction::RightToLeft;
        if backwards {
            hints.reverse();
        }
        for (i, (button, does)) in hints.into_iter().enumerate() {
            if i > 0 {
                ui.add_space(6.0);
            }
            let does = egui::Label::new(RichText::new(does).weak().size(11.0));
            if backwards {
                ui.add(does);
                mouse_icon(ui, button);
            } else {
                mouse_icon(ui, button);
                ui.add(does);
            }
        }
    }

    /// The preview, fitted into `rect`, or the file's icon.
    fn picture(&self, ui: &egui::Ui, rect: egui::Rect, badge: f32) {
        match self.texture {
            Some(texture) => {
                let tex = texture.size_vec2();
                let scale = (rect.width() / tex.x).min(rect.height() / tex.y);
                let img = egui::Rect::from_center_size(rect.center(), tex * scale);
                egui::Image::new((texture.id(), tex))
                    .corner_radius(3.0)
                    .paint_at(ui, img);
                if self.video {
                    crate::thumbnail::draw_play_badge(ui.painter(), img.center(), badge);
                }
            }
            None => crate::thumbnail::draw_file_icon(ui.painter(), rect, self.path),
        }
    }
}

/// A mouse with `button` lit up, about as tall as small text.
fn mouse_icon(ui: &mut egui::Ui, button: MouseButton) {
    use std::f32::consts::PI;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 14.0), egui::Sense::hover());
    let p = crate::theme::palette(ui);
    let painter = ui.painter();
    let r = rect.width() / 2.0;
    let (top, left, right) = (rect.top(), rect.left(), rect.right());
    let centre = egui::pos2(rect.center().x, top + r);
    // Where the buttons end and the body begins.
    let split = top + rect.height() * 0.45;
    let lit = crate::theme::ACCENT;
    match button {
        MouseButton::Left | MouseButton::Right => {
            // The quarter of the rounded top over the button, down to the split.
            let (from, edge) = if button == MouseButton::Left {
                (PI, left)
            } else {
                (1.5 * PI, right)
            };
            let mut points: Vec<egui::Pos2> = (0..=6)
                .map(|i| from + 0.5 * PI * i as f32 / 6.0)
                .map(|a| centre + r * egui::vec2(a.cos(), a.sin()))
                .collect();
            if button == MouseButton::Left {
                points.extend([egui::pos2(centre.x, split), egui::pos2(edge, split)]);
            } else {
                points.extend([egui::pos2(edge, split), egui::pos2(centre.x, split)]);
            }
            painter.add(egui::Shape::convex_polygon(points, lit, egui::Stroke::NONE));
        }
        _ => {
            let wheel = egui::Rect::from_center_size(
                egui::pos2(centre.x, top + 4.0),
                egui::vec2(2.0, 4.5),
            );
            painter.rect_filled(wheel, 1.0, lit);
        }
    }
    let line = egui::Stroke::new(1.0, p.text_weak);
    painter.line_segment([egui::pos2(left, split), egui::pos2(right, split)], line);
    painter.rect_stroke(rect, r, line, egui::StrokeKind::Inside);
}

/// Bottom-right of the primary screen's work area (above the taskbar).
fn corner_position(event_loop: &ActiveEventLoop, window: &Window) -> Option<PhysicalPosition<i32>> {
    let monitor = event_loop
        .primary_monitor()
        .or_else(|| event_loop.available_monitors().next())?;
    let gap = (SCREEN_GAP * monitor.scale_factor()) as i32;
    let size = window.outer_size();
    let (right, bottom) = work_area_corner().unwrap_or_else(|| {
        let (p, s) = (monitor.position(), monitor.size());
        (p.x + s.width as i32, p.y + s.height as i32)
    });
    Some(PhysicalPosition::new(
        right - size.width as i32 - gap,
        bottom - size.height as i32 - gap,
    ))
}

/// The bottom-right corner of the primary screen minus the taskbar.
#[cfg(windows)]
fn work_area_corner() -> Option<(i32, i32)> {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::UI::WindowsAndMessaging::{SPI_GETWORKAREA, SystemParametersInfoW};
    let mut r = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    let ok = unsafe { SystemParametersInfoW(SPI_GETWORKAREA, 0, (&raw mut r).cast(), 0) };
    (ok != 0).then_some((r.right, r.bottom))
}

#[cfg(not(windows))]
fn work_area_corner() -> Option<(i32, i32)> {
    None
}

/// Keeps the toast's window out of other processes' captures, snapr's own
/// screenshots and recordings among them (NSWindowSharingNone).
#[cfg(target_os = "macos")]
fn not_shared(window: &Window) {
    use objc2_app_kit::{NSView, NSWindowSharingType};
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(handle) = window.window_handle() else { return };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else { return };
    // SAFETY: winit's view, made on this (the main) thread, alive while
    // `window` is.
    let view: &NSView = unsafe { h.ns_view.cast().as_ref() };
    if let Some(ns_window) = view.window() {
        ns_window.setSharingType(NSWindowSharingType::None);
    }
}

/// Decodes a screenshot small enough for the preview.
/// The preview for any saved or uploaded file; `None` for files without a
/// picture (they get an icon). `ffmpeg` decodes videos on Linux.
pub fn load_preview(path: &std::path::Path, ffmpeg: &str) -> Option<egui::ColorImage> {
    crate::thumbnail::picture(path, ffmpeg).map(preview_of)
}

/// Shrinks an image to preview size.
pub fn preview_of(img: impl Into<image::DynamicImage>) -> egui::ColorImage {
    let img = img.into().thumbnail(DECODE_SIZE.0, DECODE_SIZE.1);
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw())
}

#[cfg(test)]
mod tests {
    use crate::settings::ToastSize;

    /// Renders a preview of a 16:9 screenshot to `target/toast-preview.png`:
    /// `cargo test toast_preview -- --ignored`.
    #[test]
    #[ignore]
    fn toast_preview() {
        let pixels: Vec<u8> = (0..640 * 360)
            .flat_map(|i| [(i % 640 / 3) as u8, 90, (i / 640 / 2) as u8, 255])
            .collect();
        let image = egui::ColorImage::from_rgba_unmultiplied([640, 360], &pixels);
        for (name, video, dark, size, icons, buttons) in [
            ("toast-preview", false, true, ToastSize::Large, false, false),
            ("toast-video-preview", true, true, ToastSize::Large, false, false),
            ("toast-light-preview", false, false, ToastSize::Large, false, false),
            ("toast-icons-preview", false, true, ToastSize::Large, true, false),
            ("toast-icons-light-preview", false, false, ToastSize::Large, true, false),
            ("toast-compact-preview", false, true, ToastSize::Compact, false, false),
            ("toast-compact-video-preview", true, true, ToastSize::Compact, false, false),
            ("toast-compact-light-preview", false, false, ToastSize::Compact, false, false),
            ("toast-compact-icons-preview", true, true, ToastSize::Compact, true, false),
            ("toast-touch-preview", false, true, ToastSize::Large, false, true),
            ("toast-touch-light-preview", false, false, ToastSize::Large, false, true),
            ("toast-compact-touch-preview", false, true, ToastSize::Compact, false, true),
        ] {
            let mut texture = None;
            let window = match size {
                ToastSize::Large => [332, 226],
                ToastSize::Compact => [332, 84],
            };
            crate::preview::render(name, window, 1.0, |root| {
                crate::preview::themed(root, dark);
                let t = texture.get_or_insert_with(|| {
                    root.ctx()
                        .load_texture("toast", image.clone(), Default::default())
                });
                let path = std::path::Path::new(if video { "clip.mp4" } else { "shot.png" });
                let toast = super::Contents {
                    texture: Some(t),
                    path,
                    link: Some("https://i.example.com/a8F3kQ2x.png"),
                    video,
                    size,
                    icons,
                    buttons,
                };
                toast.show(root);
            });
        }
    }
}
