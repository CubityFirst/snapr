//! The main window: an icon bar on the left (capture, recent, settings, open
//! folder) and the selected page on the right. egui, rendered through
//! egui-wgpu on the shared device.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Color32, Pos2, Rect, Sense, Shape, Stroke, Vec2, pos2, vec2};
use egui_wgpu::winit::Painter;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{ModifiersState, PhysicalKey};
use winit::window::{Icon, Window};

use crate::gallery::{self, Gallery};
use crate::gpu::Gpu;
use crate::settings::{Settings, Upload};
use crate::settings_ui::{self, Form};
use crate::tools::{self, Tools};

const ACCENT: Color32 = Color32::from_rgb(0x3d, 0x9b, 0xff);
const NAV_WIDTH: f32 = 60.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Recent,
    Tools,
    /// The General tab of Settings.
    Settings,
    Hotkeys,
    Naming,
    Destinations,
}

pub enum Action {
    Capture,
    /// New settings, and new secret keys to store (upload id → key).
    SaveSettings(Settings, HashMap<String, String>),
    TestUpload(Upload, Option<String>),
    /// Copy a link, or other text, to the clipboard.
    CopyText(String),
    CopyImage(image::RgbaImage),
    OpenUrl(String),
    /// Pick a region to read QR codes from.
    ScanQr,
    /// Pick a pixel's colour from the screen.
    PickColor,
    /// Open a folder in the file manager.
    Reveal(PathBuf),
    OpenFile(PathBuf),
    ShowInFolder(PathBuf),
    Copy(PathBuf),
    Delete(PathBuf),
    DeleteRemote(PathBuf, crate::history::Remote),
    /// Files dropped on the window, to upload.
    UploadFiles(Vec<PathBuf>),
    /// Stitch images together into a new screenshot.
    Combine(Vec<PathBuf>, crate::combine::Direction),
}

#[derive(Clone, Copy)]
enum NavIcon {
    Capture,
    Recent,
    Tools,
    Settings,
    Folder,
}

pub struct MainWindow {
    pub window: Arc<Window>,
    state: egui_winit::State,
    painter: Painter,
    modifiers: ModifiersState,
    pub page: Page,
    pub form: Form,
    pub gallery: Gallery,
    pub tools: Tools,
    /// The capture hotkey, for hints.
    pub hotkey: String,
    /// Where screenshots are saved, for the folder button.
    pub folder: PathBuf,
    /// When egui asked to be repainted next (e.g. for a blinking cursor).
    pub repaint_at: Option<Instant>,
}

impl MainWindow {
    pub fn open(
        event_loop: &ActiveEventLoop,
        gpu: &Gpu,
        page: Page,
        form: Form,
        gallery: Gallery,
        settings: &Settings,
    ) -> Result<Self, String> {
        let icon = Icon::from_rgba(crate::tray::icon_rgba(64), 64, 64).ok();
        let attrs = Window::default_attributes()
            .with_title("snapr")
            .with_inner_size(LogicalSize::new(980.0, 660.0))
            .with_min_inner_size(LogicalSize::new(640.0, 440.0))
            .with_window_icon(icon);
        let window = Arc::new(event_loop.create_window(attrs).map_err(|e| e.to_string())?);

        let ctx = egui::Context::default();
        let config = egui_wgpu::WgpuConfiguration {
            wgpu_setup: gpu.egui_setup(),
            ..Default::default()
        };
        let mut painter =
            pollster::block_on(Painter::new(ctx.clone(), config, false, Default::default()));
        pollster::block_on(painter.set_window(egui::ViewportId::ROOT, Some(window.clone())))
            .map_err(|e| format!("couldn't set up the window: {e}"))?;
        let state = egui_winit::State::new(
            ctx,
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            window.theme(),
            painter.max_texture_side(),
        );
        window.focus_window();
        Ok(Self {
            window,
            state,
            painter,
            modifiers: ModifiersState::empty(),
            page,
            form,
            gallery,
            tools: Tools::new(),
            hotkey: settings.hotkey.clone(),
            folder: PathBuf::from(&settings.folder),
            repaint_at: None,
        })
    }

    pub fn show_page(&mut self, page: Page) {
        self.page = page;
        self.window.set_visible(true);
        self.window.set_minimized(false);
        self.window.focus_window();
        self.window.request_redraw();
    }

    pub fn on_event(&mut self, event: &WindowEvent) {
        match event {
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::KeyboardInput { event, .. } if self.form.recording => {
                if let PhysicalKey::Code(code) = event.physical_key
                    && !event.repeat
                    && self.form.record(self.modifiers, code, event.state)
                {
                    self.window.request_redraw();
                }
                return; // keep the keys away from egui while recording
            }
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
    }

    fn ui(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let dark = ui.visuals().dark_mode;
        let nav_fill = if dark {
            Color32::from_rgb(22, 23, 26)
        } else {
            Color32::from_rgb(232, 234, 238)
        };
        egui::Panel::left("nav")
            .exact_size(NAV_WIDTH)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(nav_fill)
                    .inner_margin(egui::Margin::symmetric(8, 12)),
            )
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = vec2(0.0, 6.0);
                ui.vertical_centered(|ui| {
                    let capture_tip = format!("Capture region  ({})", self.hotkey);
                    if nav_button(ui, NavIcon::Capture, false, &capture_tip).clicked() {
                        actions.push(Action::Capture);
                    }
                    ui.add_space(6.0);
                    if nav_button(
                        ui,
                        NavIcon::Recent,
                        self.page == Page::Recent,
                        "Recent screenshots",
                    )
                    .clicked()
                    {
                        self.page = Page::Recent;
                    }
                    if nav_button(ui, NavIcon::Tools, self.page == Page::Tools, "Tools").clicked() {
                        self.page = Page::Tools;
                    }
                    let in_settings = !matches!(self.page, Page::Recent | Page::Tools);
                    if nav_button(ui, NavIcon::Settings, in_settings, "Settings").clicked()
                        && !in_settings
                    {
                        self.page = Page::Settings;
                    }
                });
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                    if nav_button(ui, NavIcon::Folder, false, "Open screenshots folder").clicked() {
                        actions.push(Action::Reveal(self.folder.clone()));
                    }
                });
            });
        egui::CentralPanel::default()
            .frame(
                egui::Frame::central_panel(ui.style())
                    .inner_margin(egui::Margin::symmetric(20, 16)),
            )
            .show(ui, |ui| match self.page {
                Page::Recent => {
                    let mut gallery_actions = Vec::new();
                    self.gallery.ui(ui, &self.hotkey, &mut gallery_actions);
                    actions.extend(gallery_actions.into_iter().map(|a| match a {
                        gallery::Action::Capture => Action::Capture,
                        gallery::Action::Open(p) => Action::OpenFile(p),
                        gallery::Action::ShowInFolder(p) => Action::ShowInFolder(p),
                        gallery::Action::Copy(p) => Action::Copy(p),
                        gallery::Action::CopyLink(l) => Action::CopyText(l),
                        gallery::Action::Delete(p) => Action::Delete(p),
                        gallery::Action::DeleteRemote(p, r) => Action::DeleteRemote(p, r),
                        gallery::Action::Combine(p, d) => Action::Combine(p, d),
                    }));
                }
                Page::Tools => {
                    let mut tool_actions = Vec::new();
                    self.tools.ui(ui, &mut tool_actions);
                    actions.extend(tool_actions.into_iter().map(|a| match a {
                        tools::Action::ScanQr => Action::ScanQr,
                        tools::Action::PickColor => Action::PickColor,
                        tools::Action::CopyText(t) => Action::CopyText(t),
                        tools::Action::CopyImage(i) => Action::CopyImage(i),
                        tools::Action::OpenUrl(u) => Action::OpenUrl(u),
                    }));
                }
                page => {
                    ui.horizontal(|ui| {
                        ui.heading("Settings");
                        ui.add_space(12.0);
                        for (tab, label) in [
                            (Page::Settings, "General"),
                            (Page::Hotkeys, "Hotkeys"),
                            (Page::Naming, "Paths & naming"),
                            (Page::Destinations, "Destinations"),
                        ] {
                            ui.selectable_value(&mut self.page, tab, label);
                        }
                    });
                    ui.add_space(4.0);
                    if self.page != Page::Hotkeys {
                        self.form.recording = false;
                    }
                    let mut form_actions = Vec::new();
                    match page {
                        Page::Hotkeys => self.form.hotkeys_ui(ui, &mut form_actions),
                        Page::Naming => self.form.naming_ui(ui, &mut form_actions),
                        Page::Destinations => self.form.destinations_ui(ui, &mut form_actions),
                        _ => self.form.ui(ui, &mut form_actions),
                    }
                    actions.extend(form_actions.into_iter().map(|a| match a {
                        settings_ui::Action::Save(s, secrets) => Action::SaveSettings(s, secrets),
                        settings_ui::Action::Reveal(p) => Action::Reveal(p),
                        settings_ui::Action::TestUpload(u, secret) => Action::TestUpload(u, secret),
                    }));
                }
            });
        self.drop_ui(ui, actions);
    }

    /// Uploads files dropped on the window, with a hint while they're dragged over it.
    fn drop_ui(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let (hovering, dropped) = ui.input(|i| {
            (
                !i.raw.hovered_files.is_empty(),
                i.raw
                    .dropped_files
                    .iter()
                    .map(|f| f.path().to_path_buf())
                    .collect::<Vec<_>>(),
            )
        });
        if !dropped.is_empty() {
            actions.push(Action::UploadFiles(dropped));
        }
        if !hovering {
            return;
        }
        let rect = ui.ctx().content_rect();
        let painter = ui.ctx().layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("drop"),
        ));
        painter.rect_filled(rect, 0.0, Color32::from_black_alpha(160));
        painter.rect_stroke(
            rect.shrink(12.0),
            10.0,
            Stroke::new(2.0, ACCENT),
            egui::StrokeKind::Inside,
        );
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "Drop to upload",
            egui::FontId::proportional(22.0),
            Color32::WHITE,
        );
    }

    pub fn paint(&mut self) -> Vec<Action> {
        let mut actions = Vec::new();
        let input = self.state.take_egui_input(&self.window);
        let ctx = self.state.egui_ctx().clone();
        let mut output = ctx.run_ui(input, |ui| self.ui(ui, &mut actions));
        self.state
            .handle_platform_output(&self.window, std::mem::take(&mut output.platform_output));

        let primitives =
            ctx.tessellate(std::mem::take(&mut output.shapes), output.pixels_per_point);
        let clear = egui::Rgba::from(ctx.global_style().visuals.panel_fill).to_array();
        self.painter.paint_and_update_textures(
            egui::ViewportId::ROOT,
            output.pixels_per_point,
            clear,
            &primitives,
            &mut output.textures_delta,
            Vec::new(),
            &self.window,
        );
        // Anything the painter couldn't apply (e.g. the window was minimized).
        output.textures_delta.clear();

        self.repaint_at = None;
        if let Some(vp) = output.viewport_output.get(&egui::ViewportId::ROOT) {
            if vp.repaint_delay.is_zero() {
                self.window.request_redraw();
            } else if vp.repaint_delay < Duration::from_secs(3600) {
                self.repaint_at = Some(Instant::now() + vp.repaint_delay);
            }
        }
        actions
    }
}

fn nav_button(ui: &mut egui::Ui, icon: NavIcon, selected: bool, tip: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(40.0), Sense::click());
    let visuals = ui.visuals();
    let fg = if selected {
        Color32::WHITE
    } else if resp.hovered() {
        visuals.strong_text_color()
    } else {
        visuals.text_color()
    };
    let p = ui.painter();
    if selected {
        p.rect_filled(rect, 9.0, ACCENT);
    } else if resp.hovered() {
        p.rect_filled(rect, 9.0, visuals.widgets.hovered.weak_bg_fill);
    }
    draw_icon(p, rect.shrink(10.0), icon, fg);
    resp.on_hover_text(tip)
        .on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn draw_icon(p: &egui::Painter, r: Rect, icon: NavIcon, color: Color32) {
    let s = Stroke::new(1.8, color);
    let at = |x: f32, y: f32| pos2(r.left() + r.width() * x, r.top() + r.height() * y);
    match icon {
        NavIcon::Capture => {
            // Viewfinder corners around a dot, like the app icon.
            let arm = r.width() * 0.32;
            for (c, dx, dy) in [
                (r.left_top(), 1.0, 1.0),
                (r.right_top(), -1.0, 1.0),
                (r.right_bottom(), -1.0, -1.0),
                (r.left_bottom(), 1.0, -1.0),
            ] {
                p.add(Shape::line(
                    vec![c + vec2(0.0, dy * arm), c, c + vec2(dx * arm, 0.0)],
                    Stroke::new(2.0, color),
                ));
            }
            p.circle_filled(r.center(), r.width() * 0.13, color);
        }
        NavIcon::Recent => {
            // A 2×2 grid of pictures.
            let gap = r.width() * 0.12;
            let cell = (r.width() - gap) / 2.0;
            for (x, y) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
                let min = r.min + vec2(x * (cell + gap), y * (cell + gap));
                p.rect_stroke(
                    Rect::from_min_size(min, Vec2::splat(cell)),
                    2.0,
                    s,
                    egui::StrokeKind::Inside,
                );
            }
        }
        NavIcon::Tools => {
            // A wrench: an open ring at the top right on a diagonal handle.
            let c = at(0.66, 0.34);
            let rad = r.width() * 0.26;
            let open = -std::f32::consts::FRAC_PI_4; // the jaw faces up-right
            let gap = 0.9;
            let head: Vec<Pos2> = (0..=24)
                .map(|i| {
                    let a = open + gap + i as f32 / 24.0 * (std::f32::consts::TAU - 2.0 * gap);
                    c + vec2(a.cos(), a.sin()) * rad
                })
                .collect();
            p.add(Shape::line(head, s));
            let neck = c + vec2(-1.0, 1.0) * (rad * std::f32::consts::FRAC_1_SQRT_2);
            p.line_segment([neck, at(0.06, 0.94)], Stroke::new(2.6, color));
        }
        NavIcon::Settings => {
            // A gear: eight teeth around a ring.
            let c = r.center();
            let (outer, inner) = (r.width() * 0.5, r.width() * 0.36);
            let pts: Vec<Pos2> = (0..32)
                .map(|i| {
                    let a = i as f32 / 32.0 * std::f32::consts::TAU;
                    let tooth = (i / 2) % 2 == 0;
                    let rad = if tooth { outer } else { inner };
                    c + vec2(a.cos(), a.sin()) * rad
                })
                .collect();
            p.add(Shape::closed_line(pts, s));
            p.circle_stroke(c, r.width() * 0.15, s);
        }
        NavIcon::Folder => {
            let pts = vec![
                at(0.0, 0.15),
                at(0.38, 0.15),
                at(0.48, 0.3),
                at(1.0, 0.3),
                at(1.0, 0.88),
                at(0.0, 0.88),
            ];
            p.add(Shape::closed_line(pts, s));
        }
    }
}
