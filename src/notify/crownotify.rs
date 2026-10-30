use std::time::{Duration, Instant};

use crownotify::proto::protocol::{ActionInvoked, Client, Event, NotificationClosed};
use rustix::event::{PollFd, PollFlags, Timespec, poll};

use super::{
    APP_NAME, Expiry, Notification, NotificationAction, NotificationId, Notifier, NotifyError,
    Response,
};

const REPLY_TIMEOUT: Option<Duration> = Some(Duration::from_secs(2));

/// The CrownOS notification daemon over crownos-ipc.
pub struct CrownotifyNotifier {
    client: Client,
}

impl CrownotifyNotifier {
    pub fn connect() -> Result<Self, NotifyError> {
        let mut client = Client::connect()?;
        client.subscribe_blocking::<ActionInvoked>(REPLY_TIMEOUT)?;
        client.subscribe_blocking::<NotificationClosed>(REPLY_TIMEOUT)?;
        Ok(Self { client })
    }

    fn pending_response(&mut self, id: NotificationId) -> Option<Response> {
        while let Some(event) = self.client.next_event() {
            match event {
                Event::ActionInvoked(ActionInvoked {
                    id: invoked,
                    action,
                }) if invoked == id.0 => {
                    if let Some(action) = NotificationAction::from_key(&action) {
                        return Some(Response::Invoked(action));
                    }
                }
                Event::NotificationClosed(NotificationClosed { id: closed, .. })
                    if closed == id.0 =>
                {
                    return Some(Response::Dismissed);
                }
                _ => {}
            }
        }
        None
    }

    fn wait_readable(&mut self, timeout: Option<Duration>) -> Result<(), NotifyError> {
        let timeout = timeout.and_then(|timeout| Timespec::try_from(timeout).ok());
        let mut fds = [PollFd::new(&self.client, PollFlags::IN)];
        match poll(&mut fds, timeout.as_ref()) {
            Ok(_) | Err(rustix::io::Errno::INTR) => {}
            Err(error) => return Err(error.into()),
        }
        self.client.handle_readable()?;
        Ok(())
    }
}

impl Notifier for CrownotifyNotifier {
    fn post(&mut self, notification: &Notification<'_>) -> Result<NotificationId, NotifyError> {
        let expire_timeout = match notification.expiry {
            Expiry::Default => 0,
            Expiry::Never => u32::MAX,
        };
        let actions = notification
            .actions
            .iter()
            .map(|action| action.key().to_owned())
            .collect();
        let pending = self.client.send_general_notification(
            APP_NAME.to_owned(),
            notification.summary.clone().into_owned(),
            notification.body.clone().into_owned(),
            expire_timeout,
            actions,
        )?;
        Ok(NotificationId(self.client.wait(pending, REPLY_TIMEOUT)?))
    }

    fn close(&mut self, id: NotificationId) -> Result<(), NotifyError> {
        self.client.close_notification(id.0)?;
        Ok(())
    }

    fn wait_for_response(
        &mut self,
        id: NotificationId,
        timeout: Option<Duration>,
    ) -> Result<Response, NotifyError> {
        let deadline = timeout.map(|timeout| Instant::now() + timeout);
        loop {
            if let Some(response) = self.pending_response(id) {
                return Ok(response);
            }
            let remaining =
                deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()));
            if remaining.is_some_and(|remaining| remaining.is_zero()) {
                return Ok(Response::TimedOut);
            }
            self.wait_readable(remaining)?;
        }
    }
}
