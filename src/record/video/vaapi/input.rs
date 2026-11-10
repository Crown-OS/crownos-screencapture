use ffmpeg_next::ffi::{AV_HWFRAME_MAP_DIRECT, AV_HWFRAME_MAP_READ, AVPixelFormat};
use ffmpeg_next::{frame, picture};

use super::super::Crop;
use super::super::dmabuf::map_frame;
use super::super::filter::FilterChain;
use super::super::hardware::HardwareContext;
use super::super::slots::SlotCache;
use crate::capture::{CapturedFrame, DmabufFrame, PixelFormat, PixelRect};
use crate::record::RecordError;

const MAP_FLAGS: i32 = AV_HWFRAME_MAP_READ as i32 | AV_HWFRAME_MAP_DIRECT as i32;

/// The conversion graph for one input size and format, with each ring slot mapped into VA-API.
pub struct VaapiInput {
    width: u32,
    height: u32,
    fourcc: u32,
    crop: PixelRect,
    drm_frames: HardwareContext,
    vaapi_frames: HardwareContext,
    surfaces: SlotCache<frame::Video>,
    pub filters: FilterChain,
}

impl VaapiInput {
    pub fn new(
        drm_device: &HardwareContext,
        vaapi_device: &HardwareContext,
        frame: &DmabufFrame,
        crop: Crop,
        (width, height): (u32, u32),
    ) -> Result<Self, RecordError> {
        let format = PixelFormat::from_drm_fourcc(frame.fourcc).ok_or(RecordError::WrongBuffer)?;
        let drm_frames = drm_device.frames(
            AVPixelFormat::AV_PIX_FMT_DRM_PRIME,
            software_format(format),
            frame.width,
            frame.height,
        )?;
        let vaapi_frames =
            drm_frames.derive_frames(AVPixelFormat::AV_PIX_FMT_VAAPI, vaapi_device, MAP_FLAGS)?;
        let filters = FilterChain::new(
            AVPixelFormat::AV_PIX_FMT_VAAPI,
            frame.width,
            frame.height,
            Some(&vaapi_frames),
            &format!(
                "scale_vaapi=w={width}:h={height}:format=nv12:out_range=tv:out_color_matrix=bt709"
            ),
            0,
        )?;
        Ok(Self {
            width: frame.width,
            height: frame.height,
            fourcc: frame.fourcc,
            crop: crop.within(frame.width, frame.height),
            drm_frames,
            vaapi_frames,
            surfaces: SlotCache::new(),
            filters,
        })
    }

    pub fn accepts(&self, frame: &DmabufFrame) -> bool {
        (self.width, self.height, self.fourcc) == (frame.width, frame.height, frame.fourcc)
    }

    /// Feeds the VA-API surface of `captured`'s slot into the graph, stamped `timestamp` and
    /// marked as a `picture`.
    pub fn push(
        &mut self,
        captured: &CapturedFrame,
        dmabuf: &DmabufFrame,
        timestamp: i64,
        picture: picture::Type,
    ) -> Result<(), RecordError> {
        let surface =
            self.surfaces
                .get_or_try_insert(captured.slot, captured.generation, || {
                    map_frame(
                        dmabuf,
                        &self.drm_frames,
                        &self.vaapi_frames,
                        AVPixelFormat::AV_PIX_FMT_VAAPI,
                        MAP_FLAGS,
                        self.crop,
                    )
                })?;
        surface.set_pts(Some(timestamp));
        surface.set_kind(picture);
        let pushed = self.filters.push_reference(surface);
        surface.set_kind(picture::Type::None);
        pushed
    }
}

fn software_format(format: PixelFormat) -> AVPixelFormat {
    match format {
        PixelFormat::Bgrx => AVPixelFormat::AV_PIX_FMT_BGR0,
        PixelFormat::Bgra => AVPixelFormat::AV_PIX_FMT_BGRA,
        PixelFormat::Rgbx => AVPixelFormat::AV_PIX_FMT_RGB0,
        PixelFormat::Rgba => AVPixelFormat::AV_PIX_FMT_RGBA,
    }
}
