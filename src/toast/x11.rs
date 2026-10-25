//! The toast as an X11 window, for Wayland desktops. Wayland doesn't let a
//! window choose where it goes (GNOME centres new ones), and GNOME has no
//! layer-shell, so the toast goes through XWayland instead, which does. winit
//! allows one event loop per process, so this is a bare X11 window: its mouse
//! input is read on its own thread and sent to the event loop, and it's drawn
//! from there with egui's renderer.

use std::num::NonZeroU32;
use std::ptr::NonNull;
use std::sync::Arc;

use egui_wgpu::wgpu;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::MouseButton;
use winit::raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, XcbDisplayHandle, XcbWindowHandle,
};
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xproto::{
    AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, PropMode, WindowClass,
};
use x11rb::wrapper::ConnectionExt as _;
use x11rb::xcb_ffi::XCBConnection;

use super::{Pointer, SCREEN_GAP};
use crate::gpu::Gpu;

/// Mouse input for an X11 toast, from its thread.
#[derive(Debug)]
pub struct X11Event {
    pub(super) window: u32,
    /// `None`: draw the window.
    pub(super) pointer: Option<Pointer>,
}

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

/// Whether the toast should be an X11 window: under Wayland, with XWayland.
pub fn wanted(event_loop: &winit::event_loop::ActiveEventLoop) -> bool {
    use winit::raw_window_handle::HasDisplayHandle;
    let wayland = event_loop
        .display_handle()
        .is_ok_and(|h| matches!(h.as_raw(), RawDisplayHandle::Wayland(_)));
    wayland && std::env::var_os("DISPLAY").is_some()
}

impl X11Window {
    /// A window of `size` points in the primary screen's bottom-right corner,
    /// shown at once. Its input goes to `send`.
    pub fn open(
        gpu: &Gpu,
        size: LogicalSize<f64>,
        send: impl Fn(X11Event) + Send + 'static,
    ) -> Result<Self, String> {
        let (conn, screen_num) = XCBConnection::connect(None).map_err(|e| e.to_string())?;
        let conn = Arc::new(conn);
        let screen = &conn.setup().roots[screen_num];
        let root = screen.root;
        let scale_factor = scale_factor(&conn);
        let size = size.to_physical::<u32>(scale_factor);
        let (right, bottom) = corner(&conn, root, screen.width_in_pixels, screen.height_in_pixels);
        let gap = (SCREEN_GAP * scale_factor) as i32;
        let x = right - size.width as i32 - gap;
        let y = bottom - size.height as i32 - gap;

        let id = conn.generate_id().map_err(|e| e.to_string())?;
        // Override-redirect: the window manager leaves it where it's put,
        // above other windows, without focusing it or listing it as an app.
        let aux = CreateWindowAux::new()
            .override_redirect(1)
            // The toast's colour, until it's first drawn.
            .background_pixel(0x1c1d20)
            .event_mask(
                EventMask::BUTTON_PRESS
                    | EventMask::BUTTON_RELEASE
                    | EventMask::POINTER_MOTION
                    | EventMask::ENTER_WINDOW
                    | EventMask::LEAVE_WINDOW
                    | EventMask::EXPOSURE
                    | EventMask::STRUCTURE_NOTIFY,
            );
        conn.create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            id,
            root,
            x as i16,
            y as i16,
            size.width as u16,
            size.height as u16,
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
        if let (Some(kind), Some(notification)) = (
            atom(&conn, b"_NET_WM_WINDOW_TYPE"),
            atom(&conn, b"_NET_WM_WINDOW_TYPE_NOTIFICATION"),
        ) {
            let _ = conn.change_property32(
                PropMode::REPLACE,
                id,
                kind,
                AtomEnum::ATOM,
                &[notification],
            );
        }

        let display = XcbDisplayHandle::new(
            NonNull::new(conn.get_raw_xcb_connection()),
            screen_num as i32,
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
            .ok_or("no 8-bit surface format for the preview")?;
        let config = wgpu::SurfaceConfiguration {
            format,
            present_mode: wgpu::PresentMode::Fifo,
            view_formats: vec![],
            ..surface
                .get_default_config(&gpu.adapter, size.width, size.height)
                .ok_or("the preview's surface is unsupported")?
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
            scale_factor,
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
                                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
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

/// Pixels per point: `Xft.dpi`, which the desktop sets for its scale.
fn scale_factor(conn: &XCBConnection) -> f64 {
    x11rb::resource_manager::new_from_default(conn)
        .ok()
        .and_then(|db| db.get_value::<f64>("Xft.dpi", "").ok().flatten())
        .filter(|dpi| *dpi > 0.0)
        .map_or(1.0, |dpi| dpi / 96.0)
}

/// The bottom-right corner of the primary screen, above any panel or dock.
fn corner(conn: &XCBConnection, root: u32, width: u16, height: u16) -> (i32, i32) {
    let (mut right, mut bottom) =
        primary_corner(conn, root).unwrap_or((width as i32, height as i32));
    if let Some([x, y, w, h]) = work_area(conn, root) {
        right = right.min(x + w);
        bottom = bottom.min(y + h);
    }
    (right, bottom)
}

fn primary_corner(conn: &XCBConnection, root: u32) -> Option<(i32, i32)> {
    conn.randr_query_version(1, 3).ok()?.reply().ok()?;
    let output = conn
        .randr_get_output_primary(root)
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
    (c.width > 0).then(|| (c.x as i32 + c.width as i32, c.y as i32 + c.height as i32))
}

/// The desktop's work area (all screens, minus panels and docks).
fn work_area(conn: &XCBConnection, root: u32) -> Option<[i32; 4]> {
    let reply = conn
        .get_property(
            false,
            root,
            atom(conn, b"_NET_WORKAREA")?,
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
