//! Handing a finished capture to the previewer.

use std::path::Path;
use std::time::Duration;

use crownos_preview::Client;
use thiserror::Error;

use crate::util::desktop::spawn_detached;

const PREVIEW_BINARY: &str = "crownos-preview";
const REPLY_TIMEOUT: Option<Duration> = Some(Duration::from_secs(2));

#[derive(Debug, Error)]
pub enum PreviewError {
    #[error("crownos-preview: {0}")]
    Ipc(#[from] crownos_ipc::Error),
    #[error("cannot start {PREVIEW_BINARY}: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("{0} is not valid UTF-8")]
    NonUtf8Path(String),
    #[error("crownos-preview cannot show {0}")]
    Rejected(String),
}

/// Something that can show a capture to the user.
pub trait Previewer {
    fn open(&self, path: &Path) -> Result<(), PreviewError>;
}

/// The CrownOS previewer, reached over crownos-ipc and started on demand.
#[derive(Debug, Default, Clone, Copy)]
pub struct CrownosPreview;

impl Previewer for CrownosPreview {
    fn open(&self, path: &Path) -> Result<(), PreviewError> {
        let text = path
            .to_str()
            .ok_or_else(|| PreviewError::NonUtf8Path(path.display().to_string()))?;
        let mut client = match Client::connect() {
            Ok(client) => client,
            Err(crownos_ipc::Error::ServiceUnavailable { .. }) => {
                spawn_detached(PREVIEW_BINARY, [path])?;
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };
        let pending = client.open(text.to_owned())?;
        if client.wait(pending, REPLY_TIMEOUT)? {
            Ok(())
        } else {
            Err(PreviewError::Rejected(text.to_owned()))
        }
    }
}
