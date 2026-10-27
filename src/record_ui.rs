//! What's on screen while recording: a dashed border just outside the region
//! (clicks go through it) and a bar under it with the elapsed time and
//! Stop / Pause / Restart / Abort. None of it shows up in the recording.
//! The border is red while the encoder starts, green once it's recording and
//! amber while paused.

// `Edges` and `BarWindow` only have their X11 variants on Linux.
#![cfg_attr(not(target_os = "linux"), allow(irrefutable_let_patterns))]

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Color32, CornerRadius, FontId, Pos2, Rect, Sense, Stroke, Vec2, pos2, vec2};
use egui_wgpu::wgpu;
use egui_wgpu::winit::Painter;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowAttributes, WindowId, WindowLevel};

use crate::capture;
use crate::gpu::Gpu;
#[cfg(target_os = "linux")]
use crate::toast::Pointer;
#[cfg(target_os = "linux")]
use winit::event::MouseButton;
#[cfg(target_os = "linux")]
use crate::x11::{self, X11Event};

/// Border thickness and dash length, in physical pixels.
const BORDER: u32 = 2;
const DASH: u32 = 6;
const RED: [u8; 3] = [0xe5, 0x48, 0x4d];
const GREEN: [u8; 3] = [0x3c, 0xc8, 0x6e];
const AMBER: [u8; 3] = [0xf5, 0xa5, 0x24];
const GAP_COLOR: [u8; 3] = [0x10, 0x10, 0x12];
/// The buttons' icons: on the dark bar a lighter blue than the accent, on
/// the light one a darker blue, so they're as easy to see.
const ICON_DARK: Color32 = Color32::from_rgb(0x4c, 0x9e, 0xff);
const ICON_LIGHT: Color32 = Color32::from_rgb(0x0d, 0x5f, 0xc4);
const BAR_HEIGHT: f32 = 40.0;
/// Gap between the border and the bar, in points.
const BAR_GAP: f64 = 6.0;
/// How often the timer is redrawn while recording (about 30 times a second).
const TIMER_TICK: Duration = Duration::from_millis(33);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Stop,
    TogglePause,
    Restart,
    Abort,
}

pub struct RecordingUi {
    edges: Edges,
    bar: Bar,
    started: Instant,
    paused_at: Option<Instant>,
    paused_for: Duration,
    /// Recording rather than still starting; the timer starts then.
    live: bool,
}

/// The border's four sides.
enum Edges {
    Winit(Vec<Edge>),
    /// Under Wayland, which won't let a window choose where it goes.
    #[cfg(target_os = "linux")]
    X11(Vec<x11::Dashes>),
}

impl RecordingUi {
    /// Shows the border and bar around `rect` (global physical pixels).
    /// `send_x11` passes on the bar's input when it's an X11 window, from
    /// its own thread.
    pub fn open(
        event_loop: &ActiveEventLoop,
        gpu: &Gpu,
        rect: capture::Rect,
        #[cfg(target_os = "linux")] send_x11: impl Fn(X11Event) + Send + 'static,
    ) -> Result<Self, String> {
        #[cfg(target_os = "linux")]
        if x11::wanted(event_loop) {
            match Self::open_x11(gpu, rect, send_x11) {
                Ok(ui) => return Ok(ui),
                Err(e) => eprintln!("couldn't show the recording border through XWayland: {e}"),
            }
        }
        let mut edges = Vec::new();
        for (side, horizontal) in sides(rect) {
            edges.push(Edge::open(event_loop, gpu, side, horizontal)?);
        }
        let bar = Bar::open(event_loop, gpu, rect)?;
        let ui = Self::new(gpu, Edges::Winit(edges), bar);
        if let Edges::Winit(edges) = &ui.edges {
            for e in edges {
                e.window.set_visible(true);
            }
        }
        if let BarWindow::Winit { window, .. } = &ui.bar.window {
            window.set_visible(true);
        }
        Ok(ui)
    }

    /// The border and bar as X11 windows, which XWayland puts where they're
    /// asked to go.
    #[cfg(target_os = "linux")]
    fn open_x11(
        gpu: &Gpu,
        rect: capture::Rect,
        send: impl Fn(X11Event) + Send + 'static,
    ) -> Result<Self, String> {
        let screen = x11::Screen::connect()?;
        let rect = to_x11(rect);
        let edges = sides(rect)
            .into_iter()
            .map(|(side, horizontal)| x11::Dashes::open(&screen, side, horizontal, rgb(RED), rgb(GAP_COLOR), DASH))
            .collect::<Result<Vec<_>, _>>()?;
        let scale = screen.scale_factor;
        let size = LogicalSize::new(BAR_WIDTH as f64, BAR_HEIGHT as f64).to_physical::<u32>(scale);
        let gap = (BAR_GAP * scale) as i32 + BORDER as i32;
        let (x, y) = place_bar(rect, (size.width as i32, size.height as i32), gap, &screen.monitors());
        let bar = capture::Rect {
            x,
            y,
            w: size.width,
            h: size.height,
        };
        let ctx = bar_context();
        crate::theme::follow(&ctx);
        // The bar's colour, until it's first drawn.
        let card = crate::theme::palette_of(ctx.theme() == egui::Theme::Dark).card;
        let background = u32::from_be_bytes([0, card.r(), card.g(), card.b()]);
        let window = x11::X11Window::open(
            screen,
            gpu,
            bar,
            background,
            b"_NET_WM_WINDOW_TYPE_NOTIFICATION",
            send,
        )?;
        let bar = Bar {
            window: BarWindow::X11 {
                window,
                events: Vec::new(),
                cursor: Pos2::ZERO,
            },
            ctx,
            repaint_at: None,
        };
        Ok(Self::new(gpu, Edges::X11(edges), bar))
    }

    fn new(gpu: &Gpu, edges: Edges, bar: Bar) -> Self {
        let mut ui = Self {
            edges,
            bar,
            started: Instant::now(),
            paused_at: None,
            paused_for: Duration::ZERO,
            live: false,
        };
        ui.redraw_edges(gpu);
        ui.paint_bar();
        ui
    }

    pub fn owns(&self, id: WindowId) -> bool {
        let bar = matches!(&self.bar.window, BarWindow::Winit { window, .. } if window.id() == id);
        bar || matches!(&self.edges, Edges::Winit(edges) if edges.iter().any(|e| e.window.id() == id))
    }

    pub fn paused(&self) -> bool {
        self.paused_at.is_some()
    }

    pub fn live(&self) -> bool {
        self.live
    }

    /// The encoder has started: turns the border green and starts the timer.
    pub fn set_live(&mut self, gpu: &Gpu) {
        let now = Instant::now();
        self.live = true;
        self.started = now;
        self.paused_for = Duration::ZERO;
        if self.paused_at.is_some() {
            self.paused_at = Some(now);
        }
        self.redraw_edges(gpu);
        self.redraw_bar();
    }

    /// The border's (and the bar's dot's) colour.
    fn color(&self) -> [u8; 3] {
        if self.paused_at.is_some() {
            AMBER
        } else if self.live {
            GREEN
        } else {
            RED
        }
    }

    pub fn set_paused(&mut self, gpu: &Gpu, paused: bool) {
        match (paused, self.paused_at) {
            (true, None) => self.paused_at = Some(Instant::now()),
            (false, Some(t)) => {
                self.paused_for += t.elapsed();
                self.paused_at = None;
            }
            _ => return,
        }
        self.redraw_edges(gpu);
        self.redraw_bar();
    }

    /// Recorded time so far, not counting pauses.
    fn elapsed(&self) -> Duration {
        if !self.live {
            return Duration::ZERO;
        }
        let end = self.paused_at.unwrap_or_else(Instant::now);
        end.duration_since(self.started)
            .saturating_sub(self.paused_for)
    }

    pub fn on_event(&mut self, gpu: &Gpu, id: WindowId, event: &WindowEvent) -> Option<Action> {
        let color = self.color();
        if let Edges::Winit(edges) = &mut self.edges
            && let Some(edge) = edges.iter_mut().find(|e| e.window.id() == id)
        {
            if matches!(
                event,
                WindowEvent::RedrawRequested | WindowEvent::Resized(_)
            ) {
                edge.draw(gpu, color);
            }
            return None;
        }
        if let WindowEvent::RedrawRequested = event {
            return self.paint_bar();
        }
        let BarWindow::Winit {
            window,
            state,
            painter,
        } = &mut self.bar.window
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

    /// Input for an X11 bar; ignored if it's for an earlier one.
    #[cfg(target_os = "linux")]
    pub fn on_x11_event(&mut self, event: X11Event) -> Option<Action> {
        let BarWindow::X11 {
            window,
            events,
            cursor,
        } = &mut self.bar.window
        else {
            return None;
        };
        if window.id() != event.window {
            return None;
        }
        let ppp = window.scale_factor as f32;
        match event.pointer {
            Some(Pointer::Moved(p)) => {
                *cursor = pos2(p.x as f32 / ppp, p.y as f32 / ppp);
                events.push(egui::Event::PointerMoved(*cursor));
            }
            Some(Pointer::Pressed(b) | Pointer::Released(b)) => {
                let pressed = matches!(event.pointer, Some(Pointer::Pressed(_)));
                if let Some(button) = egui_button(b) {
                    events.push(egui::Event::PointerButton {
                        pos: *cursor,
                        button,
                        pressed,
                        modifiers: Default::default(),
                    });
                }
            }
            Some(Pointer::Left) => events.push(egui::Event::PointerGone),
            Some(Pointer::Entered) | None => {}
        }
        self.paint_bar()
    }

    /// When the timer next needs repainting.
    pub fn repaint_at(&self) -> Option<Instant> {
        self.bar.repaint_at
    }

    pub fn tick(&mut self, now: Instant) {
        if self.bar.repaint_at.is_some_and(|t| t <= now) {
            self.bar.repaint_at = None;
            self.redraw_bar();
        }
    }

    fn redraw_edges(&mut self, gpu: &Gpu) {
        let color = self.color();
        match &mut self.edges {
            Edges::Winit(edges) => {
                for e in edges {
                    e.draw(gpu, color);
                }
            }
            #[cfg(target_os = "linux")]
            Edges::X11(edges) => {
                for e in edges.iter() {
                    if let Err(e) = e.set_colors(rgb(color), rgb(GAP_COLOR), DASH) {
                        eprintln!("recording border: {e}");
                    }
                }
            }
        }
    }

    /// Repaints the bar: soon for a winit window, now for an X11 one, which
    /// gets no redraw events.
    fn redraw_bar(&mut self) {
        match &self.bar.window {
            BarWindow::Winit { window, .. } => window.request_redraw(),
            #[cfg(target_os = "linux")]
            BarWindow::X11 { .. } => {
                self.paint_bar();
            }
        }
    }

    fn paint_bar(&mut self) -> Option<Action> {
        let elapsed = self.elapsed();
        let paused = self.paused_at.is_some();
        let color = self.color();
        let bar = &mut self.bar;
        let input = match &mut bar.window {
            BarWindow::Winit { window, state, .. } => state.take_egui_input(window),
            #[cfg(target_os = "linux")]
            BarWindow::X11 { window, events, .. } => egui::RawInput {
                events: std::mem::take(events),
                ..window.egui_input()
            },
        };
        let ctx = bar.ctx.clone();
        crate::theme::follow(&ctx);
        let mut action = None;
        let mut output = ctx.run_ui(input, |ui| action = bar_contents(ui, elapsed, paused, color));
        // Keep the milliseconds running while recording; hover effects
        // repaint straight away.
        let tick = if paused { Duration::MAX } else { TIMER_TICK };
        let delay = output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map_or(tick, |v| v.repaint_delay.min(tick));
        bar.repaint_at = (delay < tick || !paused).then(|| Instant::now() + delay);
        match &mut bar.window {
            BarWindow::Winit {
                window,
                state,
                painter,
            } => {
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
            #[cfg(target_os = "linux")]
            BarWindow::X11 { window, .. } => window.paint(&ctx, output),
        }
        action
    }
}

/// The border's sides around `rect`, just outside it: each one's place, and
/// whether it's horizontal.
fn sides(rect: capture::Rect) -> [(capture::Rect, bool); 4] {
    let b = BORDER as i32;
    let (w, h) = (rect.w + 2 * BORDER, rect.h);
    let side = |x, y, w, h| capture::Rect { x, y, w, h };
    [
        (side(rect.x - b, rect.y - b, w, BORDER), true),
        (side(rect.x - b, rect.bottom(), w, BORDER), true),
        (side(rect.x - b, rect.y, BORDER, h), false),
        (side(rect.right(), rect.y, BORDER, h), false),
    ]
}

/// `rect` (global physical pixels, as xcap gives them) in XWayland's pixels.
/// xcap takes monitors from XWayland and divides their places and sizes by
/// one scale for all of them, so this multiplies them by it again.
#[cfg(target_os = "linux")]
fn to_x11(rect: capture::Rect) -> capture::Rect {
    let scale = xcap::Monitor::all()
        .ok()
        .and_then(|m| m.first()?.scale_factor().ok())
        .filter(|s| *s > 0.0)
        .unwrap_or(1.0) as f64;
    capture::Rect::from_points(
        (rect.x as f64 * scale, rect.y as f64 * scale),
        (rect.right() as f64 * scale, rect.bottom() as f64 * scale),
    )
}

#[cfg(target_os = "linux")]
fn rgb([r, g, b]: [u8; 3]) -> u32 {
    u32::from_be_bytes([0, r, g, b])
}

#[cfg(target_os = "linux")]
fn egui_button(button: MouseButton) -> Option<egui::PointerButton> {
    match button {
        MouseButton::Left => Some(egui::PointerButton::Primary),
        MouseButton::Right => Some(egui::PointerButton::Secondary),
        MouseButton::Middle => Some(egui::PointerButton::Middle),
        _ => None,
    }
}

fn base_attributes() -> WindowAttributes {
    #[allow(unused_mut)]
    let mut attrs = Window::default_attributes()
        .with_title("snapr recording")
        .with_decorations(false)
        .with_resizable(false)
        .with_visible(false)
        .with_active(false)
        .with_window_level(WindowLevel::AlwaysOnTop);
    #[cfg(windows)]
    {
        use winit::platform::windows::WindowAttributesExtWindows;
        attrs = attrs.with_skip_taskbar(true);
    }
    attrs
}

/// One side of the border: a thin window filled with dashes.
struct Edge {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    horizontal: bool,
}

impl Edge {
    fn open(
        event_loop: &ActiveEventLoop,
        gpu: &Gpu,
        rect: capture::Rect,
        horizontal: bool,
    ) -> Result<Self, String> {
        let attrs = base_attributes()
            .with_position(PhysicalPosition::new(rect.x, rect.y))
            .with_inner_size(PhysicalSize::new(rect.w.max(1), rect.h.max(1)));
        let window = Arc::new(event_loop.create_window(attrs).map_err(|e| e.to_string())?);
        let _ = window.set_cursor_hittest(false);
        exclude_from_capture(&window);
        let surface = gpu
            .instance
            .create_surface(window.clone())
            .map_err(|e| e.to_string())?;
        let caps = surface.get_capabilities(&gpu.adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| {
                matches!(
                    f,
                    wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm
                )
            })
            .ok_or("no 8-bit surface format for the recording border")?;
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | (caps.usages & wgpu::TextureUsages::COPY_DST),
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            ..surface
                .get_default_config(&gpu.adapter, size.width.max(1), size.height.max(1))
                .ok_or("the recording border's surface is unsupported")?
        };
        surface.configure(&gpu.device, &config);
        Ok(Self {
            window,
            surface,
            config,
            horizontal,
        })
    }

    fn draw(&mut self, gpu: &Gpu, color: [u8; 3]) {
        let size = self.window.inner_size();
        if size.width == 0 || size.height == 0 {
            return;
        }
        if (size.width, size.height) != (self.config.width, self.config.height) {
            self.config.width = size.width;
            self.config.height = size.height;
            self.surface.configure(&gpu.device, &self.config);
        }
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f)
            | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&gpu.device, &self.config);
                self.window.request_redraw();
                return;
            }
            _ => return,
        };
        let (w, h) = (self.config.width, self.config.height);
        if self.config.usage.contains(wgpu::TextureUsages::COPY_DST) {
            let bgra = self.config.format == wgpu::TextureFormat::Bgra8Unorm;
            let pixel = |c: [u8; 3]| {
                if bgra {
                    [c[2], c[1], c[0], 255]
                } else {
                    [c[0], c[1], c[2], 255]
                }
            };
            let (on, off) = (pixel(color), pixel(GAP_COLOR));
            let mut data = Vec::with_capacity((w * h * 4) as usize);
            for y in 0..h {
                for x in 0..w {
                    let along = if self.horizontal { x } else { y };
                    data.extend_from_slice(if (along / DASH).is_multiple_of(2) {
                        &on
                    } else {
                        &off
                    });
                }
            }
            gpu.queue.write_texture(
                frame.texture.as_image_copy(),
                &data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(w * 4),
                    rows_per_image: Some(h),
                },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
            );
        } else {
            // No copies into this surface: a solid line instead of dashes.
            let view = frame.texture.create_view(&Default::default());
            let mut encoder = gpu.device.create_command_encoder(&Default::default());
            let [r, g, b] = color.map(|c| c as f64 / 255.0);
            encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("recording border"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r, g, b, a: 1.0 }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            gpu.queue.submit([encoder.finish()]);
        }
        self.window.pre_present_notify();
        gpu.queue.present(frame);
    }
}

/// The timer and buttons.
struct Bar {
    window: BarWindow,
    ctx: egui::Context,
    repaint_at: Option<Instant>,
}

#[allow(clippy::large_enum_variant)]
enum BarWindow {
    Winit {
        window: Arc<Window>,
        state: egui_winit::State,
        painter: Painter,
    },
    /// Under Wayland, which won't let a window choose where it goes.
    #[cfg(target_os = "linux")]
    X11 {
        window: x11::X11Window,
        /// Input since the last frame.
        events: Vec<egui::Event>,
        cursor: Pos2,
    },
}

fn bar_context() -> egui::Context {
    let ctx = egui::Context::default();
    crate::theme::apply(&ctx);
    ctx
}

impl Bar {
    fn open(event_loop: &ActiveEventLoop, gpu: &Gpu, rect: capture::Rect) -> Result<Self, String> {
        let size = LogicalSize::new(BAR_WIDTH as f64, BAR_HEIGHT as f64);
        let window = Arc::new(
            event_loop
                .create_window(base_attributes().with_inner_size(size))
                .map_err(|e| e.to_string())?,
        );
        exclude_from_capture(&window);
        window.set_outer_position(bar_position(event_loop, &window, rect));

        let ctx = bar_context();
        let config = egui_wgpu::WgpuConfiguration {
            wgpu_setup: gpu.egui_setup(),
            ..Default::default()
        };
        let mut painter =
            pollster::block_on(Painter::new(ctx.clone(), config, false, Default::default()));
        pollster::block_on(painter.set_window(egui::ViewportId::ROOT, Some(window.clone())))
            .map_err(|e| format!("couldn't set up the recording bar: {e}"))?;
        let state = egui_winit::State::new(
            ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            window.theme(),
            painter.max_texture_side(),
        );
        Ok(Self {
            window: BarWindow::Winit {
                window,
                state,
                painter,
            },
            ctx,
            repaint_at: None,
        })
    }
}

const BAR_WIDTH: f32 = 470.0;

fn bar_position(
    event_loop: &ActiveEventLoop,
    window: &Window,
    rect: capture::Rect,
) -> PhysicalPosition<i32> {
    let size = window.outer_size();
    let gap = (BAR_GAP * window.scale_factor()) as i32 + BORDER as i32;
    let monitors: Vec<capture::Rect> = event_loop
        .available_monitors()
        .map(|m| {
            let (p, s) = (m.position(), m.size());
            capture::Rect {
                x: p.x,
                y: p.y,
                w: s.width,
                h: s.height,
            }
        })
        .collect();
    let (x, y) = place_bar(rect, (size.width as i32, size.height as i32), gap, &monitors);
    PhysicalPosition::new(x, y)
}

/// Where a bar of `(w, h)` goes: centred under the region, or above it (or
/// inside its bottom edge) when there's no room below on that monitor, `gap`
/// from it.
fn place_bar(
    rect: capture::Rect,
    (w, h): (i32, i32),
    gap: i32,
    monitors: &[capture::Rect],
) -> (i32, i32) {
    let monitor = monitors.iter().find(|m| {
        m.contains((
            rect.x as f64 + rect.w as f64 / 2.0,
            rect.y as f64 + rect.h as f64 / 2.0,
        ))
    });
    let x = rect.x + rect.w as i32 / 2 - w / 2;
    let below = rect.bottom() + gap;
    let above = rect.y - gap - h;
    let Some(m) = monitor else {
        return (x, below);
    };
    let y = if below + h <= m.bottom() {
        below
    } else if above >= m.y {
        above
    } else {
        rect.bottom() - gap - h
    };
    (x.clamp(m.x, (m.right() - w).max(m.x)), y)
}

/// Keeps a window out of screenshots and recordings (Windows 10 2004+).
fn exclude_from_capture(window: &Window) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::HWND;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE,
        };
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
        if let Ok(handle) = window.window_handle()
            && let RawWindowHandle::Win32(h) = handle.as_raw()
        {
            unsafe { SetWindowDisplayAffinity(h.hwnd.get() as HWND, WDA_EXCLUDEFROMCAPTURE) };
        }
    }
    #[cfg(not(windows))]
    let _ = window;
}

fn bar_contents(
    ui: &mut egui::Ui,
    elapsed: Duration,
    paused: bool,
    [r, g, b]: [u8; 3],
) -> Option<Action> {
    let mut action = None;
    let p = crate::theme::palette(ui);
    egui::CentralPanel::default()
        .frame(
            egui::Frame::new()
                .fill(p.card)
                .stroke(Stroke::new(1.0, p.stroke))
                .corner_radius(CornerRadius::same(6)),
        )
        .show(ui, |ui| {
            let full = ui.max_rect();
            let painter = ui.painter().clone();
            // Status: a dot the border's colour (hollow while paused) and
            // the time.
            let dot = pos2(full.left() + 16.0, full.center().y);
            let color = Color32::from_rgb(r, g, b);
            if paused {
                painter.circle_stroke(dot, 4.0, Stroke::new(1.5, color));
            } else {
                painter.circle_filled(dot, 4.5, color);
            }
            let secs = elapsed.as_secs();
            painter.text(
                pos2(dot.x + 12.0, full.center().y),
                egui::Align2::LEFT_CENTER,
                format!("{:02}:{:02}:{:03}", secs / 60, secs % 60, elapsed.subsec_millis()),
                FontId::monospace(14.0),
                p.text_strong,
            );

            let buttons: [(Action, &str, Icon); 4] = [
                (Action::Stop, "Stop", Icon::Stop),
                (
                    Action::TogglePause,
                    if paused { "Resume" } else { "Pause" },
                    if paused { Icon::Resume } else { Icon::Pause },
                ),
                (Action::Restart, "Restart", Icon::Restart),
                (Action::Abort, "Abort", Icon::Abort),
            ];
            let mut x = full.left() + 112.0;
            let width = (full.right() - x) / buttons.len() as f32;
            for (a, label, icon) in buttons {
                let rect = Rect::from_min_size(pos2(x, full.top()), vec2(width, full.height()));
                painter.vline(x, full.y_range(), Stroke::new(1.0, p.stroke));
                let response = ui.interact(rect, ui.id().with(label), Sense::click());
                if response.hovered() {
                    painter.rect_filled(rect.shrink(1.0), 0.0, p.wash(14));
                }
                let galley = painter.layout_no_wrap(
                    label.to_string(),
                    FontId::proportional(13.0),
                    p.icon,
                );
                let content = 14.0 + 7.0 + galley.size().x;
                let icon_at = pos2(rect.center().x - content / 2.0 + 7.0, rect.center().y);
                let ink = if p.dark { ICON_DARK } else { ICON_LIGHT };
                draw_icon(&painter, icon, icon_at, ink);
                painter.galley(
                    pos2(icon_at.x + 14.0, rect.center().y - galley.size().y / 2.0),
                    galley,
                    Color32::WHITE,
                );
                if response
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    action = Some(a);
                }
                x += width;
            }
        });
    action
}

#[derive(Clone, Copy)]
enum Icon {
    Stop,
    Pause,
    Resume,
    Restart,
    Abort,
}

/// A 14-point icon centred on `c`.
fn draw_icon(painter: &egui::Painter, icon: Icon, c: Pos2, color: Color32) {
    let stroke = Stroke::new(1.5, color);
    match icon {
        Icon::Stop => {
            painter.rect_stroke(
                Rect::from_center_size(c, Vec2::splat(11.0)),
                1.5,
                stroke,
                egui::StrokeKind::Middle,
            );
        }
        Icon::Pause => {
            for dx in [-3.0, 3.0] {
                painter.rect_stroke(
                    Rect::from_center_size(c + vec2(dx, 0.0), vec2(3.0, 12.0)),
                    1.0,
                    stroke,
                    egui::StrokeKind::Middle,
                );
            }
        }
        Icon::Resume => {
            painter.add(egui::Shape::convex_polygon(
                vec![
                    c + vec2(-4.0, -6.0),
                    c + vec2(6.0, 0.0),
                    c + vec2(-4.0, 6.0),
                ],
                Color32::TRANSPARENT,
                stroke,
            ));
        }
        Icon::Restart => {
            // Most of a circle, with an arrowhead at its open end.
            let r = 5.5;
            let points: Vec<Pos2> = (0..=20)
                .map(|i| {
                    let a = -std::f32::consts::FRAC_PI_2 + 0.5 + i as f32 / 20.0 * 5.3;
                    c + vec2(a.cos(), a.sin()) * r
                })
                .collect();
            let tip = *points.last().unwrap();
            painter.add(egui::Shape::line(points, stroke));
            painter.line_segment([tip, tip + vec2(0.0, 4.0)], stroke);
            painter.line_segment([tip, tip + vec2(4.0, 0.5)], stroke);
        }
        Icon::Abort => {
            let d = 4.5;
            painter.line_segment([c + vec2(-d, -d), c + vec2(d, d)], stroke);
            painter.line_segment([c + vec2(-d, d), c + vec2(d, -d)], stroke);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::capture::Rect;

    #[test]
    fn bar_goes_under_the_region_or_above_it() {
        let monitors = [Rect { x: 0, y: 0, w: 2560, h: 1440 }, Rect { x: 2560, y: 0, w: 1920, h: 1080 }];
        let r = |x, y, w, h| Rect { x, y, w, h };
        // Below, centred.
        assert_eq!(super::place_bar(r(100, 100, 600, 400), (470, 40), 8, &monitors), (165, 508));
        // No room below: above.
        assert_eq!(super::place_bar(r(100, 1000, 600, 420), (470, 40), 8, &monitors), (165, 952));
        // The whole monitor: inside its bottom edge.
        assert_eq!(super::place_bar(r(0, 0, 2560, 1440), (470, 40), 8, &monitors), (1045, 1392));
        // Kept on the region's monitor.
        assert_eq!(super::place_bar(r(2600, 100, 100, 100), (470, 40), 8, &monitors), (2560, 208));
    }

    /// Renders the bar to `target/recording-bar-preview.png`:
    /// `cargo test recording_bar_preview -- --ignored`.
    #[test]
    #[ignore]
    fn recording_bar_preview() {
        for (name, paused, color, dark) in [
            ("recording-bar-preview", false, super::GREEN, true),
            ("recording-bar-paused-preview", true, super::AMBER, true),
            ("recording-bar-light-preview", false, super::GREEN, false),
        ] {
            crate::preview::render(
                name,
                [
                    (super::BAR_WIDTH * 1.5) as u32,
                    (super::BAR_HEIGHT * 1.5) as u32,
                ],
                1.5,
                |root| {
                    crate::preview::themed(root, dark);
                    super::bar_contents(root, Duration::from_millis(754_317), paused, color);
                },
            );
        }
    }
}
