use super::{AudioSource, CHANNELS, SAMPLE_RATE};

/// The most a source may run ahead before its oldest samples are dropped: 200 ms.
const MAX_BACKLOG: usize = SAMPLE_RATE as usize / 5 * CHANNELS;

/// Sums several sources into one stream, padding any that fall behind with silence.
pub struct Mixer {
    sources: Vec<Box<dyn AudioSource>>,
    scratch: Vec<f32>,
}

impl Mixer {
    pub fn new(sources: Vec<Box<dyn AudioSource>>) -> Self {
        Self {
            sources,
            scratch: Vec::new(),
        }
    }

    pub fn mix(&mut self, output: &mut [f32]) {
        output.fill(0.0);
        self.scratch.resize(output.len(), 0.0);
        for source in &mut self.sources {
            source.discard_beyond(MAX_BACKLOG + output.len());
            let read = source.read(&mut self.scratch);
            for (mixed, sample) in output.iter_mut().zip(&self.scratch[..read]) {
                *mixed += sample;
            }
        }
        for sample in output.iter_mut() {
            *sample = sample.clamp(-1.0, 1.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(samples: &[f32]) -> Box<dyn AudioSource> {
        let (mut producer, consumer) = rtrb::RingBuffer::new(MAX_BACKLOG * 2);
        for &sample in samples {
            producer.push(sample).unwrap();
        }
        Box::new(consumer)
    }

    #[test]
    fn sums_and_pads_sources() {
        let mut mixer = Mixer::new(vec![source(&[0.25, 0.25, 0.5, 0.5]), source(&[0.5, 0.5])]);
        let mut output = [1.0; 4];
        mixer.mix(&mut output);
        assert_eq!(output, [0.75, 0.75, 0.5, 0.5]);
    }

    #[test]
    fn clips_loud_mixes() {
        let mut mixer = Mixer::new(vec![source(&[0.9, -0.9]), source(&[0.9, -0.9])]);
        let mut output = [0.0; 2];
        mixer.mix(&mut output);
        assert_eq!(output, [1.0, -1.0]);
    }
}
