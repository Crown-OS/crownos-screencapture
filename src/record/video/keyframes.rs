use ffmpeg_next::picture;

/// Forces a keyframe whenever `interval` has passed since the last one, so seeking and crash
/// recovery keep working when a quiet screen slows the frame rate down.
#[derive(Debug, Clone, Copy)]
pub struct KeyframeSchedule {
    interval: i64,
    last: Option<i64>,
}

impl KeyframeSchedule {
    pub const fn new(interval: i64) -> Self {
        Self {
            interval,
            last: None,
        }
    }

    /// The picture type to ask of the encoder for the frame at `timestamp`.
    pub fn picture_at(&mut self, timestamp: i64) -> picture::Type {
        match self.last {
            Some(last) if timestamp - last < self.interval => picture::Type::None,
            _ => {
                self.last = Some(timestamp);
                picture::Type::I
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forces_a_keyframe_every_interval() {
        let mut schedule = KeyframeSchedule::new(2_000_000);
        assert_eq!(schedule.picture_at(0), picture::Type::I);
        assert_eq!(schedule.picture_at(1_999_999), picture::Type::None);
        assert_eq!(schedule.picture_at(2_000_000), picture::Type::I);
        assert_eq!(schedule.picture_at(3_500_000), picture::Type::None);
        assert_eq!(schedule.picture_at(9_000_000), picture::Type::I);
    }
}
