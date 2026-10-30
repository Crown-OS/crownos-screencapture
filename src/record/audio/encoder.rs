use ffmpeg_next::format::{Sample, sample};
use ffmpeg_next::{ChannelLayout, Packet, Rational, codec, encoder, frame};

use super::{CHANNELS, SAMPLE_RATE};
use crate::record::{EncodedStream, PacketSink, RecordError};

const BIT_RATE: usize = 128_000;
const PACKED_FLOAT: Sample = Sample::F32(sample::Type::Packed);

/// Opus through libopus, fed interleaved float samples.
pub struct OpusEncoder {
    encoder: encoder::Audio,
    frame: frame::Audio,
}

// SAFETY: the encoder and its frame are owned here and used from one thread at a time.
unsafe impl Send for OpusEncoder {}

impl OpusEncoder {
    pub fn open() -> Result<Self, RecordError> {
        let codec = encoder::find_by_name("libopus").ok_or(RecordError::NoEncoder("libopus"))?;
        let mut audio = codec::Context::new_with_codec(codec).encoder().audio()?;
        audio.set_rate(SAMPLE_RATE as i32);
        audio.set_format(PACKED_FLOAT);
        audio.set_channel_layout(ChannelLayout::STEREO);
        audio.set_bit_rate(BIT_RATE);
        audio.set_time_base(Rational(1, SAMPLE_RATE as i32));
        audio.set_flags(codec::Flags::GLOBAL_HEADER);
        let encoder = audio.open()?;
        let frame = frame::Audio::new(
            PACKED_FLOAT,
            encoder.frame_size() as usize,
            ChannelLayout::STEREO,
        );
        Ok(Self { encoder, frame })
    }

    /// Samples per channel in each encoded frame.
    pub fn frame_samples(&self) -> usize {
        self.encoder.frame_size() as usize
    }

    /// Encodes one frame of interleaved samples starting `position` samples into the track.
    pub fn encode(
        &mut self,
        samples: &[f32],
        position: i64,
        sink: &mut dyn PacketSink,
    ) -> Result<(), RecordError> {
        let count = self.frame_samples() * CHANNELS;
        let bytes = self.frame.data_mut(0);
        for (target, sample) in bytes
            .chunks_exact_mut(size_of::<f32>())
            .zip(samples.iter().take(count))
        {
            target.copy_from_slice(&sample.to_ne_bytes());
        }
        self.frame.set_pts(Some(position));
        self.encoder.send_frame(&self.frame)?;
        self.drain(sink)
    }

    pub fn finish(&mut self, sink: &mut dyn PacketSink) -> Result<(), RecordError> {
        self.encoder.send_eof()?;
        self.drain(sink)
    }

    fn drain(&mut self, sink: &mut dyn PacketSink) -> Result<(), RecordError> {
        loop {
            let mut packet = Packet::empty();
            match self.encoder.receive_packet(&mut packet) {
                Ok(()) => sink.audio(packet)?,
                Err(
                    ffmpeg_next::Error::Other {
                        errno: ffmpeg_next::util::error::EAGAIN,
                    }
                    | ffmpeg_next::Error::Eof,
                ) => {
                    return Ok(());
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
}

impl EncodedStream for OpusEncoder {
    fn context(&self) -> &codec::Context {
        &self.encoder
    }
}
