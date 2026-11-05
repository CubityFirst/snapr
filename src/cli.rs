//! `snapr upload`: uploads files to a destination set up in Settings and
//! prints their links, for scripts and AI agents. Keys stay in the
//! credential store; nothing but the links is printed.

use std::fmt::Arguments;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use image::RgbaImage;

use crate::history::{self, Remote};
use crate::settings::{Loaded, Settings, Upload};
use crate::upload::{Body, Progress};
use crate::clipboard_upload::{self, Contents};
use crate::{naming, output, policy, secrets};

const HELP: &str = "snapr upload - upload files and print their links

USAGE: snapr upload [OPTIONS] <FILE>...
       snapr upload [OPTIONS] --clipboard

Uploads each file to one destination from Settings and prints its link, one
per line, in the order given. They're added to Recent, as dropped files are.
Failures are reported on stderr, and the exit status is then 1.

--clipboard uploads what's on the clipboard, after any files given: files
copied in a file manager, an image (saved first, as a screenshot is, if
screenshots are saved) or text (as a .txt file).

Files are named by the destination's object key or file name, unless
--name is given. A file already in a bucket under that name is left alone
and reported as a failure, unless --force is given.

OPTIONS:
  -d, --destination <NAME>  Upload there (its name or id) instead of to the
                            Link from destination, or the first ticked one
  -n, --name <KEY>          Upload the file (only one) as this, e.g.
                            docs/shot.png; `/` makes folders, and the file's
                            extension is added if it has none. Pomf hosts
                            are sent it too, though most pick their own
  -f, --force               Replace a file already in the bucket under the
                            same name (Pomf hosts never replace anything)
  -c, --clipboard           Upload what's on the clipboard
  -l, --list                List the destinations, then exit
  -h, --help                Show this help";

/// Runs `snapr upload` with the arguments after `upload`; returns the exit
/// status.
pub fn upload(mut args: impl Iterator<Item = String>) -> i32 {
    let (mut files, mut wanted, mut list) = (Vec::new(), None, false);
    let (mut name, mut force, mut clipboard) = (None, false, false);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-d" | "--destination" => match args.next() {
                Some(name) => wanted = Some(name),
                None => return usage("--destination needs a destination's name"),
            },
            "-n" | "--name" => match args.next() {
                Some(n) => name = Some(n),
                None => return usage("--name needs a name to upload the file as"),
            },
            "-f" | "--force" => force = true,
            "-c" | "--clipboard" => clipboard = true,
            "-l" | "--list" => list = true,
            "-h" | "--help" => {
                say(format_args!("{HELP}"));
                return 0;
            }
            "--" => files.extend(args.by_ref().map(PathBuf::from)),
            s if s.starts_with('-') && s.len() > 1 => {
                return usage(&format!("unknown option: {s}"));
            }
            _ => files.push(PathBuf::from(arg)),
        }
    }

    let settings = match Settings::load() {
        (_, Loaded::Broken(e)) => return fail(&e),
        (s, _) => s,
    };
    if list {
        print_list(&settings);
        return 0;
    }
    let mut items: Vec<Item> = files.into_iter().map(Item::File).collect();
    // A clipboard that can't be read fails, as a file would, without
    // stopping the files given.
    let mut status = 0;
    if clipboard {
        // Its errors are written for snapr's window.
        let plain = |e: String| e.trim_start_matches("upload failed: ").to_owned();
        match clipboard_upload::read().map_err(plain) {
            Ok(Contents::Files(paths)) => items.extend(paths.into_iter().map(Item::File)),
            Ok(Contents::Image(image)) => items.push(Item::Image(image)),
            Ok(Contents::Text(text)) => items.push(Item::Text(text)),
            Err(e) if items.is_empty() => return fail(&e),
            Err(e) => {
                warn(format_args!("snapr upload: the clipboard: {e}"));
                status = 1;
            }
        }
    }
    if items.is_empty() {
        return usage("no files given");
    }
    if name.is_some() && items.len() > 1 {
        return usage("--name names one file, but more were given");
    }
    if let Some(n) = &name
        && object_name(n, "").is_none()
    {
        return usage(&format!("--name {n:?} has no name in it"));
    }
    if policy::uploads_disabled() {
        return fail(policy::UPLOADS_OFF);
    }
    let upload = match pick(&settings, wanted.as_deref()) {
        Ok(u) => u,
        Err(e) => return fail(&e),
    };
    let key = match upload.validate() {
        Ok(k) => k,
        Err(e) => return fail(&format!("destination {}: {e}", upload.name)),
    };
    let secret = if upload.needs_secret() {
        match secrets::get(upload) {
            Some(s) => s,
            None => {
                return fail(&format!(
                    "destination {}: no secret key stored; enter it in Settings",
                    upload.name
                ));
            }
        }
    } else {
        String::new()
    };
    let target = upload.target(secret);

    for item in items {
        let name = name.as_deref();
        let (what, result) = match item {
            Item::File(file) => {
                let result = std::path::absolute(&file)
                    .map_err(|e| e.to_string())
                    .and_then(|path| send(upload, &target, &key, name, force, &path));
                (file.display().to_string(), result)
            }
            Item::Image(image) => {
                let result = send_image(&settings, upload, &target, &key, name, force, &image);
                ("the clipboard's image".into(), result)
            }
            Item::Text(text) => {
                let mut ctx = naming::Context::new(chrono::Local::now());
                ctx.title = Some(clipboard_upload::TEXT_TITLE.into());
                let object = object_key(&key, name, "txt", &ctx);
                let body = Body::Bytes(Arc::new(text.into_bytes()));
                ("the clipboard's text".into(), put(upload, &target, &object, &body, "txt", force))
            }
        };
        match result {
            Ok(url) => say(format_args!("{url}")),
            Err(e) => {
                let hint = if e.ends_with(crate::upload::EXISTS) {
                    " (--force replaces it)"
                } else {
                    ""
                };
                warn(format_args!("snapr upload: {what}: {e}{hint}"));
                status = 1;
            }
        }
    }
    status
}

/// Something to upload: a file, or what was on the clipboard.
enum Item {
    File(PathBuf),
    Image(RgbaImage),
    Text(String),
}

/// Uploads an image from the clipboard: saved first as a screenshot would
/// be, if they are, and then sent as that file; else sent as a PNG with
/// nothing kept.
fn send_image(
    settings: &Settings,
    upload: &Upload,
    target: &crate::upload::Target,
    key: &naming::Template,
    name: Option<&str>,
    force: bool,
    image: &RgbaImage,
) -> Result<String, String> {
    let mut ctx = naming::Context::new(chrono::Local::now());
    (ctx.width, ctx.height) = image.dimensions();
    if settings.save_to_folder {
        let naming = settings.naming()?;
        if naming.uses_counter() {
            ctx.counter = output::next_counter();
        }
        let path = naming.path_for(&ctx);
        let saved = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .map_err(|e| e.to_string())
            .and_then(|()| image.save(&path).map_err(|e| e.to_string()));
        if let Err(e) = saved {
            return Err(format!("couldn't save {}: {e}", path.display()));
        }
        return send(upload, target, key, name, force, &path);
    }
    let mut png = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| format!("couldn't encode it: {e}"))?;
    let object = object_key(key, name, "png", &ctx);
    put(upload, target, &object, &Body::Bytes(Arc::new(png)), "png", force)
}

/// Sends `body` as `object`; returns its link.
fn put(
    upload: &Upload,
    target: &crate::upload::Target,
    object: &str,
    body: &Body,
    ext: &str,
    force: bool,
) -> Result<String, String> {
    upload.put(target, object, body, output::content_type(ext), force, &Progress::default())
}

/// The object key: `name` (checked before anything was sent), or else
/// rendered from the destination's key template.
fn object_key(key: &naming::Template, name: Option<&str>, ext: &str, ctx: &naming::Context) -> String {
    match name {
        Some(n) => object_name(n, ext).expect("a name"),
        None => {
            let mut ctx = ctx.clone();
            if key.uses_counter() && ctx.counter == 0 {
                ctx.counter = output::next_counter();
            }
            key.render_key(&ctx, ext)
        }
    }
}

/// Uploads one file, as `name` or else named as a dropped file would be,
/// replacing what's there already if `force`; returns its link.
fn send(
    upload: &Upload,
    target: &crate::upload::Target,
    key: &naming::Template,
    name: Option<&str>,
    force: bool,
    path: &Path,
) -> Result<String, String> {
    if !path.is_file() {
        return Err("not a file".into());
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let mut ctx = naming::Context::new(chrono::Local::now());
    ctx.title = path.file_stem().map(|s| s.to_string_lossy().into_owned());
    if let Ok((w, h)) = image::image_dimensions(path) {
        (ctx.width, ctx.height) = (w, h);
    }
    let object = object_key(key, name, &ext, &ctx);
    let url = put(upload, target, &object, &Body::File(path.to_owned()), &ext, force)?;
    history::add(path);
    let remote = upload.deletable().then(|| Remote {
        upload_id: upload.id.clone(),
        key: object,
    });
    history::set_link(path, Some(&url), remote);
    Ok(url)
}

/// The object key for `--name`: its folders tidied as a key template's are
/// (no empty ones or `..`), with `.ext` added if its last part has no
/// extension. `None` if nothing's left.
fn object_name(name: &str, ext: &str) -> Option<String> {
    let key = naming::components(name).collect::<Vec<_>>().join("/");
    if key.is_empty() {
        return None;
    }
    let last = key.rsplit('/').next().unwrap_or(&key);
    Some(match Path::new(last).extension() {
        None if !ext.is_empty() => format!("{key}.{ext}"),
        _ => key,
    })
}

/// The destination asked for by name or id (ticked or not), or else the
/// *Link from* one if it's ticked, or else the first ticked one.
fn pick<'a>(settings: &'a Settings, wanted: Option<&str>) -> Result<&'a Upload, String> {
    let uploads = &settings.uploads;
    if let Some(w) = wanted {
        return uploads
            .iter()
            .find(|u| u.id == w)
            .or_else(|| {
                uploads
                    .iter()
                    .find(|u| u.name.trim().eq_ignore_ascii_case(w.trim()))
            })
            .ok_or_else(|| format!("no destination called {w:?} (see snapr upload --list)"));
    }
    let ticked = || uploads.iter().filter(|u| u.enabled);
    ticked()
        .find(|u| u.id == settings.link_from)
        .or_else(|| ticked().next())
        .ok_or_else(|| "no upload destinations are ticked in Settings".into())
}

fn print_list(settings: &Settings) {
    if settings.uploads.is_empty() {
        say(format_args!(
            "No upload destinations; add one in Settings > Destinations."
        ));
        return;
    }
    let default = pick(settings, None).ok().map(|u| u.id.as_str());
    for u in &settings.uploads {
        let state = if Some(u.id.as_str()) == default {
            "default"
        } else if u.enabled {
            "ticked"
        } else {
            "not ticked"
        };
        say(format_args!("{}\t{:?}\t{state}\t{}", u.name, u.kind, u.id));
    }
}

/// Prints a line to stdout. Unlike `println!`, it doesn't panic once the
/// reader has gone, e.g. a Windows shell that didn't wait for snapr (a
/// windowed app there); the upload still happens.
fn say(line: Arguments) {
    let _ = writeln!(std::io::stdout(), "{line}");
}

/// `say`, for stderr.
fn warn(line: Arguments) {
    let _ = writeln!(std::io::stderr(), "{line}");
}

fn usage(msg: &str) -> i32 {
    warn(format_args!(
        "snapr upload: {msg} (see snapr upload --help)"
    ));
    2
}

fn fail(msg: &str) -> i32 {
    warn(format_args!("snapr upload: {msg}"));
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(uploads: &[(&str, bool)], link_from: &str) -> Settings {
        Settings {
            uploads: uploads
                .iter()
                .map(|&(name, enabled)| Upload {
                    id: name.to_lowercase(),
                    name: name.into(),
                    enabled,
                    ..Upload::default()
                })
                .collect(),
            link_from: link_from.into(),
            ..Settings::default()
        }
    }

    fn picked(s: &Settings, wanted: Option<&str>) -> Result<String, String> {
        pick(s, wanted).map(|u| u.name.clone())
    }

    #[test]
    fn picks_link_from_then_first_ticked() {
        let s = settings(&[("Off", false), ("A", true), ("B", true)], "b");
        assert_eq!(picked(&s, None).unwrap(), "B");
        let s = settings(&[("Off", false), ("A", true), ("B", true)], "off");
        assert_eq!(picked(&s, None).unwrap(), "A");
        let s = settings(&[("Off", false)], "");
        assert!(picked(&s, None).is_err());
    }

    #[test]
    fn names_objects() {
        let name = |n, ext| object_name(n, ext);
        assert_eq!(name("shot", "png").as_deref(), Some("shot.png"));
        assert_eq!(name("shot.jpg", "png").as_deref(), Some("shot.jpg"));
        assert_eq!(name("notes", "").as_deref(), Some("notes"));
        assert_eq!(name("/docs//a b/x.png", "png").as_deref(), Some("docs/a b/x.png"));
        assert_eq!(name(r"..\..\etc\x", "txt").as_deref(), Some("etc/x.txt"));
        assert_eq!(name("v1.2/readme", "md").as_deref(), Some("v1.2/readme.md"));
        assert_eq!(name(" / .. / ", "png"), None);
    }

    #[test]
    fn picks_by_id_or_name_even_unticked() {
        let s = settings(&[("Off", false), ("My R2", true)], "");
        assert_eq!(picked(&s, Some("off")).unwrap(), "Off");
        assert_eq!(picked(&s, Some("my r2")).unwrap(), "My R2");
        assert!(picked(&s, Some("nope")).is_err());
    }
}
