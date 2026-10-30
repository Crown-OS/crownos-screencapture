use thiserror::Error;

#[derive(Debug, Error)]
pub enum NotifyError {
    #[error("crownotify: {0}")]
    Crownotify(#[from] crownos_ipc::Error),
    #[error("notification bus: {0}")]
    Bus(#[from] zbus::Error),
    #[error("waiting for crownotify: {0}")]
    Poll(#[from] rustix::io::Errno),
}
