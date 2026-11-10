use std::time::Duration;

use rustix::time::{ClockId, clock_gettime};

/// `CLOCK_MONOTONIC`, the clock the compositor stamps frames with.
pub fn monotonic_now() -> Duration {
    let now = clock_gettime(ClockId::Monotonic);
    Duration::new(now.tv_sec.unsigned_abs(), now.tv_nsec as u32)
}

/// Spaces frames at least nine tenths of an interval apart, so jitter never costs a whole frame.
#[derive(Debug, Clone, Copy, Default)]
pub struct FramePacer {
    interval: Duration,
    last: Option<Duration>,
}

impl FramePacer {
    pub fn set_interval(&mut self, interval: Duration) {
        self.interval = interval;
    }

    /// Remembers when the last frame was presented.
    pub fn mark(&mut self, presented_at: Duration) {
        self.last = Some(presented_at);
    }

    /// How long after `now` the next frame is due; zero once it is.
    pub fn until_due(&self, now: Duration) -> Duration {
        self.last.map_or(Duration::ZERO, |last| {
            (last + self.interval * 9 / 10).saturating_sub(now)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INTERVAL: Duration = Duration::from_millis(100);

    fn pacer_marked_at(presented_at: Duration) -> FramePacer {
        let mut pacer = FramePacer::default();
        pacer.set_interval(INTERVAL);
        pacer.mark(presented_at);
        pacer
    }

    #[test]
    fn first_frame_is_due_immediately() {
        let mut pacer = FramePacer::default();
        pacer.set_interval(INTERVAL);
        assert_eq!(pacer.until_due(Duration::ZERO), Duration::ZERO);
    }

    #[test]
    fn waits_for_nine_tenths_of_the_interval() {
        let start = Duration::from_secs(5);
        let pacer = pacer_marked_at(start);
        assert_eq!(
            pacer.until_due(start + Duration::from_millis(89)),
            Duration::from_millis(1)
        );
        assert_eq!(
            pacer.until_due(start + Duration::from_millis(90)),
            Duration::ZERO
        );
        assert_eq!(
            pacer.until_due(start + Duration::from_millis(250)),
            Duration::ZERO
        );
    }

    #[test]
    fn returns_the_remaining_wait() {
        let start = Duration::from_secs(5);
        assert_eq!(
            pacer_marked_at(start).until_due(start + Duration::from_millis(30)),
            Duration::from_millis(60)
        );
    }

    #[test]
    fn zero_interval_never_waits() {
        let mut pacer = FramePacer::default();
        pacer.mark(Duration::from_secs(5));
        assert_eq!(pacer.until_due(Duration::from_secs(5)), Duration::ZERO);
    }
}
