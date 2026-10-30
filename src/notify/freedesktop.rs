use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use zbus::blocking::Connection;
use zbus::zvariant::Value;

use super::{
    APP_NAME, Expiry, Notification, NotificationAction, NotificationId, Notifier, NotifyError,
    Response,
};

const ICON: &str = "applets-screenshooter";
const NEVER_EXPIRE: i32 = 0;
const DEFAULT_EXPIRY: i32 = -1;

#[zbus::proxy(
    interface = "org.freedesktop.Notifications",
    default_service = "org.freedesktop.Notifications",
    default_path = "/org/freedesktop/Notifications"
)]
trait Notifications {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: &[&str],
        hints: HashMap<&str, Value<'_>>,
        expire_timeout: i32,
    ) -> zbus::Result<u32>;

    fn close_notification(&self, id: u32) -> zbus::Result<()>;

    #[zbus(signal)]
    fn action_invoked(&self, id: u32, action_key: String) -> zbus::Result<()>;

    #[zbus(signal)]
    fn notification_closed(&self, id: u32, reason: u32) -> zbus::Result<()>;
}

/// Any `org.freedesktop.Notifications` server, including crownotify's own D-Bus face.
pub struct FreedesktopNotifier {
    proxy: NotificationsProxyBlocking<'static>,
    responses: Receiver<(u32, Response)>,
}

impl FreedesktopNotifier {
    pub fn connect() -> Result<Self, NotifyError> {
        let connection = Connection::session()?;
        let proxy = NotificationsProxyBlocking::new(&connection)?;
        let (sender, responses) = mpsc::channel();
        forward(proxy.receive_action_invoked()?, sender.clone(), |signal| {
            let arguments = signal.args().ok()?;
            let action = NotificationAction::from_key(&arguments.action_key)?;
            Some((arguments.id, Response::Invoked(action)))
        });
        forward(proxy.receive_notification_closed()?, sender, |signal| {
            signal
                .args()
                .ok()
                .map(|arguments| (arguments.id, Response::Dismissed))
        });
        Ok(Self { proxy, responses })
    }
}

/// Relays `signals` to `sender` from a thread, since the blocking iterators cannot time out.
fn forward<Signals, Signal>(
    signals: Signals,
    sender: Sender<(u32, Response)>,
    decode: impl Fn(Signal) -> Option<(u32, Response)> + Send + 'static,
) where
    Signals: Iterator<Item = Signal> + Send + 'static,
{
    thread::spawn(move || {
        for response in signals.filter_map(decode) {
            if sender.send(response).is_err() {
                break;
            }
        }
    });
}

impl Notifier for FreedesktopNotifier {
    fn post(&mut self, notification: &Notification<'_>) -> Result<NotificationId, NotifyError> {
        let actions: Vec<&str> = notification
            .actions
            .iter()
            .flat_map(|action| [action.key(), action.label()])
            .collect();
        let mut hints = HashMap::from([("desktop-entry", Value::from("crownos-screenshot"))]);
        if let Some(image) = notification.image.and_then(|path| path.to_str()) {
            hints.insert("image-path", Value::from(image));
        }
        let expire_timeout = match notification.expiry {
            Expiry::Default => DEFAULT_EXPIRY,
            Expiry::Never => NEVER_EXPIRE,
        };
        let id = self.proxy.notify(
            APP_NAME,
            0,
            ICON,
            &notification.summary,
            &notification.body,
            &actions,
            hints,
            expire_timeout,
        )?;
        Ok(NotificationId(id))
    }

    fn close(&mut self, id: NotificationId) -> Result<(), NotifyError> {
        self.proxy.close_notification(id.0)?;
        Ok(())
    }

    fn wait_for_response(
        &mut self,
        id: NotificationId,
        timeout: Option<Duration>,
    ) -> Result<Response, NotifyError> {
        let deadline = timeout.map(|timeout| Instant::now() + timeout);
        loop {
            let received = match deadline {
                Some(deadline) => self
                    .responses
                    .recv_timeout(deadline.saturating_duration_since(Instant::now())),
                None => self
                    .responses
                    .recv()
                    .map_err(|_| RecvTimeoutError::Disconnected),
            };
            match received {
                Ok((notification, response)) if notification == id.0 => return Ok(response),
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => return Ok(Response::TimedOut),
                Err(RecvTimeoutError::Disconnected) => return Ok(Response::Dismissed),
            }
        }
    }
}
