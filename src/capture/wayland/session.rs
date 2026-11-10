use std::time::Duration;

use sctk::dispatch2::Dispatch2;
use wayland_client::protocol::wl_shm;
use wayland_client::{Connection, Proxy, QueueHandle, WEnum};
use wayland_protocols::ext::image_copy_capture::v1::client::{
    ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1},
    ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
};

use super::state::State;
use crate::capture::{PixelFormat, PixelRect};

/// What the compositor accepts for a session's buffers.
#[derive(Debug, Default, Clone)]
pub(super) struct Constraints {
    pub width: u32,
    pub height: u32,
    pub shm_formats: Vec<PixelFormat>,
    pub dmabuf_device: Option<u64>,
    pub dmabuf_formats: Vec<(PixelFormat, Vec<u64>)>,
}

impl Constraints {
    pub fn shm_format(&self) -> Option<PixelFormat> {
        PixelFormat::PREFERRED
            .into_iter()
            .find(|format| self.shm_formats.contains(format))
    }

    pub fn dmabuf_format(&self) -> Option<&(PixelFormat, Vec<u64>)> {
        PixelFormat::PREFERRED.iter().find_map(|preferred| {
            self.dmabuf_formats
                .iter()
                .find(|(format, _)| format == preferred)
        })
    }
}

#[derive(Debug, Default)]
pub(super) enum SessionStatus {
    #[default]
    Negotiating,
    Ready(Constraints),
    Stopped,
}

#[derive(Debug, Default)]
pub(super) struct SessionData {
    pending: std::sync::Mutex<Constraints>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum FrameStatus {
    Ready {
        presented_at: Duration,
        damage: Vec<PixelRect>,
    },
    Failed(FailureKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FailureKind {
    Unknown,
    BufferConstraints,
    Stopped,
}

#[derive(Debug, Default)]
pub(super) struct FrameData {
    pending: std::sync::Mutex<PendingFrame>,
}

#[derive(Debug, Default)]
struct PendingFrame {
    presented_at: Duration,
    damage: Vec<PixelRect>,
}

impl Dispatch2<ExtImageCopyCaptureSessionV1, State> for SessionData {
    fn event(
        &self,
        state: &mut State,
        _: &ExtImageCopyCaptureSessionV1,
        event: ext_image_copy_capture_session_v1::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        let Ok(mut pending) = self.pending.lock() else {
            return;
        };
        match event {
            ext_image_copy_capture_session_v1::Event::BufferSize { width, height } => {
                pending.width = width;
                pending.height = height;
            }
            ext_image_copy_capture_session_v1::Event::ShmFormat {
                format: WEnum::Value(format),
            } => {
                pending.shm_formats.extend(pixel_format_from_shm(format));
            }
            ext_image_copy_capture_session_v1::Event::DmabufDevice { device } => {
                pending.dmabuf_device = device
                    .get(..8)
                    .and_then(|bytes| bytes.try_into().ok())
                    .map(u64::from_ne_bytes);
            }
            ext_image_copy_capture_session_v1::Event::DmabufFormat { format, modifiers } => {
                if let Some(format) = PixelFormat::from_drm_fourcc(format) {
                    let modifiers = modifiers
                        .chunks_exact(8)
                        .filter_map(|bytes| bytes.try_into().ok().map(u64::from_ne_bytes))
                        .collect();
                    pending.dmabuf_formats.push((format, modifiers));
                }
            }
            ext_image_copy_capture_session_v1::Event::Done => {
                state.session = SessionStatus::Ready(std::mem::take(&mut *pending));
            }
            ext_image_copy_capture_session_v1::Event::Stopped => {
                state.session = SessionStatus::Stopped
            }
            _ => {}
        }
    }
}

impl Dispatch2<ExtImageCopyCaptureFrameV1, State> for FrameData {
    fn event(
        &self,
        state: &mut State,
        frame: &ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        let Ok(mut pending) = self.pending.lock() else {
            return;
        };
        let status = match event {
            ext_image_copy_capture_frame_v1::Event::PresentationTime {
                tv_sec_hi,
                tv_sec_lo,
                tv_nsec,
            } => {
                let seconds = (u64::from(tv_sec_hi) << 32) | u64::from(tv_sec_lo);
                pending.presented_at = Duration::new(seconds, tv_nsec);
                return;
            }
            ext_image_copy_capture_frame_v1::Event::Damage {
                x,
                y,
                width,
                height,
            } => {
                pending.damage.push(damage_rect(x, y, width, height));
                return;
            }
            ext_image_copy_capture_frame_v1::Event::Ready => FrameStatus::Ready {
                presented_at: pending.presented_at,
                damage: std::mem::take(&mut pending.damage),
            },
            ext_image_copy_capture_frame_v1::Event::Failed { reason } => {
                FrameStatus::Failed(match reason {
                    WEnum::Value(
                        ext_image_copy_capture_frame_v1::FailureReason::BufferConstraints,
                    ) => FailureKind::BufferConstraints,
                    WEnum::Value(ext_image_copy_capture_frame_v1::FailureReason::Stopped) => {
                        FailureKind::Stopped
                    }
                    _ => FailureKind::Unknown,
                })
            }
            _ => return,
        };
        state.frames.insert(frame.id(), status);
    }
}

/// The part of a damage rectangle at non-negative buffer coordinates.
fn damage_rect(x: i32, y: i32, width: i32, height: i32) -> PixelRect {
    let span = |start: i32, length: i32| {
        let first = start.max(0);
        let end = start.saturating_add(length).max(first);
        (first.unsigned_abs(), end.abs_diff(first))
    };
    let (x, width) = span(x, width);
    let (y, height) = span(y, height);
    PixelRect {
        x,
        y,
        width,
        height,
    }
}

fn pixel_format_from_shm(format: wl_shm::Format) -> Option<PixelFormat> {
    match format {
        wl_shm::Format::Xrgb8888 => Some(PixelFormat::Bgrx),
        wl_shm::Format::Argb8888 => Some(PixelFormat::Bgra),
        wl_shm::Format::Xbgr8888 => Some(PixelFormat::Rgbx),
        wl_shm::Format::Abgr8888 => Some(PixelFormat::Rgba),
        _ => None,
    }
}

pub(super) fn shm_format(format: PixelFormat) -> wl_shm::Format {
    match format {
        PixelFormat::Bgrx => wl_shm::Format::Xrgb8888,
        PixelFormat::Bgra => wl_shm::Format::Argb8888,
        PixelFormat::Rgbx => wl_shm::Format::Xbgr8888,
        PixelFormat::Rgba => wl_shm::Format::Abgr8888,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_damage_inside_the_buffer() {
        assert_eq!(
            damage_rect(10, 20, 30, 40),
            PixelRect {
                x: 10,
                y: 20,
                width: 30,
                height: 40
            }
        );
        assert_eq!(
            damage_rect(-10, -5, 30, 4),
            PixelRect {
                x: 0,
                y: 0,
                width: 20,
                height: 0
            }
        );
    }
}
