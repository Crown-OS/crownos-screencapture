use std::path::PathBuf;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};

use crate::capture::{PixelFormat, Region};

/// Screenshots and screen recordings. Without a command, opens the capture toolbar.
#[derive(Debug, Parser)]
#[command(name = "crownos-screenshot", version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Take a screenshot.
    Shot(ShotArgs),
    /// Record the screen until `crownos-screenshot stop`.
    Record(RecordArgs),
    /// Record audio only until `crownos-screenshot stop`.
    Voice(VoiceArgs),
    /// Stop the running recording.
    Stop,
    /// Let the user drag out a region and print it as x,y,width,height.
    #[command(hide = true)]
    SelectArea(SelectAreaArgs),
}

#[derive(Debug, Clone, Args)]
pub struct TargetArgs {
    /// Capture a whole display; the focused one when no name is given.
    #[arg(long, value_name = "OUTPUT", num_args = 0..=1, default_missing_value = "", conflicts_with_all = ["window", "area"])]
    pub display: Option<String>,
    /// Capture one window, by its foreign-toplevel identifier.
    #[arg(long, value_name = "IDENTIFIER", conflicts_with_all = ["area", "region"])]
    pub window: Option<String>,
    /// Drag out a region of the focused display.
    #[arg(long, conflicts_with = "region")]
    pub area: bool,
    /// A fixed region of the display, in logical pixels.
    #[arg(long, value_name = "X,Y,WIDTH,HEIGHT")]
    pub region: Option<Region>,
}

/// What to capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    Display(Option<String>),
    Window(String),
    Area,
    Region {
        output: Option<String>,
        region: Region,
    },
}

impl From<TargetArgs> for Selection {
    fn from(target: TargetArgs) -> Self {
        let output = target.display.filter(|name| !name.is_empty());
        match (target.window, target.area, target.region) {
            (Some(identifier), _, _) => Self::Window(identifier),
            (None, true, _) => Self::Area,
            (None, false, Some(region)) => Self::Region { output, region },
            (None, false, None) => Self::Display(output),
        }
    }
}

#[derive(Debug, Args)]
pub struct ShotArgs {
    #[command(flatten)]
    pub target: TargetArgs,
    /// Where to save; defaults to the screenshot folder from settings.
    #[arg(long, short)]
    pub path: Option<PathBuf>,
    /// Wait this many milliseconds before capturing.
    #[arg(long, value_name = "MILLISECONDS", default_value_t = 0)]
    pub delay: u64,
    /// Do not post a notification.
    #[arg(long)]
    pub no_notify: bool,
}

impl ShotArgs {
    pub fn delay(&self) -> Duration {
        Duration::from_millis(self.delay)
    }
}

#[derive(Debug, Args)]
pub struct RecordArgs {
    #[command(flatten)]
    pub target: TargetArgs,
    /// Where to save; defaults to the recording folder from settings.
    #[arg(long, short)]
    pub path: Option<PathBuf>,
    /// Record without microphone or system audio.
    #[arg(long)]
    pub no_audio: bool,
    /// Wait this many milliseconds before recording, on top of the countdown setting.
    #[arg(long, value_name = "MILLISECONDS", default_value_t = 0)]
    pub delay: u64,
}

#[derive(Debug, Args)]
pub struct VoiceArgs {
    /// Where to save; defaults to the recording folder from settings.
    #[arg(long, short)]
    pub path: Option<PathBuf>,
}

/// A frozen frame shared by the parent process, shown behind the selection.
#[derive(Debug, Clone, Args)]
pub struct SelectAreaArgs {
    #[arg(long, requires_all = ["width", "height", "stride", "format"])]
    pub frame_fd: Option<i32>,
    #[arg(long)]
    pub width: Option<u32>,
    #[arg(long)]
    pub height: Option<u32>,
    #[arg(long)]
    pub stride: Option<u32>,
    #[arg(long, value_parser = parse_pixel_format)]
    pub format: Option<PixelFormat>,
}

fn parse_pixel_format(name: &str) -> Result<PixelFormat, String> {
    PixelFormat::from_short_name(name).ok_or_else(|| format!("unknown pixel format {name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selection(arguments: &[&str]) -> Selection {
        let cli =
            Cli::try_parse_from(["crownos-screenshot", "shot"].iter().chain(arguments)).unwrap();
        match cli.command {
            Some(Command::Shot(shot)) => shot.target.into(),
            other => panic!("unexpected command {other:?}"),
        }
    }

    #[test]
    fn defaults_to_focused_display() {
        assert_eq!(selection(&[]), Selection::Display(None));
        assert_eq!(selection(&["--display"]), Selection::Display(None));
        assert_eq!(
            selection(&["--display", "DP-1"]),
            Selection::Display(Some("DP-1".into()))
        );
    }

    #[test]
    fn parses_targets() {
        assert_eq!(
            selection(&["--window", "abc"]),
            Selection::Window("abc".into())
        );
        assert_eq!(selection(&["--area"]), Selection::Area);
        assert_eq!(
            selection(&["--display", "HDMI-A-1", "--region", "0,0,10,20"]),
            Selection::Region {
                output: Some("HDMI-A-1".into()),
                region: Region {
                    x: 0,
                    y: 0,
                    width: 10,
                    height: 20
                }
            }
        );
    }

    #[test]
    fn rejects_conflicting_targets() {
        assert!(
            Cli::try_parse_from(["crownos-screenshot", "shot", "--area", "--window", "x"]).is_err()
        );
        assert!(
            Cli::try_parse_from([
                "crownos-screenshot",
                "shot",
                "--area",
                "--region",
                "0,0,1,1"
            ])
            .is_err()
        );
    }
}
