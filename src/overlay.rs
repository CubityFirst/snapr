use std::sync::Arc;
use std::time::{Duration, Instant};

use egui_wgpu::wgpu;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::monitor::MonitorHandle;
use winit::window::{Window, WindowLevel};

use crate::annotate::{Highlight, Style, Tool};
use crate::capture::{PlacedShot, Rect, Shot};
use crate::gpu::Gpu;
use crate::toolbar;

const BORDER_COLOR: [f32; 4] = [0x3d as f32 / 255.0, 0x9b as f32 / 255.0, 1.0, 1.0];
const BORDER_WIDTH: f32 = 2.0;
/// Brightness of the area outside the selection.
const DIM: f32 = 0.43;

/// What to draw in one frame. Coordinates are global pixels.
pub struct Frame {
    pub selection: Option<Rect>,
    /// Crosshair guides through this point.
    pub crosshair: Option<(f64, f64)>,
    /// Where the annotation layer covers the screen, if anything's drawn.
    pub layer: Option<Rect>,
    /// Dim everything when there's no selection (while choosing a region).
    pub dim_all: bool,
    /// Show the selection's size label.
    pub decorate: bool,
    pub show_toolbar: bool,
    pub tool: Tool,
    pub style: Style,
    pub can_undo: bool,
    pub can_redo: bool,
    pub cursor: egui::CursorIcon,
    /// Lines' or arrows' middle nodes (global pixels), shown as handles to
    /// curve them.
    pub arrow_nodes: Vec<(f64, f64)>,
    /// The annotation under the cursor (global pixels), outlined because it
    /// can be right-clicked away.
    pub hovered: Option<Highlight>,
    /// What to do, shown at the top of this monitor.
    pub hint: Option<&'static str>,
    /// The card beside the cursor (global pixels): the colour picker's
    /// magnifier, or the crosshair info chosen in Settings.
    pub cursor_info: Option<((f64, f64), toolbar::CursorInfo)>,
}

/// The egui context drawing the toolbar and decorations on this overlay.
struct Ui {
    state: egui_winit::State,
    renderer: egui_wgpu::Renderer,
    repaint_at: Option<Instant>,
    /// The ring around a hovered annotation, uploaded, and its id.
    ring: Option<(u64, egui::TextureHandle)>,
}

/// A borderless, always-on-top window covering one monitor and showing its
/// frozen frame.
pub struct Overlay {
    pub window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    frame_view: wgpu::TextureView,
    /// Format for textures shown on this overlay (sRGB-ness matches the surface).
    pub texture_format: wgpu::TextureFormat,
    bind_group: wgpu::BindGroup,
    uniforms: wgpu::Buffer,
    ui: Ui,
    /// The toolbar's area in window pixels, if shown.
    toolbar: Option<egui::Rect>,
    shot: Shot,
    monitor_pos: PhysicalPosition<i32>,
    /// When the last frame was presented, for custom frame-rate pacing.
    pub last_frame: Option<Instant>,
    /// A redraw postponed to respect the frame-rate limit.
    pub due: Option<Instant>,
    /// A redraw was asked for and hasn't happened yet.
    pub redraw_pending: bool,
}

impl Overlay {
    pub fn new(
        event_loop: &ActiveEventLoop,
        gpu: &Gpu,
        shot: Shot,
        monitors: &[MonitorHandle],
        vsync: bool,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let monitor = monitors
            .iter()
            .min_by_key(|m| {
                let p = m.position();
                (p.x - shot.pos.0).abs() + (p.y - shot.pos.1).abs()
            })
            .ok_or("no monitors")?
            .clone();
        let pos = monitor.position();
        let (w, h) = shot.image.dimensions();

        #[allow(unused_mut)]
        let mut attrs = Window::default_attributes()
            .with_title("snapr")
            .with_decorations(false)
            .with_resizable(false)
            .with_visible(false)
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_position(pos)
            .with_inner_size(PhysicalSize::new(w, h));
        // macOS borderless fullscreen switches Spaces with an animation, so we
        // just cover the monitor with a top-level window there instead.
        #[cfg(not(target_os = "macos"))]
        {
            attrs =
                attrs.with_fullscreen(Some(winit::window::Fullscreen::Borderless(Some(monitor))));
        }
        #[cfg(windows)]
        {
            use winit::platform::windows::WindowAttributesExtWindows;
            attrs = attrs.with_skip_taskbar(true);
        }

        let window = Arc::new(event_loop.create_window(attrs)?);
        #[cfg(windows)]
        suppress_alt_menu(&window);
        window.set_cursor(winit::window::CursorIcon::Crosshair);

        let surface = gpu.instance.create_surface(window.clone())?;
        let caps = surface.get_capabilities(&gpu.adapter);
        // Prefer a non-sRGB surface so pixel values pass through untouched.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .or_else(|| caps.formats.first().copied())
            .ok_or("surface has no supported formats")?;
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: if vsync {
                wgpu::PresentMode::AutoVsync
            } else {
                wgpu::PresentMode::AutoNoVsync
            },
            desired_maximum_frame_latency: 1,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            ..surface
                .get_default_config(&gpu.adapter, size.width.max(1), size.height.max(1))
                .ok_or("surface unsupported")?
        };
        surface.configure(&gpu.device, &config);

        // Upload the frozen frame. Match the surface's sRGB-ness so values are
        // either untouched or decoded and re-encoded identically.
        let texture_format = if format.is_srgb() {
            wgpu::TextureFormat::Rgba8UnormSrgb
        } else {
            wgpu::TextureFormat::Rgba8Unorm
        };
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("frozen frame"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: texture_format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        gpu.queue.write_texture(
            texture.as_image_copy(),
            shot.image.as_raw(),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * w),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        let uniforms = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("overlay params"),
            size: 80,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let frame_view = texture.create_view(&Default::default());
        let bind_group = bind_group(gpu, &frame_view, &uniforms, &gpu.empty_layer);

        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        // The toolbar's tooltips name its tools and their keys: show them
        // soon after the pointer stops (egui waits half a second).
        ctx.all_styles_mut(|s| s.interaction.tooltip_delay = 0.1);
        let state = egui_winit::State::new(
            ctx,
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            None,
            Some(gpu.device.limits().max_texture_dimension_2d as usize),
        );
        let renderer = egui_wgpu::Renderer::new(&gpu.device, format, Default::default());

        Ok(Self {
            window,
            surface,
            config,
            pipeline: gpu.overlay_pipeline(format),
            frame_view,
            texture_format,
            bind_group,
            uniforms,
            ui: Ui {
                state,
                renderer,
                repaint_at: None,
                ring: None,
            },
            toolbar: None,
            shot,
            monitor_pos: pos,
            last_frame: None,
            due: None,
            redraw_pending: false,
        })
    }

    /// Points the shader at a new annotation layer texture.
    pub fn set_layer(&mut self, gpu: &Gpu, layer: &wgpu::TextureView) {
        self.bind_group = bind_group(gpu, &self.frame_view, &self.uniforms, layer);
    }

    /// Paints the first frame before showing the window, so there's no flash.
    /// The session focuses one overlay once they're all shown.
    pub fn show(&mut self, gpu: &Gpu, frame: &Frame) {
        self.render(gpu, frame);
        self.window.set_visible(true);
    }

    fn origin(&self) -> PhysicalPosition<i32> {
        self.window.inner_position().unwrap_or(self.monitor_pos)
    }

    pub fn global_rect(&self) -> Rect {
        let o = self.origin();
        let s = self.window.inner_size();
        Rect {
            x: o.x,
            y: o.y,
            w: s.width,
            h: s.height,
        }
    }

    pub fn to_global(&self, p: PhysicalPosition<f64>) -> (f64, f64) {
        let o = self.origin();
        (o.x as f64 + p.x, o.y as f64 + p.y)
    }

    pub fn placed_shot(&self) -> PlacedShot<'_> {
        PlacedShot {
            image: &self.shot.image,
            rect: self.global_rect(),
        }
    }

    /// Whether a global point is over this overlay's toolbar.
    pub fn over_toolbar(&self, p: (f64, f64)) -> bool {
        let Some(r) = self.toolbar else { return false };
        let o = self.origin();
        r.contains(egui::pos2(
            (p.0 - o.x as f64) as f32,
            (p.1 - o.y as f64) as f32,
        ))
    }

    /// Asks for a redraw. The session also draws pending overlays whenever
    /// it draws another one, since Windows can leave them waiting.
    pub fn request_redraw(&mut self) {
        self.redraw_pending = true;
        self.window.request_redraw();
    }

    /// Feeds a window event to egui; requests a redraw if egui wants one.
    pub fn egui_event(&mut self, event: &WindowEvent) {
        let repaint = self.ui.state.on_window_event(&self.window, event).repaint;
        // egui asks to repaint in answer to a redraw, too. Asking again while
        // Windows is painting this overlay makes it paint it over and over,
        // and the other monitors' overlays never get their turn to redraw.
        if repaint && !matches!(event, WindowEvent::RedrawRequested) {
            self.request_redraw();
        }
    }

    /// When egui wants to animate next (e.g. a tooltip appearing).
    pub fn repaint_at(&self) -> Option<Instant> {
        self.ui.repaint_at
    }

    fn write_params(&self, gpu: &Gpu, frame: &Frame) {
        let win = self.global_rect();
        let (iw, ih) = self.shot.image.dimensions();
        let local = |r: Rect| {
            let (x, y) = ((r.x - win.x) as f32, (r.y - win.y) as f32);
            [x, y, x + r.w as f32, y + r.h as f32]
        };
        let mut flags = 0u32;
        let mut sel = [0.0f32; 4];
        if let Some(s) = frame.selection {
            flags |= 1;
            sel = local(s);
        }
        let mut layer = [0.0f32; 4];
        if let Some(l) = frame.layer {
            flags |= 4;
            layer = local(l);
        }
        if frame.dim_all {
            flags |= 8;
        }
        let mut cur = [0.0f32; 2];
        // Crosshair guides only on the monitor the cursor is on.
        if let Some(c) = frame.crosshair.filter(|&c| win.contains(c)) {
            flags |= 2;
            cur = [
                (c.0.floor() as i32 - win.x) as f32,
                (c.1.floor() as i32 - win.y) as f32,
            ];
        }
        let words: [u32; 20] = [
            sel[0].to_bits(),
            sel[1].to_bits(),
            sel[2].to_bits(),
            sel[3].to_bits(),
            BORDER_COLOR[0].to_bits(),
            BORDER_COLOR[1].to_bits(),
            BORDER_COLOR[2].to_bits(),
            BORDER_COLOR[3].to_bits(),
            layer[0].to_bits(),
            layer[1].to_bits(),
            layer[2].to_bits(),
            layer[3].to_bits(),
            cur[0].to_bits(),
            cur[1].to_bits(),
            (iw as f32 / win.w.max(1) as f32).to_bits(),
            (ih as f32 / win.h.max(1) as f32).to_bits(),
            DIM.to_bits(),
            BORDER_WIDTH.to_bits(),
            flags,
            0,
        ];
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_ne_bytes()).collect();
        gpu.queue.write_buffer(&self.uniforms, 0, &bytes);
    }

    /// Runs the egui part of the frame: toolbar and selection decorations.
    fn run_ui(&mut self, frame: &Frame) -> (Vec<toolbar::Action>, egui::FullOutput) {
        let win = self.global_rect();
        let ppp = self.window.scale_factor() as f32;
        // Decorate only the monitor holding the selection's top-left corner.
        let selection = frame
            .selection
            .filter(|s| frame.decorate && win.contains((s.x as f64, s.y as f64)))
            .map(|s| {
                let min = egui::pos2((s.x - win.x) as f32 / ppp, (s.y - win.y) as f32 / ppp);
                (
                    egui::Rect::from_min_size(min, egui::vec2(s.w as f32 / ppp, s.h as f32 / ppp)),
                    [s.w, s.h],
                )
            });
        let view = toolbar::View {
            tool: frame.tool,
            style: frame.style,
            can_undo: frame.can_undo,
            can_redo: frame.can_redo,
            show_toolbar: frame.show_toolbar,
            selection,
            cursor: frame.cursor,
            arrow_nodes: frame
                .arrow_nodes
                .iter()
                .filter(|&&n| win.contains(n))
                .map(|n| {
                    egui::pos2(
                        (n.0 - win.x as f64) as f32 / ppp,
                        (n.1 - win.y as f64) as f32 / ppp,
                    )
                })
                .collect(),
            hovered: frame.hovered.as_ref().map(|h| {
                let points = |x: i32, y: i32, w: u32, h: u32| {
                    let min = egui::pos2((x - win.x) as f32 / ppp, (y - win.y) as f32 / ppp);
                    egui::Rect::from_min_size(min, egui::vec2(w as f32 / ppp, h as f32 / ppp))
                };
                match h {
                    Highlight::Box(r) => toolbar::Hover::Box(points(r.x, r.y, r.w, r.h)),
                    Highlight::Ring { id, at, image } => {
                        let texture = match &self.ui.ring {
                            Some((uploaded, t)) if uploaded == id => t.clone(),
                            _ => {
                                let size = [image.width() as usize, image.height() as usize];
                                let t = self.ui.state.egui_ctx().load_texture(
                                    "ring",
                                    egui::ColorImage::from_rgba_premultiplied(size, image.data()),
                                    egui::TextureOptions::LINEAR,
                                );
                                self.ui.ring = Some((*id, t.clone()));
                                t
                            }
                        };
                        let rect = points(at.0, at.1, image.width(), image.height());
                        toolbar::Hover::Ring(texture.id(), rect)
                    }
                }
            }),
            hint: frame.hint,
            cursor_info: frame.cursor_info.as_ref().map(|(c, info)| {
                let at = egui::pos2(
                    (c.0 - win.x as f64) as f32 / ppp,
                    (c.1 - win.y as f64) as f32 / ppp,
                );
                (at, info.clone())
            }),
        };
        let input = self.ui.state.take_egui_input(&self.window);
        let ctx = self.ui.state.egui_ctx().clone();
        let (actions, toolbar, output) = toolbar::run(&ctx, input, &view);
        self.toolbar = toolbar.map(|r| r * ppp);
        (actions, output)
    }

    pub fn render(&mut self, gpu: &Gpu, frame: &Frame) -> Vec<toolbar::Action> {
        self.redraw_pending = false;
        let size = self.window.inner_size();
        if size.width == 0 || size.height == 0 {
            return Vec::new();
        }
        if (size.width, size.height) != (self.config.width, self.config.height) {
            self.config.width = size.width;
            self.config.height = size.height;
            self.surface.configure(&gpu.device, &self.config);
        }
        self.write_params(gpu, frame);

        let (actions, mut output) = self.run_ui(frame);
        self.ui
            .state
            .handle_platform_output(&self.window, std::mem::take(&mut output.platform_output));
        let ctx = self.ui.state.egui_ctx().clone();
        let primitives =
            ctx.tessellate(std::mem::take(&mut output.shapes), output.pixels_per_point);
        // egui insists every texture update is handled, even for skipped frames.
        let mut textures = std::mem::take(&mut output.textures_delta);
        for (id, deltas) in textures.set.drain() {
            for d in &deltas {
                self.ui
                    .renderer
                    .update_texture(&gpu.device, &gpu.queue, id, d);
            }
        }
        let freed: Vec<_> = textures.free.drain().collect();
        self.ui.repaint_at = output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map(|v| v.repaint_delay)
            .filter(|d| *d < Duration::from_secs(3600))
            .map(|d| Instant::now() + d);

        let surface_texture = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f)
            | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&gpu.device, &self.config);
                self.request_redraw();
                self.free_textures(&freed);
                return actions;
            }
            _ => {
                self.free_textures(&freed);
                return actions;
            }
        };
        let view = surface_texture.texture.create_view(&Default::default());
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [size.width, size.height],
            pixels_per_point: output.pixels_per_point,
        };
        let ui_commands = self.ui.renderer.update_buffers(
            &gpu.device,
            &gpu.queue,
            &mut encoder,
            &primitives,
            &screen,
        );
        {
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("overlay"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    ..Default::default()
                })
                .forget_lifetime();
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.draw(0..3, 0..1);
            self.ui.renderer.render(&mut pass, &primitives, &screen);
        }
        gpu.queue
            .submit(ui_commands.into_iter().chain([encoder.finish()]));
        self.window.pre_present_notify();
        gpu.queue.present(surface_texture);
        self.free_textures(&freed);
        self.last_frame = Some(Instant::now());
        actions
    }

    fn free_textures(&mut self, ids: &[egui::TextureId]) {
        for id in ids {
            self.ui.renderer.free_texture(id);
        }
    }
}

fn bind_group(
    gpu: &Gpu,
    frame: &wgpu::TextureView,
    uniforms: &wgpu::Buffer,
    layer: &wgpu::TextureView,
) -> wgpu::BindGroup {
    gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("overlay"),
        layout: &gpu.bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(frame),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: uniforms.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(layer),
            },
        ],
    })
}

/// Stops a lone Alt press from putting the overlay into the Windows menu
/// mode, which captures the mouse (no hover, no cursor changes) until the
/// next click. winit taps Alt itself when it takes focus, and the hotkey's
/// own Alt can be released over the overlay.
#[cfg(windows)]
fn suppress_alt_menu(window: &Window) {
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
    use windows_sys::Win32::UI::WindowsAndMessaging::{SC_KEYMENU, WM_SYSCOMMAND};
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    unsafe extern "system" fn proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _id: usize,
        _data: usize,
    ) -> LRESULT {
        if msg == WM_SYSCOMMAND && (wparam & 0xfff0) == SC_KEYMENU as usize {
            return 0;
        }
        unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
    }

    if let Ok(handle) = window.window_handle()
        && let RawWindowHandle::Win32(h) = handle.as_raw()
    {
        unsafe { SetWindowSubclass(h.hwnd.get() as HWND, Some(proc), 1, 0) };
    }
}

/// Checks that a recording started the way snapr does it, right after the
/// capture overlay closes, doesn't begin with the overlay's frozen screen.
/// Shows a red overlay on the primary monitor and records its middle:
/// `cargo test recording_after_overlay -- --ignored --nocapture`.
#[cfg(all(test, windows))]
mod close_timing {
    use super::*;
    use winit::application::ApplicationHandler;
    use winit::event_loop::{ControlFlow, EventLoop};
    use winit::platform::windows::EventLoopBuilderExtWindows;
    use winit::window::WindowId;

    fn red_overlay(event_loop: &ActiveEventLoop, gpu: &Gpu, m: &xcap::Monitor) -> Overlay {
        let (w, h) = (m.width().unwrap(), m.height().unwrap());
        let shot = Shot {
            image: image::RgbaImage::from_pixel(w, h, image::Rgba([255, 0, 0, 255])),
            pos: (m.x().unwrap(), m.y().unwrap()),
        };
        let monitors: Vec<_> = event_loop.available_monitors().collect();
        let mut overlay = Overlay::new(event_loop, gpu, shot, &monitors, true).unwrap();
        let frame = Frame {
            selection: None,
            crosshair: None,
            layer: None,
            dim_all: false,
            decorate: false,
            show_toolbar: false,
            tool: Tool::Select,
            style: Style::default(),
            can_undo: false,
            can_redo: false,
            cursor: egui::CursorIcon::Default,
            arrow_nodes: Vec::new(),
            hovered: None,
            hint: None,
            cursor_info: None,
        };
        overlay.show(gpu, &frame);
        overlay
    }

    /// Records the middle of the primary monitor right after a red overlay
    /// closes: started within the same callback (`blocking`), or from a
    /// later event loop pass, the way snapr does it.
    struct RecordApp {
        gpu: Option<Gpu>,
        monitor: Option<xcap::Monitor>,
        blocking: bool,
        overlay: Option<(Overlay, Instant)>,
        start_at: Option<Instant>,
        recording: Option<(crate::record::Recording, Instant)>,
        out: std::path::PathBuf,
        done: Option<std::sync::mpsc::Receiver<Result<(), String>>>,
        started: Instant,
    }

    impl RecordApp {
        fn start(&mut self) {
            let m = self.monitor.as_ref().unwrap();
            let rect = Rect {
                x: m.x().unwrap() + m.width().unwrap() as i32 / 2 - 160,
                y: m.y().unwrap() + m.height().unwrap() as i32 / 2 - 120,
                w: 320,
                h: 240,
            };
            let (tx, rx) = std::sync::mpsc::channel();
            self.done = Some(rx);
            let done: crate::record::Done = Box::new(move |r| {
                let _ = tx.send(r.map(|_| ()));
            });
            let rec = crate::record::Recording::start(
                rect,
                30,
                &Default::default(),
                false,
                &[],
                false,
                false,
                false,
                self.out.clone(),
                done,
            )
            .unwrap();
            self.recording = Some((rec, Instant::now()));
        }
    }

    impl ApplicationHandler for RecordApp {
        fn resumed(&mut self, _: &ActiveEventLoop) {}

        fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}

        fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
            event_loop.set_control_flow(ControlFlow::Poll);
            if self.started.elapsed() > Duration::from_secs(20) {
                self.overlay = None;
                return event_loop.exit();
            }
            if self.gpu.is_none() {
                self.gpu = Some(Gpu::new().unwrap());
                self.monitor = xcap::Monitor::all()
                    .unwrap()
                    .into_iter()
                    .find(|m| m.is_primary().unwrap_or(false));
                let overlay =
                    red_overlay(event_loop, self.gpu.as_ref().unwrap(), self.monitor.as_ref().unwrap());
                self.overlay = Some((overlay, Instant::now()));
                return;
            }
            if let Some((_, at)) = &self.overlay
                && at.elapsed() > Duration::from_millis(600)
            {
                self.overlay = None; // closes it
                if self.blocking {
                    self.start();
                } else {
                    self.start_at = Some(Instant::now() + Duration::from_millis(50));
                }
                return;
            }
            if self.start_at.is_some_and(|t| t <= Instant::now()) {
                self.start_at = None;
                self.start();
            }
            if let Some((rec, at)) = &mut self.recording
                && at.elapsed() > Duration::from_millis(1000)
            {
                rec.stop();
                self.recording = None;
            }
            if self.recording.is_none()
                && let Some(rx) = &self.done
                && let Ok(result) = rx.try_recv()
            {
                result.unwrap();
                event_loop.exit();
            }
        }
    }

    /// How red each of the first frames of a video is.
    fn first_frames_redness(video: &std::path::Path) -> Vec<f32> {
        let info = crate::decode::probe(video, "").unwrap();
        let mut redness = Vec::new();
        crate::decode::pictures(video, "", info, 0.0, (8, 8), &mut |p| {
            let pixels = p.image.pixels();
            let n = pixels.len() as f32;
            redness.push(pixels.map(|px| px[0] as f32 - px[1] as f32).sum::<f32>() / n);
            redness.len() < 8
        })
        .unwrap();
        redness
    }

    /// `cargo test recording_after_overlay -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn recording_after_overlay() {
        let mut event_loop = EventLoop::builder().with_any_thread(true).build().unwrap();
        for blocking in [true, false] {
            let out = std::env::temp_dir().join(format!("snapr-overlay-test-{blocking}.mp4"));
            let mut app = RecordApp {
                gpu: None,
                monitor: None,
                blocking,
                overlay: None,
                start_at: None,
                recording: None,
                out: out.clone(),
                done: None,
                started: Instant::now(),
            };
            use winit::platform::run_on_demand::EventLoopExtRunOnDemand;
            event_loop.run_app_on_demand(&mut app).unwrap();
            let redness = first_frames_redness(&out);
            let red: Vec<String> = redness.iter().map(|r| format!("{r:.0}")).collect();
            if !blocking {
                assert!(
                    redness.iter().all(|r| *r < 20.0),
                    "the recording starts with the overlay: {red:?}"
                );
            }
            let _ = std::fs::remove_file(&out);
            println!(
                "{}: redness of the first frames: {}",
                if blocking { "started in the same callback" } else { "started 50 ms later" },
                red.join(" ")
            );
        }
    }
}
