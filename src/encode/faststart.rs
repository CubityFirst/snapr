//! Moves an MP4's index (`moov`) in front of its media data (`mdat`), like
//! FFmpeg's `-movflags +faststart`, so playback can start before the whole
//! file has downloaded. The index's chunk offsets (`stco`/`co64`) are moved
//! along with the data they point at.

#![cfg_attr(not(windows), allow(dead_code))]

use std::fs::File;
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

/// A top-level box: its type, and where it is in the file (header included).
struct Mp4Box {
    kind: [u8; 4],
    start: u64,
    len: u64,
}

/// Rewrites `path` with the index first. Files that already have it first
/// (or have no media data) are left as they are.
pub fn apply(path: &Path) -> io::Result<()> {
    let mut file = File::open(path)?;
    let boxes = top_level(&mut file)?;
    let find = |kind: &[u8; 4]| boxes.iter().position(|b| &b.kind == kind);
    let moov_at = find(b"moov").ok_or_else(|| invalid("no index"))?;
    let Some(mdat_at) = find(b"mdat") else {
        return Ok(());
    };
    if moov_at < mdat_at {
        return Ok(());
    }
    let moov = &boxes[moov_at];
    let mut index = vec![0; moov.len as usize];
    file.seek(SeekFrom::Start(moov.start))?;
    file.read_exact(&mut index)?;
    // Everything from the media data up to the index moves back by the
    // index's size.
    let moved = boxes[mdat_at].start..moov.start;
    shift_offsets(&mut index, &|offset| {
        if !moved.contains(&offset) {
            return Some(offset);
        }
        offset.checked_add(moov.len)
    })?;

    let temp = path.with_extension("faststart.tmp");
    let result = (|| {
        let mut out = BufWriter::new(File::create(&temp)?);
        let mut copy = |b: &Mp4Box, out: &mut BufWriter<File>| -> io::Result<()> {
            file.seek(SeekFrom::Start(b.start))?;
            let copied = io::copy(&mut (&mut file).take(b.len), out)?;
            if copied == b.len {
                Ok(())
            } else {
                Err(invalid("the file ended early"))
            }
        };
        for b in &boxes[..mdat_at] {
            copy(b, &mut out)?;
        }
        out.write_all(&index)?;
        for (i, b) in boxes.iter().enumerate().skip(mdat_at) {
            if i != moov_at {
                copy(b, &mut out)?;
            }
        }
        out.into_inner().map_err(|e| e.into_error())?.sync_all()
    })();
    drop(file);
    match result {
        Ok(()) => std::fs::rename(&temp, path),
        Err(e) => {
            let _ = std::fs::remove_file(&temp);
            Err(e)
        }
    }
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_string())
}

/// A box's total length and header length, from the start of `data` (the
/// rest of its parent).
fn box_size(data: &[u8]) -> io::Result<(u64, usize)> {
    let short = u32::from_be_bytes(data[..4].try_into().unwrap()) as u64;
    match short {
        // Runs to the end of its parent.
        0 => Ok((data.len() as u64, 8)),
        1 => {
            let long = data.get(8..16).ok_or_else(|| invalid("truncated box"))?;
            Ok((u64::from_be_bytes(long.try_into().unwrap()), 16))
        }
        n => Ok((n, 8)),
    }
}

fn top_level(file: &mut File) -> io::Result<Vec<Mp4Box>> {
    let file_len = file.metadata()?.len();
    let mut boxes = Vec::new();
    let mut start = 0;
    while start + 8 <= file_len {
        file.seek(SeekFrom::Start(start))?;
        let mut header = [0; 16];
        let n = file.read(&mut header)?;
        let (mut len, _) = box_size(&header[..n.max(8)])?;
        if u32::from_be_bytes(header[..4].try_into().unwrap()) == 0 {
            len = file_len - start;
        }
        if len < 8 || start + len > file_len {
            return Err(invalid("broken box"));
        }
        boxes.push(Mp4Box {
            kind: header[4..8].try_into().unwrap(),
            start,
            len,
        });
        start += len;
    }
    Ok(boxes)
}

/// Applies `map` to every chunk offset in the boxes in `data`, looking into
/// the containers that lead to the sample tables.
fn shift_offsets(data: &mut [u8], map: &dyn Fn(u64) -> Option<u64>) -> io::Result<()> {
    let mut pos = 0;
    while pos + 8 <= data.len() {
        let (len, header) = box_size(&data[pos..])?;
        let len = len as usize;
        if len < header || pos + len > data.len() {
            return Err(invalid("broken box in the index"));
        }
        let kind: [u8; 4] = data[pos + 4..pos + 8].try_into().unwrap();
        let body = &mut data[pos + header..pos + len];
        match &kind {
            b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl" => shift_offsets(body, map)?,
            b"stco" | b"co64" => {
                let width = if &kind == b"stco" { 4 } else { 8 };
                // Version and flags, then the entry count.
                let count = u32::from_be_bytes(
                    body.get(4..8).ok_or_else(|| invalid("truncated offsets"))?.try_into().unwrap(),
                ) as usize;
                let entries = body
                    .get_mut(8..8 + count * width)
                    .ok_or_else(|| invalid("truncated offsets"))?;
                for entry in entries.chunks_exact_mut(width) {
                    if width == 4 {
                        let old = u32::from_be_bytes(entry.try_into().unwrap()) as u64;
                        let new = map(old)
                            .and_then(|n| u32::try_from(n).ok())
                            .ok_or_else(|| invalid("offsets too large"))?;
                        entry.copy_from_slice(&new.to_be_bytes());
                    } else {
                        let old = u64::from_be_bytes(entry.try_into().unwrap());
                        let new = map(old).ok_or_else(|| invalid("offsets too large"))?;
                        entry.copy_from_slice(&new.to_be_bytes());
                    }
                }
            }
            _ => {}
        }
        pos += len;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mp4_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut b = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        b.extend_from_slice(kind);
        b.extend_from_slice(body);
        b
    }

    /// An index with one `stco` (two chunks) and one `co64` (one chunk).
    fn index(stco: [u32; 2], co64: u64) -> Vec<u8> {
        let mut s = vec![0, 0, 0, 0, 0, 0, 0, 2];
        s.extend(stco.iter().flat_map(|o| o.to_be_bytes()));
        let mut c = vec![0, 0, 0, 0, 0, 0, 0, 1];
        c.extend(co64.to_be_bytes());
        let stbl = |t| mp4_box(b"stbl", &mp4_box(t, if t == b"stco" { &s } else { &c }));
        let trak = |t| mp4_box(b"trak", &mp4_box(b"mdia", &mp4_box(b"minf", &stbl(t))));
        mp4_box(b"moov", &[trak(b"stco"), mp4_box(b"udta", b"keep"), trak(b"co64")].concat())
    }

    #[test]
    fn moves_the_index_first() {
        let ftyp = mp4_box(b"ftyp", b"isom\0\0\0\0");
        let mdat = mp4_box(b"mdat", b"AAAABBBBCCCC");
        let data_at = (ftyp.len() + 8) as u32;
        // Placeholder offsets just to learn the index's size.
        let moov_len = index([0, 0], 0).len() as u32;
        let file = [
            ftyp.clone(),
            mdat.clone(),
            index([data_at, data_at + 4], data_at as u64 + 8),
        ]
        .concat();
        let path = std::env::temp_dir().join(format!("snapr-faststart-{}.mp4", std::process::id()));
        std::fs::write(&path, &file).unwrap();
        apply(&path).unwrap();
        let out = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        let moved = data_at + moov_len;
        let expected = [
            ftyp,
            index([moved, moved + 4], moved as u64 + 8),
            mdat,
        ]
        .concat();
        assert_eq!(out, expected);
        // The offsets point at the same data as before.
        assert_eq!(&out[moved as usize..moved as usize + 4], b"AAAA");
        assert_eq!(&out[moved as usize + 8..moved as usize + 12], b"CCCC");
    }

    #[test]
    fn leaves_an_index_that_is_already_first() {
        let file = [
            mp4_box(b"ftyp", b"isom\0\0\0\0"),
            index([40, 44], 48),
            mp4_box(b"mdat", b"AAAABBBBCCCC"),
        ]
        .concat();
        let path = std::env::temp_dir().join(format!("snapr-faststart-first-{}.mp4", std::process::id()));
        std::fs::write(&path, &file).unwrap();
        apply(&path).unwrap();
        let out = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(out, file);
    }
}
