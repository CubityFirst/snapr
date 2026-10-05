//! What's on screen while recording: a dashed border just outside the region
//! (clicks go through it) and a bar under it with the elapsed time and
//! Stop / Pause / Restart / Abort. None of it shows up in the recording.

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

/// Border thickness and dash length, in physical pixels.
const BORDER: u32 = 2;
const DASH: u32 = 6;
const RED: [u8; 3] = [0xe5, 0x48, 0x4d];
const AMBER: [u8; 3] = [0xf5, 0xa5, 0x24];
const GAP_COLOR: [u8; 3] = [0x10, 0x10, 0x12];
const ICON: Color32 = Color32::from_rgb(0x4c, 0x9e, 0xff);
const PANEL: Color32 = Color32::from_rgb(32, 33, 37);
const PANEL_STROKE: Color32 = Color32::from_rgb(58, 60, 66);
const BAR_HEIGHT: f32 = 40.0;
/// Gap between the border and the bar, in points.
const BAR_GAP: f64 = 6.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Stop,
    TogglePause,
    Restart,
    Abort,
}

pub struct RecordingUi {
    edges: Vec<Edge>,
    bar: Bar,
    started: Instant,
    paused_at: Option<Instant>,
    paused_for: Duration,
}

impl RecordingUi {
    /// Shows the border and bar around `rect` (global physical pixels).
    pub fn open(
        event_loop: &ActiveEventLoop,
        gpu: &Gpu,
        rect: capture::Rect,
    ) -> Result<Self, String> {
        let b = BORDER as i32;
        let (w, h) = (rect.w + 2 * BORDER, rect.h);
        let sides = [
            (rect.x - b, rect.y - b, w, BORDER, true),
            (rect.x - b, rect.bottom(), w, BORDER, true),
            (rect.x - b, rect.y, BORDER, h, false),
            (rect.right(), rect.y, BORDER, h, false),
        ];
        let mut edges = Vec::new();
        for (x, y, w, h, horizontal) in sides {
            edges.push(Edge::open(event_loop, gpu, (x, y), (w, h), horizontal)?);
        }
        let bar = Bar::open(event_loop, gpu, rect)?;
        let mut ui = Self {
            edges,
            bar,
            started: Instant::now(),
            paused_at: None,
            paused_for: Duration::ZERO,
        };
        ui.redraw_edges(gpu);
        ui.paint_bar();
        for e in &ui.edges {
            e.window.set_visible(true);
        }
        ui.bar.window.set_visible(true);
        Ok(ui)
    }

    pub fn owns(&self, id: WindowId) -> bool {
        self.bar.window.id() == id || self.edges.iter().any(|e| e.window.id() == id)
    }

    pub fn paused(&self) -> bool {
        self.paused_at.is_some()
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
        self.bar.window.request_redraw();
    }

    /// Recorded time so far, not counting pauses.
    fn elapsed(&self) -> Duration {
        let end = self.paused_at.unwrap_or_else(Instant::now);
        end.duration_since(self.started)
            .saturating_sub(self.paused_for)
    }

    pub fn on_event(&mut self, gpu: &Gpu, id: WindowId, event: &WindowEvent) -> Option<Action> {
        if let Some(edge) = self.edges.iter_mut().find(|e| e.window.id() == id) {
            if matches!(
                event,
                WindowEvent::RedrawRequested | WindowEvent::Resized(_)
            ) {
                let color = if self.paused_at.is_some() { AMBER } else { RED };
                edge.draw(gpu, color);
            }
            return None;
        }
        match event {
            WindowEvent::RedrawRequested => return self.paint_bar(),
            WindowEvent::Resized(size) => {
                if let (Some(w), Some(h)) = (
                    std::num::NonZeroU32::new(size.width),
                    std::num::NonZeroU32::new(size.height),
                ) {
                    self.bar
                        .painter
                        .on_window_resized(egui::ViewportId::ROOT, w, h);
                }
            }
            _ => {}
        }
        if self
            .bar
            .state
            .on_window_event(&self.bar.window, event)
            .repaint
        {
            self.bar.window.request_redraw();
        }
        None
    }

    /// When the timer next needs repainting.
    pub fn repaint_at(&self) -> Option<Instant> {
        self.bar.repaint_at
    }

    pub fn tick(&mut self, now: Instant) {
        if self.bar.repaint_at.is_some_and(|t| t <= now) {
            self.bar.repaint_at = None;
            self.bar.window.request_redraw();
        }
    }

    fn redraw_edges(&mut self, gpu: &Gpu) {
        let color = if self.paused_at.is_some() { AMBER } else { RED };
        for e in &mut self.edges {
            e.draw(gpu, color);
        }
    }

    fn paint_bar(&mut self) -> Option<Action> {
        let elapsed = self.elapsed();
        let paused = self.paused_at.is_some();
        let bar = &mut self.bar;
        let input = bar.state.take_egui_input(&bar.window);
        let ctx = bar.state.egui_ctx().clone();
        let mut action = None;
        let mut output = ctx.run_ui(input, |ui| action = bar_contents(ui, elapsed, paused));
        bar.state
            .handle_platform_output(&bar.window, std::mem::take(&mut output.platform_output));
        let primitives =
            ctx.tessellate(std::mem::take(&mut output.shapes), output.pixels_per_point);
        bar.painter.paint_and_update_textures(
            egui::ViewportId::ROOT,
            output.pixels_per_point,
            [0.0, 0.0, 0.0, 1.0],
            &primitives,
            &mut output.textures_delta,
            Vec::new(),
            &bar.window,
        );
        output.textures_delta.clear();
        // Tick the timer over at the next whole second; hover effects
        // repaint straight away.
        let to_next_second =
            Duration::from_secs(1) - Duration::from_nanos(elapsed.subsec_nanos() as u64);
        let delay = output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map_or(to_next_second, |v| v.repaint_delay.min(to_next_second));
        bar.repaint_at = (!paused || delay < to_next_second).then(|| Instant::now() + delay);
        action
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
        pos: (i32, i32),
        size: (u32, u32),
        horizontal: bool,
    ) -> Result<Self, String> {
        let attrs = base_attributes()
            .with_position(PhysicalPosition::new(pos.0, pos.1))
            .with_inner_size(PhysicalSize::new(size.0.max(1), size.1.max(1)));
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
    window: Arc<Window>,
    state: egui_winit::State,
    painter: Painter,
    repaint_at: Option<Instant>,
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

        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        let config = egui_wgpu::WgpuConfiguration {
            wgpu_setup: gpu.egui_setup(),
            ..Default::default()
        };
        let mut painter =
            pollster::block_on(Painter::new(ctx.clone(), config, false, Default::default()));
        pollster::block_on(painter.set_window(egui::ViewportId::ROOT, Some(window.clone())))
            .map_err(|e| format!("couldn't set up the recording bar: {e}"))?;
        let state = egui_winit::State::new(
            ctx,
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            window.theme(),
            painter.max_texture_side(),
        );
        Ok(Self {
            window,
            state,
            painter,
            repaint_at: None,
        })
    }
}

const BAR_WIDTH: f32 = 470.0;

/// Centred under the region, or above it (or inside its bottom edge) when
/// there's no room below on that monitor.
fn bar_position(
    event_loop: &ActiveEventLoop,
    window: &Window,
    rect: capture::Rect,
) -> PhysicalPosition<i32> {
    let scale = window.scale_factor();
    let size = window.outer_size();
    let (w, h) = (size.width as i32, size.height as i32);
    let gap = (BAR_GAP * scale) as i32 + BORDER as i32;
    let monitor = event_loop
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
        .find(|m| {
            m.contains((
                rect.x as f64 + rect.w as f64 / 2.0,
                rect.y as f64 + rect.h as f64 / 2.0,
            ))
        });
    let x = rect.x + rect.w as i32 / 2 - w / 2;
    let below = rect.bottom() + gap;
    let above = rect.y - gap - h;
    let Some(m) = monitor else {
        return PhysicalPosition::new(x, below);
    };
    let y = if below + h <= m.bottom() {
        below
    } else if above >= m.y {
        above
    } else {
        rect.bottom() - gap - h
    };
    PhysicalPosition::new(x.clamp(m.x, (m.right() - w).max(m.x)), y)
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

fn bar_contents(ui: &mut egui::Ui, elapsed: Duration, paused: bool) -> Option<Action> {
    let mut action = None;
    egui::CentralPanel::default()
        .frame(
            egui::Frame::new()
                .fill(PANEL)
                .stroke(Stroke::new(1.0, PANEL_STROKE))
                .corner_radius(CornerRadius::same(6)),
        )
        .show(ui, |ui| {
            let full = ui.max_rect();
            let painter = ui.painter().clone();
            // Status: a dot (hollow while paused) and the time.
            let dot = pos2(full.left() + 16.0, full.center().y);
            if paused {
                painter.circle_stroke(
                    dot,
                    4.0,
                    Stroke::new(1.5, Color32::from_rgb(AMBER[0], AMBER[1], AMBER[2])),
                );
            } else {
                painter.circle_filled(dot, 4.5, Color32::from_rgb(RED[0], RED[1], RED[2]));
            }
            let secs = elapsed.as_secs();
            painter.text(
                pos2(dot.x + 12.0, full.center().y),
                egui::Align2::LEFT_CENTER,
                format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60),
                FontId::monospace(14.0),
                Color32::from_rgb(230, 231, 235),
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
                painter.vline(x, full.y_range(), Stroke::new(1.0, PANEL_STROKE));
                let response = ui.interact(rect, ui.id().with(label), Sense::click());
                if response.hovered() {
                    painter.rect_filled(rect.shrink(1.0), 0.0, Color32::from_white_alpha(14));
                }
                let galley = painter.layout_no_wrap(
                    label.to_string(),
                    FontId::proportional(13.0),
                    Color32::from_rgb(222, 223, 228),
                );
                let content = 14.0 + 7.0 + galley.size().x;
                let icon_at = pos2(rect.center().x - content / 2.0 + 7.0, rect.center().y);
                draw_icon(&painter, icon, icon_at);
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
fn draw_icon(painter: &egui::Painter, icon: Icon, c: Pos2) {
    let stroke = Stroke::new(1.5, ICON);
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

    /// Renders the bar to `target/recording-bar-preview.png`:
    /// `cargo test recording_bar_preview -- --ignored`.
    #[test]
    #[ignore]
    fn recording_bar_preview() {
        for (name, paused) in [
            ("recording-bar-preview", false),
            ("recording-bar-paused-preview", true),
        ] {
            crate::preview::render(
                name,
                [
                    (super::BAR_WIDTH * 1.5) as u32,
                    (super::BAR_HEIGHT * 1.5) as u32,
                ],
                1.5,
                |root| {
                    super::bar_contents(root, Duration::from_secs(54), paused);
                },
            );
        }
    }
}
