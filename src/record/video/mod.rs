//! Encoding captured frames to compressed video.
//!
//! A [`VideoBackend`] says which buffers it wants and opens a [`VideoEncoder`] once the first
//! frame shows the real size. [`VaapiBackend`] keeps frames on the GPU end to end;
//! [`SoftwareBackend`] is the fallback for machines without a usable VA-API driver.

mod filter;
mod hardware;
mod software;
mod vaapi;

use std::path::Path;

use crownos_config::schema::{RecordingQuality, VideoCodec};
use ffmpeg_next::{Dictionary, Packet, Rational, codec, encoder, format};

pub use self::software::SoftwareBackend;
pub use self::vaapi::VaapiBackend;
use super::clock::RecordingClock;
use super::{EncodedStream, PacketSink, RecordError};
use crate::capture::{BufferKind, CapturedFrame, PixelFormat, PixelRect};

/// How the encoder should shape and compress the video.
#[derive(Debug, Clone, Copy)]
pub struct VideoSettings {
    pub codec: VideoCodec,
    pub quality: RecordingQuality,
    pub frames_per_second: u32,
    pub crop: Crop,
}

/// Which part of each captured frame to keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Crop {
    Full,
    Region(PixelRect),
}

impl Crop {
    /// The kept rectangle of a `width`×`height` frame, with even dimensions.
    pub fn within(self, width: u32, height: u32) -> PixelRect {
        let rect = match self {
            Self::Full => PixelRect::full(width, height),
            Self::Region(rect) => PixelRect {
                x: rect.x.min(width),
                y: rect.y.min(height),
                width: rect.width.min(width.saturating_sub(rect.x)),
                height: rect.height.min(height.saturating_sub(rect.y)),
            },
        };
        rect.even()
    }
}

/// A family of encoders sharing a buffer kind.
pub trait VideoBackend {
    fn name(&self) -> &'static str;

    fn buffer_kind(&self) -> BufferKind;

    /// Opens an encoder sized for `first`, which it has not encoded yet.
    fn open(
        &self,
        first: &CapturedFrame<'_>,
        render_node: Option<&Path>,
        settings: &VideoSettings,
    ) -> Result<Box<dyn VideoEncoder>, RecordError>;
}

/// A running video encoder.
pub trait VideoEncoder: EncodedStream {
    /// Encodes `frame`, stamped `timestamp` microseconds into the recording.
    fn encode(
        &mut self,
        frame: &CapturedFrame<'_>,
        timestamp: i64,
        sink: &mut dyn PacketSink,
    ) -> Result<(), RecordError>;

    /// Flushes the frames the encoder still holds.
    fn finish(&mut self, sink: &mut dyn PacketSink) -> Result<(), RecordError>;
}

/// Bits per pixel per frame for each quality step.
fn bits_per_pixel(quality: RecordingQuality) -> f64 {
    match quality {
        RecordingQuality::Low => 0.04,
        RecordingQuality::Medium => 0.08,
        RecordingQuality::High => 0.15,
        RecordingQuality::Lossless => 0.4,
    }
}

pub(crate) fn bit_rate(width: u32, height: u32, settings: &VideoSettings) -> usize {
    let pixels_per_second =
        f64::from(width) * f64::from(height) * f64::from(settings.frames_per_second);
    (pixels_per_second * bits_per_pixel(settings.quality)) as usize
}

/// Opens `name` for `width`×`height` frames of `pixel` with the shared rate control.
pub(crate) fn open_encoder(
    name: &'static str,
    pixel: format::Pixel,
    width: u32,
    height: u32,
    settings: &VideoSettings,
    configure: impl FnOnce(&mut encoder::video::Video),
    options: Dictionary<'_>,
) -> Result<encoder::Video, RecordError> {
    let codec = encoder::find_by_name(name).ok_or(RecordError::NoEncoder(name))?;
    let mut video = codec::Context::new_with_codec(codec).encoder().video()?;
    let rate = bit_rate(width, height, settings);
    video.set_width(width);
    video.set_height(height);
    video.set_format(pixel);
    video.set_time_base(Rational(1, RecordingClock::MICROSECONDS));
    video.set_frame_rate(Some(Rational(settings.frames_per_second as i32, 1)));
    video.set_gop(settings.frames_per_second * 2);
    video.set_max_b_frames(0);
    video.set_bit_rate(rate);
    video.set_max_bit_rate(rate * 3 / 2);
    video.set_flags(codec::Flags::GLOBAL_HEADER);
    configure(&mut video);
    Ok(video.open_with(options)?)
}

/// Hands every packet `encoder` has ready to `sink`. Packets without a duration get one frame
/// interval, or containers would trim the last frame off the end.
pub(crate) fn drain(
    encoder: &mut encoder::Video,
    sink: &mut dyn PacketSink,
) -> Result<(), RecordError> {
    let rate = encoder.frame_rate();
    let frame_interval = match rate.numerator() {
        0 => 0,
        frames => {
            i64::from(RecordingClock::MICROSECONDS) * i64::from(rate.denominator())
                / i64::from(frames)
        }
    };
    loop {
        let mut packet = Packet::empty();
        match encoder.receive_packet(&mut packet) {
            Ok(()) => {
                if packet.duration() == 0 {
                    packet.set_duration(frame_interval);
                }
                sink.video(packet)?;
            }
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

pub(crate) fn flush(
    encoder: &mut encoder::Video,
    sink: &mut dyn PacketSink,
) -> Result<(), RecordError> {
    encoder.send_eof()?;
    drain(encoder, sink)
}

/// The FFmpeg name of a packed 32-bit pixel layout.
pub(crate) fn ffmpeg_pixel(format: PixelFormat) -> format::Pixel {
    match format {
        PixelFormat::Bgrx => format::Pixel::BGRZ,
        PixelFormat::Bgra => format::Pixel::BGRA,
        PixelFormat::Rgbx => format::Pixel::RGBZ,
        PixelFormat::Rgba => format::Pixel::RGBA,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crops_to_even_region_inside_frame() {
        let crop = Crop::Region(PixelRect {
            x: 100,
            y: 50,
            width: 3000,
            height: 301,
        });
        assert_eq!(
            crop.within(1920, 1080),
            PixelRect {
                x: 100,
                y: 50,
                width: 1820,
                height: 300
            }
        );
        assert_eq!(
            Crop::Full.within(1366, 767),
            PixelRect {
                x: 0,
                y: 0,
                width: 1366,
                height: 766
            }
        );
    }

    #[test]
    fn scales_bit_rate_with_quality() {
        let settings = |quality| VideoSettings {
            codec: VideoCodec::H264,
            quality,
            frames_per_second: 60,
            crop: Crop::Full,
        };
        let low = bit_rate(1920, 1080, &settings(RecordingQuality::Low));
        let high = bit_rate(1920, 1080, &settings(RecordingQuality::High));
        assert!(low < high);
        assert!((4_000_000..8_000_000).contains(&low));
    }
}
