mod input;

use std::path::Path;

use crownos_config::schema::VideoCodec;
use ffmpeg_next::ffi::AVHWDeviceType;
use ffmpeg_next::{Dictionary, encoder, format, frame, picture};

use self::input::VaapiInput;
use super::hardware::HardwareContext;
use super::{Crop, VideoBackend, VideoEncoder, VideoSettings, drain, flush, open_encoder};
use crate::capture::{BufferKind, CapturedFrame, FramePixels};
use crate::record::{EncodedStream, PacketSink, RecordError};

/// Hardware encoding through VA-API, importing the compositor's dmabufs without a copy.
#[derive(Debug, Default, Clone, Copy)]
pub struct VaapiBackend;

impl VideoBackend for VaapiBackend {
    fn name(&self) -> &'static str {
        "VA-API"
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
        let drm_device =
            HardwareContext::device(AVHWDeviceType::AV_HWDEVICE_TYPE_DRM, render_node)?;
        let vaapi_device = drm_device.derive(AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI)?;
        let output = settings.crop.within(dmabuf.width, dmabuf.height);
        let output_size = (output.width, output.height);
        let mut input = VaapiInput::new(
            &drm_device,
            &vaapi_device,
            dmabuf,
            settings.crop,
            output_size,
        )?;
        let frames = input.filters.output_frames()?;
        let encoder = open_encoder(
            encoder_name(settings.codec),
            format::Pixel::VAAPI,
            output.width,
            output.height,
            settings,
            // SAFETY: the encoder context takes ownership of the new reference.
            |video| unsafe { (*video.as_mut_ptr()).hw_frames_ctx = frames.new_reference() },
            Dictionary::new(),
        )?;
        Ok(Box::new(VaapiEncoder {
            drm_device,
            vaapi_device,
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
        VideoCodec::H264 => "h264_vaapi",
        VideoCodec::Hevc => "hevc_vaapi",
        VideoCodec::Av1 => "av1_vaapi",
    }
}

struct VaapiEncoder {
    drm_device: HardwareContext,
    vaapi_device: HardwareContext,
    input: VaapiInput,
    encoder: encoder::Video,
    crop: Crop,
    output_size: (u32, u32),
    converted: frame::Video,
}

// SAFETY: every FFmpeg object here is owned by the encoder and used from one thread at a time.
unsafe impl Send for VaapiEncoder {}

impl EncodedStream for VaapiEncoder {
    fn context(&self) -> &ffmpeg_next::codec::Context {
        &self.encoder
    }
}

impl VideoEncoder for VaapiEncoder {
    fn encode(
        &mut self,
        frame: &CapturedFrame<'_>,
        timestamp: i64,
        picture: picture::Type,
        sink: &mut dyn PacketSink,
    ) -> Result<(), RecordError> {
        let FramePixels::Gpu(dmabuf) = frame.pixels else {
            return Err(RecordError::WrongBuffer);
        };
        if !self.input.accepts(dmabuf) {
            self.input = VaapiInput::new(
                &self.drm_device,
                &self.vaapi_device,
                dmabuf,
                self.crop,
                self.output_size,
            )?;
        }
        self.input.push(frame, dmabuf, timestamp, picture)?;
        while self.input.filters.pull(&mut self.converted)? {
            self.encoder.send_frame(&self.converted)?;
            drain(&mut self.encoder, sink)?;
        }
        Ok(())
    }

    fn finish(&mut self, sink: &mut dyn PacketSink) -> Result<(), RecordError> {
        flush(&mut self.encoder, sink)
    }
}
