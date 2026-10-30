//! Capturing, mixing and encoding sound.
//!
//! Each [`AudioSource`] yields interleaved stereo samples at [`SAMPLE_RATE`]; the [`Mixer`] sums
//! them on a fixed wall-clock cadence so a silent or suspended source never stalls the others.

mod capture;
mod encoder;
mod mixer;

use std::time::Duration;

pub use self::capture::{CaptureNode, PipeWireCapture};
pub use self::encoder::OpusEncoder;
pub use self::mixer::Mixer;
use super::{PacketSink, RecordError};

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;

/// Interleaved stereo samples arriving from somewhere.
pub trait AudioSource: Send {
    /// Moves up to `into.len()` samples into `into`, returning how many were written.
    fn read(&mut self, into: &mut [f32]) -> usize;

    /// Samples waiting to be read.
    fn available(&self) -> usize;

    /// Drops samples beyond `keep`, so a source that runs ahead cannot drift out of sync.
    fn discard_beyond(&mut self, keep: usize);
}

impl AudioSource for rtrb::Consumer<f32> {
    fn read(&mut self, into: &mut [f32]) -> usize {
        let count = self.slots().min(into.len());
        let Ok(chunk) = self.read_chunk(count) else {
            return 0;
        };
        let (first, second) = chunk.as_slices();
        into[..first.len()].copy_from_slice(first);
        into[first.len()..count].copy_from_slice(second);
        chunk.commit_all();
        count
    }

    fn available(&self) -> usize {
        self.slots()
    }

    fn discard_beyond(&mut self, keep: usize) {
        let excess = self.slots().saturating_sub(keep);
        if let Ok(chunk) = self.read_chunk(excess) {
            chunk.commit_all();
        }
    }
}

fn samples_in(duration: Duration) -> u64 {
    (duration.as_secs_f64() * f64::from(SAMPLE_RATE)) as u64
}

/// Captured sound, mixed and encoded as the recording runs.
pub struct AudioTrack {
    _capture: PipeWireCapture,
    mixer: Mixer,
    encoder: OpusEncoder,
    /// Samples per channel already encoded.
    encoded: u64,
    mixed: Vec<f32>,
}

impl AudioTrack {
    pub fn start(nodes: &[CaptureNode]) -> Result<Self, RecordError> {
        let (capture, sources) = PipeWireCapture::start(nodes)?;
        let encoder = OpusEncoder::open()?;
        let mixed = vec![0.0; encoder.frame_samples() * CHANNELS];
        Ok(Self {
            _capture: capture,
            mixer: Mixer::new(sources),
            encoder,
            encoded: 0,
            mixed,
        })
    }

    pub fn encoder(&self) -> &OpusEncoder {
        &self.encoder
    }

    /// Encodes every whole frame of sound captured over `elapsed`, placing the track `offset`
    /// into the recording.
    pub fn pump(
        &mut self,
        elapsed: Duration,
        sink: &mut dyn PacketSink,
        offset: Duration,
    ) -> Result<(), RecordError> {
        let due = samples_in(elapsed);
        let start = samples_in(offset);
        let frame = self.encoder.frame_samples() as u64;
        while self.encoded + frame <= due {
            self.mixer.mix(&mut self.mixed);
            self.encoder
                .encode(&self.mixed, (start + self.encoded) as i64, sink)?;
            self.encoded += frame;
        }
        Ok(())
    }

    pub fn finish(&mut self, sink: &mut dyn PacketSink) -> Result<(), RecordError> {
        self.encoder.finish(sink)
    }
}
