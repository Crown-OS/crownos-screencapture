use std::path::Path;

use crownos_config::schema::VideoCodec;
use ffmpeg_next::{Dictionary, encoder, format, frame};

use super::filter::FilterChain;
use super::{
    Crop, VideoBackend, VideoEncoder, VideoSettings, drain, ffmpeg_pixel, flush, open_encoder,
};
use crate::capture::{BufferKind, CapturedFrame, FramePixels, PixelRect, ShmFrame};
use crate::record::{EncodedStream, PacketSink, RecordError};

/// CPU encoding from shared-memory frames, for machines without VA-API.
#[derive(Debug, Default, Clone, Copy)]
pub struct SoftwareBackend;

impl VideoBackend for SoftwareBackend {
    fn name(&self) -> &'static str {
        "software"
    }

    fn buffer_kind(&self) -> BufferKind {
        BufferKind::Memory
    }

    fn open(
        &self,
        first: &CapturedFrame<'_>,
        _: Option<&Path>,
        settings: &VideoSettings,
    ) -> Result<Box<dyn VideoEncoder>, RecordError> {
        let FramePixels::Memory(shm) = first.pixels else {
            return Err(RecordError::WrongBuffer);
        };
        let output = settings.crop.within(shm.width, shm.height);
        let (name, options) = encoder_for(settings.codec);
        let encoder = open_encoder(
            name,
            format::Pixel::YUV420P,
            output.width,
            output.height,
            settings,
            |_| {},
            options,
        )?;
        Ok(Box::new(SoftwareEncoder {
            input: SoftwareInput::new(shm, settings.crop, (output.width, output.height))?,
            encoder,
            crop: settings.crop,
            output_size: (output.width, output.height),
            converted: frame::Video::empty(),
        }))
    }
}

fn encoder_for(codec: VideoCodec) -> (&'static str, Dictionary<'static>) {
    let mut options = Dictionary::new();
    let name = match codec {
        VideoCodec::H264 => {
            options.set("preset", "veryfast");
            "libx264"
        }
        VideoCodec::Hevc => {
            options.set("preset", "veryfast");
            "libx265"
        }
        VideoCodec::Av1 => {
            options.set("preset", "10");
            "libsvtav1"
        }
    };
    (name, options)
}

struct SoftwareInput {
    width: u32,
    height: u32,
    format: format::Pixel,
    filters: FilterChain,
}

impl SoftwareInput {
    fn new(frame: &ShmFrame, crop: Crop, (width, height): (u32, u32)) -> Result<Self, RecordError> {
        let format = ffmpeg_pixel(frame.format);
        let PixelRect {
            x,
            y,
            width: kept_width,
            height: kept_height,
        } = crop.within(frame.width, frame.height);
        let filters = FilterChain::new(
            format.into(),
            frame.width,
            frame.height,
            None,
            &format!(
                "crop={kept_width}:{kept_height}:{x}:{y},scale={width}:{height}:out_color_matrix=bt709:out_range=tv,format=yuv420p"
            ),
        )?;
        Ok(Self {
            width: frame.width,
            height: frame.height,
            format,
            filters,
        })
    }

    fn accepts(&self, frame: &ShmFrame) -> bool {
        (self.width, self.height, self.format)
            == (frame.width, frame.height, ffmpeg_pixel(frame.format))
    }
}

struct SoftwareEncoder {
    input: SoftwareInput,
    encoder: encoder::Video,
    crop: Crop,
    output_size: (u32, u32),
    converted: frame::Video,
}

// SAFETY: every FFmpeg object here is owned by the encoder and used from one thread at a time.
unsafe impl Send for SoftwareEncoder {}

impl EncodedStream for SoftwareEncoder {
    fn context(&self) -> &ffmpeg_next::codec::Context {
        &self.encoder
    }
}

impl VideoEncoder for SoftwareEncoder {
    fn encode(
        &mut self,
        frame: &CapturedFrame<'_>,
        timestamp: i64,
        sink: &mut dyn PacketSink,
    ) -> Result<(), RecordError> {
        let FramePixels::Memory(shm) = frame.pixels else {
            return Err(RecordError::WrongBuffer);
        };
        if !self.input.accepts(shm) {
            self.input = SoftwareInput::new(shm, self.crop, self.output_size)?;
        }
        let mut pixels = frame::Video::new(self.input.format, shm.width, shm.height);
        let stride = pixels.stride(0);
        for (target, row) in pixels
            .data_mut(0)
            .chunks_exact_mut(stride)
            .zip(shm.rows(shm.full_rect()))
        {
            target[..row.len()].copy_from_slice(row);
        }
        pixels.set_pts(Some(timestamp));
        self.input.filters.push(&pixels)?;
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
