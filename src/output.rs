use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

use image::RgbaImage;

use crate::history::Remote;
use crate::naming::{Context, Naming, Template};
use crate::settings::{Settings, Upload};
use crate::upload::{self, Body, Progress, Target};

/// An upload destination, ready to use (secret key included).
pub struct UploadTarget {
    pub upload: Upload,
    pub target: Target,
    pub key: Template,
}

/// Where screenshots go.
pub struct Config {
    pub naming: Naming,
    pub save_to_folder: bool,
    pub copy_image: bool,
    pub copy_link: bool,
    pub uploads: Vec<UploadTarget>,
}

/// What happened to a screenshot, reported back to the app.
#[derive(Debug)]
pub enum Event {
    /// A screenshot was saved, and whether it's going to be uploaded.
    Saved {
        path: PathBuf,
        uploading: bool,
    },
    /// Copied to the clipboard without saving (or a file copied from Recent).
    Copied,
    Uploaded {
        path: Option<PathBuf>,
        name: String,
        url: String,
        /// Missing where uploads can't be deleted.
        remote: Option<Remote>,
    },
    /// An upload was cancelled (the destination's name).
    Cancelled(String),
    Failed(String),
    /// Every upload of this file has finished, whether it worked or not.
    UploadsFinished(PathBuf),
}

/// An upload in progress.
pub struct Transfer {
    /// The file's name.
    pub file: String,
    /// The destination's name.
    pub destination: String,
    pub progress: Progress,
    pub started: Instant,
}

/// The uploads in progress, oldest first.
pub type Transfers = Arc<Mutex<Vec<Arc<Transfer>>>>;

enum Job {
    /// The image, naming context, and whether this is a normal capture
    /// (`false`: only copy it to the clipboard).
    Save(RgbaImage, Context, bool),
    Configure(Box<Config>),
    /// Copy an existing screenshot file to the clipboard.
    CopyFile(PathBuf),
    /// Upload an existing file (dropped on the window) as it is.
    UploadFile(PathBuf, Context),
    CopyImage(RgbaImage),
    CopyText(String),
    /// An upload thread finished.
    Uploaded {
        path: Option<PathBuf>,
        name: String,
        remote: Option<Remote>,
        result: Result<String, String>,
    },
    Flush(Sender<()>),
}

pub type Notify = Box<dyn Fn(Event) + Send>;

/// Saves, copies and uploads screenshots on a background thread, so the
/// overlay closes immediately; uploads get threads of their own. This thread
/// also owns the clipboard handle, which on Linux must stay alive for the
/// contents to remain pasteable.
pub struct Output {
    tx: Sender<Job>,
    transfers: Transfers,
}

impl Output {
    pub fn spawn(config: Config, notify: Notify) -> Self {
        let (tx, rx) = mpsc::channel::<Job>();
        let uploads_tx = tx.clone();
        let transfers = Transfers::default();
        let in_progress = transfers.clone();
        thread::spawn(move || {
            let mut config = config;
            let mut clipboard = None;
            let mut in_flight = 0usize;
            // Uploads still running for each file.
            let mut pending: HashMap<PathBuf, usize> = HashMap::new();
            let mut waiting: Vec<Sender<()>> = Vec::new();
            for job in rx {
                match job {
                    Job::Save(img, mut ctx, capture) => {
                        (ctx.width, ctx.height) = img.dimensions();
                        let mut path = None;
                        let uploading = capture && !config.uploads.is_empty();
                        if capture && config.save_to_folder {
                            if config.naming.uses_counter() {
                                ctx.counter = next_counter();
                            }
                            match save(&config.naming.path_for(&ctx), &img) {
                                Ok(p) => {
                                    notify(Event::Saved {
                                        path: p.clone(),
                                        uploading,
                                    });
                                    path = Some(p);
                                }
                                Err(e) => notify(Event::Failed(e)),
                            }
                        }
                        if config.copy_image || !capture {
                            match copy_image(&mut clipboard, &img) {
                                Ok(()) if !capture => notify(Event::Copied),
                                Ok(()) => {}
                                Err(e) => notify(Event::Failed(e)),
                            }
                        }
                        if uploading {
                            // Upload the saved file as-is, or encode it now.
                            let png = path
                                .as_ref()
                                .and_then(|p| std::fs::read(p).ok())
                                .or_else(|| encode_png(&img));
                            let Some(png) = png else {
                                notify(Event::Failed(
                                    "couldn't encode the screenshot for uploading".into(),
                                ));
                                if let Some(p) = path {
                                    notify(Event::UploadsFinished(p));
                                }
                                continue;
                            };
                            if let Some(p) = &path {
                                *pending.entry(p.clone()).or_default() += config.uploads.len();
                            }
                            in_flight += upload_all(
                                &config.uploads,
                                &mut ctx,
                                Body::Bytes(Arc::new(png)),
                                "png",
                                path,
                                &uploads_tx,
                                &in_progress,
                            );
                        }
                    }
                    Job::UploadFile(file, mut ctx) => {
                        if config.uploads.is_empty() {
                            notify(Event::Failed(
                                "upload failed: no upload destinations are set up and enabled"
                                    .into(),
                            ));
                            notify(Event::UploadsFinished(file));
                            continue;
                        }
                        // Read as it's sent, so big recordings aren't
                        // loaded whole.
                        let body = Body::File(file.clone());
                        if let Err(e) = body.len() {
                            notify(Event::Failed(format!("upload failed: {e}")));
                            notify(Event::UploadsFinished(file));
                            continue;
                        }
                        *pending.entry(file.clone()).or_default() += config.uploads.len();
                        let ext = file
                            .extension()
                            .and_then(|e| e.to_str())
                            .unwrap_or("")
                            .to_ascii_lowercase();
                        in_flight += upload_all(
                            &config.uploads,
                            &mut ctx,
                            body,
                            &ext,
                            Some(file),
                            &uploads_tx,
                            &in_progress,
                        );
                    }
                    Job::Uploaded {
                        path,
                        name,
                        remote,
                        result,
                    } => {
                        in_flight -= 1;
                        match result {
                            Ok(url) => {
                                if config.copy_link
                                    && let Err(e) = copy_text(&mut clipboard, &url)
                                {
                                    notify(Event::Failed(e));
                                }
                                notify(Event::Uploaded {
                                    path: path.clone(),
                                    name,
                                    url,
                                    remote,
                                });
                            }
                            Err(e) if e == upload::CANCELLED => notify(Event::Cancelled(name)),
                            Err(e) => {
                                notify(Event::Failed(format!("upload to {name} failed: {e}")))
                            }
                        }
                        if let Some(p) = path
                            && let Some(left) = pending.get_mut(&p)
                        {
                            *left -= 1;
                            if *left == 0 {
                                pending.remove(&p);
                                notify(Event::UploadsFinished(p));
                            }
                        }
                        if in_flight == 0 {
                            for w in waiting.drain(..) {
                                let _ = w.send(());
                            }
                        }
                    }
                    Job::Configure(c) => config = *c,
                    Job::CopyFile(path) => {
                        let result = image::open(&path)
                            .map_err(|e| format!("couldn't read {}: {e}", path.display()))
                            .and_then(|img| copy_image(&mut clipboard, &img.to_rgba8()));
                        notify(match result {
                            Ok(()) => Event::Copied,
                            Err(e) => Event::Failed(e),
                        });
                    }
                    Job::CopyImage(img) => {
                        notify(match copy_image(&mut clipboard, &img) {
                            Ok(()) => Event::Copied,
                            Err(e) => Event::Failed(e),
                        });
                    }
                    Job::CopyText(text) => {
                        notify(match copy_text(&mut clipboard, &text) {
                            Ok(()) => Event::Copied,
                            Err(e) => Event::Failed(e),
                        });
                    }
                    Job::Flush(done) => {
                        if in_flight == 0 {
                            let _ = done.send(());
                        } else {
                            waiting.push(done);
                        }
                    }
                }
            }
        });
        Self { tx, transfers }
    }

    /// The uploads in progress, kept up to date.
    pub fn transfers(&self) -> Transfers {
        self.transfers.clone()
    }

    pub fn submit(&self, img: RgbaImage, ctx: Context, capture: bool) {
        let _ = self.tx.send(Job::Save(img, ctx, capture));
    }

    pub fn copy_file(&self, path: PathBuf) {
        let _ = self.tx.send(Job::CopyFile(path));
    }

    pub fn upload_file(&self, path: PathBuf, ctx: Context) {
        let _ = self.tx.send(Job::UploadFile(path, ctx));
    }

    pub fn copy_image(&self, img: RgbaImage) {
        let _ = self.tx.send(Job::CopyImage(img));
    }

    pub fn copy_text(&self, text: String) {
        let _ = self.tx.send(Job::CopyText(text));
    }

    pub fn configure(&self, config: Config) {
        let _ = self.tx.send(Job::Configure(Box::new(config)));
    }

    /// Blocks until every submitted screenshot has been handled, uploads included.
    pub fn flush(&self) {
        let (done_tx, done_rx) = mpsc::channel();
        if self.tx.send(Job::Flush(done_tx)).is_ok() {
            let _ = done_rx.recv();
        }
    }
}

/// Uploads `body` to each destination on threads of its own, listed in
/// `transfers` while they run, reporting back with `Job::Uploaded`. Returns
/// how many uploads were started.
fn upload_all(
    uploads: &[UploadTarget],
    ctx: &mut Context,
    body: Body,
    ext: &str,
    path: Option<PathBuf>,
    tx: &Sender<Job>,
    transfers: &Transfers,
) -> usize {
    let file = match &path {
        Some(p) => p.file_name().unwrap_or_default().to_string_lossy().into_owned(),
        None => format!("Screenshot.{ext}"),
    };
    let content_type = content_type(ext);
    for u in uploads {
        if u.key.uses_counter() && ctx.counter == 0 {
            ctx.counter = next_counter();
        }
        let key = u.key.render_key(ctx, ext);
        let (target, upload, body, tx, path) = (
            u.target.clone(),
            u.upload.clone(),
            body.clone(),
            tx.clone(),
            path.clone(),
        );
        let transfer = Arc::new(Transfer {
            file: file.clone(),
            destination: upload.name.clone(),
            progress: Progress::default(),
            started: Instant::now(),
        });
        transfers.lock().unwrap().push(transfer.clone());
        let transfers = transfers.clone();
        thread::spawn(move || {
            let result = upload.put(&target, &key, &body, content_type, &transfer.progress);
            transfers
                .lock()
                .unwrap()
                .retain(|t| !Arc::ptr_eq(t, &transfer));
            let _ = tx.send(Job::Uploaded {
                path,
                remote: upload.deletable().then(|| Remote {
                    upload_id: upload.id,
                    key,
                }),
                name: upload.name,
                result,
            });
        });
    }
    uploads.len()
}

/// The `Content-Type` for a (lower-case) file extension, so browsers show
/// common files rather than downloading them.
fn content_type(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "json" => "application/json",
        "txt" | "log" | "md" => "text/plain; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        "html" | "htm" => "text/html; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn encode_png(img: &RgbaImage) -> Option<Vec<u8>> {
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    Some(png)
}

fn copy_text(clipboard: &mut Option<arboard::Clipboard>, text: &str) -> Result<(), String> {
    if clipboard.is_none() {
        *clipboard =
            Some(arboard::Clipboard::new().map_err(|e| format!("clipboard unavailable: {e}"))?);
    }
    clipboard
        .as_mut()
        .expect("just set")
        .set_text(text)
        .map_err(|e| format!("failed to copy to clipboard: {e}"))
}

fn save(path: &Path, img: &RgbaImage) -> Result<PathBuf, String> {
    let fail = |e: &dyn std::fmt::Display| format!("failed to save {}: {e}", path.display());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| fail(&e))?;
    }
    img.save(path).map_err(|e| fail(&e))?;
    Ok(path.to_owned())
}

fn counter_path() -> Option<PathBuf> {
    Some(Settings::config_dir()?.join("counter"))
}

/// The last `%i` value used (0 if none yet).
pub fn peek_counter() -> u64 {
    counter_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

fn next_counter() -> u64 {
    let n = peek_counter() + 1;
    if let Some(path) = counter_path() {
        let _ = std::fs::create_dir_all(path.parent().unwrap_or(&path));
        if let Err(e) = std::fs::write(&path, n.to_string()) {
            eprintln!("couldn't save counter: {e}");
        }
    }
    n
}

fn copy_image(clipboard: &mut Option<arboard::Clipboard>, img: &RgbaImage) -> Result<(), String> {
    if clipboard.is_none() {
        *clipboard =
            Some(arboard::Clipboard::new().map_err(|e| format!("clipboard unavailable: {e}"))?);
    }
    let data = arboard::ImageData {
        width: img.width() as usize,
        height: img.height() as usize,
        bytes: Cow::Borrowed(img.as_raw()),
    };
    clipboard
        .as_mut()
        .expect("just set")
        .set_image(data)
        .map_err(|e| format!("failed to copy to clipboard: {e}"))
}

/// Opens a file with its default app.
pub fn open(path: &Path) {
    let result = if cfg!(windows) {
        // `start` resolves the default app; the empty string is the window title.
        std::process::Command::new("cmd")
            .args(["/C", "start", ""])
            .arg(path)
            .spawn()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(path).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(path).spawn()
    };
    if let Err(e) = result {
        eprintln!("couldn't open {}: {e}", path.display());
    }
}

/// Shows a file selected in the system file manager.
/// Opens a web link in the default browser.
pub fn open_url(url: &str) {
    let result = if cfg!(windows) {
        // Not `cmd /C start`, which would treat `&` in the URL as a separator.
        std::process::Command::new("rundll32")
            .args(["url.dll,FileProtocolHandler", url])
            .spawn()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(url).spawn()
    };
    if let Err(e) = result {
        eprintln!("couldn't open {url}: {e}");
    }
}

pub fn show_in_folder(path: &Path) {
    let result = if cfg!(windows) {
        std::process::Command::new("explorer")
            .arg(format!("/select,{}", path.display()))
            .spawn()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .spawn()
    } else {
        // No portable "select"; open the containing folder.
        std::process::Command::new("xdg-open")
            .arg(path.parent().unwrap_or(path))
            .spawn()
    };
    if let Err(e) = result {
        eprintln!("couldn't show {}: {e}", path.display());
    }
}

/// Opens a folder (created if missing) or file in the system file manager.
pub fn reveal(path: &Path) {
    if path.extension().is_none() {
        let _ = std::fs::create_dir_all(path);
    }
    let program = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    if let Err(e) = std::process::Command::new(program).arg(path).spawn() {
        eprintln!("couldn't open {}: {e}", path.display());
    }
}
