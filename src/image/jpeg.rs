use std::io::Write;

use turbojpeg::{Compressor, Image, Subsamp};

use super::{EncodeError, ImageEncoder};
use crate::capture::{PixelFormat, PixelRect, ShmFrame};

const DEFAULT_QUALITY: i32 = 92;

/// JPEG through libjpeg-turbo's SIMD encoder, reading the frame in place.
#[derive(Debug, Clone, Copy)]
pub struct JpegEncoder {
    quality: i32,
}

impl Default for JpegEncoder {
    fn default() -> Self {
        Self {
            quality: DEFAULT_QUALITY,
        }
    }
}

impl ImageEncoder for JpegEncoder {
    fn extension(&self) -> &'static str {
        "jpg"
    }

    fn encode(
        &self,
        frame: &ShmFrame,
        rect: PixelRect,
        output: &mut dyn Write,
    ) -> Result<(), EncodeError> {
        let mut compressor = Compressor::new()?;
        compressor.set_quality(self.quality)?;
        compressor.set_subsamp(Subsamp::Sub2x2)?;
        let image = Image {
            pixels: frame.bytes_from(rect),
            width: rect.width as usize,
            pitch: frame.stride as usize,
            height: rect.height as usize,
            format: turbojpeg_format(frame.format),
        };
        output.write_all(&compressor.compress_to_owned(image)?)?;
        Ok(())
    }
}

fn turbojpeg_format(format: PixelFormat) -> turbojpeg::PixelFormat {
    match format {
        PixelFormat::Bgrx => turbojpeg::PixelFormat::BGRX,
        PixelFormat::Bgra => turbojpeg::PixelFormat::BGRA,
        PixelFormat::Rgbx => turbojpeg::PixelFormat::RGBX,
        PixelFormat::Rgba => turbojpeg::PixelFormat::RGBA,
    }
}
