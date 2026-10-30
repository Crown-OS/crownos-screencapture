//! Screen and voice recording: capture → encode → mux, on one thread with lock-free audio.

pub mod audio;
mod clock;
mod control;
mod error;
mod mux;
pub mod video;

use std::path::{Path, PathBuf};
use std::time::Duration;

use ffmpeg_next::{Packet, codec};

use self::audio::AudioTrack;
pub use self::audio::CaptureNode;
use self::clock::{FramePacer, RecordingClock};
pub use self::control::StopSignal;
pub use self::error::RecordError;
pub use self::mux::Container;
use self::mux::{Muxer, PendingPackets};
use self::video::{Crop, SoftwareBackend, VaapiBackend, VideoBackend, VideoEncoder, VideoSettings};
use crate::capture::{CaptureError, CaptureTarget, CursorMode, FrameStream, Region, ScreenCapture};

/// How long one pass of the recording loop waits for a frame before servicing audio.
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(5);

/// An encoder whose stream settings the muxer copies.
pub trait EncodedStream {
    fn context(&self) -> &codec::Context;
}

/// Where encoded packets go.
pub trait PacketSink {
    fn video(&mut self, packet: Packet) -> Result<(), RecordError>;
    fn audio(&mut self, packet: Packet) -> Result<(), RecordError>;
}

/// Everything about a screen recording that is known before it starts.
#[derive(Debug, Clone)]
pub struct ScreenRecording {
    pub target: CaptureTarget,
    pub region: Option<OutputRegion>,
    pub cursor: CursorMode,
    pub video: VideoSettings,
    pub audio: Vec<CaptureNode>,
    pub container: Container,
    pub path: PathBuf,
}

/// A region of an output, with that output's logical size to scale it to buffer pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputRegion {
    pub region: Region,
    pub logical_width: u32,
    pub logical_height: u32,
}

enum Attempt {
    Recorded,
    Unsupported(RecordError),
}

/// Records until `stop` is raised, on the GPU when possible and on the CPU otherwise.
/// `started` runs once the first frame is encoded.
pub fn record_screen(
    capture: &mut impl ScreenCapture,
    recording: &ScreenRecording,
    stop: &StopSignal,
    started: &mut dyn FnMut(),
) -> Result<(), RecordError> {
    ffmpeg_next::init()?;
    let backends: [&dyn VideoBackend; 2] = [&VaapiBackend, &SoftwareBackend];
    let mut last_failure = None;
    for backend in backends {
        match record_with(capture, backend, recording, stop, started)? {
            Attempt::Recorded => return Ok(()),
            Attempt::Unsupported(error) => {
                tracing::warn!("{} encoding unavailable: {error}", backend.name());
                last_failure = Some(error);
            }
        }
    }
    Err(last_failure.unwrap_or(RecordError::NoEncoder("video")))
}

fn record_with(
    capture: &mut impl ScreenCapture,
    backend: &dyn VideoBackend,
    recording: &ScreenRecording,
    stop: &StopSignal,
    started: &mut dyn FnMut(),
) -> Result<Attempt, RecordError> {
    let mut stream =
        match capture.stream(&recording.target, recording.cursor, backend.buffer_kind()) {
            Ok(stream) => stream,
            Err(
                error @ (CaptureError::Unsupported(_)
                | CaptureError::NoBufferFormat
                | CaptureError::NoRenderNode(_)),
            ) => {
                return Ok(Attempt::Unsupported(error.into()));
            }
            Err(error) => return Err(error.into()),
        };
    let render_node = stream.render_node().map(Path::to_path_buf);
    let settings = VideoSettings {
        crop: crop(recording, stream.buffer_size()),
        ..recording.video
    };
    let clock = RecordingClock::start();
    let mut pending = PendingPackets::default();
    let mut encoder = loop {
        if clock.elapsed() > FIRST_FRAME_TIMEOUT {
            return Err(CaptureError::Failed("no frame arrived").into());
        }
        let Some(frame) = stream.next_frame(POLL_INTERVAL)? else {
            continue;
        };
        let opened = backend
            .open(&frame, render_node.as_deref(), &settings)
            .and_then(|mut encoder| {
                encoder.encode(
                    &frame,
                    clock.microseconds_at(frame.presented_at),
                    &mut pending,
                )?;
                Ok(encoder)
            });
        match opened {
            Ok(encoder) => break encoder,
            Err(error) => return Ok(Attempt::Unsupported(error)),
        }
    };
    let audio_offset = clock.elapsed();
    let mut audio = (!recording.audio.is_empty())
        .then(|| AudioTrack::start(&recording.audio))
        .transpose()?;
    let mut muxer = Muxer::create(
        &recording.path,
        recording.container,
        Some(encoder.as_ref() as &dyn EncodedStream),
        audio
            .as_ref()
            .map(|track| track.encoder() as &dyn EncodedStream),
    )?;
    pending.replay(&mut muxer)?;
    started();
    let mut pacer = FramePacer::new(recording.video.frames_per_second);
    while !stop.is_raised() {
        match stream.next_frame(POLL_INTERVAL) {
            Ok(Some(frame)) => {
                let timestamp = clock.microseconds_at(frame.presented_at);
                if pacer.accept(timestamp) {
                    encoder.encode(&frame, timestamp, &mut muxer)?;
                }
            }
            Ok(None) => {}
            Err(CaptureError::Stopped) => break,
            Err(error) => return Err(error.into()),
        }
        if let Some(track) = &mut audio {
            track.pump(
                clock.elapsed().saturating_sub(audio_offset),
                &mut muxer,
                audio_offset,
            )?;
        }
    }
    finish(encoder.as_mut(), audio.as_mut(), muxer)?;
    Ok(Attempt::Recorded)
}

fn finish(
    encoder: &mut dyn VideoEncoder,
    audio: Option<&mut AudioTrack>,
    mut muxer: Muxer,
) -> Result<(), RecordError> {
    encoder.finish(&mut muxer)?;
    if let Some(track) = audio {
        track.finish(&mut muxer)?;
    }
    muxer.finish()
}

fn crop(recording: &ScreenRecording, (buffer_width, buffer_height): (u32, u32)) -> Crop {
    match recording.region {
        Some(OutputRegion {
            region,
            logical_width,
            logical_height,
        }) => Crop::Region(region.to_pixels(
            logical_width,
            logical_height,
            buffer_width,
            buffer_height,
        )),
        None => Crop::Full,
    }
}

/// Records sound only, into an Ogg Opus file, until `stop` is raised.
pub fn record_voice(
    path: &Path,
    nodes: &[CaptureNode],
    stop: &StopSignal,
    started: &mut dyn FnMut(),
) -> Result<(), RecordError> {
    ffmpeg_next::init()?;
    let mut track = AudioTrack::start(nodes)?;
    let mut muxer = Muxer::create(path, Container::OggOpus, None, Some(track.encoder()))?;
    let clock = RecordingClock::start();
    started();
    while !stop.is_raised() {
        std::thread::sleep(POLL_INTERVAL * 2);
        track.pump(clock.elapsed(), &mut muxer, Duration::ZERO)?;
    }
    track.finish(&mut muxer)?;
    muxer.finish()
}
