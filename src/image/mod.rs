//! Turning captured pixels into image files.

mod error;
mod jpeg;
mod png;

use std::io::Write;

use crownos_config::schema::ImageFormat;

pub use self::error::EncodeError;
pub use self::jpeg::JpegEncoder;
pub use self::png::PngEncoder;
use crate::capture::{PixelRect, ShmFrame};

/// An image file format the tool can write.
pub trait ImageEncoder {
    fn extension(&self) -> &'static str;

    /// Writes the `rect` part of `frame` to `output`.
    fn encode(
        &self,
        frame: &ShmFrame,
        rect: PixelRect,
        output: &mut dyn Write,
    ) -> Result<(), EncodeError>;
}

pub fn encoder_for(format: ImageFormat) -> Box<dyn ImageEncoder> {
    match format {
        ImageFormat::Png => Box::new(PngEncoder),
        ImageFormat::Jpeg => Box::new(JpegEncoder::default()),
    }
}
