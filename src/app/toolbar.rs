use anyhow::Result;

use crate::capture::{ScreenCapture, WaylandCapture};
use crate::ui::run_toolbar;

/// Opens the capture toolbar with the displays and windows on screen right now.
pub fn open() -> Result<()> {
    let (outputs, windows) = {
        let capture = WaylandCapture::connect()?;
        (capture.outputs(), capture.windows())
    };
    run_toolbar(outputs, windows)
}
