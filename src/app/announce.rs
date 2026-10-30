use std::borrow::Cow;
use std::path::Path;
use std::time::Duration;

use anyhow::Result;

use std::sync::mpsc::{self, Receiver};
use std::thread;

use crate::notify::{self, Expiry, Notification, NotificationAction, NotificationId, Response};
use crate::preview::{CrownosPreview, Previewer};
use crate::record::StopSignal;

/// How long a saved-capture notification stays clickable.
const CLICKABLE_FOR: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capture {
    Screenshot,
    Recording,
    Voice,
}

impl Capture {
    const fn summary(self) -> &'static str {
        match self {
            Self::Screenshot => "Screenshot saved",
            Self::Recording => "Recording saved",
            Self::Voice => "Voice recording saved",
        }
    }
}

/// Posts a "saved" notification and opens the preview when it is clicked. Without a
/// notification server the capture is still saved, so that is only a warning.
pub fn announce(capture: Capture, path: &Path) {
    if let Err(error) = notify_saved(capture, path) {
        tracing::warn!("cannot announce {}: {error:#}", path.display());
    }
}

fn notify_saved(capture: Capture, path: &Path) -> Result<()> {
    let mut notifier = notify::connect()?;
    let body = path
        .file_name()
        .map_or(Cow::Borrowed(""), |name| name.to_string_lossy());
    let notification = Notification {
        summary: Cow::Borrowed(capture.summary()),
        body,
        actions: &[NotificationAction::Open],
        image: (capture == Capture::Screenshot).then_some(path),
        expiry: Expiry::Default,
    };
    let id = notifier.post(&notification)?;
    if notifier.wait_for_response(id, Some(CLICKABLE_FOR))?
        == Response::Invoked(NotificationAction::Open)
    {
        CrownosPreview.open(path)?;
    }
    Ok(())
}

/// The persistent "recording" notification, whose Stop button ends the recording.
pub struct RecordingNotice {
    stop: StopSignal,
    posted: Option<Receiver<NotificationId>>,
}

impl RecordingNotice {
    pub fn new(stop: StopSignal) -> Self {
        Self { stop, posted: None }
    }

    /// Posts the notification and watches its Stop button from a background thread.
    pub fn show(&mut self, summary: &'static str) {
        let (sender, posted) = mpsc::channel();
        let stop = self.stop.clone();
        thread::spawn(move || {
            let watched = notify::connect().and_then(|mut notifier| {
                let id = notifier.post(&Notification {
                    summary: Cow::Borrowed(summary),
                    body: Cow::Borrowed("Run crownos-screenshot stop, or press Stop, to finish."),
                    actions: &[NotificationAction::Stop],
                    image: None,
                    expiry: Expiry::Never,
                })?;
                let _ = sender.send(id);
                while notifier.wait_for_response(id, None)?
                    != Response::Invoked(NotificationAction::Stop)
                {}
                stop.raise();
                Ok(())
            });
            if let Err(error) = watched {
                tracing::warn!("recording notification unavailable: {error}");
            }
        });
        self.posted = Some(posted);
    }

    /// Takes the notification down.
    pub fn dismiss(&mut self) {
        let Some(id) = self.posted.take().and_then(|posted| posted.try_recv().ok()) else {
            return;
        };
        if let Err(error) = notify::connect().and_then(|mut notifier| notifier.close(id)) {
            tracing::warn!("cannot close the recording notification: {error}");
        }
    }
}
