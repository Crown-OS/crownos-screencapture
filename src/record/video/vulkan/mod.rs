mod import;

use std::path::Path;

use crownos_config::schema::{RecordingQuality, VideoCodec};
use ffmpeg_next::ffi::AVHWDeviceType;
use ffmpeg_next::util::color;
use ffmpeg_next::{Dictionary, encoder, format, frame};

use self::import::{VulkanInput, vulkan_device};
use super::hardware::HardwareContext;
use super::{Crop, VideoBackend, VideoEncoder, VideoSettings, drain, flush, open_encoder};
use crate::capture::{BufferKind, CapturedFrame, FramePixels, PixelRect};
use crate::record::{EncodedStream, PacketSink, RecordError};

/// Hardware encoding through Vulkan Video, mapping the compositor's dmabufs into Vulkan images
/// on the GPU that rendered them.
#[derive(Debug, Default, Clone, Copy)]
pub struct VulkanBackend;

impl VideoBackend for VulkanBackend {
    fn name(&self) -> &'static str {
        "Vulkan"
    }

    fn buffer_kind(&self) -> BufferKind {
        BufferKind::Gpu
    }

    fn open(
        &self,
        first: &CapturedFrame<'_>,
        render_node: Option<&Path>,
        settings: &VideoSettings,
    ) -> Result<Box<dyn VideoEncoder>, RecordError> {
        let FramePixels::Gpu(dmabuf) = first.pixels else {
            return Err(RecordError::WrongBuffer);
        };
        let drm = HardwareContext::device(AVHWDeviceType::AV_HWDEVICE_TYPE_DRM, render_node)?;
        let vulkan = vulkan_device(&drm)?;
        let output = converted_rect(settings.crop, dmabuf.width, dmabuf.height);
        let output_size = (output.width, output.height);
        let mut input = VulkanInput::new(&drm, &vulkan, dmabuf, settings.crop, output_size)?;
        let frames = input.output_frames()?;
        let encoder = open_encoder(
            encoder_name(settings.codec),
            format::Pixel::VULKAN,
            output.width,
            output.height,
            settings,
            |video| {
                video.set_colorspace(color::Space::BT709);
                video.set_color_range(color::Range::MPEG);
                // SAFETY: the encoder context takes ownership of the new reference.
                unsafe { (*video.as_mut_ptr()).hw_frames_ctx = frames.new_reference() }
            },
            encoder_options(settings),
        )?;
        Ok(Box::new(VulkanEncoder {
            drm,
            vulkan,
            input,
            encoder,
            crop: settings.crop,
            output_size,
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

fn encoder_options(settings: &VideoSettings) -> Dictionary<'static> {
    let mut options = Dictionary::new();
    options.set("rc_mode", "cqp");
    options.set(
        "qp",
        &quantizer(settings.codec, settings.quality).to_string(),
    );
    options.set("content", "desktop");
    options.set("usage", "record");
    options
}

/// A constant quantizer per quality step; AV1 counts in q-index steps, four per H.264 step.
fn quantizer(codec: VideoCodec, quality: RecordingQuality) -> u32 {
    let h264_scale = match quality {
        RecordingQuality::Low => 32,
        RecordingQuality::Medium => 27,
        RecordingQuality::High => 23,
        RecordingQuality::Lossless => 16,
    };
    match codec {
        VideoCodec::H264 | VideoCodec::Hevc => h264_scale,
        VideoCodec::Av1 => h264_scale * 4,
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
    drm: HardwareContext,
    vulkan: HardwareContext,
    input: VulkanInput,
    encoder: encoder::Video,
    crop: Crop,
    output_size: (u32, u32),
    converted: frame::Video,
}

// SAFETY: every FFmpeg object here is owned by the encoder and used from one thread at a time.
unsafe impl Send for VulkanEncoder {}

impl EncodedStream for VulkanEncoder {
    fn context(&self) -> &ffmpeg_next::codec::Context {
        &self.encoder
    }
}

impl VideoEncoder for VulkanEncoder {
    fn encode(
        &mut self,
        frame: &CapturedFrame<'_>,
        timestamp: i64,
        sink: &mut dyn PacketSink,
    ) -> Result<(), RecordError> {
        let FramePixels::Gpu(dmabuf) = frame.pixels else {
            return Err(RecordError::WrongBuffer);
        };
        if !self.input.accepts(dmabuf) {
            self.input =
                VulkanInput::new(&self.drm, &self.vulkan, dmabuf, self.crop, self.output_size)?;
        }
        self.input.push(frame, dmabuf, timestamp)?;
        while self.input.pull(&mut self.converted)? {
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

    #[test]
    fn scales_quantizer_to_codec() {
        assert_eq!(quantizer(VideoCodec::H264, RecordingQuality::High), 23);
        assert_eq!(quantizer(VideoCodec::Av1, RecordingQuality::High), 92);
        assert!(
            quantizer(VideoCodec::Hevc, RecordingQuality::Low)
                > quantizer(VideoCodec::Hevc, RecordingQuality::Lossless)
        );
    }
}
