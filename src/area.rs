//! Asking the user for a region, in a child process that owns the selection overlay.

use std::os::fd::{AsFd, AsRawFd};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

use crate::capture::{Region, ShmFrame};

/// Shows the selection overlay on the focused output, over `frozen` when given.
/// `None` when the user cancelled.
pub fn pick_region(frozen: Option<&ShmFrame>) -> Result<Option<Region>> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("select-area")
        .stdin(Stdio::null())
        .stdout(Stdio::piped());
    let inherited = frozen
        .map(|frame| rustix::io::dup(frame.as_fd()))
        .transpose()?;
    if let (Some(frame), Some(fd)) = (frozen, &inherited) {
        command.args([
            "--frame-fd".to_owned(),
            fd.as_raw_fd().to_string(),
            "--width".to_owned(),
            frame.width.to_string(),
            "--height".to_owned(),
            frame.height.to_string(),
            "--stride".to_owned(),
            frame.stride.to_string(),
            "--format".to_owned(),
            frame.format.short_name().to_owned(),
        ]);
    }
    let output = command
        .output()
        .context("the selection overlay failed to start")?;
    drop(inherited);
    if !output.status.success() {
        return Ok(None);
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(Some(text.trim().parse()?))
}
