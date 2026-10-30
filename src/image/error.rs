use thiserror::Error;

#[derive(Debug, Error)]
pub enum EncodeError {
    #[error("PNG encoding failed: {0}")]
    Png(#[from] ::png::EncodingError),
    #[error("JPEG encoding failed: {0}")]
    Jpeg(#[from] turbojpeg::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
