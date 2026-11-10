mod shm_input;

use std::num::NonZero;
use std::path::Path;
use std::thread;

use crownos_config::schema::VideoCodec;
use ffmpeg_next::{Dictionary, encoder, format, frame, picture};

use self::shm_input::share_pixels;
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
        first: &CapturedFrame,
        _: Option<&Path>,
        settings: &VideoSettings,
    ) -> Result<Box<dyn VideoEncoder>, RecordError> {
        let FramePixels::Memory(shm) = &first.pixels else {
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

const MAX_CONVERSION_THREADS: usize = 8;

/// Threads for the colour conversion: one per core, capped where more stop speeding swscale up
/// and only compete with the encoder for cores.
fn conversion_threads(cores: usize) -> usize {
    cores.clamp(1, MAX_CONVERSION_THREADS)
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
            conversion_threads(thread::available_parallelism().map_or(1, NonZero::get)),
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

// SAFETY: every FFmpeg object here is owned by the encoder, which is opened on the capture
// thread and then moved to the encode thread, so one thread at a time uses it. The frames the
// graph may still hold keep their pixels alive through an `Arc`, which is itself `Send`.
unsafe impl Send for SoftwareEncoder {}

impl EncodedStream for SoftwareEncoder {
    fn context(&self) -> &ffmpeg_next::codec::Context {
        &self.encoder
    }
}

impl VideoEncoder for SoftwareEncoder {
    fn encode(
        &mut self,
        frame: &CapturedFrame,
        timestamp: i64,
        picture: picture::Type,
        sink: &mut dyn PacketSink,
    ) -> Result<(), RecordError> {
        let FramePixels::Memory(shm) = &frame.pixels else {
            return Err(RecordError::WrongBuffer);
        };
        if !self.input.accepts(shm) {
            self.input = SoftwareInput::new(shm, self.crop, self.output_size)?;
        }
        let mut pixels = share_pixels(shm, self.input.format)?;
        pixels.set_pts(Some(timestamp));
        pixels.set_kind(picture);
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

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::Write;
    use std::os::fd::OwnedFd;
    use std::sync::Arc;

    use rustix::fs::{MemfdFlags, memfd_create};

    use super::*;
    use crate::capture::PixelFormat;

    const WIDTH: u32 = 64;
    const HEIGHT: u32 = 48;
    const STRIDE: u32 = WIDTH * 4 + 32;
    const REGION: PixelRect = PixelRect {
        x: 6,
        y: 4,
        width: 40,
        height: 30,
    };

    fn patterned_frame(padding: usize) -> Result<ShmFrame, RecordError> {
        let mut file = File::from(
            memfd_create("software-test", MemfdFlags::CLOEXEC).map_err(std::io::Error::from)?,
        );
        let pattern: Vec<u8> = (0..(STRIDE * HEIGHT) as usize + padding)
            .map(|index| (index * 7 % 251) as u8)
            .collect();
        file.write_all(&pattern)?;
        Ok(ShmFrame::from_fd(
            OwnedFd::from(file),
            WIDTH,
            HEIGHT,
            STRIDE,
            PixelFormat::Bgrx,
        )?)
    }

    fn convert(shm: &ShmFrame, pixels: &frame::Video) -> Result<frame::Video, RecordError> {
        let mut input =
            SoftwareInput::new(shm, Crop::Region(REGION), (REGION.width, REGION.height))?;
        let mut converted = frame::Video::empty();
        input.filters.push(pixels)?;
        assert!(input.filters.pull(&mut converted)?);
        assert!(!input.filters.pull(&mut frame::Video::empty())?);
        Ok(converted)
    }

    fn copied(shm: &ShmFrame) -> frame::Video {
        let mut pixels = frame::Video::new(ffmpeg_pixel(shm.format), shm.width, shm.height);
        let stride = pixels.stride(0);
        for (target, row) in pixels
            .data_mut(0)
            .chunks_exact_mut(stride)
            .zip(shm.rows(shm.full_rect()))
        {
            target[..row.len()].copy_from_slice(row);
        }
        pixels
    }

    fn planes(video: &frame::Video) -> Vec<&[u8]> {
        (0..video.planes())
            .flat_map(|plane| {
                video
                    .data(plane)
                    .chunks(video.stride(plane))
                    .take(video.plane_height(plane) as usize)
                    .map(move |row| &row[..video.plane_width(plane) as usize])
            })
            .collect()
    }

    #[test]
    fn shared_pixels_convert_like_copied_ones_and_are_released() -> Result<(), RecordError> {
        let shm = Arc::new(patterned_frame(ShmFrame::READ_PADDING)?);
        let expected = convert(&shm, &copied(&shm))?;
        let shared = shm_input::share_pixels(&shm, ffmpeg_pixel(shm.format))?;
        assert_eq!(Arc::strong_count(&shm), 2);
        let converted = convert(&shm, &shared)?;
        assert_eq!(planes(&converted), planes(&expected));
        assert_eq!(Arc::strong_count(&shm), 1);
        Ok(())
    }

    #[test]
    fn refuses_pixels_without_read_padding() -> Result<(), RecordError> {
        let shm = Arc::new(patterned_frame(0)?);
        assert!(matches!(
            shm_input::share_pixels(&shm, ffmpeg_pixel(shm.format)),
            Err(RecordError::WrongBuffer)
        ));
        assert_eq!(Arc::strong_count(&shm), 1);
        Ok(())
    }

    #[test]
    fn conversion_threads_stay_between_one_and_the_cap() {
        assert_eq!(conversion_threads(0), 1);
        assert_eq!(conversion_threads(4), 4);
        assert_eq!(conversion_threads(64), MAX_CONVERSION_THREADS);
    }
}
