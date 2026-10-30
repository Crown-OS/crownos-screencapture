use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};

use super::announce::{Capture, RecordingNotice, announce};
use crate::area::pick_region;
use crate::capture::{CaptureTarget, OutputInfo, Region, ScreenCapture, WaylandCapture};
use crate::cli::{RecordArgs, Selection, VoiceArgs};
use crate::config::Settings;
use crate::record::video::{Crop, VideoSettings};
use crate::record::{
    CaptureNode, Container, OutputRegion, ScreenRecording, StopSignal, record_screen, record_voice,
};
use crate::util::paths::timestamped_file;

const RECORDING_PREFIX: &str = "Recording";
const VOICE_PREFIX: &str = "Voice";

pub fn run(arguments: RecordArgs) -> Result<()> {
    std::thread::sleep(Duration::from_millis(arguments.delay));
    let settings = Settings::load();
    let mut capture = WaylandCapture::connect()?;
    let Some((target, region)) = resolve_target(&mut capture, Selection::from(arguments.target))?
    else {
        return Ok(());
    };
    std::thread::sleep(Duration::from_secs(settings.0.countdown_seconds.into()));
    let container = Container::from(settings.0.video_container);
    let path = destination(arguments.path, &settings, RECORDING_PREFIX, container)?;
    let recording = ScreenRecording {
        target,
        region,
        cursor: settings.cursor(),
        video: VideoSettings {
            codec: settings.0.video_codec,
            quality: settings.0.quality,
            frames_per_second: settings.0.frame_rate,
            crop: Crop::Full,
        },
        audio: if arguments.no_audio {
            Vec::new()
        } else {
            audio_nodes(&settings)
        },
        container,
        path,
    };
    let stop = listen_for_stop()?;
    let mut notice = RecordingNotice::new(stop.clone());
    record_screen(&mut capture, &recording, &stop, &mut || {
        notice.show("Recording screen")
    })
    .context("the recording failed")?;
    drop(capture);
    notice.dismiss();
    finished(Capture::Recording, &recording.path)
}

pub fn run_voice(arguments: VoiceArgs) -> Result<()> {
    let settings = Settings::load();
    let path = destination(arguments.path, &settings, VOICE_PREFIX, Container::OggOpus)?;
    let stop = listen_for_stop()?;
    let mut notice = RecordingNotice::new(stop.clone());
    record_voice(&path, &[CaptureNode::Microphone], &stop, &mut || {
        notice.show("Recording voice")
    })
    .context("the voice recording failed")?;
    notice.dismiss();
    finished(Capture::Voice, &path)
}

/// The capture target and the part of it to keep; `None` when the user cancelled the area
/// selection.
fn resolve_target(
    capture: &mut impl ScreenCapture,
    selection: Selection,
) -> Result<Option<(CaptureTarget, Option<OutputRegion>)>> {
    let on_output = |output: OutputInfo, region: Region| {
        let kept = OutputRegion {
            region,
            logical_width: output.logical_width,
            logical_height: output.logical_height,
        };
        (CaptureTarget::Output(output.name), Some(kept))
    };
    Ok(match selection {
        Selection::Display(name) => Some((
            CaptureTarget::Output(capture.resolve_output(name.as_deref())?.name),
            None,
        )),
        Selection::Window(identifier) => Some((CaptureTarget::Window(identifier), None)),
        Selection::Region { output, region } => Some(on_output(
            capture.resolve_output(output.as_deref())?,
            region,
        )),
        Selection::Area => {
            let output = capture.focused_output()?;
            pick_region(None)?.map(|region| on_output(output, region))
        }
    })
}

fn audio_nodes(settings: &Settings) -> Vec<CaptureNode> {
    [
        (settings.0.capture_microphone, CaptureNode::Microphone),
        (settings.0.capture_system_audio, CaptureNode::SystemAudio),
    ]
    .into_iter()
    .filter_map(|(enabled, node)| enabled.then_some(node))
    .collect()
}

fn destination(
    explicit: Option<PathBuf>,
    settings: &Settings,
    prefix: &str,
    container: Container,
) -> Result<PathBuf> {
    let path = explicit.unwrap_or_else(|| {
        timestamped_file(
            &settings.recording_directory(),
            prefix,
            container.extension(),
        )
    });
    if let Some(directory) = path.parent() {
        std::fs::create_dir_all(directory)
            .with_context(|| format!("cannot create {}", directory.display()))?;
    }
    Ok(path)
}

fn listen_for_stop() -> Result<StopSignal> {
    let stop = StopSignal::default();
    stop.on_termination()?;
    stop.serve_stop_requests()?;
    Ok(stop)
}

fn finished(capture: Capture, path: &std::path::Path) -> Result<()> {
    println!("{}", path.display());
    announce(capture, path);
    Ok(())
}
