use anyhow::{Context, Result};

use crate::Client;

/// Asks the running recorder to finish. Errors when nothing is recording.
pub fn stop_recording() -> Result<()> {
    Client::connect()
        .context("no recording is running")?
        .stop()?;
    Ok(())
}

/// Stops a running recording, or runs `otherwise` when there is none.
pub fn stop_recording_or(otherwise: impl FnOnce() -> Result<()>) -> Result<()> {
    match Client::connect() {
        Ok(mut recorder) => Ok(recorder.stop()?),
        Err(_) => otherwise(),
    }
}
