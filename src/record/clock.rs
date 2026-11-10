use std::time::Duration;

pub use crate::capture::monotonic_now;

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
