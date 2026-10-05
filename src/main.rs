// No console window for release builds on Windows; snapr lives in the tray.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod annotate;
mod audio;
mod capture;
mod combine;
mod cursor;
mod destinations_ui;
mod gallery;
mod gpu;
mod history;
mod hotkey;
mod main_window;
mod naming;
mod output;
mod overlay;
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
mod thumbnail;
mod toast;
mod toolbar;
mod tools;
mod tray;
mod upload;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::event::{StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::WindowId;

use crate::annotate::{Style, Tool};
use crate::gallery::Gallery;
use crate::gpu::Gpu;
use crate::hotkey::GlobalHotkey;
use crate::main_window::{Action, MainWindow, Page};
use crate::output::Output;
use crate::session::{Outcome, Purpose, Session};
use crate::settings::{Loaded, Settings, Upload};
use crate::settings_ui::Form;
use crate::settings_ui::Test;
use crate::sound::Sound;
use crate::toast::Toast;
use crate::tray::{Tray, TrayAction};

#[derive(Debug)]
enum UserEvent {
    Hotkey,
    RecordHotkey,
    /// A screen recording was written (or failed).
    RecordingDone(Result<Option<record::Recorded>, String>, naming::Context),
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
    /// Images from Recent were stitched together (how many, the result).
    Combined(usize, Result<image::RgbaImage, String>),
}

/// A global hotkey, or why it's unavailable.
struct Hotkeys(Result<GlobalHotkey, String>);

impl Hotkeys {
    /// A hotkey that sends `event` (made fresh each time) when pressed.
    fn new(proxy: EventLoopProxy<UserEvent>, event: fn() -> UserEvent) -> Self {
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

    fn is_registered(&self) -> bool {
        self.0.as_ref().is_ok_and(|h| h.current().is_some())
    }
}

/// A recording in progress and its border and buttons.
struct ActiveRecording {
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
    /// The screen recording in progress.
    recording: Option<ActiveRecording>,
    tray: Option<Tray>,
    output: Output,
    /// Open captures. Pressing the hotkey during a capture stacks a new one
    /// on top (so the overlay itself can be captured); the last is active.
    sessions: Vec<Session>,
    /// Annotation colour and size, remembered between captures.
    style: Style,
    main_window: Option<MainWindow>,
    /// The preview in the corner of the screen after a capture.
    toast: Option<Toast>,
    /// The latest upload's screenshot and link, for a preview that's still
    /// being decoded when the upload finishes.
    last_upload: Option<(PathBuf, String)>,
    /// A capture started from the main window, delayed until it's hidden.
    capture_at: Option<(Instant, Purpose)>,
    /// Show the main window again once the capture it started is over.
    restore_main_window: bool,
    /// Chime when the next copy-to-clipboard finishes (copies from the
    /// Recent page; a capture already played the shutter).
    chime_on_copy: bool,
    /// Shown in the settings page, e.g. a startup error.
    status: Option<(String, bool)>,
    last_capture: Option<PathBuf>,
    open_settings_on_start: bool,
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
        if self.sessions.is_empty() {
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
        let out = naming.path_for(&ctx).with_extension("mp4");
        let proxy = Mutex::new(self.proxy.clone());
        let done_ctx = ctx.clone();
        let done: record::Done = Box::new(move |result| {
            let _ = proxy
                .lock()
                .unwrap()
                .send_event(UserEvent::RecordingDone(result, done_ctx));
        });
        match record::Recording::start(
            rect,
            self.settings.record_fps,
            &self.settings.ffmpeg_path,
            &self.settings.audio_sources(),
            self.settings.record_cursor,
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
            Err(e) => self.report(event_loop, format!("recording failed: {e}"), true),
        }
    }

    fn recording_done(
        &mut self,
        event_loop: &ActiveEventLoop,
        result: Result<Option<record::Recorded>, String>,
        ctx: naming::Context,
    ) {
        let (path, first_frame) = match result {
            Ok(Some(r)) => {
                if let Some(w) = r.warning {
                    self.report(event_loop, format!("recording: {w}"), true);
                }
                (r.path, r.first_frame)
            }
            Ok(None) => return self.set_status("Recording discarded".into()),
            Err(e) => return self.report(event_loop, format!("recording failed: {e}"), true),
        };
        println!("recorded {}", path.display());
        self.play(Sound::Done);
        self.last_capture = Some(path.clone());
        if self.settings.uploads.iter().any(|u| u.enabled) {
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
            self.show_toast(event_loop, path, Some(toast::preview_of(first_frame)));
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
        match Session::new(
            event_loop,
            gpu,
            s.annotate && purpose == Purpose::Screenshot,
            s.overlay_fps,
            Tool::Select,
            style,
        ) {
            Ok(mut session) => {
                session.purpose = purpose;
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

    fn end_capture(&mut self, event_loop: &ActiveEventLoop, index: usize, outcome: Outcome) {
        if index >= self.sessions.len() {
            return;
        }
        {
            // Its windows close at the end of this block.
            let session = self.sessions.remove(index);
            self.style = session.style;
            match outcome {
                Outcome::Done { image, save_file } => {
                    self.play(Sound::Capture);
                    self.output.submit(image, session.context, save_file);
                }
                Outcome::Record(rect) => {
                    self.play(Sound::Capture);
                    // Close the overlay first, or the frozen screen would
                    // be the start of the recording.
                    let context = session.context.clone();
                    drop(session);
                    self.start_recording(event_loop, rect, context);
                }
                Outcome::Scan(image) => {
                    self.play(Sound::Capture);
                    self.read_qr(image);
                }
                Outcome::Color(rgb) => self.color_picked(event_loop, rgb),
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
        let proxy = Mutex::new(self.proxy.clone());
        let wake = Box::new(move || {
            let _ = proxy.lock().unwrap().send_event(UserEvent::ThumbnailReady);
        });
        let gallery = Gallery::new(
            std::path::Path::new(&self.settings.folder),
            self.settings.ffmpeg_path.clone(),
            wake,
        );
        match MainWindow::open(event_loop, gpu, page, form, gallery, &self.settings) {
            Ok(w) => self.main_window = Some(w),
            Err(e) => eprintln!("{e}"),
        }
    }

    fn play(&self, sound: Sound) {
        if self.settings.play_sounds {
            sound::play(sound);
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
        }
        self.report(event_loop, "Settings saved".into(), false);
    }

    /// Starts a capture from the main window, once it's hidden so it isn't
    /// in the screenshot.
    fn capture_from_main_window(&mut self, purpose: Purpose) {
        if let Some(w) = &self.main_window {
            w.window.set_visible(false);
            self.restore_main_window = true;
        }
        self.capture_at = Some((Instant::now() + Duration::from_millis(250), purpose));
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
            Action::Capture => self.capture_from_main_window(Purpose::Screenshot),
            Action::ScanQr => self.capture_from_main_window(Purpose::Scan),
            Action::PickColor => self.capture_from_main_window(Purpose::PickColor),
            Action::SaveSettings(s, secrets) => self.apply_settings(event_loop, s, secrets),
            Action::TestUpload(upload, secret) => self.test_upload(upload, secret),
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
                self.prepare_toast(path.clone());
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
                .and_then(|secret| upload.target(secret).delete_object(&remote.key));
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

    /// Uploads a tiny file to the destination and deletes it again.
    fn test_upload(&self, upload: Upload, secret: Option<String>) {
        let proxy = Mutex::new(self.proxy.clone());
        let id = upload.id.clone();
        std::thread::spawn(move || {
            let result = (|| {
                upload.validate()?;
                let secret = secret
                    .or_else(|| secrets::get(&upload.id))
                    .ok_or("enter the secret access key")?;
                let target = upload.target(secret);
                let key = format!(
                    "snapr-test-{}.txt",
                    (0..8).map(|_| fastrand::alphanumeric()).collect::<String>()
                );
                target.put_object(&key, b"snapr upload test", "text/plain")?;
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

    fn main_window_event(&mut self, event_loop: &ActiveEventLoop, event: WindowEvent) {
        let Some(w) = &mut self.main_window else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => self.main_window = None,
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
            UserEvent::RecordingDone(result, ctx) => self.recording_done(event_loop, result, ctx),
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
            UserEvent::Output(output::Event::Saved(path)) => {
                println!("saved {}", path.display());
                history::add(&path);
                self.status = None;
                self.last_capture = Some(path.clone());
                if self.settings.show_toast && !self.once {
                    self.prepare_toast(path.clone());
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
                    history::set_link(path, Some(&url), Some(remote.clone()));
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
                        w.gallery.set_link(path, Some(url), Some(remote));
                    }
                    w.window.request_redraw();
                }
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
            UserEvent::Combined(count, Ok(image)) => {
                // A new screenshot: saved, copied and uploaded like a capture.
                let mut ctx = naming::Context::new(chrono::Local::now());
                ctx.title = Some("combined".into());
                self.output.submit(image, ctx, true);
                self.gallery_notice(event_loop, format!("Combined {count} images"), false);
            }
            UserEvent::Combined(_, Err(e)) => {
                self.gallery_notice(event_loop, format!("Couldn't combine: {e}"), true)
            }
            UserEvent::QrScanned(texts, preview) => {
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
            UserEvent::ThumbnailReady => {
                if let Some(w) = &self.main_window {
                    w.window.request_redraw();
                }
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        if let Some((at, purpose)) = self.capture_at
            && at <= now
        {
            self.capture_at = None;
            self.open_session(event_loop, purpose);
            if self.sessions.is_empty()
                && std::mem::take(&mut self.restore_main_window)
                && let Some(w) = &self.main_window
            {
                w.window.set_visible(true);
            }
        }
        let mut wake = self.sessions.iter_mut().filter_map(|s| s.poll(now)).min();
        wake = [wake, self.capture_at.map(|(t, _)| t)]
            .into_iter()
            .flatten()
            .min();
        match self.toast.as_ref().and_then(|t| t.expires_at) {
            Some(t) if t <= now => self.toast = None,
            Some(t) => wake = Some(wake.map_or(t, |w| w.min(t))),
            None => {}
        }
        if let Some(ui) = self.recording.as_mut().and_then(|r| r.ui.as_mut()) {
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
        let Some(secret) = secrets::get(&u.id) else {
            eprintln!("upload destination {}: no secret key stored", u.name);
            continue;
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
    let _lock = if once { None } else { single_instance() };

    let (settings, loaded) = Settings::load();
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
        recording: None,
        tray: None,
        output,
        sessions: Vec::new(),
        style: Style::default(),
        main_window: None,
        toast: None,
        last_upload: None,
        capture_at: None,
        restore_main_window: false,
        chime_on_copy: false,
        status,
        last_capture: None,
        open_settings_on_start: open_settings,
    };
    if let Err(e) = event_loop.run_app(&mut app) {
        eprintln!("event loop error: {e}");
    }
    app.output.flush();
}
