mod import;

use std::path::Path;

use crownos_config::schema::VideoCodec;
use ffmpeg_next::ffi::AVHWDeviceType;
use ffmpeg_next::{Dictionary, encoder, format, frame, picture};

use self::import::{VulkanInput, vulkan_device};
use super::hardware::HardwareContext;
use super::{Crop, VideoBackend, VideoEncoder, VideoSettings, drain, flush, open_encoder};
use crate::capture::{BufferKind, CapturedFrame, FramePixels, PixelFormat, PixelRect};
use crate::record::{EncodedStream, PacketSink, RecordError};

/// Hardware encoding through Vulkan Video, mapping the compositor's dmabufs into Vulkan images
/// on the GPU that rendered them.
#[derive(Debug, Default, Clone, Copy)]
pub struct VulkanBackend;

/// `scale_vulkan` samples every 8-bit RGB layout as R8G8B8A8, so only red-first buffers keep
/// their colours.
const RED_FIRST: [PixelFormat; 2] = [PixelFormat::Rgbx, PixelFormat::Rgba];

impl VideoBackend for VulkanBackend {
    fn name(&self) -> &'static str {
        "Vulkan"
    }

    fn buffer_kind(&self) -> BufferKind {
        BufferKind::Gpu
    }

    fn pixel_formats(&self) -> &'static [PixelFormat] {
        &RED_FIRST
    }

    fn open(
        &self,
        first: &CapturedFrame,
        render_node: Option<&Path>,
        settings: &VideoSettings,
    ) -> Result<Box<dyn VideoEncoder>, RecordError> {
        let FramePixels::Gpu(dmabuf) = &first.pixels else {
            return Err(RecordError::WrongBuffer);
        };
        let drm = HardwareContext::device(AVHWDeviceType::AV_HWDEVICE_TYPE_DRM, render_node)?;
        let vulkan = vulkan_device(&drm)?;
        let output = converted_rect(settings.crop, dmabuf.width, dmabuf.height);
        let output_size = (output.width, output.height);
        let mut input = VulkanInput::new(&drm, &vulkan, dmabuf, settings.crop, output_size)?;
        let frames = input.filters.output_frames()?;
        let mut options = Dictionary::new();
        options.set("content", "desktop");
        options.set("usage", "record");
        let encoder = open_encoder(
            encoder_name(settings.codec),
            format::Pixel::VULKAN,
            output.width,
            output.height,
            settings,
            // SAFETY: the encoder context takes ownership of the new reference.
            |video| unsafe { (*video.as_mut_ptr()).hw_frames_ctx = frames.new_reference() },
            options,
        )?;
        Ok(Box::new(VulkanEncoder {
            input,
            encoder,
            converted: frame::Video::empty(),
        }))
    }
}

fn encoder_name(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::H264 => "h264_vulkan",
        VideoCodec::Hevc => "hevc_vulkan",
        VideoCodec::Av1 => "av1_vulkan",
    }
}

/// The kept part of a `width`×`height` buffer, narrowed by two pixels along any axis it would
/// otherwise span: `scale_vulkan` only converts RGB to YUV in its own shader when both output
/// dimensions differ from its input, and falls back to an unsupported swscale path otherwise.
fn converted_rect(crop: Crop, width: u32, height: u32) -> PixelRect {
    let kept = crop.within(width, height);
    let narrowed = |length: u32, full: u32| {
        if length == full {
            length.saturating_sub(2)
        } else {
            length
        }
    };
    PixelRect {
        width: narrowed(kept.width, width),
        height: narrowed(kept.height, height),
        ..kept
    }
}

struct VulkanEncoder {
    input: VulkanInput,
    encoder: encoder::Video,
    converted: frame::Video,
}

// SAFETY: every FFmpeg object here is owned by the encoder, which is opened on the capture
// thread and then moved to the encode thread, so one thread at a time uses it. The cached
// mappings hold their own imports of the GPU memory, never a frame handle.
unsafe impl Send for VulkanEncoder {}

impl EncodedStream for VulkanEncoder {
    fn context(&self) -> &ffmpeg_next::codec::Context {
        &self.encoder
    }
}

impl VideoEncoder for VulkanEncoder {
    fn encode(
        &mut self,
        frame: &CapturedFrame,
        timestamp: i64,
        picture: picture::Type,
        sink: &mut dyn PacketSink,
    ) -> Result<(), RecordError> {
        let FramePixels::Gpu(dmabuf) = &frame.pixels else {
            return Err(RecordError::WrongBuffer);
        };
        if !self.input.accepts(dmabuf) {
            return Err(RecordError::Resized("Vulkan"));
        }
        self.input.push(frame, dmabuf, timestamp, picture)?;
        while self.input.filters.pull(&mut self.converted)? {
            // SAFETY: the converted frame is owned here; the crop it inherited from the input
            // was already applied by the scaler.
            unsafe {
                let raw = self.converted.as_mut_ptr();
                (*raw).crop_left = 0;
                (*raw).crop_top = 0;
                (*raw).crop_right = 0;
                (*raw).crop_bottom = 0;
            }
            self.encoder.send_frame(&self.converted)?;
            drain(&mut self.encoder, sink)?;
        }
        Ok(())
    }

    fn finish(&mut self, sink: &mut dyn PacketSink) -> Result<(), RecordError> {
        flush(&mut self.encoder, sink)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn narrows_axes_that_span_the_buffer() {
        assert_eq!(
            converted_rect(Crop::Full, 2560, 1600),
            PixelRect {
                x: 0,
                y: 0,
                width: 2558,
                height: 1598
            }
        );
        assert_eq!(
            converted_rect(Crop::Full, 1366, 767),
            PixelRect {
                x: 0,
                y: 0,
                width: 1364,
                height: 766
            }
        );
        let strip = PixelRect {
            x: 0,
            y: 100,
            width: 1920,
            height: 300,
        };
        assert_eq!(
            converted_rect(Crop::Region(strip), 1920, 1080),
            PixelRect {
                width: 1918,
                ..strip
            }
        );
    }

    #[test]
    fn keeps_regions_inside_the_buffer() {
        let region = PixelRect {
            x: 300,
            y: 200,
            width: 1280,
            height: 720,
        };
        assert_eq!(converted_rect(Crop::Region(region), 1920, 1080), region);
    }
}
