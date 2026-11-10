use std::sync::Once;

use ffmpeg_next::ffi::{AV_HWFRAME_MAP_READ, AVHWDeviceType, AVPixelFormat};
use ffmpeg_next::{frame, picture};
use rustix::event::{PollFd, PollFlags, Timespec, poll};

use super::super::Crop;
use super::super::dmabuf::map_frame;
use super::super::filter::FilterChain;
use super::super::hardware::HardwareContext;
use super::super::slots::SlotCache;
use super::converted_rect;
use crate::capture::{CapturedFrame, DmabufFrame, PixelFormat, PixelRect};
use crate::record::RecordError;

const MAP_FLAGS: i32 = AV_HWFRAME_MAP_READ as i32;
const WRITER_TIMEOUT: Timespec = Timespec {
    tv_sec: 0,
    tv_nsec: 100_000_000,
};
static WRITER_TIMED_OUT: Once = Once::new();

/// The Vulkan device on the GPU behind `drm`, or the default one when it cannot be derived.
pub(super) fn vulkan_device(drm: &HardwareContext) -> Result<HardwareContext, RecordError> {
    drm.derive(AVHWDeviceType::AV_HWDEVICE_TYPE_VULKAN)
        .or_else(|error| {
            tracing::warn!(
                "cannot derive Vulkan from the capture's render node ({error}); \
                 falling back to the default Vulkan device"
            );
            HardwareContext::device(AVHWDeviceType::AV_HWDEVICE_TYPE_VULKAN, None)
        })
}

/// The conversion graph for one buffer size and format, with each ring slot mapped into Vulkan.
pub(super) struct VulkanInput {
    width: u32,
    height: u32,
    fourcc: u32,
    kept: PixelRect,
    drm_frames: HardwareContext,
    vulkan_frames: HardwareContext,
    images: SlotCache<frame::Video>,
    pub filters: FilterChain,
}

impl VulkanInput {
    pub fn new(
        drm: &HardwareContext,
        vulkan: &HardwareContext,
        dmabuf: &DmabufFrame,
        crop: Crop,
        (output_width, output_height): (u32, u32),
    ) -> Result<Self, RecordError> {
        let software_format = PixelFormat::from_drm_fourcc(dmabuf.fourcc)
            .and_then(red_first_format)
            .ok_or(RecordError::WrongBuffer)?;
        let drm_frames = drm.frames(
            AVPixelFormat::AV_PIX_FMT_DRM_PRIME,
            software_format,
            dmabuf.width,
            dmabuf.height,
        )?;
        let vulkan_frames =
            drm_frames.derive_frames(AVPixelFormat::AV_PIX_FMT_VULKAN, vulkan, MAP_FLAGS)?;
        let filters = FilterChain::new(
            AVPixelFormat::AV_PIX_FMT_VULKAN,
            dmabuf.width,
            dmabuf.height,
            Some(&vulkan_frames),
            &format!(
                "setparams=colorspace=bt709:range=pc,\
                 scale_vulkan=w={output_width}:h={output_height}:format=nv12:out_range=tv"
            ),
            0,
        )?;
        Ok(Self {
            width: dmabuf.width,
            height: dmabuf.height,
            fourcc: dmabuf.fourcc,
            kept: converted_rect(crop, dmabuf.width, dmabuf.height),
            drm_frames,
            vulkan_frames,
            images: SlotCache::new(),
            filters,
        })
    }

    pub fn accepts(&self, dmabuf: &DmabufFrame) -> bool {
        (self.width, self.height, self.fourcc) == (dmabuf.width, dmabuf.height, dmabuf.fourcc)
    }

    /// Feeds the Vulkan image of `captured`'s slot into the graph, stamped `timestamp` and
    /// marked as a `picture`.
    pub fn push(
        &mut self,
        captured: &CapturedFrame,
        dmabuf: &DmabufFrame,
        timestamp: i64,
        picture: picture::Type,
    ) -> Result<(), RecordError> {
        wait_for_writer(dmabuf)?;
        let image = self
            .images
            .get_or_try_insert(captured.slot, captured.generation, || {
                map_frame(
                    dmabuf,
                    &self.drm_frames,
                    &self.vulkan_frames,
                    AVPixelFormat::AV_PIX_FMT_VULKAN,
                    MAP_FLAGS,
                    self.kept,
                )
            })?;
        image.set_pts(Some(timestamp));
        image.set_kind(picture);
        let pushed = self.filters.push_reference(image);
        image.set_kind(picture::Type::None);
        pushed
    }
}

/// The FFmpeg layout of `format` for red-first layouts only: `scale_vulkan` samples every
/// 8-bit RGB layout as R8G8B8A8, so blue-first buffers would come out with red and blue swapped.
fn red_first_format(format: PixelFormat) -> Option<AVPixelFormat> {
    match format {
        PixelFormat::Rgbx => Some(AVPixelFormat::AV_PIX_FMT_RGB0),
        PixelFormat::Rgba => Some(AVPixelFormat::AV_PIX_FMT_RGBA),
        PixelFormat::Bgrx | PixelFormat::Bgra => None,
    }
}

/// Waits until the compositor has finished rendering into `dmabuf`, for at most
/// [`WRITER_TIMEOUT`]. FFmpeg only waits on the buffer's implicit fence when it maps it, and each
/// mapping serves many frames.
fn wait_for_writer(dmabuf: &DmabufFrame) -> Result<(), RecordError> {
    let mut fds = [PollFd::new(&dmabuf.fd, PollFlags::IN)];
    loop {
        match poll(&mut fds, Some(&WRITER_TIMEOUT)) {
            Ok(0) => {
                WRITER_TIMED_OUT.call_once(|| {
                    tracing::warn!("the compositor's rendering is still pending; encoding anyway");
                });
                return Ok(());
            }
            Ok(_) => return Ok(()),
            Err(rustix::io::Errno::INTR) => {}
            Err(error) => return Err(std::io::Error::from(error).into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_red_first_layouts() {
        assert_eq!(
            red_first_format(PixelFormat::Rgbx),
            Some(AVPixelFormat::AV_PIX_FMT_RGB0)
        );
        assert_eq!(red_first_format(PixelFormat::Bgrx), None);
        assert_eq!(red_first_format(PixelFormat::Bgra), None);
    }
}
