// No console window for release builds on Windows; snapr lives in the tray.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod about;
mod annotate;
mod atomic;
#[cfg(any(windows, target_os = "linux"))]
mod autostart;
mod audio;
mod capture;
mod cli;
mod color_names;
mod combine;
mod cursor;
mod decode;
mod destinations_ui;
#[cfg(windows)]
mod dxgi;
mod encode;
mod fonts;
mod gallery;
mod gpu;
#[cfg(windows)]
mod hdr;
mod history;
mod hotkey;
#[cfg(windows)]
mod install;
mod locale;
#[cfg(target_os = "macos")]
mod macos;
mod main_window;
#[cfg(windows)]
mod mf;
mod naming;
mod output;
mod overlay;
mod pin;
mod policy;
mod player;
mod pomf;
#[cfg(target_os = "linux")]
mod portal;
#[cfg(test)]
mod preview;
mod qr;
mod record;
mod record_ui;
mod remembered;
mod secrets;
mod session;
mod settings;
mod settings_search;
mod settings_ui;
mod setup;
mod sound;
#[cfg(windows)]
mod start_menu;
mod stats;
mod stats_ui;
mod theme;
mod thumbnail;
mod toast;
mod toolbar;
mod touch;
mod tools;
mod transfer;
mod tray;
mod update;
mod upload;
#[cfg(target_os = "linux")]
mod x11;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::event::{StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::WindowId;

use crate::annotate::{RedactImage, Style, Tool};
use crate::gallery::Gallery;
use crate::gpu::Gpu;
use crate::hotkey::GlobalHotkey;
use crate::main_window::{Action, MainWindow, Page};
use crate::output::Output;
use crate::pin::{Pin, Place};
use crate::session::{Outcome, Purpose, Session};
use crate::settings::{Loaded, RecordFormat, Settings, ToolAction, ToolHotkey, TrayClick, Upload};
use crate::settings_ui::Form;
use crate::settings_ui::Test;
use crate::sound::Sound;
use crate::toast::Toast;
use crate::tray::{Tray, TrayAction};

#[derive(Debug)]
enum UserEvent {
    Hotkey,
    RecordHotkey,
    /// A screen recording was written (or failed), with its id.
    RecordingDone(u64, Result<Option<record::Recorded>, String>, naming::Context),
    Tray(TrayAction),
    /// Something happened to a screenshot (saved, copied, uploaded, failed).
    Output(output::Event),
    /// An upload destination's test finished.
    UploadTest(String, Result<String, String>),
    /// A thumbnail finished loading for the main window.
    ThumbnailReady,
    /// Deleting an uploaded copy finished (destination name, result).
    RemoteDeleted(PathBuf, String, Result<(), String>),
    /// A saved screenshot's corner preview was decoded.
    /// `None` for a file without a picture, which gets an icon.
    ToastReady(PathBuf, Option<egui::ColorImage>),
    /// Mouse input for the corner preview, when it's an X11 window.
    #[cfg(target_os = "linux")]
    ToastX11(x11::X11Event),
    /// The desktop's dark style was read, or changed (Linux).
    #[cfg(target_os = "linux")]
    DesktopStyleChanged,
    /// Mouse input for the recording bar, when it's an X11 window.
    #[cfg(target_os = "linux")]
    RecordingX11(x11::X11Event),
    /// A region was read for QR codes: their text, and a preview of it.
    QrScanned(Vec<String>, egui::ColorImage),
    /// A tool's global hotkey was pressed.
    ToolHotkey(ToolAction),
    /// Images from Recent were stitched together (how many, the result).
    Combined(usize, Result<image::RgbaImage, String>),
    /// An image file to pin to the screen was read.
    PinLoaded(PathBuf, Result<image::RgbaImage, String>),
    /// Time for the automatic check for updates.
    UpdateDue,
    /// Checking for or installing an update got further.
    Update(update::Status),
    /// The desktop couldn't bind the hotkeys (Wayland binds them later).
    HotkeyProblem(String),
    /// The keys the desktop has bound to the hotkeys changed.
    HotkeyKeysChanged,
    /// The desktop answered snapr's request to take screenshots.
    #[cfg(target_os = "linux")]
    ScreenshotsAllowed(Result<(), String>),
}

/// A global hotkey, or why it's unavailable.
struct Hotkeys(Result<GlobalHotkey, String>);

impl Hotkeys {
    /// A hotkey that sends `event` (made fresh each time) when pressed.
    /// `id` and `description` name it to the desktop (see `GlobalHotkey::new`).
    fn new(
        proxy: EventLoopProxy<UserEvent>,
        id: &str,
        description: &str,
        event: impl Fn() -> UserEvent + Send + 'static,
    ) -> Self {
        let proxy = Mutex::new(proxy);
        Self(GlobalHotkey::new(id, description, Box::new(move || {
            let _ = proxy.lock().unwrap().send_event(event());
        })))
    }

    fn set(&mut self, spec: &str) -> Result<(), String> {
        self.0.as_mut().map_err(|e| e.clone())?.set(spec)
    }

    /// Like `set`, but an empty spec removes the hotkey (or, where the
    /// desktop picks the keys, lets it ask for some).
    fn set_optional(&mut self, spec: &str) -> Result<(), String> {
        if spec.trim().is_empty() && !hotkey::desktop_picks_keys() {
            self.clear();
            return Ok(());
        }
        self.set_or_offer(spec)
    }

    /// Like `set`, but where the desktop picks the keys an empty spec lets
    /// it ask for some.
    fn set_or_offer(&mut self, spec: &str) -> Result<(), String> {
        let h = self.0.as_mut().map_err(|e| e.clone())?;
        if spec.trim().is_empty() && hotkey::desktop_picks_keys() {
            return h.offer();
        }
        h.set(spec)
    }

    /// Unregisters the hotkey and waits until it's free for another.
    fn clear(&mut self) {
        if let Ok(h) = &mut self.0 {
            h.clear();
        }
    }

    fn is_registered(&self) -> bool {
        self.0.as_ref().is_ok_and(|h| h.current().is_some())
    }
}

/// A capture started from the main window.
#[derive(Debug, Clone, Copy)]
enum Start {
    /// Pick a region (or pixel) on the frozen screen.
    Pick(Purpose),
    /// Take this straight away.
    Grab(capture::Target),
}

/// A recording in progress and its border and buttons.
struct ActiveRecording {
    /// Tells its `RecordingDone` from an earlier (restarted) one's.
    id: u64,
    recording: record::Recording,
    ui: Option<record_ui::RecordingUi>,
    /// The selection it was started from, and its naming, for Restart.
    selection: capture::Rect,
    context: naming::Context,
}

struct App {
    proxy: EventLoopProxy<UserEvent>,
    settings: Settings,
    once: bool,
    gpu: Result<Gpu, String>,
    hotkeys: Option<Hotkeys>,
    record_hotkeys: Option<Hotkeys>,
    /// The tools' hotkeys (none with `--once`).
    tool_hotkeys: Vec<Hotkeys>,
    /// The screen recording in progress.
    recording: Option<ActiveRecording>,
    tray: Option<Tray>,
    /// How far the uploads have got, as last shown on the tray icon.
    tray_progress: Option<tray::Progress>,
    output: Output,
    /// Open captures. Pressing the hotkey during a capture stacks a new one
    /// on top (so the overlay itself can be captured); the last is active.
    sessions: Vec<Session>,
    /// Annotation colour and size, remembered between captures.
    style: Style,
    /// Pixels across the magnifier (its zoom), remembered between captures.
    magnifier: u32,
    /// Both of those (and the text tool's font) as last saved, for the next
    /// run.
    remembered: remembered::Remembered,
    /// The saved font, looked up among the installed fonts at the first
    /// capture rather than holding up the start.
    font_pending: Option<String>,
    main_window: Option<MainWindow>,
    /// The preview in the corner of the screen after a capture.
    toast: Option<Toast>,
    /// Images pinned to the screen.
    pins: Vec<Pin>,
    /// The latest upload's screenshot and link, for a preview that's still
    /// being decoded when the upload finishes.
    last_upload: Option<(PathBuf, String)>,
    /// Files whose corner preview waits until their uploads finish, so it
    /// can carry the link: with the preview already made (recordings), or
    /// `None` to decode it then.
    toast_after_upload: HashMap<PathBuf, Option<egui::ColorImage>>,
    /// The image redaction tool's picture, with the file and its modified
    /// time when it was loaded, so it's only decoded again when it changes.
    redact_image: Option<(PathBuf, Option<std::time::SystemTime>, RedactImage)>,
    /// A capture started from the main window, delayed until it's hidden.
    capture_at: Option<(Instant, Start)>,
    /// A recording of this region, started once the capture overlay that
    /// chose it is off the screen.
    record_at: Option<(Instant, capture::Rect, naming::Context)>,
    /// Show the main window again once the capture it started is over.
    restore_main_window: bool,
    /// The app that was in front when the capture started, to give the
    /// focus back to once it's over.
    #[cfg(target_os = "macos")]
    focus_back: Option<objc2::rc::Retained<objc2_app_kit::NSRunningApplication>>,
    /// Chime when the next copy-to-clipboard finishes (copies from the
    /// Recent page; a capture already played the shutter).
    chime_on_copy: bool,
    /// Shown in the settings page, e.g. a startup error.
    status: Option<(String, bool)>,
    last_capture: Option<PathBuf>,
    /// The last screenshot's region, for capturing it again.
    last_region: Option<capture::Rect>,
    open_settings_on_start: bool,
    /// Open the window on Recent rather than Settings: snapr was just moved.
    open_recent_on_start: bool,
    /// What's been captured and which tools were used, for the Stats page.
    stats: stats::Stats,
    /// Where checking for and installing updates is up to.
    update: update::Status,
    /// Start the program again once the event loop exits (to run an update).
    restart: bool,
    /// Start this copy of the program once the event loop exits, with its
    /// window on Settings if true: snapr's been moved to the Programs folder.
    #[cfg(windows)]
    moved_to: Option<(PathBuf, bool)>,
}

impl App {
    fn start_capture(&mut self, event_loop: &ActiveEventLoop) {
        self.open_session(event_loop, Purpose::Screenshot);
    }

    fn run_tool(&mut self, event_loop: &ActiveEventLoop, action: ToolAction) {
        let purpose = match action {
            ToolAction::PickColor => Purpose::PickColor,
            ToolAction::ScanQr => Purpose::Scan,
            ToolAction::PinRegion => Purpose::Pin,
        };
        self.open_session(event_loop, purpose);
    }

    /// The record hotkey: stops the recording in progress, or picks a region
    /// to start one.
    fn toggle_recording(&mut self, event_loop: &ActiveEventLoop) {
        if self.recording.is_some() {
            return self.recording_action(event_loop, record_ui::Action::Stop);
        }
        if self.sessions.is_empty() && self.record_at.is_none() {
            self.open_session(event_loop, Purpose::Record);
        }
    }

    /// A button on the recording bar (or the hotkey, for Stop).
    fn recording_action(&mut self, event_loop: &ActiveEventLoop, action: record_ui::Action) {
        use record_ui::Action;
        let Some(active) = &mut self.recording else {
            return;
        };
        match action {
            Action::TogglePause => {
                let paused = !active.ui.as_ref().is_some_and(|u| u.paused());
                active.recording.set_paused(paused);
                if let (Some(ui), Ok(gpu)) = (&mut active.ui, &self.gpu) {
                    ui.set_paused(gpu, paused);
                }
            }
            Action::Stop => {
                let mut active = self.recording.take().expect("checked above");
                active.recording.stop();
                self.set_status("Finishing the recording\u{2026}".into());
            }
            Action::Abort => {
                self.recording
                    .take()
                    .expect("checked above")
                    .recording
                    .abort();
            }
            Action::Restart => {
                let mut old = self.recording.take().expect("checked above");
                old.recording.abort();
                drop(old.ui.take());
                let context = naming::Context {
                    time: chrono::Local::now(),
                    ..old.context
                };
                self.start_recording(event_loop, old.selection, context);
            }
        }
    }

    fn set_status(&mut self, msg: String) {
        println!("{msg}");
        self.status = Some((msg, false));
        if let Some(w) = &mut self.main_window {
            w.form.status = self.status.clone();
            w.window.request_redraw();
        }
    }

    /// Pins only redraw when something happens to them, so after the theme
    /// changes they're asked to.
    fn redraw_pins(&self) {
        for pin in &self.pins {
            pin.window.request_redraw();
        }
    }

    fn start_recording(
        &mut self,
        event_loop: &ActiveEventLoop,
        rect: capture::Rect,
        ctx: naming::Context,
    ) {
        let naming = match self.settings.naming() {
            Ok(n) => n,
            Err(e) => return self.report_on(event_loop, e, true, Page::Naming),
        };
        let out = naming
            .path_for(&ctx)
            .with_extension(self.settings.record_format.extension());
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let proxy = Mutex::new(self.proxy.clone());
        let done_ctx = ctx.clone();
        let done: record::Done = Box::new(move |result| {
            let _ = proxy
                .lock()
                .unwrap()
                .send_event(UserEvent::RecordingDone(id, result, done_ctx));
        });
        match record::Recording::start(
            rect,
            self.settings.record_fps,
            &crate::encode::FfmpegConfig {
                program: self.settings.ffmpeg_path.clone(),
                options: self.settings.ffmpeg.clone(),
            },
            self.settings.record_with_ffmpeg,
            &self.settings.audio_sources(),
            self.settings.record_cursor,
            self.settings.record_hdr,
            self.settings.record_format == RecordFormat::Av1,
            out,
            done,
        ) {
            Ok(recording) => {
                let ui = match &self.gpu {
                    Ok(gpu) => {
                        #[cfg(target_os = "linux")]
                        let send_x11 = {
                            let proxy = Mutex::new(self.proxy.clone());
                            move |e| {
                                let _ = proxy.lock().unwrap().send_event(UserEvent::RecordingX11(e));
                            }
                        };
                        record_ui::RecordingUi::open(
                            event_loop,
                            gpu,
                            recording.rect,
                            #[cfg(target_os = "linux")]
                            send_x11,
                        )
                        .inspect_err(|e| eprintln!("recording border: {e}"))
                        .ok()
                    }
                    Err(_) => None,
                };
                self.recording = Some(ActiveRecording {
                    id,
                    recording,
                    ui,
                    selection: rect,
                    context: ctx,
                });
                let stop = match self.settings.record_hotkey.trim() {
                    "" => String::new(),
                    hk => format!(" \u{2014} press {hk} to stop"),
                };
                self.set_status(format!("Recording{stop}"));
            }
            Err(e) => {
                self.show_main_window_again();
                self.report_on(event_loop, format!("recording failed: {e}"), true, Page::Recording)
            }
        }
    }

    /// Shows the main window again if a capture started from it hid it.
    fn show_main_window_again(&mut self) {
        if std::mem::take(&mut self.restore_main_window)
            && let Some(w) = &self.main_window
        {
            w.window.set_visible(true);
        }
    }

    fn recording_done(
        &mut self,
        event_loop: &ActiveEventLoop,
        id: u64,
        result: Result<Option<record::Recorded>, String>,
        ctx: naming::Context,
    ) {
        // Still showing: it couldn't start, or failed partway.
        if self.recording.as_ref().is_some_and(|r| r.id == id) {
            self.recording = None;
        }
        if self.recording.is_none() {
            self.show_main_window_again();
        }
        let (path, first_frame, warning) = match result {
            Ok(Some(r)) => (r.path, r.first_frame, r.warning),
            Ok(None) => return self.set_status("Recording discarded".into()),
            Err(e) => return self.report_on(event_loop, format!("recording failed: {e}"), true, Page::Recording),
        };
        println!("recorded {}", path.display());
        self.play(Sound::Done);
        self.count(|s| s.count(stats::Kind::Recording, chrono::Local::now()));
        self.last_capture = Some(path.clone());
        let uploading = !policy::uploads_disabled() && self.settings.uploads.iter().any(|u| u.enabled);
        if uploading {
            self.output.upload_file(path.clone(), ctx);
        }
        // The video's fine, so a warning (say, H.264 where AV1 wasn't
        // available) only goes on the status line, without opening the window.
        self.status = Some(match warning {
            Some(w) => {
                eprintln!("recording: {w}");
                (format!("Recording saved to {} \u{2014} {w}", path.display()), true)
            }
            None => (format!("Recording saved to {}", path.display()), false),
        });
        if let Some(w) = &mut self.main_window {
            w.form.status = self.status.clone();
            w.form.last_capture = self.last_capture.clone();
            w.window.request_redraw();
        }
        history::add(&path);
        if let Some(w) = &mut self.main_window {
            w.gallery.add(path.clone());
        }
        if self.settings.show_toast {
            let preview = toast::preview_of(first_frame);
            if uploading {
                self.toast_after_upload.insert(path, Some(preview));
            } else {
                self.show_toast(event_loop, path, Some(preview));
            }
        }
    }

    fn open_session(&mut self, event_loop: &ActiveEventLoop, purpose: Purpose) {
        if self.sessions.len() >= MAX_NESTED_CAPTURES {
            return;
        }
        if self.main_window.as_ref().is_some_and(|w| w.form.recording) {
            return; // the user is recording a new hotkey
        }
        let gpu = match &self.gpu {
            Ok(g) => g,
            Err(e) => return self.report(event_loop, e.clone(), true),
        };
        if let Some(family) = self.font_pending.take() {
            self.style.text.font = fonts::find(&family);
        }
        let s = &self.settings;
        // Every capture starts by selecting a region; a nested capture keeps
        // the colour and size of the one below.
        let style = self.sessions.last().map_or(self.style, |s| s.style);
        let magnifier = self.sessions.last().map_or(self.magnifier, |s| s.magnifier);
        // Our own windows a click can take, like any other app's.
        // Before the overlay takes the focus.
        #[cfg(target_os = "macos")]
        let in_front = self.sessions.is_empty().then(macos::other_app_in_front).flatten();
        let snap_to: Vec<&winit::window::Window> = self
            .main_window
            .iter()
            .map(|w| &*w.window)
            .chain(self.pins.iter().map(|p| &*p.window))
            .collect();
        match Session::new(
            event_loop,
            gpu,
            s.annotate && purpose == Purpose::Screenshot,
            s.overlay_fps,
            s.crosshair_info,
            magnifier,
            Tool::Select,
            style,
            &snap_to,
        ) {
            Ok(mut session) => {
                #[cfg(target_os = "macos")]
                if self.sessions.is_empty() {
                    self.focus_back = in_front;
                }
                session.purpose = purpose;
                if purpose == Purpose::Screenshot && self.settings.annotate {
                    session.redact_image = self.load_redact_image();
                }
                self.sessions.push(session);
            }
            Err(e) => {
                self.report(event_loop, e, true);
                if self.once {
                    event_loop.exit();
                }
            }
        }
    }

    /// The image redaction tool's picture, if one is set and can be read.
    fn load_redact_image(&mut self) -> Option<RedactImage> {
        let path = PathBuf::from(self.settings.redact_image.trim());
        if path.as_os_str().is_empty() {
            return None;
        }
        let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        let cached = self
            .redact_image
            .as_ref()
            .is_some_and(|(p, m, _)| *p == path && *m == modified);
        if !cached {
            match RedactImage::load(&path, false) {
                Ok(img) => self.redact_image = Some((path, modified, img)),
                Err(e) => {
                    eprintln!("redaction image: {e}");
                    self.redact_image = None;
                    return None;
                }
            }
        }
        let mut img = self.redact_image.as_ref()?.2.clone();
        img.stretch = self.settings.redact_stretch;
        Some(img)
    }

    fn end_capture(&mut self, event_loop: &ActiveEventLoop, index: usize, outcome: Outcome) {
        if index >= self.sessions.len() {
            return;
        }
        // What leaves nothing of snapr's that needs the focus; the others
        // open a window (results, a pin).
        #[cfg(target_os = "macos")]
        let back_to_app = matches!(outcome, Outcome::Cancel | Outcome::Done { .. } | Outcome::Record(_));
        {
            // Its windows close at the end of this block.
            let session = self.sessions.remove(index);
            self.style = session.style;
            self.magnifier = session.magnifier;
            let remembered = remembered::Remembered::new(self.style, self.magnifier);
            if remembered != self.remembered {
                remembered.save();
                self.remembered = remembered;
            }
            match outcome {
                Outcome::Done {
                    image,
                    save_file,
                    rect,
                } => {
                    self.last_region = Some(rect);
                    if let Some(w) = &mut self.main_window {
                        w.last_region = Some(rect);
                    }
                    self.play(Sound::Capture);
                    self.count(|s| s.count(stats::Kind::Screenshot, chrono::Local::now()));
                    self.output.submit(image, session.context, save_file);
                }
                Outcome::Record(rect) => {
                    self.play(Sound::Capture);
                    // The overlay only leaves the screen once the event
                    // loop has handled its windows closing; starting now
                    // would put the frozen screen at the start of the
                    // recording.
                    let at = Instant::now() + Duration::from_millis(50);
                    self.record_at = Some((at, rect, session.context));
                }
                Outcome::Scan(image) => {
                    self.play(Sound::Capture);
                    self.count(|s| s.qr_scans += 1);
                    self.read_qr(image);
                }
                Outcome::Color(rgb) => self.color_picked(event_loop, rgb),
                Outcome::Pin(image, rect) => {
                    self.play(Sound::Capture);
                    let at = winit::dpi::PhysicalPosition::new(rect.x, rect.y);
                    self.pin(event_loop, image, Place::At(at));
                }
                _ => {}
            }
        }
        // Back to the capture underneath, if any.
        if let Some(below) = self.sessions.last_mut() {
            below.resume();
            return;
        }
        // A recording keeps it hidden until it's done, so it isn't in the video.
        if self.recording.is_none() && self.record_at.is_none() {
            self.show_main_window_again();
        }
        #[cfg(target_os = "macos")]
        if let Some(app) = self.focus_back.take()
            && back_to_app
        {
            macos::give_focus_to(&app);
        }
        if self.once {
            // Give the output thread a chance to finish before quitting.
            self.output.flush();
            event_loop.exit();
        }
    }

    fn open_main_window(&mut self, event_loop: &ActiveEventLoop, page: Page) {
        if let Some(w) = &mut self.main_window {
            return w.show_page(page);
        }
        let gpu = match &self.gpu {
            Ok(g) => g,
            Err(e) => return eprintln!("can't open the window: {e}"),
        };
        // It may have been turned on or off in the desktop's settings since.
        #[cfg(target_os = "linux")]
        {
            self.settings.start_with_windows = autostart::enabled();
        }
        let mut form = Form::new(&self.settings);
        form.status = self.status.clone();
        form.last_capture = self.last_capture.clone();
        form.update = self.update.clone();
        let proxy = Mutex::new(self.proxy.clone());
        let wake = Box::new(move || {
            let _ = proxy.lock().unwrap().send_event(UserEvent::ThumbnailReady);
        });
        let gallery = Gallery::new(
            std::path::Path::new(&self.settings.folder),
            self.settings.ffmpeg_path.clone(),
            wake,
        );
        let transfers = self.output.transfers();
        let stats = self.stats.clone();
        match MainWindow::open(
            event_loop,
            gpu,
            page,
            form,
            gallery,
            transfers,
            &self.settings,
            stats,
        ) {
            Ok(mut w) => {
                w.last_region = self.last_region;
                w.tools.pinned = self.pins.len();
                self.main_window = Some(w);
                #[cfg(target_os = "macos")]
                macos::show_in_dock(true);
            }
            Err(e) => eprintln!("{e}"),
        }
    }

    /// Updates the stats, saves them and shows them on the Stats page.
    fn count(&mut self, update: impl FnOnce(&mut stats::Stats)) {
        update(&mut self.stats);
        self.stats.save();
        if let Some(w) = &mut self.main_window {
            w.stats = self.stats.clone();
            if w.page == Page::Stats {
                w.window.request_redraw();
            }
        }
    }

    fn play(&self, sound: Sound) {
        if self.settings.play_sounds {
            sound::play(sound, self.settings.sound_volume);
        }
    }

    /// Shows a message on the settings page, opening it for errors.
    fn report(&mut self, event_loop: &ActiveEventLoop, msg: String, is_error: bool) {
        self.report_on(event_loop, msg, is_error, Page::Settings);
    }

    /// Shows a message, opening the window on `page` for errors.
    fn report_on(&mut self, event_loop: &ActiveEventLoop, msg: String, is_error: bool, page: Page) {
        eprintln!("{msg}");
        if is_error {
            self.play(Sound::Error);
        }
        self.status = Some((msg, is_error));
        if is_error && !self.once {
            self.open_main_window(event_loop, page);
        }
        if let Some(w) = &mut self.main_window {
            w.form.status = self.status.clone();
            w.window.request_redraw();
        }
    }

    fn apply_settings(
        &mut self,
        event_loop: &ActiveEventLoop,
        new: Settings,
        new_secrets: HashMap<String, String>,
    ) {
        for (id, secret) in &new_secrets {
            // Stored by kind and id, so it needs the destination itself.
            let Some(upload) = new.uploads.iter().find(|u| u.id == *id) else {
                continue;
            };
            if let Err(e) = secrets::set(upload, secret) {
                return self.report_on(event_loop, e, true, Page::Destinations);
            }
        }
        let config = match output_config(&new) {
            Ok(c) => c,
            Err(e) => return self.report_on(event_loop, e, true, Page::Naming),
        };
        if let Some(hk) = &mut self.hotkeys
            && let Err(e) = hk.set(&new.hotkey)
        {
            return self.report_on(event_loop, e, true, Page::Hotkeys);
        }
        if let Some(hk) = &mut self.record_hotkeys
            && let Err(e) = hk.set_optional(&new.record_hotkey)
        {
            return self.report_on(
                event_loop,
                format!("Record hotkey: {e}"),
                true,
                Page::Hotkeys,
            );
        }
        if !self.once
            && new.tool_hotkeys != self.settings.tool_hotkeys
            && let Err(e) = self.set_tool_hotkeys(&new.tool_hotkeys)
        {
            // Put the old ones back.
            let old = self.settings.tool_hotkeys.clone();
            let _ = self.set_tool_hotkeys(&old);
            return self.report_on(event_loop, e, true, Page::Hotkeys);
        }
        // Only on a change: another copy (e.g. with its own SNAPR_CONFIG_DIR)
        // shouldn't undo this one's.
        #[cfg(any(windows, target_os = "linux"))]
        if new.start_with_windows != self.settings.start_with_windows
            && let Err(e) = autostart::set(new.start_with_windows)
        {
            return self.report(event_loop, e, true);
        }
        #[cfg(windows)]
        if new.start_menu != self.settings.start_menu
            && let Err(e) = start_menu::set(new.start_menu)
        {
            return self.report(event_loop, e, true);
        }
        if let Err(e) = new.save() {
            return self.report(event_loop, e, true);
        }
        self.output.configure(config);
        // Forget the secret keys of removed upload destinations.
        for old in &self.settings.uploads {
            if !new.uploads.iter().any(|u| u.id == old.id) {
                secrets::delete(old);
            }
        }
        if let Some(t) = &self.tray {
            t.set_hotkey(tray_hotkey(&new.hotkey));
            t.set_click(new.tray_click);
            t.set_style(new.tray_icon);
        }
        if new.theme != self.settings.theme {
            theme::pick(new.theme);
            self.redraw_pins();
        }
        self.settings = new;
        if let Some(w) = &mut self.main_window {
            w.form.saved(&self.settings);
            w.hotkey = self.settings.hotkey.clone();
            w.record_hotkey = self.settings.record_hotkey.clone();
            w.folder = PathBuf::from(&self.settings.folder);
            w.speed_unit = self.settings.speed_unit;
        }
        self.report(event_loop, "Settings saved".into(), false);
    }

    /// Replaces the tools' hotkeys. Returns the first that couldn't be
    /// registered; the others still are.
    fn set_tool_hotkeys(&mut self, list: &[ToolHotkey]) -> Result<(), String> {
        // Free the old keys first: one may move to another tool.
        for h in &mut self.tool_hotkeys {
            h.clear();
        }
        let (hotkeys, result) = tool_hotkeys(&self.proxy, list);
        self.tool_hotkeys = hotkeys;
        result
    }

    /// Starts a capture from the main window, once it's hidden so it isn't
    /// in the screenshot.
    fn capture_from_main_window(&mut self, start: Start) {
        if let Some(w) = &mut self.main_window {
            // Its sound would end up in a recording.
            w.gallery.stop_playback();
            w.window.set_visible(false);
            self.restore_main_window = true;
        }
        self.capture_at = Some((Instant::now() + Duration::from_millis(250), start));
    }

    /// Captures a window, display, everything or a known region straight
    /// away, with no overlay to pick or annotate on.
    fn grab(&mut self, event_loop: &ActiveEventLoop, target: capture::Target) {
        let time = chrono::Local::now();
        match capture::grab(&target) {
            Ok((image, process, title)) => {
                self.play(Sound::Capture);
                self.count(|s| s.count(stats::Kind::Screenshot, time));
                let ctx = naming::Context {
                    process,
                    title,
                    ..naming::Context::new(time)
                };
                self.output.submit(image, ctx, true);
            }
            Err(e) => self.report(event_loop, e, true),
        }
    }

    /// Looks for QR codes in a captured region, off the event loop, and
    /// shows what was found on the Tools page.
    fn read_qr(&self, image: image::RgbaImage) {
        let proxy = Mutex::new(self.proxy.clone());
        std::thread::spawn(move || {
            let texts = qr::decode(&image);
            let (preview, _) = thumbnail::to_egui(image.into(), (320, 320));
            let _ = proxy
                .lock()
                .unwrap()
                .send_event(UserEvent::QrScanned(texts, preview));
        });
    }

    /// Copies a picked colour's hex code and shows it on the Tools page.
    fn color_picked(&mut self, event_loop: &ActiveEventLoop, rgb: [u8; 3]) {
        self.count(|s| s.colors_picked += 1);
        self.chime_on_copy = true;
        let picker = self.settings.color_picker;
        let text = tools::color_text(picker.copy_as, rgb);
        self.output.copy_text(text.clone());
        self.set_status(format!("Copied {text}"));
        // Picked from the Tools page, the window comes back by itself.
        if picker.show_window && !self.restore_main_window {
            self.open_main_window(event_loop, Page::Tools);
        }
        if let Some(w) = &mut self.main_window {
            w.tools.picked(rgb);
            w.window.request_redraw();
        }
    }

    fn main_window_action(&mut self, event_loop: &ActiveEventLoop, action: Action) {
        match action {
            Action::Capture => self.capture_from_main_window(Start::Pick(Purpose::Screenshot)),
            Action::CaptureTarget(t) => self.capture_from_main_window(Start::Grab(t)),
            Action::Record if self.recording.is_some() => {
                self.recording_action(event_loop, record_ui::Action::Stop)
            }
            Action::Record => self.capture_from_main_window(Start::Pick(Purpose::Record)),
            Action::ScanQr => self.capture_from_main_window(Start::Pick(Purpose::Scan)),
            Action::PickColor => self.capture_from_main_window(Start::Pick(Purpose::PickColor)),
            Action::PinRegion => self.capture_from_main_window(Start::Pick(Purpose::Pin)),
            Action::PinImage(image) => {
                let monitor = self.main_window.as_ref().and_then(|w| w.window.current_monitor());
                self.pin(event_loop, image, Place::Center(monitor));
            }
            Action::PinFile(path) => {
                let proxy = Mutex::new(self.proxy.clone());
                std::thread::spawn(move || {
                    let image = image::open(&path)
                        .map(|i| i.to_rgba8())
                        .map_err(|e| e.to_string());
                    let _ = proxy
                        .lock()
                        .unwrap()
                        .send_event(UserEvent::PinLoaded(path, image));
                });
            }
            Action::ClosePins => {
                self.pins.clear();
                self.pins_changed();
            }
            Action::SaveSettings(s, secrets) => self.apply_settings(event_loop, s, secrets),
            Action::ImportTools(tools) => {
                self.style = tools.style();
                self.magnifier = tools.magnifier();
                self.font_pending = tools.font().map(String::from);
                tools.save();
                self.remembered = tools;
            }
            Action::TestUpload(upload, secret) => self.test_upload(upload, secret, false),
            Action::UploadIcon(upload, secret) => self.test_upload(upload, secret, true),
            Action::CheckForUpdates => self.check_for_updates(),
            #[cfg(target_os = "linux")]
            Action::AllowScreenshots => {
                let proxy = Mutex::new(self.proxy.clone());
                std::thread::spawn(move || {
                    let result = portal::ask_for_screenshots();
                    let _ = proxy.lock().unwrap().send_event(UserEvent::ScreenshotsAllowed(result));
                });
            }
            Action::RestartToUpdate => {
                if let Some(save) = self.main_window.as_mut().and_then(|w| w.flush_settings()) {
                    self.main_window_action(event_loop, save);
                }
                self.restart = true;
                event_loop.exit();
            }
            #[cfg(windows)]
            Action::MoveToPrograms { settings } => {
                if let Some(save) = self.main_window.as_mut().and_then(|w| w.flush_settings()) {
                    self.main_window_action(event_loop, save);
                }
                match install::copy_home() {
                    Ok(installed) => {
                        // Hidden now: closing it would wait for the event
                        // loop, which is ending, and the new copy opens its own.
                        if let Some(w) = &self.main_window {
                            w.window.set_visible(false);
                        }
                        self.moved_to = Some((installed, settings));
                        event_loop.exit();
                    }
                    Err(e) => self.report_on(event_loop, e, true, Page::Settings),
                }
            }
            Action::CopyText(text) => {
                self.chime_on_copy = true;
                self.output.copy_text(text);
            }
            Action::CopyImage(image) => {
                self.chime_on_copy = true;
                self.output.copy_image(image);
            }
            Action::OpenUrl(url) => output::open_url(&url),
            Action::Reveal(p) => output::reveal(&p),
            Action::OpenFile(p) => output::open(&p),
            Action::ShowInFolder(p) => output::show_in_folder(&p),
            Action::Copy(p) => {
                self.chime_on_copy = true;
                self.output.copy_file(p);
            }
            Action::DeleteRemote(p, remote) => self.delete_remote(event_loop, p, remote),
            Action::UploadFiles(paths) => self.upload_files(event_loop, paths),
            Action::Combine(paths, dir) => {
                let proxy = Mutex::new(self.proxy.clone());
                std::thread::spawn(move || {
                    let result = combine::files(&paths, dir);
                    let event = UserEvent::Combined(paths.len(), result);
                    let _ = proxy.lock().unwrap().send_event(event);
                });
            }
            Action::Delete(p) => match trash::delete(&p) {
                Ok(()) => {
                    history::remove(&p);
                    if let Some(w) = &mut self.main_window {
                        w.gallery.remove(&p);
                    }
                }
                Err(e) => self.report(
                    event_loop,
                    format!("couldn't delete {}: {e}", p.display()),
                    true,
                ),
            },
        }
    }

    /// Uploads files dropped on the main window to the enabled destinations.
    /// PNGs are added to Recent so their links show up there.
    fn upload_files(&mut self, event_loop: &ActiveEventLoop, paths: Vec<PathBuf>) {
        if policy::uploads_disabled() {
            return self.report_on(event_loop, policy::UPLOADS_OFF.into(), true, Page::Destinations);
        }
        for path in paths {
            if !path.is_file() {
                self.report_on(
                    event_loop,
                    format!("upload failed: {} isn't a file", path.display()),
                    true,
                    Page::Destinations,
                );
                continue;
            }
            let mut ctx = naming::Context::new(chrono::Local::now());
            ctx.title = path.file_stem().map(|s| s.to_string_lossy().into_owned());
            if let Ok((w, h)) = image::image_dimensions(&path) {
                (ctx.width, ctx.height) = (w, h);
            }
            history::add(&path);
            if let Some(w) = &mut self.main_window {
                w.gallery.add(path.clone());
            }
            if self.settings.show_toast {
                self.toast_after_upload.insert(path.clone(), None);
            }
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            self.status = Some((format!("Uploading {name}\u{2026}"), false));
            if let Some(w) = &mut self.main_window {
                w.form.status = self.status.clone();
                w.window.request_redraw();
            }
            self.output.upload_file(path, ctx);
        }
    }

    /// Deletes an uploaded copy of a screenshot from its bucket.
    fn delete_remote(
        &mut self,
        event_loop: &ActiveEventLoop,
        path: PathBuf,
        remote: history::Remote,
    ) {
        let Some(upload) = self
            .settings
            .uploads
            .iter()
            .find(|u| u.id == remote.upload_id)
            .cloned()
        else {
            let msg = "Couldn't delete remotely: that upload destination was removed from Settings"
                .into();
            return self.gallery_notice(event_loop, msg, true);
        };
        let proxy = Mutex::new(self.proxy.clone());
        std::thread::spawn(move || {
            let result = secrets::get(&upload)
                .ok_or_else(|| "its secret access key isn't saved".to_string())
                .and_then(|secret| match upload.target(secret) {
                    crate::upload::Target::S3(t) => t.delete_object(&remote.key),
                    crate::upload::Target::Pomf(_) => Err("Pomf uploads can't be deleted".into()),
                });
            let _ = proxy.lock().unwrap().send_event(UserEvent::RemoteDeleted(
                path,
                upload.name,
                result,
            ));
        });
    }

    /// Shows a message on the Recent page.
    fn gallery_notice(&mut self, _event_loop: &ActiveEventLoop, msg: String, is_error: bool) {
        eprintln!("{msg}");
        self.play(if is_error { Sound::Error } else { Sound::Done });
        if let Some(w) = &mut self.main_window {
            w.gallery.notice = Some((msg, is_error));
            w.window.request_redraw();
        }
    }

    /// Uploads a tiny file to the destination and deletes it again, or with
    /// `icon` (or where uploads can't be deleted), uploads snapr's icon where
    /// a screenshot would go and leaves it there to look at.
    fn test_upload(&self, upload: Upload, secret: Option<String>, icon: bool) {
        let proxy = Mutex::new(self.proxy.clone());
        let id = upload.id.clone();
        std::thread::spawn(move || {
            let result = (|| {
                let template = upload.validate()?;
                let secret = if upload.needs_secret() {
                    secret
                        .or_else(|| secrets::get(&upload))
                        .ok_or("enter the secret access key")?
                } else {
                    String::new()
                };
                let target = upload.target(secret);
                if icon || !upload.deletable() {
                    const SIZE: u32 = 256;
                    let image = image::RgbaImage::from_raw(SIZE, SIZE, crate::tray::icon_rgba(SIZE))
                        .ok_or("couldn't draw the icon")?;
                    let mut png = Vec::new();
                    image
                        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                        .map_err(|e| format!("couldn't encode the icon: {e}"))?;
                    let ctx = naming::Context {
                        title: Some("snapr".into()),
                        width: SIZE,
                        height: SIZE,
                        ..naming::Context::new(chrono::Local::now())
                    };
                    let key = template.render_key(&ctx, "png");
                    let body = crate::upload::Body::Bytes(Arc::new(png));
                    let progress = crate::upload::Progress::default();
                    let link = upload.put(&target, &key, &body, "image/png", &progress)?;
                    return Ok(format!("Uploaded the icon: {link}"));
                }
                let crate::upload::Target::S3(target) = target else {
                    unreachable!("only S3 uploads can be deleted");
                };
                let key = format!(
                    "snapr-test-{}.txt",
                    (0..8).map(|_| fastrand::alphanumeric()).collect::<String>()
                );
                let body = crate::upload::Body::Bytes(Arc::new(b"snapr upload test".to_vec()));
                let progress = crate::upload::Progress::default();
                target.put_object(&key, &body, "text/plain", &progress)?;
                let deleted = target.delete_object(&key);
                let link = upload.link(&key)?;
                Ok(match deleted {
                    Ok(()) => format!("Upload works. Links will look like {link}"),
                    Err(e) => {
                        format!("Upload works, but deleting the test file failed ({e}): {link}")
                    }
                })
            })();
            let _ = proxy
                .lock()
                .unwrap()
                .send_event(UserEvent::UploadTest(id, result.map_err(|e: String| e)));
        });
    }

    /// Looks for a newer release and installs it, in the background.
    fn check_for_updates(&self) {
        let proxy = Mutex::new(self.proxy.clone());
        update::check(move |status| {
            let _ = proxy.lock().unwrap().send_event(UserEvent::Update(status));
        });
    }

    /// Decodes a saved screenshot for the corner preview, off the event loop.
    fn prepare_toast(&self, path: PathBuf) {
        let proxy = Mutex::new(self.proxy.clone());
        let ffmpeg = self.settings.ffmpeg_path.clone();
        std::thread::spawn(move || {
            let image = toast::load_preview(&path, &ffmpeg);
            let _ = proxy
                .lock()
                .unwrap()
                .send_event(UserEvent::ToastReady(path, image));
        });
    }

    fn show_toast(
        &mut self,
        event_loop: &ActiveEventLoop,
        path: PathBuf,
        image: Option<egui::ColorImage>,
    ) {
        let Ok(gpu) = &self.gpu else { return };
        let link = self
            .last_upload
            .as_ref()
            .filter(|(p, _)| *p == path)
            .map(|(_, l)| l.clone());
        // A new capture replaces the previous preview.
        self.toast = None;
        #[cfg(target_os = "linux")]
        let send_x11 = {
            let proxy = Mutex::new(self.proxy.clone());
            move |e| {
                let _ = proxy.lock().unwrap().send_event(UserEvent::ToastX11(e));
            }
        };
        match Toast::open(
            event_loop,
            gpu,
            path,
            image,
            link,
            #[cfg(target_os = "linux")]
            send_x11,
        ) {
            Ok(t) => self.toast = Some(t),
            Err(e) => eprintln!("{e}"),
        }
    }

    fn toast_event(&mut self, event: WindowEvent) {
        let Some(t) = &mut self.toast else { return };
        let action = t.on_event(&event);
        self.toast_action(action);
    }

    fn toast_action(&mut self, action: Option<toast::Action>) {
        let Some(t) = &mut self.toast else { return };
        match action {
            None => {}
            Some(toast::Action::Open) => {
                match &t.link {
                    Some(link) => output::open_url(link),
                    None => output::open(&t.path),
                }
                self.toast = None;
            }
            Some(toast::Action::CopyImage) if t.video => {
                // There's no image to copy; copy the link once there is one.
                if let Some(link) = &t.link {
                    self.chime_on_copy = true;
                    self.output.copy_text(link.clone());
                    self.toast = None;
                }
            }
            Some(toast::Action::CopyImage) => {
                self.chime_on_copy = true;
                self.output.copy_file(t.path.clone());
                self.toast = None;
            }
            Some(toast::Action::Dismiss) => self.toast = None,
        }
    }

    /// Pins an image to the screen, on top of everything.
    fn pin(&mut self, event_loop: &ActiveEventLoop, image: image::RgbaImage, place: Place) {
        let gpu = match &self.gpu {
            Ok(g) => g,
            Err(e) => return self.report(event_loop, e.clone(), true),
        };
        match Pin::open(event_loop, gpu, image, place) {
            Ok(pin) => {
                self.pins.push(pin);
                self.count(|s| s.pins += 1);
                self.pins_changed();
            }
            Err(e) => self.report(event_loop, e, true),
        }
    }

    /// Shows how many images are pinned on the Tools page.
    fn pins_changed(&mut self) {
        if let Some(w) = &mut self.main_window {
            w.tools.pinned = self.pins.len();
            if w.page == Page::Tools {
                w.window.request_redraw();
            }
        }
    }

    fn pin_event(&mut self, index: usize, event: WindowEvent) {
        match self.pins[index].on_event(&event) {
            None => {}
            Some(pin::Action::Copy(image)) => {
                self.chime_on_copy = true;
                self.output.copy_image(image);
            }
            Some(pin::Action::Close) => {
                self.pins.remove(index);
                self.pins_changed();
            }
        }
    }

    fn main_window_event(&mut self, event_loop: &ActiveEventLoop, event: WindowEvent) {
        let Some(w) = &mut self.main_window else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => {
                let save = w.flush_settings();
                self.main_window = None;
                #[cfg(target_os = "macos")]
                macos::show_in_dock(false);
                if let Some(save) = save {
                    self.main_window_action(event_loop, save);
                }
            }
            WindowEvent::RedrawRequested => {
                for action in w.paint() {
                    self.main_window_action(event_loop, action);
                }
            }
            other => w.on_event(&other),
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        if cause != StartCause::Init {
            return;
        }
        if self.once {
            return self.start_capture(event_loop);
        }
        // The tray has to be created once the event loop is running (macOS).
        let proxy = Mutex::new(self.proxy.clone());
        let callback: tray::Callback = Arc::new(move |action| {
            let _ = proxy.lock().unwrap().send_event(UserEvent::Tray(action));
        });
        let hotkey = self
            .hotkeys
            .as_ref()
            .is_some_and(Hotkeys::is_registered)
            .then(|| tray_hotkey(&self.settings.hotkey))
            .flatten();
        match Tray::new(hotkey, callback) {
            Ok(t) => {
                t.set_click(self.settings.tray_click);
                t.set_style(self.settings.tray_icon);
                self.tray = Some(t);
            }
            Err(e) => {
                // Without a tray the window is the only way in.
                self.status.get_or_insert((
                    format!("{e} \u{2014} closing this window keeps snapr running"),
                    true,
                ));
                self.open_settings_on_start = true;
            }
        }
        if self.open_settings_on_start {
            let page = if !self.settings.setup_done {
                Page::Setup
            } else if self.open_recent_on_start {
                Page::Recent
            } else {
                Page::Settings
            };
            self.open_main_window(event_loop, page);
        }
    }

    fn resumed(&mut self, _: &ActiveEventLoop) {}

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Hotkey | UserEvent::Tray(TrayAction::Capture) => {
                self.start_capture(event_loop)
            }
            UserEvent::RecordHotkey => {
                if !self.main_window.as_ref().is_some_and(|w| w.form.recording) {
                    self.toggle_recording(event_loop)
                }
            }
            UserEvent::RecordingDone(id, result, ctx) => {
                self.recording_done(event_loop, id, result, ctx)
            }
            UserEvent::Tray(TrayAction::Click) => match self.settings.tray_click {
                // `Menu` never gets here: the tray shows the menu itself.
                TrayClick::Open | TrayClick::Menu => {
                    self.open_main_window(event_loop, Page::Recent)
                }
                TrayClick::Record => self.toggle_recording(event_loop),
                // Not a second capture on top of one already open (say, from
                // a double-click).
                _ if !self.sessions.is_empty() => {}
                TrayClick::Capture => self.start_capture(event_loop),
                TrayClick::Tool(action) => self.run_tool(event_loop, action),
            },
            UserEvent::Tray(TrayAction::Recent) => self.open_main_window(event_loop, Page::Recent),
            UserEvent::Tray(TrayAction::Settings) => {
                self.open_main_window(event_loop, Page::Settings)
            }
            UserEvent::Tray(TrayAction::OpenFolder) => {
                output::reveal(&PathBuf::from(&self.settings.folder))
            }
            UserEvent::Tray(TrayAction::Quit) => event_loop.exit(),
            UserEvent::Output(output::Event::Copied) => {
                println!("copied to clipboard");
                if std::mem::take(&mut self.chime_on_copy) {
                    self.play(Sound::Done);
                }
            }
            UserEvent::Output(output::Event::Saved { path, uploading }) => {
                println!("saved {}", path.display());
                history::add(&path);
                self.status = None;
                self.last_capture = Some(path.clone());
                if self.settings.show_toast && !self.once {
                    if uploading {
                        self.toast_after_upload.insert(path.clone(), None);
                    } else {
                        self.prepare_toast(path.clone());
                    }
                }
                if let Some(w) = &mut self.main_window {
                    w.form.status = None;
                    w.form.last_capture = self.last_capture.clone();
                    w.gallery.add(path);
                    w.window.request_redraw();
                }
            }
            // The tray icon picks them up in `about_to_wait`.
            UserEvent::Output(output::Event::UploadsStarted) => {}
            UserEvent::Output(output::Event::Uploaded { name, url }) => {
                self.play(Sound::Done);
                println!("{url}");
                self.set_status(format!("Uploaded to {name}"));
            }
            UserEvent::Output(output::Event::Linked {
                path,
                name,
                url,
                remote,
                copied,
            }) => {
                if let Some(path) = &path {
                    history::set_link(path, Some(&url), remote.clone());
                    self.last_upload = Some((path.clone(), url.clone()));
                    if let Some(t) = self.toast.as_mut().filter(|t| t.path == *path) {
                        t.set_link(url.clone());
                    }
                }
                if copied {
                    self.set_status(format!("Uploaded to {name} (link copied)"));
                }
                if let Some(w) = &mut self.main_window
                    && let Some(path) = &path
                {
                    w.gallery.set_link(path, Some(url), remote);
                    w.window.request_redraw();
                }
            }
            UserEvent::Output(output::Event::UploadsFinished(path)) => {
                match self.toast_after_upload.remove(&path) {
                    Some(Some(preview)) => self.show_toast(event_loop, path, Some(preview)),
                    Some(None) => self.prepare_toast(path),
                    None => {}
                }
            }
            UserEvent::Output(output::Event::Cancelled(name)) => {
                self.set_status(locale::spell(&format!("Upload to {name} cancelled")).into_owned());
            }
            UserEvent::Output(output::Event::Failed(e)) => {
                let page = if e.starts_with("upload") {
                    Page::Destinations
                } else {
                    Page::Settings
                };
                self.report_on(event_loop, e, true, page);
            }
            UserEvent::UploadTest(id, result) => {
                if let Some(w) = &mut self.main_window {
                    let test = match result {
                        Ok(msg) => Test::Passed(msg),
                        Err(e) => Test::Failed(e),
                    };
                    w.form.tests.insert(id, test);
                    w.window.request_redraw();
                }
            }
            UserEvent::RemoteDeleted(path, name, result) => {
                let (msg, is_error) = match result {
                    Ok(()) => {
                        history::set_link(&path, None, None);
                        if let Some(w) = &mut self.main_window {
                            w.gallery.set_link(&path, None, None);
                        }
                        (format!("Deleted from {name}"), false)
                    }
                    Err(e) => (format!("Couldn't delete from {name}: {e}"), true),
                };
                self.gallery_notice(event_loop, msg, is_error);
            }
            UserEvent::ToastReady(path, image) => self.show_toast(event_loop, path, image),
            #[cfg(target_os = "linux")]
            UserEvent::ToastX11(e) => {
                let action = self.toast.as_mut().and_then(|t| t.on_x11_event(e));
                self.toast_action(action);
            }
            #[cfg(target_os = "linux")]
            UserEvent::DesktopStyleChanged => {
                if theme::picked() == settings::Theme::System {
                    self.redraw_pins();
                    if let Some(w) = &self.main_window {
                        w.window.request_redraw();
                    }
                }
            }
            #[cfg(target_os = "linux")]
            UserEvent::RecordingX11(e) => {
                let action = self
                    .recording
                    .as_mut()
                    .and_then(|r| r.ui.as_mut())
                    .and_then(|ui| ui.on_x11_event(e));
                if let Some(action) = action {
                    self.recording_action(event_loop, action);
                }
            }
            UserEvent::ToolHotkey(action) | UserEvent::Tray(TrayAction::Tool(action)) => {
                self.run_tool(event_loop, action)
            }
            UserEvent::Combined(count, Ok(image)) => {
                // A new screenshot: saved, copied and uploaded like a capture.
                let mut ctx = naming::Context::new(chrono::Local::now());
                ctx.title = Some("combined".into());
                self.output.submit(image, ctx, true);
                self.gallery_notice(event_loop, format!("Combined {count} images"), false);
            }
            UserEvent::PinLoaded(_, Ok(image)) => {
                let monitor = self.main_window.as_ref().and_then(|w| w.window.current_monitor());
                self.pin(event_loop, image, Place::Center(monitor));
            }
            UserEvent::PinLoaded(path, Err(e)) => self.report(
                event_loop,
                format!("couldn't pin {}: {e}", path.display()),
                true,
            ),
            UserEvent::Combined(_, Err(e)) => {
                self.gallery_notice(event_loop, format!("Couldn't combine: {e}"), true)
            }
            UserEvent::QrScanned(texts, preview) => {
                self.count(|s| s.qr_codes += texts.len() as u64);
                self.play(if texts.is_empty() {
                    Sound::Error
                } else {
                    Sound::Done
                });
                self.open_main_window(event_loop, Page::Tools);
                if let Some(w) = &mut self.main_window {
                    w.tools.scanned(texts, preview);
                    w.window.request_redraw();
                }
            }
            UserEvent::UpdateDue => {
                if self.settings.check_for_updates {
                    self.check_for_updates();
                }
            }
            UserEvent::Update(status) => {
                if let Some(w) = &mut self.main_window {
                    w.form.update = status.clone();
                    w.window.request_redraw();
                }
                self.update = status;
            }
            UserEvent::HotkeyProblem(e) => self.report_on(event_loop, e, true, Page::Hotkeys),
            UserEvent::HotkeyKeysChanged => {
                if let Some(w) = &self.main_window {
                    w.window.request_redraw();
                }
            }
            #[cfg(target_os = "linux")]
            UserEvent::ScreenshotsAllowed(result) => {
                let (msg, is_error) = match result {
                    Ok(()) => ("snapr can take screenshots now".into(), false),
                    // Still the card, with why.
                    Err(e) => (format!("{e} \u{2014} {}", portal::SCREENSHOTS_OFF), true),
                };
                self.report(event_loop, msg, is_error);
            }
            UserEvent::ThumbnailReady => {
                if let Some(w) = &self.main_window {
                    w.window.request_redraw();
                }
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        if let Some((at, start)) = self.capture_at
            && at <= now
        {
            self.capture_at = None;
            match start {
                Start::Pick(purpose) => self.open_session(event_loop, purpose),
                Start::Grab(target) => self.grab(event_loop, target),
            }
            if self.sessions.is_empty() {
                self.show_main_window_again();
            }
        }
        if self.record_at.as_ref().is_some_and(|(at, ..)| *at <= now)
            && let Some((_, rect, context)) = self.record_at.take()
        {
            #[cfg(windows)]
            overlay::wait_until_off_screen();
            self.start_recording(event_loop, rect, context);
        }
        let mut wake = self.sessions.iter_mut().filter_map(|s| s.poll(now)).min();
        wake = [
            wake,
            self.capture_at.map(|(t, _)| t),
            self.record_at.as_ref().map(|(t, ..)| *t),
        ]
        .into_iter()
            .flatten()
            .min();
        match self.toast.as_ref().and_then(|t| t.expires_at) {
            Some(t) if t <= now => self.toast = None,
            Some(t) => wake = Some(wake.map_or(t, |w| w.min(t))),
            None => {}
        }
        if let Some(active) = &mut self.recording
            && let Some(ui) = &mut active.ui
        {
            // Red until the encoder has started, then green.
            if !ui.live() {
                match &self.gpu {
                    Ok(gpu) if active.recording.live() => ui.set_live(gpu),
                    _ => {
                        let t = now + Duration::from_millis(30);
                        wake = Some(wake.map_or(t, |w| w.min(t)));
                    }
                }
            }
            ui.tick(now);
            if let Some(t) = ui.repaint_at() {
                wake = Some(wake.map_or(t, |w| w.min(t)));
            }
        }
        if let Some(tray) = &self.tray {
            let progress = output::percent_done(&self.output.transfers().lock().unwrap())
                .map(|percent| tray::Progress {
                    percent,
                    number: self.settings.tray_percentage,
                });
            if progress != self.tray_progress {
                self.tray_progress = progress;
                tray.set_progress(progress);
            }
            if progress.is_some() {
                let t = now + Duration::from_millis(250);
                wake = Some(wake.map_or(t, |w| w.min(t)));
            }
        }
        if let Some(w) = &mut self.main_window {
            match w.repaint_at {
                Some(t) if t <= now => {
                    w.repaint_at = None;
                    w.window.request_redraw();
                }
                Some(t) => wake = Some(wake.map_or(t, |w| w.min(t))),
                None => {}
            }
        }
        event_loop.set_control_flow(wake.map_or(ControlFlow::Wait, ControlFlow::WaitUntil));
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if self
            .main_window
            .as_ref()
            .is_some_and(|w| w.window.id() == id)
        {
            return self.main_window_event(event_loop, event);
        }
        if self.toast.as_ref().is_some_and(|t| t.is_window(id)) {
            return self.toast_event(event);
        }
        if let Some(index) = self.pins.iter().position(|p| p.window.id() == id) {
            return self.pin_event(index, event);
        }
        let Ok(gpu) = &self.gpu else { return };
        if let Some(ui) = self.recording.as_mut().and_then(|r| r.ui.as_mut())
            && ui.owns(id)
        {
            if let Some(action) = ui.on_event(gpu, id, &event) {
                self.recording_action(event_loop, action);
            }
            return;
        }
        let Some(index) = self.sessions.iter().position(|s| s.has_window(id)) else {
            return;
        };
        match self.sessions[index].window_event(gpu, id, &event) {
            Outcome::Continue => {}
            outcome => self.end_capture(event_loop, index, outcome),
        }
    }
}

/// Where screenshots go, from the settings. Enabled uploads with a problem
/// (or no stored secret key) are left out and reported; all of them are
/// while uploading is turned off by policy.
fn output_config(settings: &Settings) -> Result<output::Config, String> {
    let mut uploads = Vec::new();
    let enabled = settings.uploads.iter().filter(|u| u.enabled && !policy::uploads_disabled());
    for u in enabled {
        let key = match u.validate() {
            Ok(k) => k,
            Err(e) => {
                eprintln!("upload destination {}: {e}", u.name);
                continue;
            }
        };
        let secret = if u.needs_secret() {
            let Some(secret) = secrets::get(u) else {
                eprintln!("upload destination {}: no secret key stored", u.name);
                continue;
            };
            secret
        } else {
            String::new()
        };
        uploads.push(output::UploadTarget {
            upload: u.clone(),
            target: u.target(secret),
            key,
        });
    }
    Ok(output::Config {
        naming: settings.naming()?,
        save_to_folder: settings.save_to_folder,
        copy_image: settings.copy_to_clipboard,
        copy_link: settings.copy_link,
        link_from: settings.link_from.clone(),
        uploads,
    })
}

/// Registers a hotkey for each tool hotkey setting. Returns those that
/// worked, and the first error.
fn tool_hotkeys(
    proxy: &EventLoopProxy<UserEvent>,
    list: &[ToolHotkey],
) -> (Vec<Hotkeys>, Result<(), String>) {
    let mut result = Ok(());
    let mut hotkeys = Vec::new();
    for (i, t) in list.iter().enumerate() {
        let action = t.action;
        let id = ToolHotkey::id(list, i);
        let mut h = Hotkeys::new(proxy.clone(), &id, action.label(), move || {
            UserEvent::ToolHotkey(action)
        });
        match h.set_or_offer(&t.hotkey) {
            Ok(()) => hotkeys.push(h),
            Err(e) => {
                if result.is_ok() {
                    result = Err(format!("{} hotkey: {e}", action.label()));
                }
            }
        }
    }
    (hotkeys, result)
}

/// The capture hotkey for the tray's menu: none where the desktop picks the
/// keys, as it may have picked others.
fn tray_hotkey(spec: &str) -> Option<&str> {
    (!hotkey::desktop_picks_keys()).then_some(spec)
}

/// How many captures can be stacked by pressing the hotkey during a capture.
const MAX_NESTED_CAPTURES: usize = 4;

const HELP: &str = "snapr - region screenshot tool

Runs in the system tray; use its menu or the settings window to configure.

USAGE: snapr [OPTIONS]
       snapr upload [OPTIONS] <FILE>...

OPTIONS:
      --once       Capture immediately with the saved settings, then exit
      --settings   Open the settings window on start
  -h, --help       Show this help

COMMANDS:
  upload           Upload files and print their links (see snapr upload --help)";

/// Holds an exclusive lock on a file in the config folder for as long as
/// snapr runs, so only one instance handles the hotkey.
fn single_instance() -> Option<std::fs::File> {
    let dir = Settings::config_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    let file = std::fs::File::create(dir.join("snapr.lock")).ok()?;
    match file.try_lock() {
        Ok(()) => Some(file),
        Err(std::fs::TryLockError::WouldBlock) => {
            #[cfg(windows)]
            tray::signal_running_instance();
            eprintln!("snapr is already running");
            std::process::exit(0);
        }
        Err(_) => None, // locking unsupported; run anyway
    }
}

fn main() {
    // Release builds have no console; reattach to the launching terminal (if
    // any) so --help and errors are still visible there.
    #[cfg(all(windows, not(debug_assertions)))]
    unsafe {
        windows_sys::Win32::System::Console::AttachConsole(
            windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS,
        );
    }

    let mut args = std::env::args().skip(1).peekable();
    if args.next_if_eq("upload").is_some() {
        std::process::exit(cli::upload(args));
    }
    // What Settings > Apps runs.
    #[cfg(windows)]
    if args.next_if_eq("--uninstall").is_some() {
        std::process::exit(install::uninstall());
    }
    let (mut once, mut open_settings) = (false, false);
    // The copy snapr was moved from, to remove (`install::relaunch`).
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut moved_from: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--once" => once = true,
            "--settings" => open_settings = true,
            #[cfg(windows)]
            "--moved-from" if args.peek().is_some() => moved_from = args.next().map(PathBuf::from),
            "--help" | "-h" => {
                println!("{HELP}");
                return;
            }
            other => {
                eprintln!("unknown argument: {other} (see --help)");
                std::process::exit(2);
            }
        }
    }
    update::init();
    #[cfg(windows)]
    if let Some(old) = moved_from.clone() {
        install::remove_moved(old);
    }
    // Ready for the text tool's font list by the time it's opened.
    fonts::preload();
    let lock = if once { None } else { single_instance() };

    #[cfg_attr(not(target_os = "linux"), allow(unused_mut))]
    let (mut settings, loaded) = Settings::load();
    theme::pick(settings.theme);
    // On Linux the desktop's settings can turn it on or off too.
    #[cfg(target_os = "linux")]
    {
        settings.start_with_windows = autostart::enabled();
    }
    // Point it at this file again, in case snapr was moved.
    #[cfg(any(windows, target_os = "linux"))]
    if settings.start_with_windows
        && !once
        && let Err(e) = autostart::refresh()
    {
        eprintln!("{e}");
    }
    // Development builds would point it at themselves.
    #[cfg(windows)]
    if settings.start_menu
        && !once
        && !cfg!(debug_assertions)
        && let Err(e) = start_menu::refresh()
    {
        eprintln!("{e}");
    }
    // Listed in Settings > Apps, to uninstall it from there.
    #[cfg(windows)]
    if !once && let Err(e) = install::register() {
        eprintln!("{e}");
    }
    let mut status = None;
    match loaded {
        Loaded::FirstRun => open_settings = true,
        Loaded::Existing => {}
        Loaded::Broken(e) => {
            status = Some((format!("{e} \u{2014} using defaults"), true));
            open_settings = true;
        }
    }

    #[allow(unused_mut)]
    let mut builder = EventLoop::<UserEvent>::with_user_event();
    #[cfg(target_os = "macos")]
    {
        use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
        builder.with_activation_policy(ActivationPolicy::Accessory);
    }
    let event_loop = builder.build().expect("failed to create event loop");
    event_loop.set_control_flow(ControlFlow::Wait);
    #[cfg(target_os = "macos")]
    macos::ask_for_screen_recording();
    let proxy = event_loop.create_proxy();
    // Under *System*, windows follow the desktop's style (see `theme::preference`).
    #[cfg(target_os = "linux")]
    {
        let proxy = Mutex::new(proxy.clone());
        portal::watch_dark_style(move |dark| {
            theme::set_desktop_dark(dark);
            let _ = proxy.lock().unwrap().send_event(UserEvent::DesktopStyleChanged);
        });
    }
    // Development builds are restarted too often to check each time.
    if !once && !cfg!(debug_assertions) {
        let proxy = Mutex::new(proxy.clone());
        update::schedule(move || {
            let _ = proxy.lock().unwrap().send_event(UserEvent::UpdateDue);
        });
    }

    if !once {
        let problems = Mutex::new(proxy.clone());
        hotkey::on_problem(move |e| {
            let _ = problems.lock().unwrap().send_event(UserEvent::HotkeyProblem(e));
        });
        let keys = Mutex::new(proxy.clone());
        hotkey::on_keys_changed(move || {
            let _ = keys.lock().unwrap().send_event(UserEvent::HotkeyKeysChanged);
        });
    }
    let hotkeys = (!once).then(|| {
        let mut h = Hotkeys::new(proxy.clone(), hotkey::CAPTURE, "Capture region", || UserEvent::Hotkey);
        if let Err(e) = h.set(&settings.hotkey) {
            status = Some((e, true));
            open_settings = true;
        }
        h
    });
    let record_hotkeys = (!once).then(|| {
        let mut h = Hotkeys::new(proxy.clone(), hotkey::RECORD, "Record region", || {
            UserEvent::RecordHotkey
        });
        if let Err(e) = h.set_optional(&settings.record_hotkey) {
            status.get_or_insert((format!("Record hotkey: {e}"), true));
            open_settings = true;
        }
        h
    });
    let tool_hotkeys = if once {
        Vec::new()
    } else {
        let (hotkeys, result) = tool_hotkeys(&proxy, &settings.tool_hotkeys);
        if let Err(e) = result {
            status.get_or_insert((e, true));
            open_settings = true;
        }
        hotkeys
    };
    // Asked for from the window, as GNOME only asks the focused app, and a
    // capture from the hotkey or the tray has no snapr window focused.
    #[cfg(target_os = "linux")]
    if !once && portal::gnome() && portal::screenshots_blocked() {
        status.get_or_insert((portal::SCREENSHOTS_OFF.into(), true));
        open_settings = true;
    }

    let config = output_config(&settings).unwrap_or_else(|e| {
        status = Some((format!("{e} \u{2014} using default naming"), true));
        open_settings = true;
        output_config(&Settings::default()).expect("default naming is valid")
    });
    let output_proxy = Mutex::new(proxy.clone());
    let output = Output::spawn(
        config,
        Box::new(move |event| {
            let _ = output_proxy
                .lock()
                .unwrap()
                .send_event(UserEvent::Output(event));
        }),
    );

    let gpu = Gpu::new();
    if let Err(e) = &gpu {
        status = Some((e.clone(), true));
    }

    if let Some((msg, true)) = &status {
        eprintln!("{msg}");
    }

    let remembered = remembered::Remembered::load();
    let mut app = App {
        proxy,
        settings,
        once,
        gpu,
        hotkeys,
        record_hotkeys,
        tool_hotkeys,
        recording: None,
        tray: None,
        tray_progress: None,
        output,
        sessions: Vec::new(),
        style: remembered.style(),
        magnifier: remembered.magnifier(),
        font_pending: remembered.font().map(String::from),
        remembered,
        main_window: None,
        toast: None,
        pins: Vec::new(),
        last_upload: None,
        toast_after_upload: HashMap::new(),
        redact_image: None,
        capture_at: None,
        record_at: None,
        restore_main_window: false,
        #[cfg(target_os = "macos")]
        focus_back: None,
        chime_on_copy: false,
        status,
        last_capture: None,
        last_region: None,
        open_settings_on_start: open_settings || moved_from.is_some(),
        open_recent_on_start: moved_from.is_some() && !open_settings,
        stats: stats::Stats::load(),
        update: update::Status::default(),
        restart: false,
        #[cfg(windows)]
        moved_to: None,
    };
    if let Err(e) = event_loop.run_app(&mut app) {
        eprintln!("event loop error: {e}");
    }
    app.output.flush();
    hotkey::release();
    #[cfg(windows)]
    let moved_to = app.moved_to.take();
    #[cfg(not(windows))]
    let moved_to = None::<()>;
    if app.restart || moved_to.is_some() {
        // Let go of the tray icon and the single-instance lock first, or the
        // new copy would hand over to this one and quit.
        drop(app);
        drop(lock);
        #[cfg(windows)]
        let started = match moved_to {
            Some((installed, settings)) => install::relaunch(&installed, settings),
            None => update::restart(),
        };
        #[cfg(not(windows))]
        let started = update::restart();
        if let Err(e) = started {
            eprintln!("{e}");
        }
    }
}
