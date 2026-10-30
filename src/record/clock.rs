use std::time::Duration;

use rustix::time::{ClockId, clock_gettime};

/// `CLOCK_MONOTONIC`, the clock the compositor stamps frames with.
pub fn monotonic_now() -> Duration {
    let now = clock_gettime(ClockId::Monotonic);
    Duration::new(now.tv_sec.unsigned_abs(), now.tv_nsec as u32)
}

/// Converts monotonic instants into microsecond timestamps from the start of a recording.
#[derive(Debug, Clone, Copy)]
pub struct RecordingClock {
    started_at: Duration,
}

impl RecordingClock {
    pub const MICROSECONDS: i32 = 1_000_000;

    pub fn start() -> Self {
        Self {
            started_at: monotonic_now(),
        }
    }

    pub fn microseconds_at(&self, instant: Duration) -> i64 {
        instant.saturating_sub(self.started_at).as_micros() as i64
    }

    pub fn elapsed(&self) -> Duration {
        monotonic_now().saturating_sub(self.started_at)
    }
}

/// Drops frames that arrive faster than the target frame rate.
#[derive(Debug)]
pub struct FramePacer {
    interval: i64,
    last: Option<i64>,
}

impl FramePacer {
    pub fn new(frames_per_second: u32) -> Self {
        Self {
            interval: i64::from(RecordingClock::MICROSECONDS) / i64::from(frames_per_second.max(1)),
            last: None,
        }
    }

    pub fn accept(&mut self, timestamp: i64) -> bool {
        let due = self
            .last
            .is_none_or(|last| timestamp - last >= self.interval * 9 / 10);
        if due {
            self.last = Some(timestamp);
        }
        due
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paces_to_frame_rate() {
        let mut pacer = FramePacer::new(30);
        assert!(pacer.accept(0));
        assert!(!pacer.accept(16_667));
        assert!(pacer.accept(33_333));
        assert!(pacer.accept(66_000));
    }
}
