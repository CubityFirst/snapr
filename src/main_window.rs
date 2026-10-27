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
use crate::theme::{self, ACCENT};
use crate::tools::{self, Tools};

const NAV_WIDTH: f32 = 60.0;
/// The Settings page's list of sections and search box, with the gap
/// after it.
const SETTINGS_SIDEBAR_WIDTH: f32 = 200.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Recent,
    Tools,
    Stats,
    /// The General tab of Settings.
    Settings,
    Capture,
    Hotkeys,
    Naming,
    Destinations,
    Recording,
    About,
    /// First-time setup, in place of the icon bar and pages.
    Setup,
}

pub enum Action {
    Capture,
    /// Capture a window, display, everything or the last region, without
    /// picking a region first.
    CaptureTarget(capture::Target),
    /// Pick a region to record, or stop the recording in progress.
    Record,
    /// New settings, and new secret keys to store (upload id → key).
    SaveSettings(Settings, HashMap<String, String>),
    TestUpload(Upload, Option<String>),
    /// Upload snapr's icon to a destination, as a screenshot would be.
    UploadIcon(Upload, Option<String>),
    CheckForUpdates,
    /// Have the desktop ask the user to let snapr take screenshots.
    #[cfg(target_os = "linux")]
    AllowScreenshots,
    /// Quit and start the updated program.
    RestartToUpdate,
    /// Copy snapr to the Programs folder and start it from there, with its
    /// window on Settings if true.
    #[cfg(windows)]
    MoveToPrograms { settings: bool },
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
    /// Use the capture tools' settings from an imported file.
    ImportTools(crate::remembered::Remembered),
}

impl From<settings_ui::Action> for Action {
    fn from(a: settings_ui::Action) -> Self {
        match a {
            settings_ui::Action::Save(s, secrets) => Action::SaveSettings(s, secrets),
            settings_ui::Action::Reveal(p) => Action::Reveal(p),
            settings_ui::Action::TestUpload(u, secret) => Action::TestUpload(u, secret),
            settings_ui::Action::UploadIcon(u, secret) => Action::UploadIcon(u, secret),
            settings_ui::Action::CheckForUpdates => Action::CheckForUpdates,
            #[cfg(target_os = "linux")]
            settings_ui::Action::AllowScreenshots => Action::AllowScreenshots,
            settings_ui::Action::RestartToUpdate => Action::RestartToUpdate,
            #[cfg(windows)]
            settings_ui::Action::MoveToPrograms { settings } => Action::MoveToPrograms { settings },
            settings_ui::Action::OpenUrl(u) => Action::OpenUrl(u),
            settings_ui::Action::ImportTools(t) => Action::ImportTools(t),
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
    setup: crate::setup::Setup,
    /// The search in the Settings sidebar.
    search: crate::settings_search::Search,
    /// The capture hotkey, for hints.
    pub hotkey: String,
    /// The record hotkey, for the capture menu.
    pub record_hotkey: String,
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
    /// The theme the title bar was last put in; `None` to put it in the
    /// picked one at the next paint.
    title_bar: Option<egui::ThemePreference>,
}

const SIZE: LogicalSize<f64> = LogicalSize::new(980.0, 660.0);
const MIN_SIZE: LogicalSize<f64> = LogicalSize::new(800.0, 440.0);

/// The window's size when it opens: `SIZE`, but no more than about 70% of
/// the screen's width and 75% of its height (leaving room for the taskbar),
/// so it doesn't fill a small or highly scaled screen; never under
/// `MIN_SIZE`.
fn opening_size(event_loop: &ActiveEventLoop) -> LogicalSize<f64> {
    // Wayland doesn't say which screen is the primary one.
    let Some(monitor) = event_loop.primary_monitor() else { return SIZE };
    let screen: LogicalSize<f64> = monitor.size().to_logical(monitor.scale_factor());
    fit(screen)
}

fn fit(screen: LogicalSize<f64>) -> LogicalSize<f64> {
    LogicalSize::new(
        SIZE.width.min(screen.width * 0.7).max(MIN_SIZE.width),
        SIZE.height.min(screen.height * 0.75).max(MIN_SIZE.height),
    )
}

impl MainWindow {
    #[allow(clippy::too_many_arguments)]
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
            .with_inner_size(opening_size(event_loop))
            .with_min_inner_size(MIN_SIZE)
            .with_window_icon(icon);
        // Names it as `snapr.desktop`'s, so GNOME knows it's snapr's: it
        // only shows the dialog for a permission to the focused app's
        // request (see `portal`). On X11 it's the window class.
        #[cfg(target_os = "linux")]
        let attrs = winit::platform::wayland::WindowAttributesExtWayland::with_name(attrs, "snapr", "snapr");
        let window = Arc::new(event_loop.create_window(attrs).map_err(|e| e.to_string())?);

        let ctx = egui::Context::default();
        theme::apply(&ctx);
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
            setup: Default::default(),
            search: Default::default(),
            hotkey: settings.hotkey.clone(),
            record_hotkey: settings.record_hotkey.clone(),
            folder: PathBuf::from(&settings.folder),
            repaint_at: None,
            transfers,
            speed_unit: settings.speed_unit,
            last_region: None,
            capture_menu: Default::default(),
            title_bar: None,
        })
    }

    pub fn show_page(&mut self, page: Page) {
        // Setup stays until it's finished or skipped; errors (e.g. a hotkey
        // taken by another program) show on its last page.
        if self.page != Page::Setup {
            self.page = page;
        }
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
            // Windows puts the title bar back in the system's theme when
            // that changes, even with another one picked.
            WindowEvent::ThemeChanged(_) => self.title_bar = None,
            _ => {}
        }
        if self.state.on_window_event(&self.window, event).repaint {
            self.window.request_redraw();
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        if self.page == Page::Setup {
            return self.setup_ui(ui, actions);
        }
        let nav_fill = theme::palette(ui).nav;
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
                        gallery::Action::CaptureTarget(t) => Action::CaptureTarget(t),
                        gallery::Action::Record => Action::Record,
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
                    self.tools.ui(
                        ui,
                        &self.form.saved.tool_hotkeys,
                        &mut self.form.draft.color_picker,
                        &mut tool_actions,
                    );
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
                _ => {
                    let mut form_actions = Vec::new();
                    settings_page(ui, &mut self.page, &mut self.search, &mut self.form, &mut form_actions);
                    actions.extend(form_actions.into_iter().map(Action::from));
                    if std::mem::take(&mut self.form.run_setup) {
                        self.setup = Default::default();
                        self.page = Page::Setup;
                    }
                }
            });
        let mut form_actions = Vec::new();
        self.form.autosave(ui.ctx(), &mut form_actions);
        actions.extend(form_actions.into_iter().map(Action::from));
        self.drop_ui(ui, actions);
    }

    /// First-time setup, filling the window.
    fn setup_ui(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let mut form_actions = Vec::new();
        egui::CentralPanel::default()
            .frame(
                egui::Frame::central_panel(ui.style())
                    .inner_margin(egui::Margin::symmetric(32, 24)),
            )
            .show(ui, |ui| match self.setup.ui(ui, &mut self.form, &mut form_actions) {
                Some(crate::setup::Finish::Done) => self.page = Page::Recent,
                Some(crate::setup::Finish::OpenSettings) => self.page = Page::Settings,
                None => {}
            });
        self.form.autosave(ui.ctx(), &mut form_actions);
        actions.extend(form_actions.into_iter().map(Action::from));
    }

    /// Saves settings edited too recently to have been saved yet.
    pub fn flush_settings(&mut self) -> Option<Action> {
        self.form.flush().map(Action::from)
    }

    /// The arrow under the capture button, and its menu of other things to
    /// capture: the last region, a window, a display, everything or a
    /// recording.
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
                theme::compact_menu(ui);
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
                    theme::compact_menu(ui);
                    if let Some(id) = window_items(ui, windows) {
                        actions.push(Action::CaptureTarget(capture::Target::Window(id)));
                        ui.close();
                    }
                });
                ui.menu_button("Display", |ui| {
                    theme::compact_menu(ui);
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
                ui.separator();
                let record = egui::Button::new("Record region\u{2026}")
                    .shortcut_text(self.record_hotkey.as_str());
                if ui.add(record).clicked() {
                    actions.push(Action::Record);
                    ui.close();
                }
            });
    }

    /// Uploads files dropped on the window, with a hint while they're dragged
    /// over it. Files dragged here from snapr itself (the gallery, a toast)
    /// are left alone.
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
        if !dropped.is_empty() && !crate::output::own_drop(&self.window) {
            actions.push(Action::UploadFiles(dropped));
        }
        if crate::policy::uploads_disabled() {
            return; // dropping only reports why not
        }
        if !hovering {
            return;
        }
        let rect = ui.ctx().content_rect();
        let painter = ui.ctx().layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("drop"),
        ));
        let p = theme::palette(ui);
        let shade = if p.dark {
            Color32::from_black_alpha(160)
        } else {
            Color32::from_white_alpha(200)
        };
        painter.rect_filled(rect, 0.0, shade);
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
            p.text_strong,
        );
    }

    pub fn paint(&mut self) -> Vec<Action> {
        let mut actions = Vec::new();
        let input = self.state.take_egui_input(&self.window);
        let ctx = self.state.egui_ctx().clone();
        // The theme being picked in Settings, before it's saved for the
        // other windows.
        let theme = theme::preference(self.form.draft.theme);
        ctx.set_theme(theme);
        if self.title_bar != Some(theme) {
            self.title_bar = Some(theme);
            self.window.set_theme(match theme {
                egui::ThemePreference::System => None,
                egui::ThemePreference::Light => Some(winit::window::Theme::Light),
                egui::ThemePreference::Dark => Some(winit::window::Theme::Dark),
            });
        }
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

/// The Settings page: the sidebar of sections and search, and the section
/// that's showing beside it.
fn settings_page(
    ui: &mut egui::Ui,
    page: &mut Page,
    search: &mut crate::settings_search::Search,
    form: &mut Form,
    actions: &mut Vec<settings_ui::Action>,
) {
    // What was showing as the frame began, should the sidebar change it.
    let showing = *page;
    egui::Panel::left("settings-sidebar")
        .exact_size(SETTINGS_SIDEBAR_WIDTH)
        .resizable(false)
        .frame(egui::Frame::new().inner_margin(egui::Margin { right: 16, ..Default::default() }))
        .show(ui, |ui| search.sidebar(ui, page, &form.draft));
    if *page != Page::Hotkeys {
        form.recording = false;
    }
    egui::Frame::new()
        .inner_margin(egui::Margin { left: 20, ..Default::default() })
        .show(ui, |ui| {
            // Above the section, rather than in the status line at the end of
            // it, where a short window hides it: nothing works without it.
            if let Some((msg, _)) = &form.status
                && settings_ui::permission_card(ui, msg, actions)
            {
                ui.add_space(16.0);
            }
            ui.heading(crate::settings_search::section_name(showing));
            ui.add_space(10.0);
            match showing {
                Page::Capture => form.capture_ui(ui, actions),
                Page::Hotkeys => form.hotkeys_ui(ui, actions),
                Page::Naming => form.naming_ui(ui, actions),
                Page::Destinations => form.destinations_ui(ui, actions),
                Page::Recording => form.recording_ui(ui, actions),
                Page::About => form.about_ui(ui, actions),
                _ => form.ui(ui, actions),
            }
        });
}

/// A button on the icon bar: Capture filled with the accent, the page
/// that's showing tinted with it.
fn nav_button(ui: &mut egui::Ui, icon: NavIcon, selected: bool, tip: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(40.0), Sense::click());
    let palette = theme::palette(ui);
    let capture = matches!(icon, NavIcon::Capture);
    let (fill, fg) = if capture {
        // snapr's logo, in its blue.
        if resp.hovered() {
            (palette.control_hover, theme::ACCENT_HOVER)
        } else {
            (Color32::TRANSPARENT, ACCENT)
        }
    } else if selected {
        (palette.selected, ACCENT)
    } else if resp.hovered() {
        (palette.control_hover, palette.text_strong)
    } else {
        (Color32::TRANSPARENT, palette.text_weak)
    };
    let p = ui.painter();
    p.rect_filled(rect, 9.0, fill);
    // The logo on its 32-unit grid comes out crisp at 24 px.
    let inset = if capture { 8.0 } else { 10.0 };
    draw_icon(p, rect.shrink(inset), icon, fg);
    resp.on_hover_text(tip)
        .on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A small down arrow under the capture button, for its menu.
fn chevron_button(ui: &mut egui::Ui) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(40.0, 14.0), Sense::click());
    let palette = theme::palette(ui);
    let fg = if resp.hovered() {
        palette.text_strong
    } else {
        palette.text_weak
    };
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(rect, 4.0, palette.control_hover);
    }
    let c = rect.center();
    p.add(Shape::line(
        vec![c + vec2(-4.0, -2.0), c + vec2(0.0, 2.0), c + vec2(4.0, -2.0)],
        Stroke::new(1.6, fg),
    ));
    resp.on_hover_text("More ways to capture")
        .on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The windows of a capture menu, one item each; returns the id of the one
/// clicked.
pub(crate) fn window_items(ui: &mut egui::Ui, windows: &[capture::WindowInfo]) -> Option<u32> {
    if windows.is_empty() {
        ui.weak("No windows");
    }
    let mut clicked = None;
    egui::ScrollArea::vertical()
        .max_height(420.0)
        .show(ui, |ui| {
            for w in windows {
                let button =
                    egui::Button::new(ellipsize(&w.title, 60)).shortcut_text(w.app.as_str());
                if ui.add(button).on_hover_text(&w.title).clicked() {
                    clicked = Some(w.id);
                }
            }
        });
    clicked
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
        NavIcon::Capture => crate::settings_ui::paint_logo(p, r, color),
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
            // A toolbox: a handle on top, the lid's seam across the
            // middle, and a latch where it meets the front. Drawn on a
            // 24-unit grid.
            let g = |x: f32, y: f32| at(x / 24.0, y / 24.0);
            let rounded = |min: Pos2, max: Pos2, round: f32| {
                let rect = Rect::from_min_max(min, max);
                p.rect_stroke(rect, round, s, egui::StrokeKind::Middle);
            };
            rounded(g(8.5, 4.0), g(15.5, 8.0), 1.25);
            rounded(g(2.5, 8.0), g(21.5, 20.5), 1.7);
            p.line_segment([g(2.5, 13.5), g(10.5, 13.5)], s);
            p.line_segment([g(13.5, 13.5), g(21.5, 13.5)], s);
            rounded(g(10.5, 12.0), g(13.5, 15.0), 0.5);
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
    fn opening_size_fits_the_screen() {
        let size = |w, h| {
            let s = fit(LogicalSize::new(w, h));
            (s.width.round(), s.height.round())
        };
        assert_eq!(size(1920.0, 1080.0), (980.0, 660.0)); // 1080p at 100%
        assert_eq!(size(1280.0, 800.0), (896.0, 600.0)); // 2560x1600 at 200%
        assert_eq!(size(1024.0, 576.0), (800.0, 440.0)); // never under the minimum
    }

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

    /// Renders the Settings page at the window's usual width and its
    /// narrowest, with a search typed in the second, to
    /// `target/settings-page-<width>-preview.png`:
    /// `cargo test settings_page_preview -- --ignored`.
    #[test]
    #[ignore]
    fn settings_page_preview() {
        // The page's width beside the icon bar, inside its margins.
        for (window, query) in [(980, ""), (800, "sound")] {
            let mut page = Page::Recording;
            let mut search = crate::settings_search::Search::with_query(query);
            let mut form = Form::new(&Settings::default());
            let name = format!("settings-page-{window}-preview");
            crate::preview::render(&name, [window - 60, 640], 1.0, |root| {
                crate::theme::apply(root.ctx());
                egui::CentralPanel::default()
                    .frame(egui::Frame::central_panel(root.style()).inner_margin(egui::Margin::symmetric(20, 16)))
                    .show(root, |ui| settings_page(ui, &mut page, &mut search, &mut form, &mut Vec::new()));
            });
        }
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
            crate::theme::apply(root.ctx());
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
