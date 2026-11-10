use std::os::fd::AsFd;
use std::sync::mpsc::SyncSender;

use super::clock::RecordingClock;
use super::encode::{EncodeJob, EncoderEvents};
use super::video::{Crop, KeyframeSchedule};
use super::{RecordError, StopSignal};
use crate::capture::{CaptureError, FrameStream};

/// The capture thread's side of a running recording: it hands the encode thread every frame
/// whose damage reaches the crop.
pub struct FrameFeed<'run> {
    pub stop: &'run StopSignal,
    pub events: &'run EncoderEvents,
    pub crop: Crop,
    pub clock: RecordingClock,
    pub keyframes: KeyframeSchedule,
}

impl FrameFeed<'_> {
    /// Hands `jobs` every frame whose damage reaches the crop until the recording is stopped, the
    /// source goes away or the encoder ends, then hangs up.
    pub fn run(
        mut self,
        stream: &mut impl FrameStream,
        jobs: SyncSender<EncodeJob>,
    ) -> Result<(), RecordError> {
        let wakers = [self.stop.as_fd(), self.events.as_fd()];
        while !self.stop.is_raised() && !self.events.has_ended() {
            let frame = match stream.next_frame(&wakers, None) {
                Ok(Some(frame)) => frame,
                Ok(None) => continue,
                Err(CaptureError::Stopped) => break,
                Err(error) => return Err(error.into()),
            };
            if !self.crop.is_changed_by(&frame.damage, frame.pixels.size()) {
                continue;
            }
            let timestamp = self.clock.microseconds_at(frame.presented_at);
            let picture = self.keyframes.picture_at(timestamp);
            let job = EncodeJob {
                frame,
                timestamp,
                picture,
            };
            if jobs.send(job).is_err() {
                break;
            }
        }
        Ok(())
    }
}
