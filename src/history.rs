//! The list of saved screenshots shown on the Recent page, stored in the
//! config folder as one line per screenshot (oldest first):
//! `path`, or `path<TAB>link<TAB>upload id<TAB>object key` once uploaded.

use std::path::{Path, PathBuf};

use crate::settings::Settings;

/// How many entries the history file keeps.
const MAX_ENTRIES: usize = 500;

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub path: PathBuf,
    /// Where it was uploaded, if it was.
    pub link: Option<String>,
    /// The uploaded object, so it can be deleted again. Missing for uploads
    /// recorded before this was kept.
    pub remote: Option<Remote>,
}

impl Entry {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            link: None,
            remote: None,
        }
    }
}

/// An uploaded object: the destination's id in the settings, and its key.
#[derive(Debug, Clone, PartialEq)]
pub struct Remote {
    pub upload_id: String,
    pub key: String,
}

fn file() -> Option<PathBuf> {
    Some(Settings::config_dir()?.join("history.txt"))
}

fn read() -> Vec<Entry> {
    let Some(text) = file().and_then(|f| std::fs::read_to_string(f).ok()) else {
        return Vec::new();
    };
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(parse)
        .collect()
}

fn parse(line: &str) -> Entry {
    let mut fields = line.splitn(4, '\t');
    let path = fields.next().unwrap_or_default();
    let link = fields.next().filter(|l| !l.is_empty()).map(str::to_string);
    let remote = match (fields.next(), fields.next()) {
        (Some(id), Some(key)) if !id.is_empty() && !key.is_empty() => Some(Remote {
            upload_id: id.to_string(),
            key: key.to_string(),
        }),
        _ => None,
    };
    Entry {
        path: path.into(),
        link,
        remote,
    }
}

fn format(e: &Entry) -> String {
    match (&e.link, &e.remote) {
        (Some(link), Some(r)) => {
            format!("{}\t{link}\t{}\t{}\n", e.path.display(), r.upload_id, r.key)
        }
        (Some(link), None) => format!("{}\t{link}\n", e.path.display()),
        _ => format!("{}\n", e.path.display()),
    }
}

fn write(entries: &[Entry]) {
    let Some(path) = file() else { return };
    let start = entries.len().saturating_sub(MAX_ENTRIES);
    let text: String = entries[start..].iter().map(format).collect();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = std::fs::write(&path, text) {
        eprintln!("couldn't save history: {e}");
    }
}

/// Whether there's no history file yet, i.e. snapr hasn't saved anything.
pub fn is_new() -> bool {
    file().is_none_or(|f| !f.exists())
}

/// Screenshots that still exist, newest first.
pub fn load() -> Vec<Entry> {
    let mut seen = std::collections::HashSet::new();
    read()
        .into_iter()
        .rev()
        .filter(|e| seen.insert(e.path.clone()) && e.path.is_file())
        .collect()
}

pub fn add(path: &Path) {
    let mut entries = read();
    entries.retain(|e| e.path != path);
    entries.push(Entry::new(path.to_owned()));
    write(&entries);
}

/// Records where a screenshot was uploaded, or with `None`s that it no
/// longer is.
pub fn set_link(path: &Path, link: Option<&str>, remote: Option<Remote>) {
    let mut entries = read();
    let entry = match entries.iter().rposition(|e| e.path == path) {
        Some(i) => &mut entries[i],
        None if link.is_none() => return,
        None => {
            entries.push(Entry::new(path.to_owned()));
            entries.last_mut().expect("just pushed")
        }
    };
    entry.link = link.map(str::to_string);
    entry.remote = remote;
    write(&entries);
}

pub fn remove(path: &Path) {
    let mut entries = read();
    entries.retain(|e| e.path != path);
    write(&entries);
}

/// Forgets every entry. The screenshots themselves are left alone.
pub fn clear() {
    write(&[]);
}

/// Screenshots and recordings in the save folder (and up to three levels of sub-folders), newest
/// first, for when there's no history file yet.
pub fn scan(folder: &Path, limit: usize) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: u32, out: &mut Vec<(std::time::SystemTime, PathBuf)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() && depth < 3 {
                walk(&path, depth + 1, out);
            } else if meta.is_file()
                && matches!(
                    crate::thumbnail::kind(&path),
                    crate::thumbnail::Kind::Image | crate::thumbnail::Kind::Video
                )
            {
                out.push((meta.modified().unwrap_or(std::time::UNIX_EPOCH), path));
            }
        }
    }
    let mut found = Vec::new();
    walk(folder, 0, &mut found);
    found.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
    found.into_iter().take(limit).map(|(_, p)| p).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_round_trip() {
        for e in [
            Entry::new("C:\\shots\\a b.png".into()),
            Entry {
                path: "/home/me/x.png".into(),
                link: Some("https://img.example.com/2026-10/x.png".into()),
                remote: None,
            },
            Entry {
                path: "/home/me/y.png".into(),
                link: Some("https://img.example.com/y.png".into()),
                remote: Some(Remote {
                    upload_id: "pt5uva7kmher".into(),
                    key: "y.png".into(),
                }),
            },
        ] {
            assert_eq!(parse(format(&e).trim_end_matches('\n')), e);
        }
    }
}
