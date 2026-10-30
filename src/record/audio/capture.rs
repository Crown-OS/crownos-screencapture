use std::io::Cursor;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use pipewire as pw;
use pw::properties::{PropertiesBox, properties};
use pw::spa::param::ParamType;
use pw::spa::param::audio::{AudioFormat, AudioInfoRaw};
use pw::spa::pod::serialize::PodSerializer;
use pw::spa::pod::{Object, Pod, Value};
use pw::spa::utils::{Direction, SpaTypes};
use pw::stream::{StreamBox, StreamFlags};
use rtrb::{Producer, RingBuffer};

use super::{AudioSource, CHANNELS, SAMPLE_RATE};
use crate::record::RecordError;

/// One second of interleaved stereo per source.
const RING_CAPACITY: usize = SAMPLE_RATE as usize * CHANNELS;

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
    pub fn start(nodes: &[CaptureNode]) -> Result<(Self, Vec<Box<dyn AudioSource>>), RecordError> {
        let (producers, sources): (Vec<_>, Vec<Box<dyn AudioSource>>) = nodes
            .iter()
            .map(|_| {
                let (producer, consumer) = RingBuffer::new(RING_CAPACITY);
                (producer, Box::new(consumer) as Box<dyn AudioSource>)
            })
            .unzip();
        let (quit, quit_receiver) = pw::channel::channel();
        let (ready_sender, ready) = mpsc::sync_channel(1);
        let nodes = nodes.to_vec();
        let thread = thread::Builder::new()
            .name("pipewire".into())
            .spawn(move || {
                if let Err(error) = run_loop(&nodes, producers, quit_receiver, &ready_sender) {
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
    producers: Vec<Producer<f32>>,
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
    for (node, producer) in nodes.iter().zip(producers) {
        let stream = StreamBox::new(&core, node.stream_name(), node.properties())?;
        let listener = stream
            .add_local_listener_with_user_data(producer)
            .process(|stream, producer| {
                let Some(mut buffer) = stream.dequeue_buffer() else {
                    return;
                };
                let Some(data) = buffer.datas_mut().first_mut() else {
                    return;
                };
                let length = data.chunk().size() as usize;
                let Some(bytes) = data.data() else {
                    return;
                };
                let samples = bytes[..length.min(bytes.len())]
                    .chunks_exact(size_of::<f32>())
                    .map(|sample| f32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]));
                let room = producer.slots().min(length / size_of::<f32>());
                if let Ok(chunk) = producer.write_chunk_uninit(room) {
                    chunk.fill_from_iter(samples);
                }
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
