//! Fonts for the text tool: the installed ones, found on a background thread
//! by reading just their name tables, and loaded whole when first used.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// An installed font family's regular face.
#[derive(Debug, Clone)]
pub struct Font {
    pub family: String,
    path: PathBuf,
    /// Which face, in a collection (.ttc).
    index: u32,
}

static INSTALLED: OnceLock<Vec<Font>> = OnceLock::new();
static SCANNING: std::sync::Once = std::sync::Once::new();

/// Starts looking for installed fonts, if that hasn't started yet.
pub fn preload() {
    SCANNING.call_once(|| {
        std::thread::spawn(|| {
            INSTALLED.get_or_init(scan);
        });
    });
}

/// The installed fonts, sorted by family; `None` while they're still being
/// found.
pub fn installed() -> Option<&'static [Font]> {
    preload();
    INSTALLED.get().map(Vec::as_slice)
}

/// The installed font named `family`, waiting for the fonts to be found if
/// they're still being looked for.
pub fn find(family: &str) -> Option<u16> {
    let fonts = INSTALLED.get_or_init(scan);
    fonts.iter().position(|f| f.family == family).map(|i| i as u16)
}

/// The family name of installed font `i`.
pub fn family(i: u16) -> Option<&'static str> {
    Some(installed()?.get(i as usize)?.family.as_str())
}

/// The file of installed font `i` (from [`installed`]), read once, and its
/// face index. `None` if it can't be read.
pub fn data(i: u16) -> Option<(Arc<Vec<u8>>, u32)> {
    static LOADED: Mutex<Option<HashMap<u16, Option<Arc<Vec<u8>>>>>> = Mutex::new(None);
    let font = installed()?.get(i as usize)?;
    let mut loaded = LOADED.lock().unwrap_or_else(|e| e.into_inner());
    let data = loaded
        .get_or_insert_with(HashMap::new)
        .entry(i)
        .or_insert_with(|| std::fs::read(&font.path).ok().map(Arc::new))
        .clone()?;
    Some((data, font.index))
}

fn dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if cfg!(windows) {
        let windir = std::env::var_os("WINDIR").map_or_else(|| "C:\\Windows".into(), PathBuf::from);
        dirs.push(windir.join("Fonts"));
        if let Some(local) = dirs::data_local_dir() {
            dirs.push(local.join("Microsoft\\Windows\\Fonts"));
        }
    } else if cfg!(target_os = "macos") {
        dirs.extend(["/System/Library/Fonts", "/Library/Fonts"].map(PathBuf::from));
        if let Some(home) = dirs::home_dir() {
            dirs.push(home.join("Library/Fonts"));
        }
    } else {
        dirs.extend(["/usr/share/fonts", "/usr/local/share/fonts"].map(PathBuf::from));
        if let Some(home) = dirs::home_dir() {
            dirs.push(home.join(".local/share/fonts"));
            dirs.push(home.join(".fonts"));
        }
    }
    dirs
}

fn scan() -> Vec<Font> {
    let mut files = Vec::new();
    for dir in dirs() {
        collect(&dir, &mut files, 0);
    }
    let mut by_family: HashMap<String, Font> = HashMap::new();
    for path in files {
        for (family, index) in regular_faces(&path).unwrap_or_default() {
            by_family.entry(family.clone()).or_insert(Font {
                family,
                path: path.clone(),
                index,
            });
        }
    }
    let mut fonts: Vec<Font> = by_family.into_values().collect();
    fonts.sort_by_key(|f| f.family.to_lowercase());
    fonts
}

/// Font files under `dir`, a few folders deep (Linux keeps them in some).
fn collect(dir: &Path, files: &mut Vec<PathBuf>, depth: u32) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if depth < 4 {
                collect(&path, files, depth + 1);
            }
        } else if path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
            ["ttf", "otf", "ttc", "otc"].contains(&e.to_ascii_lowercase().as_str())
        }) {
            files.push(path);
        }
    }
}

/// The family names of the file's regular faces, with their indexes,
/// reading only the parts of the file that hold them.
fn regular_faces(path: &Path) -> Option<Vec<(String, u32)>> {
    let mut file = std::fs::File::open(path).ok()?;
    let read_at = |file: &mut std::fs::File, offset: u64, len: usize| -> Option<Vec<u8>> {
        let mut buf = vec![0; len];
        file.seek(SeekFrom::Start(offset)).ok()?;
        file.read_exact(&mut buf).ok()?;
        Some(buf)
    };
    let len = file.metadata().ok()?.len();
    // Enough for a collection's header and a face's table directory.
    let head = read_at(&mut file, 0, len.min(64 * 1024) as usize)?;
    let count = ttf_parser::fonts_in_collection(&head).unwrap_or(1);
    let mut faces = Vec::new();
    for index in 0..count.min(64) {
        let Ok(raw) = ttf_parser::RawFace::parse(&head, index) else {
            continue;
        };
        let Some(record) = raw
            .table_records
            .into_iter()
            .find(|r| r.tag == ttf_parser::Tag::from_bytes(b"name"))
        else {
            continue;
        };
        if record.offset as u64 + record.length as u64 > len {
            continue;
        }
        let Some(bytes) = read_at(&mut file, record.offset as u64, record.length as usize) else {
            continue;
        };
        let Some(names) = ttf_parser::name::Table::parse(&bytes) else {
            continue;
        };
        let name = |id: u16| english_name(&names, id);
        let (Some(family), Some(style)) = (name(ttf_parser::name_id::FAMILY), name(ttf_parser::name_id::SUBFAMILY))
        else {
            continue;
        };
        // Symbol and hidden system fonts start with a dot or an @ (vertical).
        if ["Regular", "Normal", "Book", "Roman"].contains(&style.as_str())
            && !family.starts_with(['.', '@'])
        {
            faces.push((family, index));
        }
    }
    Some(faces)
}

/// A name from the table, in US English if it has it.
fn english_name(names: &ttf_parser::name::Table, id: u16) -> Option<String> {
    let candidates = names.names.into_iter().filter(|n| n.name_id == id && n.is_unicode());
    let mut first = None;
    for n in candidates {
        let Some(s) = n.to_string() else { continue };
        if n.language_id == 0x0409 {
            return Some(s);
        }
        first.get_or_insert(s);
    }
    first
}

#[cfg(test)]
mod tests {
    /// Lists the installed fonts it finds, and how long that takes:
    /// `cargo test list_fonts -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn list_fonts() {
        let started = std::time::Instant::now();
        let fonts = super::scan();
        println!("{} fonts in {:?}", fonts.len(), started.elapsed());
        for f in fonts.iter().take(40) {
            println!("{} ({}, face {})", f.family, f.path.display(), f.index);
        }
        assert!(!fonts.is_empty());
    }
}
