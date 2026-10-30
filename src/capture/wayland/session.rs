use std::time::Duration;

use sctk::dispatch2::Dispatch2;
use wayland_client::protocol::wl_shm;
use wayland_client::{Connection, Proxy, QueueHandle, WEnum};
use wayland_protocols::ext::image_copy_capture::v1::client::{
    ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1},
    ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
};

use super::state::State;
use crate::capture::PixelFormat;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FrameStatus {
    Ready { presented_at: Duration },
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
    presented_at: std::sync::Mutex<Duration>,
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
        let Ok(mut presented_at) = self.presented_at.lock() else {
            return;
        };
        let status = match event {
            ext_image_copy_capture_frame_v1::Event::PresentationTime {
                tv_sec_hi,
                tv_sec_lo,
                tv_nsec,
            } => {
                let seconds = (u64::from(tv_sec_hi) << 32) | u64::from(tv_sec_lo);
                *presented_at = Duration::new(seconds, tv_nsec);
                return;
            }
            ext_image_copy_capture_frame_v1::Event::Ready => FrameStatus::Ready {
                presented_at: *presented_at,
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
