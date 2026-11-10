use super::source::SourceReader;

/// Sums several sources on one timeline, so a silent or suspended source never stalls the others.
pub struct Mixer {
    sources: Vec<SourceReader>,
}

impl Mixer {
    pub fn new(sources: Vec<SourceReader>) -> Self {
        Self { sources }
    }

    /// Mixes the window that starts `window_start` frames after `origin_ns` into `output`.
    pub fn mix(&mut self, origin_ns: i64, window_start: i64, output: &mut [f32]) {
        output.fill(0.0);
        for source in &mut self.sources {
            source.add_window(origin_ns, window_start, output);
        }
        for sample in output.iter_mut() {
            *sample = sample.clamp(-1.0, 1.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::audio::CHANNELS;
    use crate::record::audio::source::tests::queue;
    use crate::record::audio::source::timed_source;

    #[test]
    fn sums_sources_on_one_timeline() {
        let (mut early, early_reader) = timed_source(8, 2);
        let (mut late, late_reader) = timed_source(8, 2);
        queue(&mut early, 0, &[0.25; 4]);
        queue(&mut late, 2, &[0.5; 2]);
        let mut mixer = Mixer::new(vec![early_reader, late_reader]);
        let mut output = [1.0; 4 * CHANNELS];
        mixer.mix(0, 0, &mut output);
        assert_eq!(output, [0.25, 0.25, 0.25, 0.25, 0.75, 0.75, 0.75, 0.75]);
    }

    #[test]
    fn clips_loud_mixes() {
        let (mut first, first_reader) = timed_source(2, 1);
        let (mut second, second_reader) = timed_source(2, 1);
        queue(&mut first, 0, &[0.9]);
        queue(&mut second, 0, &[0.9]);
        let mut mixer = Mixer::new(vec![first_reader, second_reader]);
        let mut output = [0.0; CHANNELS];
        mixer.mix(0, 0, &mut output);
        assert_eq!(output, [1.0; CHANNELS]);
    }
}
