use thiserror::Error;

use crate::capture::CaptureError;

#[derive(Debug, Error)]
pub enum RecordError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error("FFmpeg: {0}")]
    Ffmpeg(#[from] ffmpeg_next::Error),
    #[error("FFmpeg {call} failed with code {code}")]
    FfmpegCall { call: &'static str, code: i32 },
    #[error("no {0} encoder is available")]
    NoEncoder(&'static str),
    #[error("PipeWire: {0}")]
    PipeWire(#[from] pipewire::Error),
    #[error("the capture delivered a buffer the encoder cannot read")]
    WrongBuffer,
    #[error("a recording is already running")]
    AlreadyRecording,
    #[error("recorder service: {0}")]
    Service(#[from] crownos_ipc::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Turns a negative FFmpeg return code into an error.
pub(crate) fn check(code: i32, call: &'static str) -> Result<i32, RecordError> {
    if code < 0 {
        Err(RecordError::FfmpegCall { call, code })
    } else {
        Ok(code)
    }
}
