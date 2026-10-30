mod gpu;
mod probe;
mod session;
mod state;
mod stream;

use std::os::fd::AsFd;
use std::time::Duration;

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::{wl_buffer::WlBuffer, wl_shm_pool::WlShmPool};
use wayland_client::{Connection, EventQueue, Proxy, QueueHandle};
use wayland_protocols::ext::image_capture_source::v1::client::ext_image_capture_source_v1::ExtImageCaptureSourceV1;
use wayland_protocols::ext::image_copy_capture::v1::client::{
    ext_image_copy_capture_frame_v1::ExtImageCopyCaptureFrameV1,
    ext_image_copy_capture_manager_v1::Options,
    ext_image_copy_capture_session_v1::ExtImageCopyCaptureSessionV1,
};

use self::session::{Constraints, FailureKind, FrameData, FrameStatus, SessionData, SessionStatus};
use self::state::{CaptureGlobals, Silent, State};
pub use self::stream::WaylandStream;
use super::{
    BufferKind, CaptureError, CaptureTarget, CursorMode, OutputInfo, ScreenCapture, ShmFrame,
    WindowInfo,
};

/// Screen capture over `ext-image-copy-capture-v1`.
pub struct WaylandCapture {
    connection: Connection,
    queue: EventQueue<State>,
    handle: QueueHandle<State>,
    state: State,
    globals: CaptureGlobals,
}

impl WaylandCapture {
    pub fn connect() -> Result<Self, CaptureError> {
        let connection = Connection::connect_to_env()?;
        let (globals, mut queue) = registry_queue_init::<State>(&connection)?;
        let handle = queue.handle();
        let mut state = State::new(&globals, &handle)?;
        let capture_globals = CaptureGlobals::bind(&globals, &handle)?;
        queue.roundtrip(&mut state)?;
        queue.roundtrip(&mut state)?;
        Ok(Self {
            connection,
            queue,
            handle,
            state,
            globals: capture_globals,
        })
    }

    fn source(&self, target: &CaptureTarget) -> Result<ExtImageCaptureSourceV1, CaptureError> {
        match target {
            CaptureTarget::Output(name) => {
                let output = self
                    .state
                    .wl_output(name)
                    .ok_or_else(|| CaptureError::UnknownOutput(name.clone()))?;
                Ok(self
                    .globals
                    .output_sources
                    .create_source(&output, &self.handle, Silent))
            }
            CaptureTarget::Window(identifier) => {
                let manager =
                    self.globals
                        .window_sources
                        .as_ref()
                        .ok_or(CaptureError::Unsupported(
                            "ext-foreign-toplevel-image-capture-source-manager-v1",
                        ))?;
                let toplevel = self
                    .state
                    .toplevel(identifier)
                    .ok_or_else(|| CaptureError::UnknownWindow(identifier.clone()))?;
                Ok(manager.create_source(&toplevel, &self.handle, Silent))
            }
        }
    }

    fn open_session(
        &mut self,
        target: &CaptureTarget,
        cursor: CursorMode,
    ) -> Result<(ExtImageCopyCaptureSessionV1, Constraints), CaptureError> {
        let source = self.source(target)?;
        let options = match cursor {
            CursorMode::Painted => Options::PaintCursors,
            CursorMode::Hidden => Options::empty(),
        };
        self.state.session = SessionStatus::Negotiating;
        let session = self.globals.manager.create_session(
            &source,
            options,
            &self.handle,
            SessionData::default(),
        );
        source.destroy();
        let constraints = self.await_constraints()?;
        Ok((session, constraints))
    }

    fn await_constraints(&mut self) -> Result<Constraints, CaptureError> {
        loop {
            match std::mem::take(&mut self.state.session) {
                SessionStatus::Ready(constraints) => return Ok(constraints),
                SessionStatus::Stopped => return Err(CaptureError::Stopped),
                SessionStatus::Negotiating => self.queue.blocking_dispatch(&mut self.state)?,
            };
        }
    }

    fn shm_buffer(&self, frame: &ShmFrame) -> (WlShmPool, WlBuffer) {
        let pool = self.state.shm.wl_shm().create_pool(
            frame.as_fd(),
            i32::try_from(frame.byte_len()).unwrap_or(i32::MAX),
            &self.handle,
            Silent,
        );
        let buffer = pool.create_buffer(
            0,
            frame.width as i32,
            frame.height as i32,
            frame.stride as i32,
            session::shm_format(frame.format),
            &self.handle,
            Silent,
        );
        (pool, buffer)
    }

    fn request_frame(
        &self,
        session: &ExtImageCopyCaptureSessionV1,
        buffer: &WlBuffer,
    ) -> ExtImageCopyCaptureFrameV1 {
        let frame = session.create_frame(&self.handle, FrameData::default());
        frame.attach_buffer(buffer);
        frame.damage_buffer(0, 0, i32::MAX, i32::MAX);
        frame.capture();
        frame
    }

    fn take_frame_status(&mut self, frame: &ExtImageCopyCaptureFrameV1) -> Option<FrameStatus> {
        self.state.frames.remove(&frame.id())
    }

    /// Reads and dispatches events, waiting at most `timeout` for the socket to become readable.
    fn dispatch_timeout(&mut self, timeout: Duration) -> Result<(), CaptureError> {
        self.connection.flush()?;
        if let Some(guard) = self.queue.prepare_read() {
            let connection_fd = guard.connection_fd();
            let mut fds = [PollFd::new(&connection_fd, PollFlags::IN)];
            let timeout = Timespec::try_from(timeout).unwrap_or(Timespec {
                tv_sec: 1,
                tv_nsec: 0,
            });
            let readiness = poll(&mut fds, Some(&timeout));
            match readiness {
                Ok(0) | Err(rustix::io::Errno::INTR) => drop(guard),
                Ok(_) => match guard.read() {
                    Ok(_) => {}
                    Err(wayland_client::backend::WaylandError::Io(error))
                        if error.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(error) => return Err(error.into()),
                },
                Err(error) => return Err(CaptureError::Io(error.into())),
            }
        }
        self.queue.dispatch_pending(&mut self.state)?;
        Ok(())
    }
}

impl ScreenCapture for WaylandCapture {
    type Stream<'capture> = WaylandStream<'capture>;

    fn outputs(&self) -> Vec<OutputInfo> {
        self.state.output_infos()
    }

    fn windows(&self) -> Vec<WindowInfo> {
        self.state.window_infos()
    }

    fn focused_output(&mut self) -> Result<OutputInfo, CaptureError> {
        let probed = self
            .probe_focused_output()?
            .and_then(|output| self.state.output_info(&output));
        probed
            .or_else(|| self.outputs().into_iter().next())
            .ok_or(CaptureError::NoOutput)
    }

    fn capture(
        &mut self,
        target: &CaptureTarget,
        cursor: CursorMode,
    ) -> Result<ShmFrame, CaptureError> {
        let (session, constraints) = self.open_session(target, cursor)?;
        let format = constraints
            .shm_format()
            .ok_or(CaptureError::NoBufferFormat)?;
        let pixels = ShmFrame::allocate(constraints.width, constraints.height, format)?;
        let (pool, buffer) = self.shm_buffer(&pixels);
        let frame = self.request_frame(&session, &buffer);
        let status = loop {
            if let Some(status) = self.take_frame_status(&frame) {
                break status;
            }
            self.queue.blocking_dispatch(&mut self.state)?;
        };
        frame.destroy();
        buffer.destroy();
        pool.destroy();
        session.destroy();
        self.connection.flush()?;
        match status {
            FrameStatus::Ready { .. } => Ok(pixels),
            FrameStatus::Failed(FailureKind::Stopped) => Err(CaptureError::Stopped),
            FrameStatus::Failed(FailureKind::BufferConstraints) => {
                Err(CaptureError::Failed("buffer constraints changed"))
            }
            FrameStatus::Failed(FailureKind::Unknown) => {
                Err(CaptureError::Failed("compositor error"))
            }
        }
    }

    fn stream(
        &mut self,
        target: &CaptureTarget,
        cursor: CursorMode,
        kind: BufferKind,
    ) -> Result<WaylandStream<'_>, CaptureError> {
        WaylandStream::start(self, target, cursor, kind)
    }
}
