//! Capturing, mixing and encoding sound.
//!
//! PipeWire stamps every captured buffer with its `CLOCK_MONOTONIC` capture time; the [`Mixer`]
//! cuts the recording timeline into encoder-sized windows and places each source's frames in them
//! by that time, so device clock drift is absorbed a few frames at a time.

mod capture;
mod encoder;
mod mixer;
mod source;

use std::time::Duration;

pub use self::capture::{CaptureNode, PipeWireCapture};
pub use self::encoder::OpusEncoder;
pub use self::mixer::Mixer;
use super::clock::monotonic_now;
use super::{PacketSink, RecordError};

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;

const NANOSECONDS: i64 = 1_000_000_000;
/// How long a window waits for late buffers before it is mixed.
const MIX_DELAY: Duration = Duration::from_millis(250);

fn nanoseconds(duration: Duration) -> i64 {
    i64::try_from(duration.as_nanos()).unwrap_or(i64::MAX)
}

/// Frames at [`SAMPLE_RATE`] from `from_ns` to `to_ns`, rounded to the nearest frame.
fn frames_between(from_ns: i64, to_ns: i64) -> i64 {
    let scaled = i128::from(to_ns - from_ns) * i128::from(SAMPLE_RATE);
    let half = i128::from(NANOSECONDS / 2);
    ((scaled + half).div_euclid(i128::from(NANOSECONDS))) as i64
}

/// Captured sound, mixed and encoded as the recording runs.
pub struct AudioTrack {
    _capture: PipeWireCapture,
    mixer: Mixer,
    encoder: OpusEncoder,
    /// When the recording started, on `CLOCK_MONOTONIC`.
    origin_ns: Option<i64>,
    /// Frames already encoded, counted from the start of the recording.
    encoded: i64,
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
            origin_ns: None,
            encoded: 0,
            mixed,
        })
    }

    pub fn encoder(&self) -> &OpusEncoder {
        &self.encoder
    }

    /// Encodes every window of the recording that ended at least [`MIX_DELAY`] ago.
    ///
    /// `elapsed + offset` must be the time since the recording started; the first call anchors
    /// the track there, and sound is placed by capture time, so how the two are split is unused.
    pub fn pump(
        &mut self,
        elapsed: Duration,
        sink: &mut dyn PacketSink,
        offset: Duration,
    ) -> Result<(), RecordError> {
        let now_ns = nanoseconds(monotonic_now());
        let origin_ns = *self
            .origin_ns
            .get_or_insert_with(|| now_ns - nanoseconds(elapsed + offset));
        self.encode_until(origin_ns, now_ns - nanoseconds(MIX_DELAY), sink)
    }

    /// Encodes the sound captured up to now and flushes the encoder.
    pub fn finish(&mut self, sink: &mut dyn PacketSink) -> Result<(), RecordError> {
        if let Some(origin_ns) = self.origin_ns {
            self.encode_until(origin_ns, nanoseconds(monotonic_now()), sink)?;
        }
        self.encoder.finish(sink)
    }

    fn encode_until(
        &mut self,
        origin_ns: i64,
        until_ns: i64,
        sink: &mut dyn PacketSink,
    ) -> Result<(), RecordError> {
        let due = frames_between(origin_ns, until_ns);
        let frame = self.encoder.frame_samples() as i64;
        while self.encoded + frame <= due {
            self.mixer.mix(origin_ns, self.encoded, &mut self.mixed);
            self.encoder.encode(&self.mixed, self.encoded, sink)?;
            self.encoded += frame;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_nanoseconds_to_nearest_frame() {
        assert_eq!(frames_between(0, NANOSECONDS), i64::from(SAMPLE_RATE));
        assert_eq!(frames_between(1_000, 1_000 + 62_500), 3);
        assert_eq!(frames_between(0, 20_000), 1);
        assert_eq!(frames_between(62_500, 0), -3);
    }
}
