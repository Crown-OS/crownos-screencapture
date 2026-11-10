use rtrb::{Consumer, Producer, RingBuffer};

use super::{CHANNELS, SAMPLE_RATE, frames_between};

const FRAME_BYTES: usize = CHANNELS * size_of::<f32>();
/// Timing error tolerated before a source is nudged back into place: 1 ms.
const DEADBAND: i64 = SAMPLE_RATE as i64 / 1000;
/// Timing error beyond which a source is re-placed at once instead of nudged: 10 ms.
const RESYNC: i64 = SAMPLE_RATE as i64 / 100;
/// Frames dropped or repeated per mixed window while nudging, about 0.2 % of a 20 ms window.
const MAX_NUDGE: i64 = 2;

/// A run of frames whose first frame was captured at `captured_at_ns` on `CLOCK_MONOTONIC`.
#[derive(Debug, Clone, Copy)]
struct Marker {
    captured_at_ns: i64,
    frames: usize,
}

/// Creates the lock-free rings one source is captured through, holding up to `frames` frames
/// in at most `runs` buffers.
pub fn timed_source(frames: usize, runs: usize) -> (SourceWriter, SourceReader) {
    let (samples, sample_reader) = RingBuffer::new(frames * CHANNELS);
    let (markers, marker_reader) = RingBuffer::new(runs);
    (
        SourceWriter { samples, markers },
        SourceReader {
            samples: sample_reader,
            markers: marker_reader,
            head: 0,
            remaining: 0,
            synced: false,
            last_frame: [0.0; CHANNELS],
        },
    )
}

/// The capture end of a source; real-time safe.
pub struct SourceWriter {
    samples: Producer<f32>,
    markers: Producer<Marker>,
}

impl SourceWriter {
    /// Queues interleaved little-endian float frames whose first frame was captured at
    /// `captured_at_ns`. A buffer is cut short when the ring is nearly full and dropped when no
    /// marker fits, so every queued sample has a timestamp.
    pub fn push(&mut self, captured_at_ns: i64, bytes: &[u8]) {
        let frames = (bytes.len() / FRAME_BYTES).min(self.samples.slots() / CHANNELS);
        if frames == 0 || self.markers.is_full() {
            return;
        }
        let Ok(chunk) = self.samples.write_chunk_uninit(frames * CHANNELS) else {
            return;
        };
        let (samples, _) = bytes.as_chunks();
        chunk.fill_from_iter(samples.iter().copied().map(f32::from_le_bytes));
        let _ = self.markers.push(Marker {
            captured_at_ns,
            frames,
        });
    }
}

/// The mixing end of a source, which places frames on the recording timeline by capture time.
pub struct SourceReader {
    samples: Consumer<f32>,
    markers: Consumer<Marker>,
    head: i64,
    remaining: usize,
    synced: bool,
    last_frame: [f32; CHANNELS],
}

impl SourceReader {
    /// Adds the frames captured during the window starting `window_start` frames after
    /// `origin_ns` into `output`, leaving silence where none were captured.
    pub fn add_window(&mut self, origin_ns: i64, window_start: i64, output: &mut [f32]) {
        let end = window_start + (output.len() / CHANNELS) as i64;
        let mut position = window_start;
        let mut nudge_budget = MAX_NUDGE;
        while position < end {
            let Some(head) = self.head(origin_ns) else {
                self.synced = false;
                return;
            };
            let lag = head - position;
            if lag != 0 && (!self.synced || lag.abs() > RESYNC) {
                self.synced = false;
                if lag > 0 {
                    position = head.min(end);
                } else {
                    self.skip(lag.unsigned_abs() as usize);
                }
                continue;
            }
            self.synced = true;
            let nudge = (lag.abs() - DEADBAND).clamp(0, nudge_budget.min(end - position));
            nudge_budget -= nudge;
            let offset = (position - window_start) as usize * CHANNELS;
            if lag > 0 {
                self.repeat_last_frame(&mut output[offset..], nudge as usize);
                position += nudge;
            } else {
                self.skip(nudge as usize);
            }
            let offset = (position - window_start) as usize * CHANNELS;
            position += self.copy_into(&mut output[offset..], (end - position) as usize) as i64;
        }
    }

    fn head(&mut self, origin_ns: i64) -> Option<i64> {
        while self.remaining == 0 {
            let marker = self.markers.pop().ok()?;
            self.head = frames_between(origin_ns, marker.captured_at_ns);
            self.remaining = marker.frames;
        }
        Some(self.head)
    }

    fn skip(&mut self, frames: usize) {
        let frames = frames.min(self.remaining);
        match self.samples.read_chunk(frames * CHANNELS) {
            Ok(chunk) => {
                chunk.commit_all();
                self.advance(frames);
            }
            Err(_) => self.remaining = 0,
        }
    }

    fn copy_into(&mut self, output: &mut [f32], frames: usize) -> usize {
        let frames = frames.min(self.remaining);
        let Ok(chunk) = self.samples.read_chunk(frames * CHANNELS) else {
            self.remaining = 0;
            return 0;
        };
        let (first, second) = chunk.as_slices();
        let samples = || first.iter().chain(second);
        for (mixed, sample) in output.iter_mut().zip(samples()) {
            *mixed += sample;
        }
        for (kept, sample) in self.last_frame.iter_mut().rev().zip(samples().rev()) {
            *kept = *sample;
        }
        chunk.commit_all();
        self.advance(frames);
        frames
    }

    fn repeat_last_frame(&self, output: &mut [f32], frames: usize) {
        for frame in output.chunks_exact_mut(CHANNELS).take(frames) {
            for (mixed, sample) in frame.iter_mut().zip(self.last_frame) {
                *mixed += sample;
            }
        }
    }

    fn advance(&mut self, frames: usize) {
        self.head += frames as i64;
        self.remaining -= frames;
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::record::audio::NANOSECONDS;

    const WINDOW: usize = 960;

    pub fn queue(writer: &mut SourceWriter, at_frame: i64, frames: &[f32]) {
        queue_at(
            writer,
            at_frame * NANOSECONDS / i64::from(SAMPLE_RATE),
            frames,
        );
    }

    fn queue_at(writer: &mut SourceWriter, captured_at_ns: i64, frames: &[f32]) {
        let bytes: Vec<u8> = frames
            .iter()
            .flat_map(|&sample| [sample; CHANNELS])
            .flat_map(f32::to_le_bytes)
            .collect();
        writer.push(captured_at_ns, &bytes);
    }

    fn window(reader: &mut SourceReader, start: i64, frames: usize) -> Vec<f32> {
        let mut output = vec![0.0; frames * CHANNELS];
        reader.add_window(0, start, &mut output);
        output.into_iter().step_by(CHANNELS).collect()
    }

    fn synced_after_one_window() -> (SourceWriter, SourceReader) {
        let (mut writer, mut reader) = timed_source(8 * WINDOW, 16);
        queue(&mut writer, 0, &[0.1; WINDOW]);
        assert_eq!(window(&mut reader, 0, WINDOW), [0.1; WINDOW]);
        (writer, reader)
    }

    #[test]
    fn places_frames_by_capture_time() {
        let (mut writer, mut reader) = timed_source(16, 4);
        queue(&mut writer, 3, &[0.5, 0.5]);
        assert_eq!(window(&mut reader, 0, 6), [0.0, 0.0, 0.0, 0.5, 0.5, 0.0]);
    }

    #[test]
    fn drops_frames_captured_before_the_window() {
        let (mut writer, mut reader) = timed_source(16, 4);
        queue(&mut writer, -2, &[0.1, 0.2, 0.3, 0.4]);
        assert_eq!(window(&mut reader, 0, 2), [0.3, 0.4]);
    }

    #[test]
    fn leaves_silence_without_frames() {
        let (_writer, mut reader) = timed_source(16, 4);
        assert_eq!(window(&mut reader, 0, 4), [0.0; 4]);
    }

    #[test]
    fn repeats_frames_while_a_source_runs_slow() {
        let (mut writer, mut reader) = synced_after_one_window();
        queue(&mut writer, WINDOW as i64 + 100, &[0.2; 2 * WINDOW]);
        let mixed = window(&mut reader, WINDOW as i64, WINDOW);
        assert_eq!(mixed[..3], [0.1, 0.1, 0.2]);
        assert_eq!(reader.head - 2 * WINDOW as i64, 100 - MAX_NUDGE);
    }

    #[test]
    fn drops_frames_while_a_source_runs_fast() {
        let (mut writer, mut reader) = synced_after_one_window();
        let ramp: Vec<f32> = (0..2 * WINDOW)
            .map(|frame| frame as f32 / 10_000.0)
            .collect();
        queue(&mut writer, WINDOW as i64 - 100, &ramp);
        let mixed = window(&mut reader, WINDOW as i64, WINDOW);
        assert_eq!(mixed[0], ramp[MAX_NUDGE as usize]);
        assert_eq!(reader.head - 2 * WINDOW as i64, MAX_NUDGE - 100);
    }

    #[test]
    fn ignores_jitter_within_the_deadband() {
        let (mut writer, mut reader) = synced_after_one_window();
        queue(&mut writer, WINDOW as i64 + DEADBAND, &[0.2; WINDOW]);
        assert_eq!(window(&mut reader, WINDOW as i64, WINDOW), [0.2; WINDOW]);
    }

    #[test]
    fn re_places_a_source_that_jumps() {
        let (mut writer, mut reader) = synced_after_one_window();
        let jump = RESYNC as usize + 20;
        queue(&mut writer, (WINDOW + jump) as i64, &[0.2; WINDOW]);
        let mixed = window(&mut reader, WINDOW as i64, WINDOW);
        assert!(mixed[..jump].iter().all(|&sample| sample == 0.0));
        assert!(mixed[jump..].iter().all(|&sample| sample == 0.2));
    }

    fn follow_device_clock(speed: f64) {
        const QUANTUM: usize = 1024;
        let device_rate = f64::from(SAMPLE_RATE) * speed;
        let (mut writer, mut reader) = timed_source(8 * WINDOW, 64);
        let captured_at_ns = |buffer: usize| ((buffer * QUANTUM) as f64 / device_rate * 1e9) as i64;
        let mut buffers = 0;
        for start in (0..3_000).map(|index| index * WINDOW as i64) {
            while frames_between(0, captured_at_ns(buffers)) < start + 4 * WINDOW as i64 {
                queue_at(&mut writer, captured_at_ns(buffers), &[0.5; QUANTUM]);
                buffers += 1;
            }
            assert_eq!(window(&mut reader, start, WINDOW), [0.5; WINDOW]);
            assert!((reader.head - start - WINDOW as i64).abs() <= DEADBAND + MAX_NUDGE);
        }
    }

    #[test]
    fn follows_a_fast_device_clock_without_gaps() {
        follow_device_clock(1.0005);
    }

    #[test]
    fn follows_a_slow_device_clock_without_gaps() {
        follow_device_clock(0.9995);
    }

    #[test]
    fn re_places_a_source_that_jumps_past_the_window() {
        let (mut writer, mut reader) = synced_after_one_window();
        queue(&mut writer, 2 * WINDOW as i64 + 100, &[0.2; WINDOW]);
        assert_eq!(window(&mut reader, WINDOW as i64, WINDOW), [0.0; WINDOW]);
        let mixed = window(&mut reader, 2 * WINDOW as i64, WINDOW);
        assert!(mixed[..100].iter().all(|&sample| sample == 0.0));
        assert!(mixed[100..].iter().all(|&sample| sample == 0.2));
    }

    #[test]
    fn keeps_every_queued_frame_timestamped() {
        let (mut writer, mut reader) = timed_source(4, 1);
        queue(&mut writer, 0, &[0.1; 6]);
        queue(&mut writer, 4, &[0.2; 2]);
        assert_eq!(window(&mut reader, 0, 6), [0.1, 0.1, 0.1, 0.1, 0.0, 0.0]);
    }
}
