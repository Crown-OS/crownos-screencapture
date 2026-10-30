//! Getting pixels out of the compositor.
//!
//! [`ScreenCapture`] is the seam between the tool and a capture protocol; the only backend today
//! is [`WaylandCapture`], built on `ext-image-copy-capture-v1`.

mod error;
mod frame;
mod target;
mod wayland;

use std::path::Path;
use std::time::Duration;

pub use error::CaptureError;
pub use frame::{
    BufferKind, CapturedFrame, DmabufFrame, FramePixels, PixelFormat, PlaneLayout, ShmFrame,
};
pub use target::{CaptureTarget, CursorMode, OutputInfo, PixelRect, Region, WindowInfo};
pub use wayland::WaylandCapture;

/// A source of screen pixels.
pub trait ScreenCapture {
    type Stream<'capture>: FrameStream
    where
        Self: 'capture;

    fn outputs(&self) -> Vec<OutputInfo>;

    fn windows(&self) -> Vec<WindowInfo>;

    /// The output the user is working on, where new surfaces open.
    fn focused_output(&mut self) -> Result<OutputInfo, CaptureError>;

    /// Captures one frame into memory.
    fn capture(
        &mut self,
        target: &CaptureTarget,
        cursor: CursorMode,
    ) -> Result<ShmFrame, CaptureError>;

    /// Starts a stream of frames into buffers of `kind`.
    fn stream(
        &mut self,
        target: &CaptureTarget,
        cursor: CursorMode,
        kind: BufferKind,
    ) -> Result<Self::Stream<'_>, CaptureError>;

    /// The output named `name`, or the focused one when `name` is `None`.
    fn resolve_output(&mut self, name: Option<&str>) -> Result<OutputInfo, CaptureError> {
        match name {
            Some(name) => self
                .outputs()
                .into_iter()
                .find(|output| output.name == name)
                .ok_or_else(|| CaptureError::UnknownOutput(name.to_owned())),
            None => self.focused_output(),
        }
    }
}

/// Frames arriving from a running capture.
pub trait FrameStream {
    /// Waits up to `timeout` for the next frame; `None` when nothing changed on screen.
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<CapturedFrame<'_>>, CaptureError>;

    /// The size of the buffers frames currently arrive in.
    fn buffer_size(&self) -> (u32, u32);

    /// The render node of the GPU holding the frames, for streams of GPU buffers.
    fn render_node(&self) -> Option<&Path>;
}
