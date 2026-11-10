//! Encoding captured frames to compressed video.
//!
//! A [`VideoBackend`] says which buffers it wants and opens a [`VideoEncoder`] once the first
//! frame shows the real size. [`VaapiBackend`] keeps frames on the GPU end to end;
//! [`SoftwareBackend`] is the fallback for machines without a usable VA-API driver.

mod dmabuf;
mod filter;
mod hardware;
mod keyframes;
mod rate;
mod slots;
mod software;
mod vaapi;
mod vulkan;

use std::path::Path;

use crownos_config::schema::{RecordingQuality, VideoCodec};
use ffmpeg_next::util::color;
use ffmpeg_next::{Dictionary, Packet, Rational, codec, encoder, format, picture};

pub use self::keyframes::KeyframeSchedule;
pub use self::software::SoftwareBackend;
pub use self::vaapi::VaapiBackend;
pub use self::vulkan::VulkanBackend;
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

    /// Whether `damage` on a frame of `size` may have changed the kept pixels. Empty damage is
    /// unknown damage, so it may.
    pub fn is_changed_by(self, damage: &[PixelRect], (width, height): (u32, u32)) -> bool {
        let Self::Region(_) = self else {
            return true;
        };
        let kept = self.within(width, height);
        damage.is_empty() || damage.iter().any(|rect| rect.intersects(&kept))
    }
}

/// A family of encoders sharing a buffer kind.
pub trait VideoBackend {
    fn name(&self) -> &'static str;

    fn buffer_kind(&self) -> BufferKind;

    /// The pixel layouts this backend can take, best first.
    fn pixel_formats(&self) -> &'static [PixelFormat] {
        &PixelFormat::PREFERRED
    }

    /// Opens an encoder sized for `first`, which it has not encoded yet.
    fn open(
        &self,
        first: &CapturedFrame,
        render_node: Option<&Path>,
        settings: &VideoSettings,
    ) -> Result<Box<dyn VideoEncoder>, RecordError>;
}

/// A running video encoder.
pub trait VideoEncoder: EncodedStream + Send {
    /// Encodes `frame`, stamped `timestamp` microseconds into the recording, forcing a
    /// keyframe when `picture` is `I`.
    fn encode(
        &mut self,
        frame: &CapturedFrame,
        timestamp: i64,
        picture: picture::Type,
        sink: &mut dyn PacketSink,
    ) -> Result<(), RecordError>;

    /// Flushes the frames the encoder still holds.
    fn finish(&mut self, sink: &mut dyn PacketSink) -> Result<(), RecordError>;
}

/// Opens `name` for `width`×`height` frames of `pixel` at constant quality, tagged as BT.709
/// limited range. Keyframes come from a [`KeyframeSchedule`]; the GOP is only a backstop.
pub(crate) fn open_encoder(
    name: &'static str,
    pixel: format::Pixel,
    width: u32,
    height: u32,
    settings: &VideoSettings,
    configure: impl FnOnce(&mut encoder::video::Video),
    mut options: Dictionary<'_>,
) -> Result<encoder::Video, RecordError> {
    let codec = encoder::find_by_name(name).ok_or(RecordError::NoEncoder(name))?;
    let mut video = codec::Context::new_with_codec(codec).encoder().video()?;
    video.set_width(width);
    video.set_height(height);
    video.set_format(pixel);
    video.set_time_base(Rational(1, RecordingClock::MICROSECONDS));
    video.set_frame_rate(Some(Rational(settings.frames_per_second as i32, 1)));
    video.set_gop(settings.frames_per_second * 10);
    video.set_max_b_frames(0);
    video.set_colorspace(color::Space::BT709);
    video.set_color_primaries(color::Primaries::BT709);
    video.set_color_transfer_characteristic(color::TransferCharacteristic::BT709);
    video.set_color_range(color::Range::MPEG);
    video.set_flags(codec::Flags::GLOBAL_HEADER);
    configure(&mut video);
    rate::add_constant_quality(&mut options, name, settings.quality)?;
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
    fn region_changes_only_under_damage() {
        let size = (1920, 1080);
        let crop = Crop::Region(PixelRect {
            x: 100,
            y: 100,
            width: 200,
            height: 200,
        });
        let damage = |x, y| PixelRect {
            x,
            y,
            width: 50,
            height: 50,
        };
        assert!(crop.is_changed_by(&[], size));
        assert!(crop.is_changed_by(&[damage(0, 0), damage(280, 280)], size));
        assert!(!crop.is_changed_by(&[damage(0, 0), damage(1000, 500)], size));
        assert!(!crop.is_changed_by(&[damage(300, 100), damage(100, 50)], size));
        assert!(Crop::Full.is_changed_by(&[damage(0, 0)], size));
    }
}
