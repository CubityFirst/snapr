// No console window for release builds on Windows; snapr lives in the tray.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod annotate;
#[cfg(windows)]
mod autostart;
mod audio;
mod capture;
mod combine;
mod cursor;
mod decode;
mod destinations_ui;
#[cfg(windows)]
mod dxgi;
mod encode;
mod gallery;
mod gpu;
#[cfg(windows)]
mod hdr;
mod history;
mod hotkey;
mod main_window;
#[cfg(windows)]
mod mf;
mod naming;
mod output;
mod overlay;
mod pin;
mod player;
mod pomf;
#[cfg(test)]
mod preview;
mod qr;
mod record;
mod record_ui;
mod secrets;
mod session;
mod settings;
mod settings_ui;
mod sound;
mod stats;
mod stats_ui;
mod thumbnail;
mod toast;
mod toolbar;
mod tools;
mod tray;
mod update;
mod upload;

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
use crate::session::{MAGNIFIER_PIXELS, Outcome, Purpose, Session};
use crate::settings::{Loaded, RecordFormat, Settings, ToolAction, ToolHotkey, Upload};
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
}

/// A global hotkey, or why it's unavailable.
struct Hotkeys(Result<GlobalHotkey, String>);

impl Hotkeys {
    /// A hotkey that sends `event` (made fresh each time) when pressed.
    fn new(
        proxy: EventLoopProxy<UserEvent>,
        event: impl Fn() -> UserEvent + Send + 'static,
    ) -> Self {
        let proxy = Mutex::new(proxy);
        Self(GlobalHotkey::new(Box::new(move || {
            let _ = proxy.lock().unwrap().send_event(event());
        })))
    }

    fn set(&mut self, spec: &str) -> Result<(), String> {
        self.0.as_mut().map_err(|e| e.clone())?.set(spec)
    }

    /// Like `set`, but an empty spec removes the hotkey.
    fn set_optional(&mut self, spec: &str) -> Result<(), String> {
        let h = self.0.as_mut().map_err(|e| e.clone())?;
        if spec.trim().is_empty() {
            h.clear();
            return Ok(());
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
    output: Output,
    /// Open captures. Pressing the hotkey during a capture stacks a new one
    /// on top (so the overlay itself can be captured); the last is active.
    sessions: Vec<Session>,
    /// Annotation colour and size, remembered between captures.
    style: Style,
    /// Pixels across the magnifier (its zoom), remembered between captures.
    magnifier: u32,
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
    /// Chime when the next copy-to-clipboard finishes (copies from the
    /// Recent page; a capture already played the shutter).
    chime_on_copy: bool,
    /// Shown in the settings page, e.g. a startup error.
    status: Option<(String, bool)>,
    last_capture: Option<PathBuf>,
    /// The last screenshot's region, for capturing it again.
    last_region: Option<capture::Rect>,
    open_settings_on_start: bool,
    /// What's been captured and which tools were used, for the Stats page.
    stats: stats::Stats,
    /// Where checking for and installing updates is up to.
    update: update::Status,
    /// Start the program again once the event loop exits (to run an update).
    restart: bool,
}

impl App {
    fn start_capture(&mut self, event_loop: &ActiveEventLoop) {
        self.open_session(event_loop, Purpose::Screenshot);
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
                    Ok(gpu) => record_ui::RecordingUi::open(event_loop, gpu, recording.rect)
                        .inspect_err(|e| eprintln!("recording border: {e}"))
                        .ok(),
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
            Err(e) => self.report_on(event_loop, format!("recording failed: {e}"), true, Page::Recording),
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
        let (path, first_frame) = match result {
            Ok(Some(r)) => {
                if let Some(w) = r.warning {
                    self.report_on(event_loop, format!("recording: {w}"), true, Page::Recording);
                }
                (r.path, r.first_frame)
            }
            Ok(None) => return self.set_status("Recording discarded".into()),
            Err(e) => return self.report_on(event_loop, format!("recording failed: {e}"), true, Page::Recording),
        };
        println!("recorded {}", path.display());
        self.play(Sound::Done);
        self.count(|s| s.count(stats::Kind::Recording, chrono::Local::now()));
        self.last_capture = Some(path.clone());
        let uploading = self.settings.uploads.iter().any(|u| u.enabled);
        if uploading {
            self.output.upload_file(path.clone(), ctx);
        }
        self.status = Some((format!("Recording saved to {}", path.display()), false));
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
        let s = &self.settings;
        // Every capture starts by selecting a region; a nested capture keeps
        // the colour and size of the one below.
        let style = self.sessions.last().map_or(self.style, |s| s.style);
        let magnifier = self.sessions.last().map_or(self.magnifier, |s| s.magnifier);
        // Our own windows a click can take, like any other app's.
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
        {
            // Its windows close at the end of this block.
            let session = self.sessions.remove(index);
            self.style = session.style;
            self.magnifier = session.magnifier;
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
        if std::mem::take(&mut self.restore_main_window)
            && let Some(w) = &self.main_window
        {
            w.window.set_visible(true);
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
            if let Err(e) = secrets::set(id, secret) {
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
        #[cfg(windows)]
        if new.start_with_windows != self.settings.start_with_windows
            && let Err(e) = autostart::set(new.start_with_windows)
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
                secrets::delete(&old.id);
            }
        }
        if let Some(t) = &self.tray {
            t.set_hotkey(Some(&new.hotkey));
        }
        self.settings = new;
        if let Some(w) = &mut self.main_window {
            w.form.saved(&self.settings);
            w.hotkey = self.settings.hotkey.clone();
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
        let [r, g, b] = rgb;
        self.count(|s| s.colors_picked += 1);
        self.chime_on_copy = true;
        self.output.copy_text(format!("#{r:02X}{g:02X}{b:02X}"));
        self.open_main_window(event_loop, Page::Tools);
        if let Some(w) = &mut self.main_window {
            w.tools.picked(rgb);
            w.window.request_redraw();
        }
    }

    fn main_window_action(&mut self, event_loop: &ActiveEventLoop, action: Action) {
        match action {
            Action::Capture => self.capture_from_main_window(Start::Pick(Purpose::Screenshot)),
            Action::CaptureTarget(t) => self.capture_from_main_window(Start::Grab(t)),
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
            Action::TestUpload(upload, secret) => self.test_upload(upload, secret, false),
            Action::UploadIcon(upload, secret) => self.test_upload(upload, secret, true),
            Action::CheckForUpdates => self.check_for_updates(),
            Action::RestartToUpdate => {
                if let Some(save) = self.main_window.as_mut().and_then(|w| w.flush_settings()) {
                    self.main_window_action(event_loop, save);
                }
                self.restart = true;
                event_loop.exit();
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
            let result = secrets::get(&upload.id)
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
                        .or_else(|| secrets::get(&upload.id))
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
        match Toast::open(event_loop, gpu, path, image, link) {
            Ok(t) => self.toast = Some(t),
            Err(e) => eprintln!("{e}"),
        }
    }

    fn toast_event(&mut self, event: WindowEvent) {
        let Some(t) = &mut self.toast else { return };
        match t.on_event(&event) {
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
            .then(|| self.settings.hotkey.clone());
        match Tray::new(hotkey.as_deref(), callback) {
            Ok(t) => self.tray = Some(t),
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
            self.open_main_window(event_loop, Page::Settings);
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
            UserEvent::Output(output::Event::Uploaded {
                path,
                name,
                url,
                remote,
            }) => {
                println!("uploaded to {name}: {url}");
                self.play(Sound::Done);
                if let Some(path) = &path {
                    history::set_link(path, Some(&url), remote.clone());
                    self.last_upload = Some((path.clone(), url.clone()));
                    if let Some(t) = self.toast.as_mut().filter(|t| t.path == *path) {
                        t.set_link(url.clone());
                    }
                }
                let copied = if self.settings.copy_link {
                    " (link copied)"
                } else {
                    ""
                };
                self.status = Some((format!("Uploaded to {name}{copied}"), false));
                if let Some(w) = &mut self.main_window {
                    w.form.status = self.status.clone();
                    if let Some(path) = &path {
                        w.gallery.set_link(path, Some(url), remote);
                    }
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
                self.set_status(format!("Upload to {name} cancelled"));
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
            UserEvent::ToolHotkey(action) => {
                let purpose = match action {
                    ToolAction::PickColor => Purpose::PickColor,
                    ToolAction::ScanQr => Purpose::Scan,
                    ToolAction::PinRegion => Purpose::Pin,
                };
                self.open_session(event_loop, purpose);
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
            if self.sessions.is_empty()
                && std::mem::take(&mut self.restore_main_window)
                && let Some(w) = &self.main_window
            {
                w.window.set_visible(true);
            }
        }
        if self.record_at.as_ref().is_some_and(|(at, ..)| *at <= now)
            && let Some((_, rect, context)) = self.record_at.take()
        {
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
        if self.toast.as_ref().is_some_and(|t| t.window.id() == id) {
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
/// (or no stored secret key) are left out and reported.
fn output_config(settings: &Settings) -> Result<output::Config, String> {
    let mut uploads = Vec::new();
    for u in settings.uploads.iter().filter(|u| u.enabled) {
        let key = match u.validate() {
            Ok(k) => k,
            Err(e) => {
                eprintln!("upload destination {}: {e}", u.name);
                continue;
            }
        };
        let secret = if u.needs_secret() {
            let Some(secret) = secrets::get(&u.id) else {
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
    for t in list {
        let action = t.action;
        let mut h = Hotkeys::new(proxy.clone(), move || UserEvent::ToolHotkey(action));
        match h.set(&t.hotkey) {
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

/// How many captures can be stacked by pressing the hotkey during a capture.
const MAX_NESTED_CAPTURES: usize = 4;

const HELP: &str = "snapr - region screenshot tool

Runs in the system tray; use its menu or the settings window to configure.

USAGE: snapr [OPTIONS]

OPTIONS:
      --once       Capture immediately with the saved settings, then exit
      --settings   Open the settings window on start
  -h, --help       Show this help";

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

    let (mut once, mut open_settings) = (false, false);
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--once" => once = true,
            "--settings" => open_settings = true,
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
    let lock = if once { None } else { single_instance() };

    let (settings, loaded) = Settings::load();
    // Point it at this file again, in case snapr was moved.
    #[cfg(windows)]
    if settings.start_with_windows
        && !once
        && let Err(e) = autostart::refresh()
    {
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
    let proxy = event_loop.create_proxy();
    // Development builds are restarted too often to check each time.
    if !once && !cfg!(debug_assertions) {
        let proxy = Mutex::new(proxy.clone());
        update::schedule(move || {
            let _ = proxy.lock().unwrap().send_event(UserEvent::UpdateDue);
        });
    }

    let hotkeys = (!once).then(|| {
        let mut h = Hotkeys::new(proxy.clone(), || UserEvent::Hotkey);
        if let Err(e) = h.set(&settings.hotkey) {
            status = Some((e, true));
            open_settings = true;
        }
        h
    });
    let record_hotkeys = (!once).then(|| {
        let mut h = Hotkeys::new(proxy.clone(), || UserEvent::RecordHotkey);
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
        output,
        sessions: Vec::new(),
        style: Style::default(),
        magnifier: MAGNIFIER_PIXELS,
        main_window: None,
        toast: None,
        pins: Vec::new(),
        last_upload: None,
        toast_after_upload: HashMap::new(),
        redact_image: None,
        capture_at: None,
        record_at: None,
        restore_main_window: false,
        chime_on_copy: false,
        status,
        last_capture: None,
        last_region: None,
        open_settings_on_start: open_settings,
        stats: stats::Stats::load(),
        update: update::Status::default(),
        restart: false,
    };
    if let Err(e) = event_loop.run_app(&mut app) {
        eprintln!("event loop error: {e}");
    }
    app.output.flush();
    if app.restart {
        // Let go of the tray icon and the single-instance lock first, or the
        // new copy would hand over to this one and quit.
        drop(app);
        drop(lock);
        if let Err(e) = update::restart() {
            eprintln!("{e}");
        }
    }
}
