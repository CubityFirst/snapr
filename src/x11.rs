//! Windows that have to go in a particular place, as X11 windows, for
//! Wayland desktops: the corner preview, and the border and bar around a
//! recording. Wayland doesn't let a window choose where it goes (GNOME centres
//! new ones), and GNOME has no layer-shell, so they go through XWayland
//! instead, which does. winit allows one event loop per process, so these are
//! bare X11 windows: their mouse input is read on their own thread and sent to
//! the event loop, and they're drawn from there with egui's renderer.

use std::num::NonZeroU32;
use std::ptr::NonNull;
use std::sync::Arc;

use egui_wgpu::wgpu;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::MouseButton;
use winit::raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, XcbDisplayHandle, XcbWindowHandle,
};
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::shape::{ConnectionExt as _, SK, SO};
use x11rb::protocol::xproto::{
    AtomEnum, ChangeWindowAttributesAux, ClipOrdering, ConnectionExt as _, CreateGCAux,
    CreateWindowAux, EventMask, PropMode, Rectangle, WindowClass,
};
use x11rb::wrapper::ConnectionExt as _;
use x11rb::xcb_ffi::XCBConnection;

use crate::capture;
use crate::gpu::Gpu;
use crate::toast::Pointer;

/// Mouse input for an X11 window, from its thread.
#[derive(Debug)]
pub struct X11Event {
    pub window: u32,
    /// `None`: draw the window.
    pub pointer: Option<Pointer>,
}

/// Whether to use X11 windows: under Wayland, with XWayland.
pub fn wanted(event_loop: &winit::event_loop::ActiveEventLoop) -> bool {
    use winit::raw_window_handle::HasDisplayHandle;
    let wayland = event_loop
        .display_handle()
        .is_ok_and(|h| matches!(h.as_raw(), RawDisplayHandle::Wayland(_)));
    wayland && std::env::var_os("DISPLAY").is_some()
}

/// A connection to XWayland. Positions and sizes on it are in its pixels.
pub struct Screen {
    conn: Arc<XCBConnection>,
    screen_num: usize,
    /// Pixels per point: `Xft.dpi`, which the desktop sets for its scale.
    pub scale_factor: f64,
}

impl Screen {
    pub fn connect() -> Result<Self, String> {
        let (conn, screen_num) = XCBConnection::connect(None).map_err(|e| e.to_string())?;
        let scale_factor = x11rb::resource_manager::new_from_default(&conn)
            .ok()
            .and_then(|db| db.get_value::<f64>("Xft.dpi", "").ok().flatten())
            .filter(|dpi| *dpi > 0.0)
            .map_or(1.0, |dpi| dpi / 96.0);
        Ok(Self {
            conn: Arc::new(conn),
            screen_num,
            scale_factor,
        })
    }

    fn root(&self) -> u32 {
        self.conn.setup().roots[self.screen_num].root
    }

    /// The bottom-right corner of the primary screen, above any panel or dock.
    pub fn corner(&self) -> (i32, i32) {
        let screen = &self.conn.setup().roots[self.screen_num];
        let (mut right, mut bottom) = self
            .primary()
            .map(|m| (m.right(), m.bottom()))
            .unwrap_or((screen.width_in_pixels as i32, screen.height_in_pixels as i32));
        if let Some([x, y, w, h]) = self.work_area() {
            right = right.min(x + w);
            bottom = bottom.min(y + h);
        }
        (right, bottom)
    }

    fn primary(&self) -> Option<capture::Rect> {
        let conn = &self.conn;
        conn.randr_query_version(1, 3).ok()?.reply().ok()?;
        let output = conn
            .randr_get_output_primary(self.root())
            .ok()?
            .reply()
            .ok()?
            .output;
        let crtc = conn
            .randr_get_output_info(output, x11rb::CURRENT_TIME)
            .ok()?
            .reply()
            .ok()?
            .crtc;
        let c = conn
            .randr_get_crtc_info(crtc, x11rb::CURRENT_TIME)
            .ok()?
            .reply()
            .ok()?;
        (c.width > 0).then_some(capture::Rect {
            x: c.x as i32,
            y: c.y as i32,
            w: c.width as u32,
            h: c.height as u32,
        })
    }

    /// The monitors, as xrandr lists them.
    pub fn monitors(&self) -> Vec<capture::Rect> {
        let reply = (|| {
            self.conn.randr_query_version(1, 5).ok()?.reply().ok()?;
            self.conn
                .randr_get_monitors(self.root(), true)
                .ok()?
                .reply()
                .ok()
        })();
        reply.map_or_else(Vec::new, |r| {
            r.monitors
                .iter()
                .map(|m| capture::Rect {
                    x: m.x as i32,
                    y: m.y as i32,
                    w: m.width as u32,
                    h: m.height as u32,
                })
                .collect()
        })
    }

    /// The desktop's work area (all screens, minus panels and docks).
    fn work_area(&self) -> Option<[i32; 4]> {
        let reply = self
            .conn
            .get_property(
                false,
                self.root(),
                atom(&self.conn, b"_NET_WORKAREA")?,
                AtomEnum::CARDINAL,
                0,
                4,
            )
            .ok()?
            .reply()
            .ok()?;
        let v: Vec<i32> = reply.value32()?.map(|n| n as i32).collect();
        let area: [i32; 4] = v.try_into().ok()?;
        (area[2] > 0 && area[3] > 0).then_some(area)
    }

    /// An override-redirect window: the window manager leaves it where it's
    /// put, above other windows, without focusing it or listing it as an app.
    fn create_window(
        &self,
        rect: capture::Rect,
        background: u32,
        events: EventMask,
        kind: &[u8],
    ) -> Result<Owned, String> {
        let conn = &self.conn;
        let id = conn.generate_id().map_err(|e| e.to_string())?;
        let aux = CreateWindowAux::new()
            .override_redirect(1)
            .background_pixel(background)
            .event_mask(events);
        conn.create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            id,
            self.root(),
            rect.x as i16,
            rect.y as i16,
            rect.w.max(1) as u16,
            rect.h.max(1) as u16,
            0,
            WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &aux,
        )
        .map_err(|e| e.to_string())?;
        let window = Owned {
            conn: conn.clone(),
            id,
        };
        if let (Some(property), Some(kind)) = (
            atom(conn, b"_NET_WM_WINDOW_TYPE"),
            atom(conn, kind),
        ) {
            let _ = conn.change_property32(
                PropMode::REPLACE,
                id,
                property,
                AtomEnum::ATOM,
                &[kind],
            );
        }
        Ok(window)
    }
}

/// A window drawn by egui, with its mouse input sent to the event loop.
pub struct X11Window {
    // Dropped before `window`, which it draws to.
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    renderer: egui_wgpu::Renderer,
    device: wgpu::Device,
    queue: wgpu::Queue,
    window: Owned,
    pub scale_factor: f64,
}

/// Destroys the window when dropped, which also ends its thread.
struct Owned {
    conn: Arc<XCBConnection>,
    id: u32,
}

impl Drop for Owned {
    fn drop(&mut self) {
        let _ = self.conn.destroy_window(self.id);
        let _ = self.conn.flush();
    }
}

impl X11Window {
    /// A window at `rect` on `screen` (which it takes, as its input is read
    /// from the connection), shown at once, filled with `background` until
    /// it's first drawn. `kind` is its `_NET_WM_WINDOW_TYPE_…`. Its input
    /// goes to `send`.
    pub fn open(
        screen: Screen,
        gpu: &Gpu,
        rect: capture::Rect,
        background: u32,
        kind: &[u8],
        send: impl Fn(X11Event) + Send + 'static,
    ) -> Result<Self, String> {
        let events = EventMask::BUTTON_PRESS
            | EventMask::BUTTON_RELEASE
            | EventMask::POINTER_MOTION
            | EventMask::ENTER_WINDOW
            | EventMask::LEAVE_WINDOW
            | EventMask::EXPOSURE
            | EventMask::STRUCTURE_NOTIFY;
        let window = screen.create_window(rect, background, events, kind)?;
        let (conn, id) = (&screen.conn, window.id);

        let display = XcbDisplayHandle::new(
            NonNull::new(conn.get_raw_xcb_connection()),
            screen.screen_num as i32,
        );
        let target = wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: Some(RawDisplayHandle::Xcb(display)),
            raw_window_handle: RawWindowHandle::Xcb(XcbWindowHandle::new(
                NonZeroU32::new(id).ok_or("no window id")?,
            )),
        };
        // SAFETY: the connection and window outlive the surface: both are
        // held by `window`, which is dropped after it.
        let surface =
            unsafe { gpu.instance.create_surface_unsafe(target) }.map_err(|e| e.to_string())?;
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
            .ok_or("no 8-bit surface format")?;
        let config = wgpu::SurfaceConfiguration {
            format,
            present_mode: wgpu::PresentMode::Fifo,
            view_formats: vec![],
            ..surface
                .get_default_config(&gpu.adapter, rect.w.max(1), rect.h.max(1))
                .ok_or("the window's surface is unsupported")?
        };
        surface.configure(&gpu.device, &config);
        let renderer = egui_wgpu::Renderer::new(&gpu.device, format, Default::default());

        let events = conn.clone();
        std::thread::spawn(move || {
            while let Ok(event) = events.wait_for_event() {
                let pointer = match event {
                    Event::ButtonPress(e) => button(e.detail).map(Pointer::Pressed),
                    Event::ButtonRelease(e) => button(e.detail).map(Pointer::Released),
                    Event::MotionNotify(e) => Some(Pointer::Moved(PhysicalPosition::new(
                        e.event_x as f64,
                        e.event_y as f64,
                    ))),
                    Event::EnterNotify(_) => Some(Pointer::Entered),
                    Event::LeaveNotify(_) => Some(Pointer::Left),
                    Event::Expose(e) if e.count == 0 => None,
                    Event::DestroyNotify(_) => break,
                    _ => continue,
                };
                send(X11Event {
                    window: id,
                    pointer,
                });
            }
        });
        conn.map_window(id).map_err(|e| e.to_string())?;
        conn.flush().map_err(|e| e.to_string())?;

        Ok(Self {
            surface,
            config,
            renderer,
            device: gpu.device.clone(),
            queue: gpu.queue.clone(),
            window,
            scale_factor: screen.scale_factor,
        })
    }

    pub fn id(&self) -> u32 {
        self.window.id
    }

    /// egui's input for a frame: the window's size and scale.
    pub fn egui_input(&self) -> egui::RawInput {
        let ppp = self.scale_factor as f32;
        let size = egui::vec2(self.config.width as f32, self.config.height as f32) / ppp;
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            ..Default::default()
        };
        input
            .viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .native_pixels_per_point = Some(ppp);
        input
    }

    pub fn paint(&mut self, ctx: &egui::Context, mut output: egui::FullOutput) {
        let mut textures = std::mem::take(&mut output.textures_delta);
        for (id, deltas) in &textures.set {
            for delta in deltas {
                self.renderer
                    .update_texture(&self.device, &self.queue, *id, delta);
            }
        }
        let primitives =
            ctx.tessellate(std::mem::take(&mut output.shapes), output.pixels_per_point);
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f)
            | wgpu::CurrentSurfaceTexture::Suboptimal(f) => Some(f),
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                None
            }
            _ => None,
        };
        if let Some(frame) = frame {
            let [r, g, b, a] = crate::theme::backdrop(ctx).map(f64::from);
            let clear = wgpu::Color { r, g, b, a };
            let screen = egui_wgpu::ScreenDescriptor {
                size_in_pixels: [self.config.width, self.config.height],
                pixels_per_point: output.pixels_per_point,
            };
            let view = frame.texture.create_view(&Default::default());
            let mut encoder = self.device.create_command_encoder(&Default::default());
            let mut commands = self.renderer.update_buffers(
                &self.device,
                &self.queue,
                &mut encoder,
                &primitives,
                &screen,
            );
            {
                let mut pass = encoder
                    .begin_render_pass(&wgpu::RenderPassDescriptor {
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &view,
                            depth_slice: None,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(clear),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        ..Default::default()
                    })
                    .forget_lifetime();
                self.renderer.render(&mut pass, &primitives, &screen);
            }
            commands.push(encoder.finish());
            self.queue.submit(commands);
            self.queue.present(frame);
        }
        for id in &textures.free {
            self.renderer.free_texture(id);
        }
        textures.clear();
    }
}

/// A window filled with dashes, which clicks go through: one side of the
/// border around a recording. The X server repaints it, so it needs no
/// thread or GPU surface.
pub struct Dashes {
    window: Owned,
    size: PhysicalSize<u32>,
    depth: u8,
    horizontal: bool,
}

impl Dashes {
    /// Shown at once, with dashes as `set_colors` draws them.
    pub fn open(
        screen: &Screen,
        rect: capture::Rect,
        horizontal: bool,
        on: u32,
        off: u32,
        dash: u32,
    ) -> Result<Self, String> {
        let window = screen.create_window(
            rect,
            off,
            EventMask::NO_EVENT,
            b"_NET_WM_WINDOW_TYPE_NOTIFICATION",
        )?;
        let conn = &screen.conn;
        // An empty input shape: clicks go to whatever's under it.
        conn.shape_rectangles(SO::SET, SK::INPUT, ClipOrdering::UNSORTED, window.id, 0, 0, &[])
            .map_err(|e| e.to_string())?;
        let dashes = Self {
            window,
            size: PhysicalSize::new(rect.w.max(1), rect.h.max(1)),
            depth: conn.setup().roots[screen.screen_num].root_depth,
            horizontal,
        };
        dashes.set_colors(on, off, dash)?;
        conn.map_window(dashes.window.id).map_err(|e| e.to_string())?;
        conn.flush().map_err(|e| e.to_string())?;
        Ok(dashes)
    }

    /// Dashes of `dash` pixels, alternately `on` and `off` (`0xRRGGBB`).
    pub fn set_colors(&self, on: u32, off: u32, dash: u32) -> Result<(), String> {
        let (conn, id) = (&self.window.conn, self.window.id);
        let (w, h) = (self.size.width as u16, self.size.height as u16);
        let e = |e: x11rb::errors::ConnectionError| e.to_string();
        // Drawn into the window's background, which the X server paints
        // whenever it's uncovered.
        let pixmap = conn.generate_id().map_err(|e| e.to_string())?;
        conn.create_pixmap(self.depth, pixmap, id, w, h).map_err(e)?;
        let gc = conn.generate_id().map_err(|e| e.to_string())?;
        conn.create_gc(gc, pixmap, &CreateGCAux::new().foreground(off))
            .map_err(e)?;
        conn.poly_fill_rectangle(pixmap, gc, &[Rectangle { x: 0, y: 0, width: w, height: h }])
            .map_err(e)?;
        conn.change_gc(gc, &x11rb::protocol::xproto::ChangeGCAux::new().foreground(on))
            .map_err(e)?;
        let length = if self.horizontal { w } else { h };
        let dashes: Vec<Rectangle> = (0..length as u32)
            .step_by(2 * dash as usize)
            .map(|at| {
                let (at, dash) = (at as i16, dash as u16);
                if self.horizontal {
                    Rectangle { x: at, y: 0, width: dash, height: h }
                } else {
                    Rectangle { x: 0, y: at, width: w, height: dash }
                }
            })
            .collect();
        conn.poly_fill_rectangle(pixmap, gc, &dashes).map_err(e)?;
        conn.change_window_attributes(id, &ChangeWindowAttributesAux::new().background_pixmap(pixmap))
            .map_err(e)?;
        conn.clear_area(false, id, 0, 0, 0, 0).map_err(e)?;
        // The window keeps the pixmap for as long as it's its background.
        conn.free_gc(gc).map_err(e)?;
        conn.free_pixmap(pixmap).map_err(e)?;
        conn.flush().map_err(e)
    }
}

fn button(detail: u8) -> Option<MouseButton> {
    match detail {
        1 => Some(MouseButton::Left),
        2 => Some(MouseButton::Middle),
        3 => Some(MouseButton::Right),
        _ => None,
    }
}

fn atom(conn: &XCBConnection, name: &[u8]) -> Option<u32> {
    Some(conn.intern_atom(false, name).ok()?.reply().ok()?.atom)
}
