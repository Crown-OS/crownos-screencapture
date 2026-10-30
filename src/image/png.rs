use std::io::Write;

use png::{BitDepth, ColorType, Compression, Encoder};

use super::{EncodeError, ImageEncoder};
use crate::capture::{PixelRect, ShmFrame};

/// Lossless PNG through `fdeflate`, streamed row by row.
#[derive(Debug, Default, Clone, Copy)]
pub struct PngEncoder;

impl ImageEncoder for PngEncoder {
    fn extension(&self) -> &'static str {
        "png"
    }

    fn encode(
        &self,
        frame: &ShmFrame,
        rect: PixelRect,
        output: &mut dyn Write,
    ) -> Result<(), EncodeError> {
        let channels = if frame.format.has_alpha() { 4 } else { 3 };
        let mut encoder = Encoder::new(output, rect.width, rect.height);
        encoder.set_color(if channels == 4 {
            ColorType::Rgba
        } else {
            ColorType::Rgb
        });
        encoder.set_depth(BitDepth::Eight);
        encoder.set_compression(Compression::Fast);
        let mut writer = encoder.write_header()?;
        let mut stream = writer.stream_writer()?;
        let mut row_bytes = vec![0; rect.width as usize * channels];
        for row in frame.rows(rect) {
            for (pixel, target) in row
                .chunks_exact(4)
                .zip(row_bytes.chunks_exact_mut(channels))
            {
                let (red, blue) = if frame.format.is_bgr() {
                    (pixel[2], pixel[0])
                } else {
                    (pixel[0], pixel[2])
                };
                target[..3].copy_from_slice(&[red, pixel[1], blue]);
                if channels == 4 {
                    target[3] = pixel[3];
                }
            }
            stream.write_all(&row_bytes)?;
        }
        stream.finish()?;
        Ok(())
    }
}
