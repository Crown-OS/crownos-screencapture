use thiserror::Error;

#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("cannot connect to the Wayland display: {0}")]
    Connect(#[from] wayland_client::ConnectError),
    #[error("Wayland connection failed: {0}")]
    Dispatch(#[from] wayland_client::DispatchError),
    #[error("Wayland registry: {0}")]
    Globals(#[from] wayland_client::globals::GlobalError),
    #[error("Wayland protocol: {0}")]
    Wayland(#[from] wayland_client::backend::WaylandError),
    #[error("Wayland bind: {0}")]
    Bind(#[from] wayland_client::globals::BindError),
    #[error("the compositor does not support {0}")]
    Unsupported(&'static str),
    #[error("no output named {0}")]
    UnknownOutput(String),
    #[error("no window with identifier {0}")]
    UnknownWindow(String),
    #[error("no output is connected")]
    NoOutput,
    #[error("the compositor offers no usable buffer format")]
    NoBufferFormat,
    #[error("the capture failed: {0}")]
    Failed(&'static str),
    #[error("the captured source went away")]
    Stopped,
    #[error("invalid region '{0}', expected x,y,width,height")]
    InvalidRegion(String),
    #[error("no render node for GPU {0}")]
    NoRenderNode(u64),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
