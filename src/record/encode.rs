//! The encode thread: it filters, encodes and muxes the frames the capture thread hands it, and
//! mixes sound in between.

use std::io;
use std::os::fd::{AsFd, BorrowedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

use ffmpeg_next::picture;

use super::RecordError;
use super::audio::AudioTrack;
use super::clock::RecordingClock;
use super::mux::Muxer;
use super::video::VideoEncoder;
use crate::capture::CapturedFrame;
use crate::util::wakeup::Wakeup;

/// How often sound is mixed and encoded, well inside its mix delay.
pub const AUDIO_CADENCE: Duration = Duration::from_millis(20);

/// A captured frame on its way to the encoder, stamped and typed by the capture thread.
pub struct EncodeJob {
    pub frame: CapturedFrame,
    pub timestamp: i64,
    pub picture: picture::Type,
}

/// Wakes the capture thread whenever the encoder drops a frame, so a held slot may be free, and
/// once it stops taking frames.
#[derive(Debug)]
pub struct EncoderEvents {
    wakeup: Wakeup,
    ended: AtomicBool,
}

impl EncoderEvents {
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            wakeup: Wakeup::new()?,
            ended: AtomicBool::new(false),
        })
    }

    /// Whether the encode thread has stopped taking frames. Clears the wake-up first, so an
    /// event arriving after the check wakes the next poll.
    pub fn has_ended(&self) -> bool {
        self.wakeup.clear();
        self.ended.load(Ordering::Acquire)
    }

    fn released(&self) {
        self.wakeup.wake();
    }

    fn end(&self) {
        self.ended.store(true, Ordering::Release);
        self.wakeup.wake();
    }
}

impl AsFd for EncoderEvents {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.wakeup.as_fd()
    }
}

/// What the encode thread owns while the recording runs.
pub struct Encoding {
    pub encoder: Box<dyn VideoEncoder>,
    pub audio: Option<AudioTrack>,
    pub muxer: Muxer,
    pub clock: RecordingClock,
}

impl Encoding {
    /// Encodes `jobs` until the capture thread hangs up, then flushes the encoders and writes the
    /// trailer, also after an error, which it returns first.
    pub fn run(
        mut self,
        jobs: Receiver<EncodeJob>,
        events: &EncoderEvents,
    ) -> Result<(), RecordError> {
        let encoded = self.encode_all(&jobs, events);
        events.end();
        drop(jobs);
        let finished = self.finish();
        encoded.and(finished)
    }

    fn encode_all(
        &mut self,
        jobs: &Receiver<EncodeJob>,
        events: &EncoderEvents,
    ) -> Result<(), RecordError> {
        loop {
            let job = match self.audio {
                Some(_) => jobs.recv_timeout(AUDIO_CADENCE),
                None => jobs.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match job {
                Ok(job) => {
                    self.encode(job)?;
                    events.released();
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            }
            if let Some(track) = &mut self.audio {
                track.pump(self.clock.elapsed(), &mut self.muxer)?;
            }
        }
    }

    /// Encodes `job` and drops its frame, releasing the slot.
    fn encode(&mut self, job: EncodeJob) -> Result<(), RecordError> {
        self.encoder
            .encode(&job.frame, job.timestamp, job.picture, &mut self.muxer)
    }

    fn finish(mut self) -> Result<(), RecordError> {
        let video = self.encoder.finish(&mut self.muxer);
        let audio = self
            .audio
            .as_mut()
            .map_or(Ok(()), |track| track.finish(&mut self.muxer));
        let trailer = self.muxer.finish();
        video.and(audio).and(trailer)
    }
}
