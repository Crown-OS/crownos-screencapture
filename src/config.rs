//! The `screenshot` section of the CrownOS settings.

use std::path::PathBuf;

use crownos_config::schema::Screenshot;

use crate::capture::CursorMode;
use crate::util::paths::{default_recording_directory, default_screenshot_directory, expand_home};

/// The user's capture settings, read once per run.
#[derive(Debug, Clone)]
pub struct Settings(pub Screenshot);

impl Settings {
    pub fn load() -> Self {
        Self(crownos_config::load::<Screenshot>(Screenshot::SECTION))
    }

    pub fn screenshot_directory(&self) -> PathBuf {
        self.0
            .screenshot_directory
            .as_deref()
            .map_or_else(default_screenshot_directory, |path| {
                expand_home(path).into_owned()
            })
    }

    pub fn recording_directory(&self) -> PathBuf {
        self.0
            .recording_directory
            .as_deref()
            .map_or_else(default_recording_directory, |path| {
                expand_home(path).into_owned()
            })
    }

    pub fn cursor(&self) -> CursorMode {
        CursorMode::from_visibility(self.0.show_cursor)
    }
}
