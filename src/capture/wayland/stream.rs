use std::os::fd::{AsFd, BorrowedFd};
use std::path::Path;
use std::sync::Arc;
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
use super::session::{Constraints, FailureKind, FrameStatus, SessionStatus};
use super::state::Silent;
use crate::capture::{
    BufferKind, CaptureError, CaptureTarget, CapturedFrame, CursorMode, DmabufFrame, FramePacer,
    FramePixels, FrameStream, PixelFormat, ShmFrame, monotonic_now,
};

/// Enough buffers that the compositor keeps rendering while the encoder holds a few frames.
const RING_SIZE: usize = 4;

/// A slot's pixel memory, shared with the frames handed out from it; the Wayland objects and
/// the GBM buffer stay with the slot on the capture thread.
enum SlotPixels {
    Memory {
        frame: Arc<ShmFrame>,
        pool: WlShmPool,
    },
    Gpu {
        frame: Arc<DmabufFrame>,
        _buffer_object: BufferObject<()>,
    },
}

struct Slot {
    pixels: SlotPixels,
    buffer: WlBuffer,
}

impl Slot {
    fn frame_pixels(&self) -> FramePixels {
        match &self.pixels {
            SlotPixels::Memory { frame, .. } => FramePixels::Memory(Arc::clone(frame)),
            SlotPixels::Gpu { frame, .. } => FramePixels::Gpu(Arc::clone(frame)),
        }
    }

    /// Whether every frame handed out from this slot has been dropped, so the compositor may
    /// overwrite it. Unique ownership is checked with acquire ordering, so the readers' last
    /// accesses happen before the slot is attached again.
    fn is_released(&mut self) -> bool {
        match &mut self.pixels {
            SlotPixels::Memory { frame, .. } => Arc::get_mut(frame).is_some(),
            SlotPixels::Gpu { frame, .. } => Arc::get_mut(frame).is_some(),
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
    formats: &'static [PixelFormat],
    allocator: Option<GpuAllocator>,
    ring: Vec<Slot>,
    in_flight: Option<(ExtImageCopyCaptureFrameV1, usize)>,
    next_slot: usize,
    pacer: FramePacer,
    size: (u32, u32),
    generation: u64,
}

impl<'capture> WaylandStream<'capture> {
    pub(super) fn start(
        capture: &'capture mut WaylandCapture,
        target: &CaptureTarget,
        cursor: CursorMode,
        kind: BufferKind,
        formats: &'static [PixelFormat],
    ) -> Result<Self, CaptureError> {
        let (session, constraints) = capture.open_session(target, cursor)?;
        let mut stream = Self {
            capture,
            session,
            kind,
            formats,
            allocator: None,
            ring: Vec::new(),
            in_flight: None,
            next_slot: 0,
            pacer: FramePacer::default(),
            size: (0, 0),
            generation: 0,
        };
        stream.allocate_ring(&constraints)?;
        stream.submit(0);
        Ok(stream)
    }

    fn allocate_ring(&mut self, constraints: &Constraints) -> Result<(), CaptureError> {
        self.ring.clear();
        self.generation += 1;
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
            .shm_format(self.formats)
            .ok_or(CaptureError::NoBufferFormat)?;
        let frame = ShmFrame::allocate(constraints.width, constraints.height, format)?;
        let (pool, buffer) = self.capture.shm_buffer(&frame);
        Ok(Slot {
            pixels: SlotPixels::Memory {
                frame: Arc::new(frame),
                pool,
            },
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
            .dmabuf_format(self.formats)
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
                frame: Arc::new(frame),
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

    /// Sends the next request once it is due and a slot is released. Otherwise returns how long
    /// until it is due, or `None` while a request is in flight or every slot is still held.
    fn submit_when_due(&mut self) -> Option<Duration> {
        if self.in_flight.is_some() {
            return None;
        }
        let slot = first_from(&mut self.ring, self.next_slot, Slot::is_released)?;
        let until_due = self.pacer.until_due(monotonic_now());
        if !until_due.is_zero() {
            return Some(until_due);
        }
        self.submit(slot);
        None
    }

    /// The slot and outcome of the request in flight, once the compositor has answered it.
    fn take_answer(&mut self) -> Option<(usize, FrameStatus)> {
        let (frame, slot) = self.in_flight.as_ref()?;
        let status = self.capture.take_frame_status(frame)?;
        let slot = *slot;
        frame.destroy();
        self.in_flight = None;
        Some((slot, status))
    }

    fn renegotiate(&mut self) -> Result<(), CaptureError> {
        let constraints = self.capture.await_constraints()?;
        self.allocate_ring(&constraints)?;
        self.next_slot = 0;
        Ok(())
    }
}

impl FrameStream for WaylandStream<'_> {
    fn next_frame(
        &mut self,
        wakers: &[BorrowedFd<'_>],
        timeout: Option<Duration>,
    ) -> Result<Option<CapturedFrame>, CaptureError> {
        let deadline = timeout.map(|timeout| monotonic_now() + timeout);
        loop {
            let until_due = self.submit_when_due();
            match self.take_answer() {
                Some((
                    slot,
                    FrameStatus::Ready {
                        presented_at,
                        damage,
                    },
                )) => {
                    self.pacer.mark(presented_at);
                    self.next_slot = (slot + 1) % self.ring.len();
                    let frame = CapturedFrame {
                        pixels: self.ring[slot].frame_pixels(),
                        slot,
                        generation: self.generation,
                        presented_at,
                        damage,
                    };
                    self.submit_when_due();
                    return Ok(Some(frame));
                }
                Some((_, FrameStatus::Failed(FailureKind::BufferConstraints))) => {
                    self.renegotiate()?;
                    continue;
                }
                Some((_, FrameStatus::Failed(FailureKind::Stopped))) => {
                    return Err(CaptureError::Stopped);
                }
                Some((_, FrameStatus::Failed(FailureKind::Unknown))) => {
                    return Err(CaptureError::Failed("compositor error"));
                }
                None => {}
            }
            if matches!(self.capture.state.session, SessionStatus::Stopped) {
                return Err(CaptureError::Stopped);
            }
            let remaining = deadline.map(|deadline| deadline.saturating_sub(monotonic_now()));
            if remaining.is_some_and(|remaining| remaining.is_zero()) {
                return Ok(None);
            }
            let wait = remaining.into_iter().chain(until_due).min();
            if self.capture.dispatch_until(wakers, wait)? {
                return Ok(None);
            }
        }
    }

    fn set_frame_interval(&mut self, interval: Duration) {
        self.pacer.set_interval(interval);
    }

    fn capacity(&self) -> usize {
        RING_SIZE
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

/// The first index from `start` on, wrapping around, whose item `accepts`: in a ring handed out in
/// order, the free slot that was released longest ago.
fn first_from<T>(
    items: &mut [T],
    start: usize,
    mut accepts: impl FnMut(&mut T) -> bool,
) -> Option<usize> {
    let count = items.len();
    (0..count)
        .map(|offset| (start + offset) % count)
        .find(|&index| accepts(&mut items[index]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_first_free_slot_from_the_start_wrapping_around() {
        let mut held = [true, false, true, false];
        let free = |slot: &mut bool| !*slot;
        assert_eq!(first_from(&mut held, 0, free), Some(1));
        assert_eq!(first_from(&mut held, 2, free), Some(3));
        held[3] = true;
        assert_eq!(first_from(&mut held, 2, free), Some(1));
        held[1] = true;
        assert_eq!(first_from(&mut held, 2, free), None);
        assert_eq!(first_from(&mut [], 0, free), None);
    }
}
