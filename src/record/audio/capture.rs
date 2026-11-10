use std::io::Cursor;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use pipewire as pw;
use pw::properties::{PropertiesBox, properties};
use pw::spa::param::ParamType;
use pw::spa::param::audio::{AudioFormat, AudioInfoRaw};
use pw::spa::pod::serialize::PodSerializer;
use pw::spa::pod::{Object, Pod, Value};
use pw::spa::utils::{Direction, Fraction, SpaTypes};
use pw::stream::{Stream, StreamBox, StreamFlags};

use super::source::{SourceReader, SourceWriter, timed_source};
use super::{CHANNELS, NANOSECONDS, SAMPLE_RATE, monotonic_now, nanoseconds};
use crate::record::RecordError;

/// One second of frames per source.
const RING_FRAMES: usize = SAMPLE_RATE as usize;
/// Captured buffers a source may queue, enough for a second at a 64-frame quantum.
const RING_RUNS: usize = 1024;
const FRAME_BYTES: usize = CHANNELS * size_of::<f32>();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureNode {
    /// The default input device.
    Microphone,
    /// What the default output device is playing.
    SystemAudio,
}

impl CaptureNode {
    fn stream_name(self) -> &'static str {
        match self {
            Self::Microphone => "crownos-screenshot-microphone",
            Self::SystemAudio => "crownos-screenshot-system-audio",
        }
    }

    fn properties(self) -> PropertiesBox {
        let mut properties = properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Production",
            *pw::keys::NODE_NAME => self.stream_name(),
        };
        if self == Self::SystemAudio {
            properties.insert(*pw::keys::STREAM_CAPTURE_SINK, "true");
        }
        properties
    }
}

/// PipeWire capture streams running on their own loop thread, each filling a lock-free ring.
pub struct PipeWireCapture {
    quit: pw::channel::Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl PipeWireCapture {
    pub fn start(nodes: &[CaptureNode]) -> Result<(Self, Vec<SourceReader>), RecordError> {
        let (writers, sources) = nodes
            .iter()
            .map(|_| timed_source(RING_FRAMES, RING_RUNS))
            .unzip();
        let (quit, quit_receiver) = pw::channel::channel();
        let (ready_sender, ready) = mpsc::sync_channel(1);
        let nodes = nodes.to_vec();
        let thread = thread::Builder::new()
            .name("pipewire".into())
            .spawn(move || {
                if let Err(error) = run_loop(&nodes, writers, quit_receiver, &ready_sender) {
                    let _ = ready_sender.send(Err(error));
                }
            })?;
        let capture = Self {
            quit,
            thread: Some(thread),
        };
        match ready.recv() {
            Ok(Ok(())) => Ok((capture, sources)),
            Ok(Err(error)) => Err(error.into()),
            Err(_) => Err(RecordError::PipeWire(pw::Error::CreationFailed)),
        }
    }
}

impl Drop for PipeWireCapture {
    fn drop(&mut self) {
        let _ = self.quit.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run_loop(
    nodes: &[CaptureNode],
    writers: Vec<SourceWriter>,
    quit: pw::channel::Receiver<()>,
    ready: &mpsc::SyncSender<Result<(), pw::Error>>,
) -> Result<(), pw::Error> {
    pw::init();
    let main_loop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&main_loop, None)?;
    let core = context.connect_rc(None)?;
    let format = format_pod()?;
    let mut streams = Vec::with_capacity(nodes.len());
    let mut listeners = Vec::with_capacity(nodes.len());
    for (node, writer) in nodes.iter().zip(writers) {
        let stream = StreamBox::new(&core, node.stream_name(), node.properties())?;
        let listener = stream
            .add_local_listener_with_user_data(writer)
            .process(|stream, writer| {
                let Some(mut buffer) = stream.dequeue_buffer() else {
                    return;
                };
                let Some(data) = buffer.datas_mut().first_mut() else {
                    return;
                };
                let (offset, size) = (data.chunk().offset() as usize, data.chunk().size() as usize);
                let Some(bytes) = data.data().and_then(|bytes| bytes.get(offset..)) else {
                    return;
                };
                let bytes = &bytes[..size.min(bytes.len())];
                writer.push(
                    first_frame_captured_at(stream, bytes.len() / FRAME_BYTES),
                    bytes,
                );
            })
            .register()?;
        let mut parameters = [Pod::from_bytes(&format).ok_or(pw::Error::CreationFailed)?];
        stream.connect(
            Direction::Input,
            None,
            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS,
            &mut parameters,
        )?;
        streams.push(stream);
        listeners.push(listener);
    }
    let _quit = quit.attach(main_loop.loop_(), {
        let main_loop = main_loop.clone();
        move |()| main_loop.quit()
    });
    let _ = ready.send(Ok(()));
    main_loop.run();
    Ok(())
}

/// When the first of `frames` frames dequeued this cycle was captured, on `CLOCK_MONOTONIC`: by
/// the graph clock when the stream reports it, otherwise by when the buffer arrived.
fn first_frame_captured_at(stream: &Stream, frames: usize) -> i64 {
    stream
        .time()
        .ok()
        .and_then(|time| captured_at(time.now(), time.delay(), time.rate()))
        .unwrap_or_else(|| {
            nanoseconds(monotonic_now()) - frames as i64 * NANOSECONDS / i64::from(SAMPLE_RATE)
        })
}

/// The capture time of the first frame delivered in the graph cycle at `now_ns`, whose `delay`
/// ticks of `rate` include the quantum the device spent capturing it.
fn captured_at(now_ns: i64, delay: i64, rate: Fraction) -> Option<i64> {
    if now_ns <= 0 || rate.num == 0 || rate.denom == 0 {
        return None;
    }
    let delay_ns = i128::from(delay.max(0)) * i128::from(NANOSECONDS) * i128::from(rate.num)
        / i128::from(rate.denom);
    Some(now_ns - delay_ns as i64)
}

/// Interleaved 32-bit float stereo at 48 kHz, which PipeWire converts any device to.
fn format_pod() -> Result<Vec<u8>, pw::Error> {
    let mut info = AudioInfoRaw::new();
    info.set_format(AudioFormat::F32LE);
    info.set_rate(SAMPLE_RATE);
    info.set_channels(CHANNELS as u32);
    let mut position = [0; pw::spa::sys::SPA_AUDIO_MAX_CHANNELS as usize];
    position[0] = pw::spa::sys::SPA_AUDIO_CHANNEL_FL;
    position[1] = pw::spa::sys::SPA_AUDIO_CHANNEL_FR;
    info.set_position(position);
    let object = Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties: info.into(),
    };
    PodSerializer::serialize(Cursor::new(Vec::new()), &Value::Object(object))
        .map(|(cursor, _)| cursor.into_inner())
        .map_err(|_| pw::Error::CreationFailed)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PER_SAMPLE: Fraction = Fraction {
        num: 1,
        denom: SAMPLE_RATE,
    };

    #[test]
    fn subtracts_graph_delay_from_cycle_time() {
        assert_eq!(
            captured_at(NANOSECONDS, 480, PER_SAMPLE),
            Some(NANOSECONDS - 10_000_000)
        );
    }

    #[test]
    fn ignores_negative_delay() {
        assert_eq!(
            captured_at(NANOSECONDS, -480, PER_SAMPLE),
            Some(NANOSECONDS)
        );
    }

    #[test]
    fn reports_nothing_without_a_graph_clock() {
        let unset = Fraction { num: 0, denom: 0 };
        assert_eq!(captured_at(NANOSECONDS, 480, unset), None);
        assert_eq!(captured_at(0, 480, PER_SAMPLE), None);
    }
}
