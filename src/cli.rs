//! `snapr upload`: uploads files to a destination set up in Settings and
//! prints their links, for scripts and AI agents. Keys stay in the
//! credential store; nothing but the links is printed.

use std::fmt::Arguments;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::history::{self, Remote};
use crate::settings::{Loaded, Settings, Upload};
use crate::upload::{Body, Progress};
use crate::{naming, output, policy, secrets};

const HELP: &str = "snapr upload - upload files and print their links

USAGE: snapr upload [OPTIONS] <FILE>...

Uploads each file to one destination from Settings and prints its link, one
per line, in the order given. They're added to Recent, as dropped files are.
Failures are reported on stderr, and the exit status is then 1.

OPTIONS:
  -d, --destination <NAME>  Upload there (its name or id) instead of to the
                            Link from destination, or the first ticked one
  -l, --list                List the destinations, then exit
  -h, --help                Show this help";

/// Runs `snapr upload` with the arguments after `upload`; returns the exit
/// status.
pub fn upload(mut args: impl Iterator<Item = String>) -> i32 {
    let (mut files, mut wanted, mut list) = (Vec::new(), None, false);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-d" | "--destination" => match args.next() {
                Some(name) => wanted = Some(name),
                None => return usage("--destination needs a destination's name"),
            },
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
    if files.is_empty() {
        return usage("no files given");
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

    let mut status = 0;
    for file in files {
        let result = std::path::absolute(&file)
            .map_err(|e| e.to_string())
            .and_then(|path| send(upload, &target, &key, &path));
        match result {
            Ok(url) => say(format_args!("{url}")),
            Err(e) => {
                warn(format_args!("snapr upload: {}: {e}", file.display()));
                status = 1;
            }
        }
    }
    status
}

/// Uploads one file, named as a dropped file would be; returns its link.
fn send(
    upload: &Upload,
    target: &crate::upload::Target,
    key: &naming::Template,
    path: &Path,
) -> Result<String, String> {
    if !path.is_file() {
        return Err("not a file".into());
    }
    let mut ctx = naming::Context::new(chrono::Local::now());
    ctx.title = path.file_stem().map(|s| s.to_string_lossy().into_owned());
    if let Ok((w, h)) = image::image_dimensions(path) {
        (ctx.width, ctx.height) = (w, h);
    }
    if key.uses_counter() {
        ctx.counter = output::next_counter();
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let object = key.render_key(&ctx, &ext);
    let body = Body::File(path.to_owned());
    let url = upload.put(
        target,
        &object,
        &body,
        output::content_type(&ext),
        &Progress::default(),
    )?;
    history::add(path);
    let remote = upload.deletable().then(|| Remote {
        upload_id: upload.id.clone(),
        key: object,
    });
    history::set_link(path, Some(&url), remote);
    Ok(url)
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
    fn picks_by_id_or_name_even_unticked() {
        let s = settings(&[("Off", false), ("My R2", true)], "");
        assert_eq!(picked(&s, Some("off")).unwrap(), "Off");
        assert_eq!(picked(&s, Some("my r2")).unwrap(), "My R2");
        assert!(picked(&s, Some("nope")).is_err());
    }
}
