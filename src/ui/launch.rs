use std::borrow::Cow;

use crate::util::desktop::{spawn_detached, spawn_self};

/// Gives the compositor time to take the toolbar off screen before the capture starts.
const SETTLE_MILLISECONDS: &str = "200";
const SETTINGS_BINARY: &str = "crownsettings";
const SETTINGS_PAGE: &str = "screenshot";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureMode {
    Record,
    Screenshot,
}

impl CaptureMode {
    pub const ALL: [Self; 2] = [Self::Record, Self::Screenshot];

    pub fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|mode| *mode == self)
            .unwrap_or_default()
    }

    pub fn from_index(index: usize) -> Self {
        Self::ALL.get(index).copied().unwrap_or(Self::Screenshot)
    }

    const fn subcommand(self) -> &'static str {
        match self {
            Self::Record => "record",
            Self::Screenshot => "shot",
        }
    }
}

/// What a click in the toolbar starts, each in its own process so the toolbar can close first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Launch {
    Display {
        mode: CaptureMode,
        output: Option<String>,
    },
    Window {
        mode: CaptureMode,
        identifier: String,
    },
    Area {
        mode: CaptureMode,
    },
    Voice,
    Settings,
}

impl Launch {
    pub fn arguments(&self) -> Vec<Cow<'_, str>> {
        match self {
            Self::Display { mode, output } => capture(*mode, "--display", output.as_deref()),
            Self::Window { mode, identifier } => capture(*mode, "--window", Some(identifier)),
            Self::Area { mode } => capture(*mode, "--area", None),
            Self::Voice => vec![Cow::Borrowed("voice")],
            Self::Settings => vec![Cow::Borrowed(SETTINGS_PAGE)],
        }
    }

    /// Starts the launch and closes the toolbar.
    pub fn run(&self) -> ! {
        let arguments = self.arguments();
        let arguments = arguments.iter().map(Cow::as_ref);
        let started = match self {
            Self::Settings => spawn_detached(SETTINGS_BINARY, arguments),
            _ => spawn_self(arguments),
        };
        if let Err(error) = started {
            tracing::error!("cannot start {self:?}: {error}");
        }
        std::process::exit(0)
    }
}

fn capture<'a>(
    mode: CaptureMode,
    target: &'static str,
    value: Option<&'a str>,
) -> Vec<Cow<'a, str>> {
    let mut arguments = vec![Cow::Borrowed(mode.subcommand()), Cow::Borrowed(target)];
    arguments.extend(value.map(Cow::Borrowed));
    arguments.extend([Cow::Borrowed("--delay"), Cow::Borrowed(SETTLE_MILLISECONDS)]);
    arguments
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_capture_arguments() {
        let launch = Launch::Display {
            mode: CaptureMode::Screenshot,
            output: Some("DP-1".into()),
        };
        assert_eq!(
            launch.arguments(),
            ["shot", "--display", "DP-1", "--delay", SETTLE_MILLISECONDS]
        );
        let launch = Launch::Area {
            mode: CaptureMode::Record,
        };
        assert_eq!(
            launch.arguments(),
            ["record", "--area", "--delay", SETTLE_MILLISECONDS]
        );
    }

    #[test]
    fn maps_toggle_indices() {
        for mode in CaptureMode::ALL {
            assert_eq!(CaptureMode::from_index(mode.index()), mode);
        }
    }
}
