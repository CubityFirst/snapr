//! The preview that pops up in the corner of the screen after a capture.
//! Left-click opens the link (or the image, before it's uploaded), middle-click
//! copies the image, right-click dismisses it, and dragging it drops the file
//! into another app. It goes away by itself after a few seconds, unless the
//! mouse is over it.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Color32, RichText};
use egui_wgpu::winit::Painter;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowLevel};

use crate::gpu::Gpu;

/// Largest preview, in points; the window fits the image's shape.
const MAX_PREVIEW: (f32, f32) = (320.0, 200.0);
const CAPTION_HEIGHT: f32 = 26.0;
const MARGIN: f32 = 6.0;
/// Gap between the toast and the screen's corner, in points.
const SCREEN_GAP: f64 = 16.0;
/// How long it stays up, and how long after the mouse leaves it.
const SHOWN_FOR: Duration = Duration::from_secs(6);
const AFTER_HOVER: Duration = Duration::from_secs(2);
/// How far (in points) the mouse moves with the button down before it's a
/// drag rather than a click.
const DRAG_THRESHOLD: f64 = 6.0;
/// Pixels decoded for the preview (twice the size, for high-DPI screens).
pub const DECODE_SIZE: (u32, u32) = (640, 400);

pub enum Action {
    /// Open the link, or the image if there's no link yet.
    Open,
    CopyImage,
    Dismiss,
}

pub struct Toast {
    pub window: Arc<Window>,
    state: egui_winit::State,
    painter: Painter,
    /// The preview; `None` shows a file icon.
    texture: Option<egui::TextureHandle>,
    pub path: PathBuf,
    pub link: Option<String>,
    /// A recording rather than a screenshot: the preview is its first frame.
    pub video: bool,
    /// When it goes away; `None` while the mouse is over it.
    pub expires_at: Option<Instant>,
    cursor: PhysicalPosition<f64>,
    /// Where the left button went down, until it's released or a drag starts.
    pressed_at: Option<PhysicalPosition<f64>>,
}

impl Toast {
    pub fn open(
        event_loop: &ActiveEventLoop,
        gpu: &Gpu,
        path: PathBuf,
        image: Option<egui::ColorImage>,
        link: Option<String>,
    ) -> Result<Self, String> {
        // A file without a picture gets a 16:10 icon area.
        let [w, h] = image.as_ref().map_or([320, 200], |i| i.size);
        let scale = (MAX_PREVIEW.0 / w as f32)
            .min(MAX_PREVIEW.1 / h as f32)
            .min(1.0);
        let preview = egui::vec2(w as f32 * scale, h as f32 * scale);
        // Wide enough for the caption even for tall, narrow images.
        let size = LogicalSize::new(
            (preview.x.max(220.0) + 2.0 * MARGIN) as f64,
            (preview.y + CAPTION_HEIGHT + 2.0 * MARGIN) as f64,
        );

        #[allow(unused_mut)]
        let mut attrs = Window::default_attributes()
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
        if let Some(pos) = corner_position(event_loop, &window) {
            window.set_outer_position(pos);
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
            .map_err(|e| format!("couldn't set up the preview: {e}"))?;
        let state = egui_winit::State::new(
            ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            window.theme(),
            painter.max_texture_side(),
        );
        let texture = image.map(|i| ctx.load_texture("toast", i, egui::TextureOptions::LINEAR));
        let video = crate::thumbnail::kind(&path) == crate::thumbnail::Kind::Video;
        let mut toast = Self {
            window,
            state,
            painter,
            texture,
            path,
            link,
            video,
            expires_at: Some(Instant::now() + SHOWN_FOR),
            cursor: PhysicalPosition::new(0.0, 0.0),
            pressed_at: None,
        };
        // Paint before showing it, so it doesn't flash white.
        toast.paint();
        toast.window.set_visible(true);
        Ok(toast)
    }

    pub fn set_link(&mut self, link: String) {
        self.link = Some(link);
        self.window.request_redraw();
    }

    pub fn on_event(&mut self, event: &WindowEvent) -> Option<Action> {
        match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => self.pressed_at = Some(self.cursor),
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button,
                ..
            } => match button {
                // Not if the press turned into a drag.
                MouseButton::Left if self.pressed_at.take().is_some() => return Some(Action::Open),
                MouseButton::Middle => return Some(Action::CopyImage),
                MouseButton::Right => return Some(Action::Dismiss),
                _ => {}
            },
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = *position;
                // Also after a drag, which swallows the mouse's comings and goings.
                self.expires_at = None;
                if let Some(from) = self.pressed_at {
                    let (dx, dy) = (position.x - from.x, position.y - from.y);
                    let threshold = DRAG_THRESHOLD * self.window.scale_factor();
                    if dx * dx + dy * dy >= threshold * threshold {
                        self.pressed_at = None;
                        crate::output::drag_out(&self.window, std::slice::from_ref(&self.path));
                        self.expires_at = Some(Instant::now() + AFTER_HOVER);
                    }
                }
            }
            WindowEvent::CursorEntered { .. } => self.expires_at = None,
            WindowEvent::CursorLeft { .. } => self.expires_at = Some(Instant::now() + AFTER_HOVER),
            WindowEvent::RedrawRequested => self.paint(),
            WindowEvent::Resized(size) => {
                if let (Some(w), Some(h)) = (
                    std::num::NonZeroU32::new(size.width),
                    std::num::NonZeroU32::new(size.height),
                ) {
                    self.painter.on_window_resized(egui::ViewportId::ROOT, w, h);
                }
            }
            _ => {}
        }
        if self.state.on_window_event(&self.window, event).repaint {
            self.window.request_redraw();
        }
        None
    }

    fn paint(&mut self) {
        let input = self.state.take_egui_input(&self.window);
        let ctx = self.state.egui_ctx().clone();
        let texture = self.texture.clone();
        let caption = match (self.link.is_some(), self.video) {
            (true, _) => "Click to open the link",
            (false, true) => "Click to play",
            (false, false) => "Click to open",
        };
        let hint = if self.video {
            "middle: copy link \u{00b7} right: close"
        } else {
            "middle: copy \u{00b7} right: close"
        };
        let video = self.video;
        let path = self.path.clone();
        let mut output = ctx.run_ui(input, |ui| {
            contents(ui, texture.as_ref(), &path, caption, hint, video)
        });
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

/// The preview image and the caption under it.
fn contents(
    ui: &mut egui::Ui,
    texture: Option<&egui::TextureHandle>,
    path: &std::path::Path,
    caption: &str,
    hint: &str,
    video: bool,
) {
    egui::CentralPanel::default()
        .frame(
            egui::Frame::new()
                .fill(Color32::from_rgb(28, 29, 32))
                .stroke(egui::Stroke::new(1.0, Color32::from_rgb(58, 60, 66)))
                .inner_margin(MARGIN),
        )
        .show(ui, |ui| {
            let image_area =
                egui::vec2(ui.available_width(), ui.available_height() - CAPTION_HEIGHT);
            let (rect, _) = ui.allocate_exact_size(image_area, egui::Sense::hover());
            match texture {
                Some(texture) => {
                    let tex = texture.size_vec2();
                    let scale = (image_area.x / tex.x).min(image_area.y / tex.y);
                    let img = egui::Rect::from_center_size(rect.center(), tex * scale);
                    egui::Image::new((texture.id(), tex))
                        .corner_radius(3.0)
                        .paint_at(ui, img);
                    if video {
                        crate::thumbnail::draw_play_badge(ui.painter(), img.center(), 20.0);
                    }
                }
                None => crate::thumbnail::draw_file_icon(ui.painter(), rect, path),
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(caption).color(Color32::WHITE).size(12.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(hint).weak().size(11.0));
                });
            });
        });
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
    /// Renders a preview of a 16:9 screenshot to `target/toast-preview.png`:
    /// `cargo test toast_preview -- --ignored`.
    #[test]
    #[ignore]
    fn toast_preview() {
        let pixels: Vec<u8> = (0..640 * 360)
            .flat_map(|i| [(i % 640 / 3) as u8, 90, (i / 640 / 2) as u8, 255])
            .collect();
        let image = egui::ColorImage::from_rgba_unmultiplied([640, 360], &pixels);
        for (name, video) in [("toast-preview", false), ("toast-video-preview", true)] {
            let mut texture = None;
            crate::preview::render(name, [332, 218], 1.0, |root| {
                let t = texture.get_or_insert_with(|| {
                    root.ctx()
                        .load_texture("toast", image.clone(), Default::default())
                });
                let hint = if video {
                    "middle: copy link \u{00b7} right: close"
                } else {
                    "middle: copy \u{00b7} right: close"
                };
                let path = std::path::Path::new(if video { "clip.mp4" } else { "shot.png" });
                super::contents(root, Some(t), path, "Click to open the link", hint, video);
            });
        }
    }
}
