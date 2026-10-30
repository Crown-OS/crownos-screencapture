use std::os::fd::AsFd;
use std::path::Path;
use std::time::Duration;

use gbm::BufferObject;
use wayland_client::protocol::{wl_buffer::WlBuffer, wl_shm_pool::WlShmPool};
use wayland_protocols::ext::image_copy_capture::v1::client::{
    ext_image_copy_capture_frame_v1::ExtImageCopyCaptureFrameV1,
    ext_image_copy_capture_session_v1::ExtImageCopyCaptureSessionV1,
};
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_buffer_params_v1::Flags;

use super::WaylandCapture;
use super::gpu::GpuAllocator;
use super::session::{Constraints, FailureKind, FrameStatus};
use super::state::Silent;
use crate::capture::{
    BufferKind, CaptureError, CaptureTarget, CapturedFrame, CursorMode, DmabufFrame, FramePixels,
    FrameStream, ShmFrame,
};

/// Enough buffers that the compositor never renders into one the encoder still reads.
const RING_SIZE: usize = 4;

enum SlotPixels {
    Memory {
        frame: ShmFrame,
        pool: WlShmPool,
    },
    Gpu {
        frame: DmabufFrame,
        _buffer_object: BufferObject<()>,
    },
}

struct Slot {
    pixels: SlotPixels,
    buffer: WlBuffer,
}

impl Slot {
    fn frame_pixels(&self) -> FramePixels<'_> {
        match &self.pixels {
            SlotPixels::Memory { frame, .. } => FramePixels::Memory(frame),
            SlotPixels::Gpu { frame, .. } => FramePixels::Gpu(frame),
        }
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.buffer.destroy();
        if let SlotPixels::Memory { pool, .. } = &self.pixels {
            pool.destroy();
        }
    }
}

/// A running capture, filling a ring of buffers frame after frame.
pub struct WaylandStream<'capture> {
    capture: &'capture mut WaylandCapture,
    session: ExtImageCopyCaptureSessionV1,
    kind: BufferKind,
    allocator: Option<GpuAllocator>,
    ring: Vec<Slot>,
    in_flight: Option<(ExtImageCopyCaptureFrameV1, usize)>,
    size: (u32, u32),
}

impl<'capture> WaylandStream<'capture> {
    pub(super) fn start(
        capture: &'capture mut WaylandCapture,
        target: &CaptureTarget,
        cursor: CursorMode,
        kind: BufferKind,
    ) -> Result<Self, CaptureError> {
        let (session, constraints) = capture.open_session(target, cursor)?;
        let mut stream = Self {
            capture,
            session,
            kind,
            allocator: None,
            ring: Vec::new(),
            in_flight: None,
            size: (0, 0),
        };
        stream.allocate_ring(&constraints)?;
        stream.submit(0);
        Ok(stream)
    }

    fn allocate_ring(&mut self, constraints: &Constraints) -> Result<(), CaptureError> {
        self.ring.clear();
        self.size = (constraints.width, constraints.height);
        for _ in 0..RING_SIZE {
            let slot = match self.kind {
                BufferKind::Memory => self.memory_slot(constraints)?,
                BufferKind::Gpu => self.gpu_slot(constraints)?,
            };
            self.ring.push(slot);
        }
        Ok(())
    }

    fn memory_slot(&self, constraints: &Constraints) -> Result<Slot, CaptureError> {
        let format = constraints
            .shm_format()
            .ok_or(CaptureError::NoBufferFormat)?;
        let frame = ShmFrame::allocate(constraints.width, constraints.height, format)?;
        let (pool, buffer) = self.capture.shm_buffer(&frame);
        Ok(Slot {
            pixels: SlotPixels::Memory { frame, pool },
            buffer,
        })
    }

    fn gpu_slot(&mut self, constraints: &Constraints) -> Result<Slot, CaptureError> {
        let linux_dmabuf = self
            .capture
            .globals
            .linux_dmabuf
            .clone()
            .ok_or(CaptureError::Unsupported("linux-dmabuf-v1"))?;
        let (format, modifiers) = constraints
            .dmabuf_format()
            .ok_or(CaptureError::NoBufferFormat)?;
        if self.allocator.is_none() {
            let device = constraints
                .dmabuf_device
                .ok_or(CaptureError::Unsupported("dmabuf capture"))?;
            self.allocator = Some(GpuAllocator::open(device)?);
        }
        let allocator = self
            .allocator
            .as_ref()
            .ok_or(CaptureError::Unsupported("dmabuf capture"))?;
        let (buffer_object, frame) =
            allocator.allocate(constraints.width, constraints.height, *format, modifiers)?;
        let params = linux_dmabuf.create_params(&self.capture.handle, Silent);
        for (index, plane) in frame.planes.iter().enumerate() {
            params.add(
                frame.fd.as_fd(),
                index as u32,
                plane.offset,
                plane.stride,
                (frame.modifier >> 32) as u32,
                frame.modifier as u32,
            );
        }
        let buffer = params.create_immed(
            frame.width as i32,
            frame.height as i32,
            frame.fourcc,
            Flags::empty(),
            &self.capture.handle,
            Silent,
        );
        params.destroy();
        Ok(Slot {
            pixels: SlotPixels::Gpu {
                frame,
                _buffer_object: buffer_object,
            },
            buffer,
        })
    }

    fn submit(&mut self, slot: usize) {
        let frame = self
            .capture
            .request_frame(&self.session, &self.ring[slot].buffer);
        self.in_flight = Some((frame, slot));
    }

    fn renegotiate(&mut self) -> Result<(), CaptureError> {
        let constraints = self.capture.await_constraints()?;
        self.allocate_ring(&constraints)?;
        self.submit(0);
        Ok(())
    }
}

impl FrameStream for WaylandStream<'_> {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<CapturedFrame<'_>>, CaptureError> {
        let Some((frame, slot)) = self.in_flight.take() else {
            return Err(CaptureError::Stopped);
        };
        let status = match self.capture.take_frame_status(&frame) {
            Some(status) => status,
            None => {
                self.capture.dispatch_timeout(timeout)?;
                match self.capture.take_frame_status(&frame) {
                    Some(status) => status,
                    None => {
                        self.in_flight = Some((frame, slot));
                        return Ok(None);
                    }
                }
            }
        };
        frame.destroy();
        match status {
            FrameStatus::Ready { presented_at } => {
                self.submit((slot + 1) % self.ring.len());
                Ok(Some(CapturedFrame {
                    pixels: self.ring[slot].frame_pixels(),
                    slot,
                    presented_at,
                }))
            }
            FrameStatus::Failed(FailureKind::BufferConstraints) => {
                self.renegotiate().map(|()| None)
            }
            FrameStatus::Failed(FailureKind::Stopped) => Err(CaptureError::Stopped),
            FrameStatus::Failed(FailureKind::Unknown) => {
                Err(CaptureError::Failed("compositor error"))
            }
        }
    }

    fn buffer_size(&self) -> (u32, u32) {
        self.size
    }

    fn render_node(&self) -> Option<&Path> {
        self.allocator
            .as_ref()
            .map(|allocator| allocator.node.as_path())
    }
}

impl Drop for WaylandStream<'_> {
    fn drop(&mut self) {
        if let Some((frame, _)) = self.in_flight.take() {
            frame.destroy();
        }
        self.ring.clear();
        self.session.destroy();
        let _ = self.capture.connection.flush();
    }
}
