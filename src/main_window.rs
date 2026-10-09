//! The main window: an icon bar on the left (capture, recent, tools; open
//! folder and settings at the bottom) and the selected page on the right. egui, rendered through
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

use crate::capture::{self, DisplayInfo, WindowInfo};
use crate::gallery::{self, Gallery};
use crate::gpu::Gpu;
use crate::output::{Transfer, Transfers};
use crate::settings::{Settings, SpeedUnit, Upload};
use crate::settings_ui::{self, Form};
use crate::tools::{self, Tools};

const ACCENT: Color32 = Color32::from_rgb(0x3d, 0x9b, 0xff);
const NAV_WIDTH: f32 = 60.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Recent,
    Tools,
    Stats,
    /// The General tab of Settings.
    Settings,
    Hotkeys,
    Naming,
    Destinations,
    Recording,
}

pub enum Action {
    Capture,
    /// Capture a window, display, everything or the last region, without
    /// picking a region first.
    CaptureTarget(capture::Target),
    /// New settings, and new secret keys to store (upload id → key).
    SaveSettings(Settings, HashMap<String, String>),
    TestUpload(Upload, Option<String>),
    /// Upload snapr's icon to a destination, as a screenshot would be.
    UploadIcon(Upload, Option<String>),
    CheckForUpdates,
    /// Quit and start the updated program.
    RestartToUpdate,
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
    /// Pick a region to pin to the screen.
    PinRegion,
    PinImage(image::RgbaImage),
    /// Pin an image file to the screen.
    PinFile(PathBuf),
    ClosePins,
}

impl From<settings_ui::Action> for Action {
    fn from(a: settings_ui::Action) -> Self {
        match a {
            settings_ui::Action::Save(s, secrets) => Action::SaveSettings(s, secrets),
            settings_ui::Action::Reveal(p) => Action::Reveal(p),
            settings_ui::Action::TestUpload(u, secret) => Action::TestUpload(u, secret),
            settings_ui::Action::UploadIcon(u, secret) => Action::UploadIcon(u, secret),
            settings_ui::Action::CheckForUpdates => Action::CheckForUpdates,
            settings_ui::Action::RestartToUpdate => Action::RestartToUpdate,
            settings_ui::Action::OpenUrl(u) => Action::OpenUrl(u),
        }
    }
}

/// One upload: what's going where, a progress bar with how much is done,
/// the speed and time left, and a button to cancel it.
fn transfer_row(ui: &mut egui::Ui, t: &Transfer, unit: SpeedUnit) {
    use std::sync::atomic::Ordering;
    let sent = t.progress.sent.load(Ordering::Relaxed);
    let total = t.progress.total.load(Ordering::Relaxed);
    let cancelling = t.progress.cancel.load(Ordering::Relaxed);
    let fraction = if total > 0 {
        sent as f32 / total as f32
    } else {
        0.0
    };
    let elapsed = t.started.elapsed().as_secs_f64();
    // Average speed; steadier than the last moment's, which jumps around
    // as parts start and finish.
    let rate = if elapsed > 1.0 { sent as f64 / elapsed } else { 0.0 };
    let mut text = format!("{:.0}%", fraction * 100.0);
    if total > 0 {
        text += &format!(" \u{00b7} {} of {}", bytes(sent), bytes(total));
    }
    if cancelling {
        text += " \u{00b7} cancelling\u{2026}";
    } else if sent >= total && total > 0 {
        text += " \u{00b7} finishing\u{2026}";
    } else if rate > 0.0 && sent > 0 {
        text += &format!(" \u{00b7} {}", speed(rate, unit));
        let left = (total - sent) as f64 / rate;
        text += &format!(" \u{00b7} {} left", duration(left));
    }
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(format!("Uploading {} to {}", t.file, t.destination)).strong(),
        );
    });
    ui.horizontal(|ui| {
        let cancel_width = 64.0;
        let bar_width = (ui.available_width() - cancel_width - 8.0).max(80.0);
        ui.add(
            egui::ProgressBar::new(fraction)
                .desired_width(bar_width)
                .text(text)
                .animate(sent == 0 && !cancelling),
        );
        if ui
            .add_enabled(!cancelling, egui::Button::new("Cancel"))
            .on_hover_text("Stop this upload; nothing is left in the bucket")
            .clicked()
        {
            t.progress.cancel.store(true, Ordering::Relaxed);
        }
    });
    ui.add_space(2.0);
}

/// A byte count in B, KB, MB or GB.
fn bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = n as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else if v < 10.0 {
        format!("{v:.1} {}", UNITS[unit])
    } else {
        format!("{v:.0} {}", UNITS[unit])
    }
}

/// Bytes per second as MB/s (1024-based, like file sizes) or Mbps
/// (1000-based bits, like network speeds).
fn speed(bytes_per_sec: f64, unit: SpeedUnit) -> String {
    match unit {
        SpeedUnit::Bytes => format!("{}/s", bytes(bytes_per_sec as u64)),
        SpeedUnit::Bits => {
            let bits = bytes_per_sec * 8.0;
            let (v, unit) = if bits >= 1e9 {
                (bits / 1e9, "Gbps")
            } else if bits >= 1e6 {
                (bits / 1e6, "Mbps")
            } else {
                (bits / 1e3, "Kbps")
            };
            if v < 10.0 {
                format!("{v:.1} {unit}")
            } else {
                format!("{v:.0} {unit}")
            }
        }
    }
}

/// A rough time left: seconds, minutes, or hours and minutes.
fn duration(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    match s {
        0..60 => format!("{s} s"),
        60..3600 => format!("{} min", s.div_ceil(60)),
        _ => format!("{} h {} min", s / 3600, s % 3600 / 60),
    }
}

#[derive(Clone, Copy)]
enum NavIcon {
    Capture,
    Recent,
    Tools,
    Stats,
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
    pub stats: crate::stats::Stats,
    /// The capture hotkey, for hints.
    pub hotkey: String,
    /// Where screenshots are saved, for the folder button.
    pub folder: PathBuf,
    /// When egui asked to be repainted next (e.g. for a blinking cursor).
    pub repaint_at: Option<Instant>,
    /// Uploads in progress, shown along the bottom.
    transfers: Transfers,
    pub speed_unit: SpeedUnit,
    /// The last screenshot's region, for "Last region".
    pub last_region: Option<capture::Rect>,
    /// The windows and displays listed in the capture menu, read when it opens.
    capture_menu: (Vec<WindowInfo>, Vec<DisplayInfo>),
}

impl MainWindow {
    pub fn open(
        event_loop: &ActiveEventLoop,
        gpu: &Gpu,
        page: Page,
        form: Form,
        gallery: Gallery,
        transfers: Transfers,
        settings: &Settings,
        stats: crate::stats::Stats,
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
            stats,
            hotkey: settings.hotkey.clone(),
            folder: PathBuf::from(&settings.folder),
            repaint_at: None,
            transfers,
            speed_unit: settings.speed_unit,
            last_region: None,
            capture_menu: Default::default(),
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
                    ui.add_space(-4.0);
                    self.capture_menu_ui(ui, actions);
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
                    if nav_button(ui, NavIcon::Stats, self.page == Page::Stats, "Stats").clicked() {
                        self.page = Page::Stats;
                    }
                });
                // Bottom-up: the first button added sits lowest.
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                    let in_settings = !matches!(self.page, Page::Recent | Page::Tools | Page::Stats);
                    if nav_button(ui, NavIcon::Settings, in_settings, "Settings").clicked()
                        && !in_settings
                    {
                        self.page = Page::Settings;
                    }
                    if nav_button(ui, NavIcon::Folder, false, "Open screenshots folder").clicked() {
                        actions.push(Action::Reveal(self.folder.clone()));
                    }
                });
            });
        if self.page != Page::Recent {
            self.gallery.stop_playback();
        }
        let transfers = self.transfers.lock().unwrap().clone();
        if !transfers.is_empty() {
            egui::Panel::bottom("uploads")
                .resizable(false)
                .frame(
                    egui::Frame::side_top_panel(ui.style())
                        .inner_margin(egui::Margin::symmetric(20, 10)),
                )
                .show(ui, |ui| {
                    for t in &transfers {
                        transfer_row(ui, t, self.speed_unit);
                    }
                });
            // Keep the numbers moving.
            ui.ctx().request_repaint_after(Duration::from_millis(250));
        }
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
                        gallery::Action::Pin(p) => Action::PinFile(p),
                    }));
                }
                Page::Tools => {
                    let mut tool_actions = Vec::new();
                    self.tools
                        .ui(ui, &self.form.saved.tool_hotkeys, &mut tool_actions);
                    for a in tool_actions {
                        actions.push(match a {
                            tools::Action::ScanQr => Action::ScanQr,
                            tools::Action::PickColor => Action::PickColor,
                            tools::Action::CopyText(t) => Action::CopyText(t),
                            tools::Action::CopyImage(i) => Action::CopyImage(i),
                            tools::Action::OpenUrl(u) => Action::OpenUrl(u),
                            tools::Action::PinRegion => Action::PinRegion,
                            tools::Action::PinImage(i) => Action::PinImage(i),
                            tools::Action::PinFile(p) => Action::PinFile(p),
                            tools::Action::ClosePins => Action::ClosePins,
                            tools::Action::AddHotkey(tool) => {
                                self.form.add_tool_hotkey(tool);
                                self.page = Page::Hotkeys;
                                continue;
                            }
                        });
                    }
                }
                Page::Stats => crate::stats_ui::ui(ui, &self.stats),
                page => {
                    ui.horizontal(|ui| {
                        ui.heading("Settings");
                        ui.add_space(12.0);
                        for (tab, label) in [
                            (Page::Settings, "General"),
                            (Page::Hotkeys, "Hotkeys"),
                            (Page::Naming, "Paths & naming"),
                            (Page::Destinations, "Destinations"),
                            (Page::Recording, "Recording"),
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
                        Page::Recording => self.form.recording_ui(ui, &mut form_actions),
                        _ => self.form.ui(ui, &mut form_actions),
                    }
                    actions.extend(form_actions.into_iter().map(Action::from));
                }
            });
        let mut form_actions = Vec::new();
        self.form.autosave(ui.ctx(), &mut form_actions);
        actions.extend(form_actions.into_iter().map(Action::from));
        self.drop_ui(ui, actions);
    }

    /// Saves settings edited too recently to have been saved yet.
    pub fn flush_settings(&mut self) -> Option<Action> {
        self.form.flush().map(Action::from)
    }

    /// The arrow under the capture button, and its menu of other things to
    /// capture: the last region, a window, a display or everything.
    fn capture_menu_ui(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let chevron = chevron_button(ui);
        if chevron.clicked() {
            // Read fresh each time it opens: windows come and go.
            self.capture_menu = (capture::windows(), capture::displays());
        }
        let (windows, displays) = &self.capture_menu;
        egui::Popup::menu(&chevron)
            .align(egui::RectAlign::RIGHT_START)
            .gap(4.0)
            .show(|ui| {
                let region = egui::Button::new("Region").shortcut_text(self.hotkey.as_str());
                if ui.add(region).clicked() {
                    actions.push(Action::Capture);
                    ui.close();
                }
                let last = ui
                    .add_enabled(self.last_region.is_some(), egui::Button::new("Last region"))
                    .on_disabled_hover_text("Take a region screenshot first");
                if last.clicked()
                    && let Some(r) = self.last_region
                {
                    actions.push(Action::CaptureTarget(capture::Target::Region(r)));
                    ui.close();
                }
                ui.separator();
                ui.menu_button("Window", |ui| {
                    if windows.is_empty() {
                        ui.weak("No windows");
                    }
                    egui::ScrollArea::vertical()
                        .max_height(420.0)
                        .show(ui, |ui| {
                            for w in windows {
                                let button = egui::Button::new(ellipsize(&w.title, 60))
                                    .shortcut_text(w.app.as_str());
                                if ui.add(button).on_hover_text(&w.title).clicked() {
                                    let target = capture::Target::Window(w.id);
                                    actions.push(Action::CaptureTarget(target));
                                    ui.close();
                                }
                            }
                        });
                });
                ui.menu_button("Display", |ui| {
                    if displays.is_empty() {
                        ui.weak("No displays");
                    }
                    for (i, d) in displays.iter().enumerate() {
                        let mut label = format!("{}. {}", i + 1, d.name);
                        if d.primary {
                            label += " (primary)";
                        }
                        let size = format!("{}\u{00d7}{}", d.rect.w, d.rect.h);
                        if ui
                            .add(egui::Button::new(label).shortcut_text(size))
                            .clicked()
                        {
                            actions.push(Action::CaptureTarget(capture::Target::Display(d.id)));
                            ui.close();
                        }
                    }
                });
                let everything = if displays.len() > 1 {
                    "Fullscreen (all displays)"
                } else {
                    "Fullscreen"
                };
                if ui.button(everything).clicked() {
                    actions.push(Action::CaptureTarget(capture::Target::Everything));
                    ui.close();
                }
            });
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

        if let Some(paths) = self.gallery.drag_out.take() {
            crate::output::drag_out(&self.window, &paths);
            // The drag swallowed the button's release; let egui know.
            let pos = ctx.input(|i| i.pointer.latest_pos()).unwrap_or_default();
            let events = &mut self.state.egui_input_mut().events;
            events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            });
            events.push(egui::Event::PointerGone);
            self.window.request_redraw();
        }

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

/// A small down arrow under the capture button, for its menu.
fn chevron_button(ui: &mut egui::Ui) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(40.0, 14.0), Sense::click());
    let visuals = ui.visuals();
    let fg = if resp.hovered() {
        visuals.strong_text_color()
    } else {
        visuals.weak_text_color()
    };
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(rect, 4.0, visuals.widgets.hovered.weak_bg_fill);
    }
    let c = rect.center();
    p.add(Shape::line(
        vec![c + vec2(-4.0, -2.0), c + vec2(0.0, 2.0), c + vec2(4.0, -2.0)],
        Stroke::new(1.6, fg),
    ));
    resp.on_hover_text("More ways to capture")
        .on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// `s` cut to `max` characters, with an ellipsis if it was longer.
fn ellipsize(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('\u{2026}');
    out
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
        NavIcon::Stats => {
            // Three columns of rising height on a baseline.
            let w = r.width() * 0.2;
            for (x, h) in [(0.12, 0.45), (0.5, 0.75), (0.88, 0.3)] {
                let col = Rect::from_min_max(
                    pos2(at(x, 0.0).x - w / 2.0, at(0.0, 0.92 - h).y),
                    pos2(at(x, 0.0).x + w / 2.0, at(0.0, 0.92).y),
                );
                p.rect_filled(col, 1.5, color);
            }
            p.line_segment([at(-0.05, 1.0), at(1.05, 1.0)], s);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_sizes_and_times() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1536), "1.5 KB");
        assert_eq!(bytes(6 * 1024 * 1024 * 1024), "6.0 GB");
        assert_eq!(bytes(250 * 1024 * 1024), "250 MB");
        assert_eq!(speed(13.0 * 1024.0 * 1024.0, SpeedUnit::Bytes), "13 MB/s");
        assert_eq!(speed(12_500_000.0, SpeedUnit::Bits), "100 Mbps");
        assert_eq!(speed(250_000_000.0, SpeedUnit::Bits), "2.0 Gbps");
        assert_eq!(speed(50_000.0, SpeedUnit::Bits), "400 Kbps");
        assert_eq!(duration(42.0), "42 s");
        assert_eq!(duration(61.0), "2 min");
        assert_eq!(duration(3.0 * 3600.0 + 125.0), "3 h 2 min");
    }

    /// Renders the uploads panel to `target/uploads-preview.png`:
    /// `cargo test uploads_preview -- --ignored`.
    #[test]
    #[ignore]
    fn uploads_preview() {
        use std::sync::atomic::Ordering;
        let transfer = |file: &str, dest: &str, sent: u64, total: u64, secs: u64| {
            let t = Transfer {
                file: file.into(),
                destination: dest.into(),
                progress: Default::default(),
                started: Instant::now() - Duration::from_secs(secs),
            };
            t.progress.sent.store(sent, Ordering::Relaxed);
            t.progress.total.store(total, Ordering::Relaxed);
            t
        };
        const GB: u64 = 1024 * 1024 * 1024;
        let list = [
            transfer("Recording 2026-10-05 14.02.mp4", "R2", 2 * GB + GB / 2, 6 * GB, 200),
            transfer("Screenshot 2026-10-05 14.10.png", "S3 backup", 0, 900_000, 0),
        ];
        crate::preview::render("uploads-preview", [820, 520], 1.0, |root| {
            egui::Panel::bottom("uploads")
                .resizable(false)
                .frame(
                    egui::Frame::side_top_panel(root.style())
                        .inner_margin(egui::Margin::symmetric(20, 10)),
                )
                .show(root, |ui| {
                    for (t, unit) in list.iter().zip([SpeedUnit::Bytes, SpeedUnit::Bits]) {
                        transfer_row(ui, t, unit);
                    }
                });
            egui::CentralPanel::default().show(root, |ui| {
                ui.heading("Recent");
            });
        });
    }
}
