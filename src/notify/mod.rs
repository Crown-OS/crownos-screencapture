//! Telling the user a capture happened, and hearing back what they clicked.
//!
//! [`Notifier`] is implemented by [`CrownotifyNotifier`] (crownos-ipc, on CrownOS) and
//! [`FreedesktopNotifier`] (`org.freedesktop.Notifications`, everywhere else).

mod crownotify;
mod error;
mod freedesktop;

use std::borrow::Cow;
use std::path::Path;
use std::time::Duration;

pub use self::crownotify::CrownotifyNotifier;
pub use self::error::NotifyError;
pub use self::freedesktop::FreedesktopNotifier;
use crate::util::desktop::is_crownos;

pub const APP_NAME: &str = "Screenshot";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationAction {
    /// Clicking the notification itself.
    Open,
    Stop,
}

impl NotificationAction {
    pub const fn key(self) -> &'static str {
        match self {
            Self::Open => "default",
            Self::Stop => "Stop",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Open => "Open",
            Self::Stop => "Stop",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        [Self::Open, Self::Stop]
            .into_iter()
            .find(|action| action.key() == key)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Expiry {
    #[default]
    Default,
    Never,
}

#[derive(Debug, Clone)]
pub struct Notification<'a> {
    pub summary: Cow<'a, str>,
    pub body: Cow<'a, str>,
    pub actions: &'a [NotificationAction],
    pub image: Option<&'a Path>,
    pub expiry: Expiry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotificationId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Response {
    Invoked(NotificationAction),
    Dismissed,
    TimedOut,
}

/// A notification server.
pub trait Notifier: Send {
    fn post(&mut self, notification: &Notification<'_>) -> Result<NotificationId, NotifyError>;

    fn close(&mut self, id: NotificationId) -> Result<(), NotifyError>;

    /// Blocks until the user acts on or dismisses `id`, or `timeout` passes.
    fn wait_for_response(
        &mut self,
        id: NotificationId,
        timeout: Option<Duration>,
    ) -> Result<Response, NotifyError>;
}

/// crownotify on CrownOS, the freedesktop server elsewhere or when crownotify is not running.
pub fn connect() -> Result<Box<dyn Notifier>, NotifyError> {
    if is_crownos() {
        match CrownotifyNotifier::connect() {
            Ok(notifier) => return Ok(Box::new(notifier)),
            Err(error) => tracing::warn!(
                "crownotify unavailable, using org.freedesktop.Notifications: {error}"
            ),
        }
    }
    Ok(Box::new(FreedesktopNotifier::connect()?))
}
