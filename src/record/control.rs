use std::os::fd::{AsFd, BorrowedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use crownos_ipc::{ServiceBuilder, blocking};
use signal_hook::consts::{SIGINT, SIGTERM};

use super::RecordError;
use crate::protocol::protocol::{self, Handler};
use crate::util::wakeup::Wakeup;

/// Set once anything asks the recording to finish; its descriptor turns readable at the same
/// time, so a recorder can sleep in `poll` until then.
#[derive(Debug, Clone)]
pub struct StopSignal {
    raised: Arc<AtomicBool>,
    wakeup: Arc<Wakeup>,
}

impl StopSignal {
    pub fn new() -> Result<Self, RecordError> {
        Ok(Self {
            raised: Arc::default(),
            wakeup: Arc::new(Wakeup::new()?),
        })
    }

    pub fn raise(&self) {
        self.raised.store(true, Ordering::Release);
        self.wakeup.wake();
    }

    pub fn is_raised(&self) -> bool {
        self.raised.load(Ordering::Acquire)
    }

    /// Raises the signal on SIGINT and SIGTERM.
    pub fn on_termination(&self) -> Result<(), RecordError> {
        for signal in [SIGINT, SIGTERM] {
            signal_hook::flag::register(signal, Arc::clone(&self.raised))?;
            signal_hook::low_level::pipe::register(signal, self.wakeup.writer()?)?;
        }
        Ok(())
    }

    /// Serves `crownos-screenshot stop` for as long as the recording runs.
    pub fn serve_stop_requests(&self) -> Result<(), RecordError> {
        if protocol::Client::connect().is_ok() {
            return Err(RecordError::AlreadyRecording);
        }
        let mut server = ServiceBuilder::new(protocol::SERVICE).build()?;
        let mut handler = StopHandler(self.clone());
        thread::spawn(move || {
            let stop = handler.0.clone();
            let served = blocking::serve(&mut server, |server, peer, message| {
                protocol::dispatch(&mut handler, server, peer, message)?;
                Ok(!stop.is_raised())
            });
            if let Err(error) = served {
                tracing::warn!("recorder service stopped: {error}");
            }
        });
        Ok(())
    }
}

impl AsFd for StopSignal {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.wakeup.as_fd()
    }
}

struct StopHandler(StopSignal);

impl Handler for StopHandler {
    fn stop(&mut self) {
        self.0.raise();
    }
}

#[cfg(test)]
mod tests {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};

    use super::*;

    #[test]
    fn raising_wakes_the_descriptor() -> Result<(), RecordError> {
        let stop = StopSignal::new()?;
        let readable = |stop: &StopSignal| {
            let mut fds = [PollFd::new(stop, PollFlags::IN)];
            poll(&mut fds, Some(&Timespec::default())).map(|ready| ready > 0)
        };
        assert!(!stop.is_raised());
        assert!(!readable(&stop).map_err(std::io::Error::from)?);
        stop.clone().raise();
        assert!(stop.is_raised());
        assert!(readable(&stop).map_err(std::io::Error::from)?);
        Ok(())
    }
}
