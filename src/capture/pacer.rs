use std::time::Duration;

use rustix::time::{ClockId, clock_gettime};

/// `CLOCK_MONOTONIC`, the clock the compositor stamps frames with.
pub fn monotonic_now() -> Duration {
    let now = clock_gettime(ClockId::Monotonic);
    Duration::new(now.tv_sec.unsigned_abs(), now.tv_nsec as u32)
}

/// Schedules capture requests one interval apart on a fixed timeline, asking a little early so
/// the compositor's next repaint lands on time, and restarting the timeline after a pause.
#[derive(Debug, Clone, Copy, Default)]
pub struct FramePacer {
    interval: Duration,
    next_due: Option<Duration>,
}

impl FramePacer {
    pub fn set_interval(&mut self, interval: Duration) {
        self.interval = interval;
    }

    /// Moves the timeline on from a frame presented at `presented_at`.
    pub fn mark(&mut self, presented_at: Duration) {
        let restart = presented_at.saturating_sub(self.interval / 2);
        let anchor = self.next_due.map_or(presented_at, |due| due.max(restart));
        self.next_due = Some(anchor + self.interval);
    }

    /// How long after `now` the next request should go out; zero once it should.
    pub fn until_due(&self, now: Duration) -> Duration {
        self.next_due.map_or(Duration::ZERO, |due| {
            due.saturating_sub(self.interval / 5).saturating_sub(now)
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
    fn asks_a_fifth_of_an_interval_early() {
        let start = Duration::from_secs(5);
        let pacer = pacer_marked_at(start);
        assert_eq!(pacer.until_due(start), Duration::from_millis(80));
        assert_eq!(
            pacer.until_due(start + Duration::from_millis(79)),
            Duration::from_millis(1)
        );
        assert_eq!(
            pacer.until_due(start + Duration::from_millis(80)),
            Duration::ZERO
        );
    }

    #[test]
    fn keeps_the_timeline_when_frames_arrive_late() {
        let start = Duration::from_secs(5);
        let mut pacer = pacer_marked_at(start);
        pacer.mark(start + Duration::from_millis(107));
        assert_eq!(
            pacer.until_due(start + Duration::from_millis(107)),
            Duration::from_millis(73)
        );
    }

    #[test]
    fn restarts_the_timeline_after_a_pause() {
        let start = Duration::from_secs(5);
        let mut pacer = pacer_marked_at(start);
        pacer.mark(start + Duration::from_secs(1));
        assert_eq!(
            pacer.until_due(start + Duration::from_secs(1)),
            Duration::from_millis(30)
        );
    }

    #[test]
    fn zero_interval_never_waits() {
        let mut pacer = FramePacer::default();
        pacer.mark(Duration::from_secs(5));
        assert_eq!(pacer.until_due(Duration::from_secs(5)), Duration::ZERO);
    }
}
