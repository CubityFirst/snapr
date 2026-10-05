//! A small WebM (Matroska) writer: one video and/or one audio track,
//! clusters starting at video key frames, and an index (Cues) so players
//! can seek. Timestamps are in milliseconds.

#![cfg_attr(not(windows), allow(dead_code))]

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

/// Element IDs.
mod id {
    pub const EBML: u32 = 0x1A45_DFA3;
    pub const EBML_VERSION: u32 = 0x4286;
    pub const EBML_READ_VERSION: u32 = 0x42F7;
    pub const EBML_MAX_ID_LENGTH: u32 = 0x42F2;
    pub const EBML_MAX_SIZE_LENGTH: u32 = 0x42F3;
    pub const DOC_TYPE: u32 = 0x4282;
    pub const DOC_TYPE_VERSION: u32 = 0x4287;
    pub const DOC_TYPE_READ_VERSION: u32 = 0x4285;
    pub const SEGMENT: u32 = 0x1853_8067;
    pub const SEEK_HEAD: u32 = 0x114D_9B74;
    pub const SEEK: u32 = 0x4DBB;
    pub const SEEK_ID: u32 = 0x53AB;
    pub const SEEK_POSITION: u32 = 0x53AC;
    pub const VOID: u32 = 0xEC;
    pub const INFO: u32 = 0x1549_A966;
    pub const TIMECODE_SCALE: u32 = 0x2A_D7B1;
    pub const DURATION: u32 = 0x4489;
    pub const MUXING_APP: u32 = 0x4D80;
    pub const WRITING_APP: u32 = 0x5741;
    pub const TRACKS: u32 = 0x1654_AE6B;
    pub const TRACK_ENTRY: u32 = 0xAE;
    pub const TRACK_NUMBER: u32 = 0xD7;
    pub const TRACK_UID: u32 = 0x73C5;
    pub const TRACK_TYPE: u32 = 0x83;
    pub const CODEC_ID: u32 = 0x86;
    pub const CODEC_PRIVATE: u32 = 0x63A2;
    pub const CODEC_DELAY: u32 = 0x56AA;
    pub const SEEK_PRE_ROLL: u32 = 0x56BB;
    pub const DEFAULT_DURATION: u32 = 0x23_E383;
    pub const VIDEO: u32 = 0xE0;
    pub const PIXEL_WIDTH: u32 = 0xB0;
    pub const PIXEL_HEIGHT: u32 = 0xBA;
    pub const COLOUR: u32 = 0x55B0;
    pub const MATRIX_COEFFICIENTS: u32 = 0x55B1;
    pub const RANGE: u32 = 0x55B9;
    pub const TRANSFER_CHARACTERISTICS: u32 = 0x55BA;
    pub const PRIMARIES: u32 = 0x55BB;
    pub const AUDIO: u32 = 0xE1;
    pub const SAMPLING_FREQUENCY: u32 = 0xB5;
    pub const CHANNELS: u32 = 0x9F;
    pub const CLUSTER: u32 = 0x1F43_B675;
    pub const TIMECODE: u32 = 0xE7;
    pub const SIMPLE_BLOCK: u32 = 0xA3;
    pub const CUES: u32 = 0x1C53_BB6B;
    pub const CUE_POINT: u32 = 0xBB;
    pub const CUE_TIME: u32 = 0xB3;
    pub const CUE_TRACK_POSITIONS: u32 = 0xB7;
    pub const CUE_TRACK: u32 = 0xF7;
    pub const CUE_CLUSTER_POSITION: u32 = 0xF1;
}

const VIDEO_TRACK: u64 = 1;
const AUDIO_TRACK: u64 = 2;
/// Room kept at the start for the SeekHead, written at the end.
const SEEK_HEAD_ROOM: usize = 96;
/// Blocks store their time as a 16-bit offset from the cluster's.
const MAX_CLUSTER_SPAN_MS: u64 = 30_000;

pub struct VideoTrack {
    /// "V_VP9", ...
    pub codec: &'static str,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

pub struct AudioTrack {
    /// "A_OPUS", ...
    pub codec: &'static str,
    pub sample_rate: u32,
    pub channels: u16,
    pub codec_private: Vec<u8>,
    /// Nanoseconds of decoder delay to skip at the start (Opus pre-skip).
    pub codec_delay_ns: u64,
    pub seek_pre_roll_ns: u64,
}

struct Block {
    track: u64,
    time_ms: u64,
    key: bool,
    data: Vec<u8>,
}

pub struct Writer {
    out: BufWriter<File>,
    /// Where the Segment's contents start (positions are relative to it).
    segment_start: u64,
    seek_head_at: u64,
    duration_at: u64,
    info_at: u64,
    tracks_at: u64,
    has_video: bool,
    has_audio: bool,
    /// The latest audio block's time.
    audio_ms: Option<u64>,
    /// Key frame times where clusters start, once the audio has caught up
    /// (it arrives a little after the video).
    cuts: VecDeque<u64>,
    /// Blocks not yet written, waiting to be put in time order.
    pending: Vec<Block>,
    /// Key frame times (ms) and their clusters' positions.
    cues: Vec<(u64, u64)>,
}

impl Writer {
    pub fn create(path: &Path, video: Option<&VideoTrack>, audio: Option<&AudioTrack>) -> io::Result<Self> {
        let mut out = BufWriter::new(File::create(path)?);
        let header = [
            uint(id::EBML_VERSION, 1),
            uint(id::EBML_READ_VERSION, 1),
            uint(id::EBML_MAX_ID_LENGTH, 4),
            uint(id::EBML_MAX_SIZE_LENGTH, 8),
            string(id::DOC_TYPE, "webm"),
            uint(id::DOC_TYPE_VERSION, 4),
            uint(id::DOC_TYPE_READ_VERSION, 2),
        ]
        .concat();
        out.write_all(&element(id::EBML, &header))?;
        // The Segment's size is filled in at the end (8-byte size field).
        out.write_all(&id_bytes(id::SEGMENT))?;
        out.write_all(&[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF])?;
        let segment_start = out.stream_position()?;

        let seek_head_at = out.stream_position()? - segment_start;
        out.write_all(&void(SEEK_HEAD_ROOM))?;

        let info_at = out.stream_position()? - segment_start;
        let info_body = [
            uint(id::TIMECODE_SCALE, 1_000_000),
            string(id::MUXING_APP, "snapr"),
            string(id::WRITING_APP, "snapr"),
        ]
        .concat();
        // Duration last, as an 8-byte float to fill in at the end.
        let info_len = info_body.len() + 2 + 1 + 8;
        out.write_all(&id_bytes(id::INFO))?;
        out.write_all(&size(info_len as u64))?;
        out.write_all(&info_body)?;
        out.write_all(&id_bytes(id::DURATION))?;
        out.write_all(&size(8))?;
        let duration_at = out.stream_position()?;
        out.write_all(&0f64.to_be_bytes())?;

        let tracks_at = out.stream_position()? - segment_start;
        let mut entries = Vec::new();
        if let Some(v) = video {
            let colour = [
                // BT.709, limited range: what snapr's encoders are given.
                uint(id::MATRIX_COEFFICIENTS, 1),
                uint(id::RANGE, 1),
                uint(id::TRANSFER_CHARACTERISTICS, 1),
                uint(id::PRIMARIES, 1),
            ]
            .concat();
            let settings = [
                uint(id::PIXEL_WIDTH, v.width as u64),
                uint(id::PIXEL_HEIGHT, v.height as u64),
                element(id::COLOUR, &colour),
            ]
            .concat();
            entries.push(element(
                id::TRACK_ENTRY,
                &[
                    uint(id::TRACK_NUMBER, VIDEO_TRACK),
                    uint(id::TRACK_UID, VIDEO_TRACK),
                    uint(id::TRACK_TYPE, 1),
                    string(id::CODEC_ID, v.codec),
                    uint(id::DEFAULT_DURATION, 1_000_000_000 / v.fps.max(1) as u64),
                    element(id::VIDEO, &settings),
                ]
                .concat(),
            ));
        }
        if let Some(a) = audio {
            let settings = [
                float(id::SAMPLING_FREQUENCY, a.sample_rate as f64),
                uint(id::CHANNELS, a.channels as u64),
            ]
            .concat();
            entries.push(element(
                id::TRACK_ENTRY,
                &[
                    uint(id::TRACK_NUMBER, AUDIO_TRACK),
                    uint(id::TRACK_UID, AUDIO_TRACK),
                    uint(id::TRACK_TYPE, 2),
                    string(id::CODEC_ID, a.codec),
                    element(id::CODEC_PRIVATE, &a.codec_private),
                    uint(id::CODEC_DELAY, a.codec_delay_ns),
                    uint(id::SEEK_PRE_ROLL, a.seek_pre_roll_ns),
                    element(id::AUDIO, &settings),
                ]
                .concat(),
            ));
        }
        out.write_all(&element(id::TRACKS, &entries.concat()))?;
        Ok(Self {
            out,
            segment_start,
            seek_head_at,
            duration_at,
            info_at,
            tracks_at,
            has_video: video.is_some(),
            has_audio: audio.is_some(),
            audio_ms: None,
            cuts: VecDeque::new(),
            pending: Vec::new(),
            cues: Vec::new(),
        })
    }

    pub fn video(&mut self, data: &[u8], time_ms: u64, key: bool) -> io::Result<()> {
        if key {
            // A key frame starts a cluster.
            self.cuts.push_back(time_ms);
        }
        self.push(VIDEO_TRACK, data, time_ms, key)?;
        self.cut()
    }

    pub fn audio(&mut self, data: &[u8], time_ms: u64) -> io::Result<()> {
        self.audio_ms = Some(time_ms);
        // Audio frames all stand alone.
        self.push(AUDIO_TRACK, data, time_ms, !self.has_video)?;
        self.cut()
    }

    /// Writes the clusters before each key frame the audio has reached.
    fn cut(&mut self) -> io::Result<()> {
        while let Some(&at) = self.cuts.front() {
            if self.has_audio && self.audio_ms.is_none_or(|a| a < at) {
                break;
            }
            self.cuts.pop_front();
            self.flush_before(at)?;
        }
        Ok(())
    }

    fn push(&mut self, track: u64, data: &[u8], time_ms: u64, key: bool) -> io::Result<()> {
        self.pending.push(Block {
            track,
            time_ms,
            key,
            data: data.to_vec(),
        });
        // Without key frames to cut at (audio only, or very long gaps),
        // keep clusters within what a block's time offset can reach.
        let earliest = self.pending.iter().map(|b| b.time_ms).min().unwrap_or(time_ms);
        if time_ms.saturating_sub(earliest) > MAX_CLUSTER_SPAN_MS {
            self.flush_before(time_ms)?;
        }
        Ok(())
    }

    /// Writes the pending blocks earlier than `time_ms` as a cluster.
    fn flush_before(&mut self, time_ms: u64) -> io::Result<()> {
        let (mut blocks, rest): (Vec<_>, Vec<_>) = self.pending.drain(..).partition(|b| b.time_ms < time_ms);
        self.pending = rest;
        if blocks.is_empty() {
            return Ok(());
        }
        // Stable: video before audio at the same time.
        blocks.sort_by_key(|b| b.time_ms);
        let start = blocks[0].time_ms;
        let position = self.out.stream_position()? - self.segment_start;
        if blocks.iter().any(|b| b.key && b.track == if self.has_video { VIDEO_TRACK } else { AUDIO_TRACK }) {
            let first_key = blocks.iter().find(|b| b.key).map_or(start, |b| b.time_ms);
            self.cues.push((first_key, position));
        }
        let mut body = uint(id::TIMECODE, start);
        for b in &blocks {
            let mut block = vec![0x80 | b.track as u8];
            block.extend_from_slice(&((b.time_ms - start) as i16).to_be_bytes());
            block.push(if b.key { 0x80 } else { 0 });
            block.extend_from_slice(&b.data);
            body.extend(element(id::SIMPLE_BLOCK, &block));
        }
        self.out.write_all(&element(id::CLUSTER, &body))
    }

    /// Writes what's left, the index and the length (`duration_ms`).
    pub fn finish(mut self, duration_ms: f64) -> io::Result<()> {
        self.flush_before(u64::MAX)?;
        let cues_at = self.out.stream_position()? - self.segment_start;
        let points: Vec<u8> = self
            .cues
            .iter()
            .flat_map(|&(time, position)| {
                let track = if self.has_video { VIDEO_TRACK } else { AUDIO_TRACK };
                let positions = [uint(id::CUE_TRACK, track), uint(id::CUE_CLUSTER_POSITION, position)].concat();
                element(
                    id::CUE_POINT,
                    &[uint(id::CUE_TIME, time), element(id::CUE_TRACK_POSITIONS, &positions)].concat(),
                )
            })
            .collect();
        if !points.is_empty() {
            self.out.write_all(&element(id::CUES, &points))?;
        }
        let end = self.out.stream_position()?;

        let seek = |target: u32, at: u64| {
            element(
                id::SEEK,
                &[element(id::SEEK_ID, &id_bytes(target)), uint(id::SEEK_POSITION, at)].concat(),
            )
        };
        let mut entries = [seek(id::INFO, self.info_at), seek(id::TRACKS, self.tracks_at)].concat();
        if !points.is_empty() {
            entries.extend(seek(id::CUES, cues_at));
        }
        let mut head = element(id::SEEK_HEAD, &entries);
        if head.len() + 2 > SEEK_HEAD_ROOM {
            return Err(io::Error::other("index too large"));
        }
        head.extend(void(SEEK_HEAD_ROOM - head.len()));
        self.out.seek(SeekFrom::Start(self.segment_start + self.seek_head_at))?;
        self.out.write_all(&head)?;
        self.out.seek(SeekFrom::Start(self.duration_at))?;
        self.out.write_all(&duration_ms.to_be_bytes())?;
        self.out.seek(SeekFrom::Start(self.segment_start - 8))?;
        let mut segment_size = (end - self.segment_start).to_be_bytes();
        segment_size[0] = 0x01; // the 8-byte size marker
        self.out.write_all(&segment_size)?;
        self.out.flush()
    }
}

fn id_bytes(id: u32) -> Vec<u8> {
    let bytes = id.to_be_bytes();
    let skip = bytes.iter().take_while(|&&b| b == 0).count();
    bytes[skip..].to_vec()
}

/// An element size in the shortest form that holds it.
fn size(n: u64) -> Vec<u8> {
    let len = (1..=8).find(|&l| n < (1u64 << (7 * l)) - 1).unwrap_or(8);
    let mut bytes = n.to_be_bytes()[8 - len..].to_vec();
    bytes[0] |= 0x80 >> (len - 1);
    bytes
}

fn element(id: u32, body: &[u8]) -> Vec<u8> {
    [id_bytes(id), size(body.len() as u64), body.to_vec()].concat()
}

fn uint(id: u32, n: u64) -> Vec<u8> {
    let bytes = n.to_be_bytes();
    let skip = bytes.iter().take_while(|&&b| b == 0).count().min(7);
    element(id, &bytes[skip..])
}

fn float(id: u32, n: f64) -> Vec<u8> {
    element(id, &n.to_be_bytes())
}

fn string(id: u32, s: &str) -> Vec<u8> {
    element(id, s.as_bytes())
}

/// A Void element taking up exactly `len` bytes (at least 2).
fn void(len: usize) -> Vec<u8> {
    // One byte for the ID, then a one-byte size, or an 8-byte one for
    // room this big.
    if len < 10 {
        let mut v = vec![id::VOID as u8, 0x80 | (len - 2) as u8];
        v.resize(len, 0);
        return v;
    }
    let mut v = vec![id::VOID as u8, 0x01];
    v.extend_from_slice(&((len - 9) as u64).to_be_bytes()[1..]);
    v.resize(len, 0);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_sizes() {
        assert_eq!(size(0), [0x80]);
        assert_eq!(size(126), [0xFE]);
        // 127 is "unknown" in one byte, so it takes two.
        assert_eq!(size(127), [0x40, 0x7F]);
        assert_eq!(size(300), [0x41, 0x2C]);
        assert_eq!(id_bytes(id::SEGMENT), [0x18, 0x53, 0x80, 0x67]);
        assert_eq!(id_bytes(id::TIMECODE), [0xE7]);
        assert_eq!(void(2), [0xEC, 0x80]);
        assert_eq!(void(5).len(), 5);
        assert_eq!(void(96).len(), 96);
        // An 8-byte size: the rest after the ID and size fields.
        assert_eq!(&void(20)[..9], &[0xEC, 0x01, 0, 0, 0, 0, 0, 0, 11]);
        assert_eq!(uint(id::TRACK_NUMBER, 1), [0xD7, 0x81, 0x01]);
    }

    #[test]
    fn writes_clusters_in_time_order() {
        let path = std::env::temp_dir().join(format!("snapr-webm-{}.webm", std::process::id()));
        let video = VideoTrack {
            codec: "V_VP9",
            width: 64,
            height: 32,
            fps: 10,
        };
        let mut w = Writer::create(&path, Some(&video), None).unwrap();
        for n in 0..25u64 {
            w.video(&[n as u8; 4], n * 100, n % 10 == 0).unwrap();
        }
        w.finish(2500.0).unwrap();
        let data = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        // Three clusters (key frames at 0, 1000, 2000 ms) and an index.
        let count = |id: &[u8]| data.windows(id.len()).filter(|w| w == &id).count();
        assert_eq!(count(&[0x1F, 0x43, 0xB6, 0x75]), 3);
        assert_eq!(count(&[0x1C, 0x53, 0xBB, 0x6B]), 2); // Cues, and its SeekHead entry
        assert_eq!(&data[..4], &[0x1A, 0x45, 0xDF, 0xA3]);
    }
}
