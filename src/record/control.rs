use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use crownos_ipc::{ServiceBuilder, blocking};
use signal_hook::consts::{SIGINT, SIGTERM};

use super::RecordError;
use crate::protocol::protocol::{self, Handler};

/// Set once anything asks the recording to finish.
#[derive(Debug, Clone, Default)]
pub struct StopSignal(Arc<AtomicBool>);

impl StopSignal {
    pub fn raise(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_raised(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    /// Raises the signal on SIGINT and SIGTERM.
    pub fn on_termination(&self) -> Result<(), RecordError> {
        for signal in [SIGINT, SIGTERM] {
            signal_hook::flag::register(signal, Arc::clone(&self.0))?;
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

struct StopHandler(StopSignal);

impl Handler for StopHandler {
    fn stop(&mut self) {
        self.0.raise();
    }
}
