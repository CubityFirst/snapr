//! Opus sound for WebM recordings, encoded in Rust (`opus-rs`, a port of
//! libopus), as neither Windows nor macOS has an Opus encoder.

use opus_rs::{Application, OpusEncoder};

/// Opus works at 48 kHz.
pub const RATE: u32 = 48_000;
/// 20 ms per packet.
const FRAME: usize = 960;
/// Samples (per channel) the encoder delays its output by, which decoders
/// skip: libopus's lookahead at 48 kHz.
pub const PRE_SKIP: u16 = 312;
const BITRATE: i32 = 128_000;

pub struct Opus {
    encoder: OpusEncoder,
    channels: usize,
    /// Samples waiting for a whole packet's worth.
    pending: Vec<f32>,
    /// Packets encoded so far.
    packets: u64,
}

/// An encoded packet and when it starts (ms).
pub struct Packet {
    pub data: Vec<u8>,
    pub time_ms: u64,
}

impl Opus {
    pub fn new(channels: u16) -> Result<Self, String> {
        let mut encoder = OpusEncoder::new(RATE as i32, channels as usize, Application::Audio)
            .map_err(|e| format!("couldn't set up the Opus encoder: {e}"))?;
        encoder.bitrate_bps = BITRATE;
        encoder.use_cbr = false;
        Ok(Self {
            encoder,
            channels: channels as usize,
            pending: Vec::new(),
            packets: 0,
        })
    }

    /// The `OpusHead` header that WebM stores as the track's codec data.
    pub fn head(&self) -> Vec<u8> {
        let mut head = b"OpusHead".to_vec();
        head.push(1); // version
        head.push(self.channels as u8);
        head.extend_from_slice(&PRE_SKIP.to_le_bytes());
        head.extend_from_slice(&RATE.to_le_bytes());
        head.extend_from_slice(&0i16.to_le_bytes()); // output gain
        head.push(0); // mono or stereo, no channel mapping table
        head
    }

    /// Encodes interleaved samples at 48 kHz, returning the packets that are
    /// complete.
    pub fn push(&mut self, samples: &[f32]) -> Result<Vec<Packet>, String> {
        self.pending.extend_from_slice(samples);
        let mut packets = Vec::new();
        let whole = FRAME * self.channels;
        while self.pending.len() >= whole {
            let frame: Vec<f32> = self.pending.drain(..whole).collect();
            packets.push(self.encode(&frame)?);
        }
        Ok(packets)
    }

    /// The last partial packet, padded with silence.
    pub fn finish(&mut self) -> Result<Vec<Packet>, String> {
        if self.pending.is_empty() {
            return Ok(Vec::new());
        }
        let mut frame = std::mem::take(&mut self.pending);
        frame.resize(FRAME * self.channels, 0.0);
        Ok(vec![self.encode(&frame)?])
    }

    fn encode(&mut self, frame: &[f32]) -> Result<Packet, String> {
        let mut out = vec![0u8; 4000];
        let len = self
            .encoder
            .encode(frame, FRAME, &mut out)
            .map_err(|e| format!("the Opus encoder failed: {e}"))?;
        out.truncate(len);
        let time_ms = self.packets * 20;
        self.packets += 1;
        Ok(Packet { data: out, time_ms })
    }
}
