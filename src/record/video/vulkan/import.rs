use std::ffi::c_void;
use std::os::fd::{AsFd, AsRawFd};
use std::ptr;

use ffmpeg_next::ffi::{
    AV_HWFRAME_MAP_READ, AVBufferRef, AVDRMFrameDescriptor, AVFrame, AVHWDeviceType, AVPixelFormat,
    av_buffer_create, av_buffer_unref, av_frame_ref, av_frame_unref,
    av_hwdevice_ctx_create_derived, av_hwframe_ctx_create_derived, av_hwframe_map,
};
use ffmpeg_next::frame;
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fs::{SeekFrom, seek};

use super::super::filter::FilterChain;
use super::super::hardware::HardwareContext;
use super::converted_rect;
use crate::capture::{CapturedFrame, DmabufFrame, PixelFormat, PixelRect};
use crate::record::RecordError;
use crate::record::error::check;
use crate::record::video::Crop;

const WRITER_TIMEOUT: Timespec = Timespec {
    tv_sec: 0,
    tv_nsec: 100_000_000,
};

/// The Vulkan device on the GPU behind `drm`, or the default one when it cannot be derived.
pub(super) fn vulkan_device(drm: &HardwareContext) -> Result<HardwareContext, RecordError> {
    adopt_created("av_hwdevice_ctx_create_derived", |derived| {
        // SAFETY: `derived` receives a new reference on success; the DRM device is valid.
        unsafe {
            av_hwdevice_ctx_create_derived(
                derived,
                AVHWDeviceType::AV_HWDEVICE_TYPE_VULKAN,
                drm.as_ptr(),
                0,
            )
        }
    })
    .or_else(|error| {
        tracing::warn!(
            "cannot derive Vulkan from the capture's render node ({error}); \
             falling back to the default Vulkan device"
        );
        HardwareContext::device(AVHWDeviceType::AV_HWDEVICE_TYPE_VULKAN, None)
    })
}

/// Owns the buffer reference an FFmpeg constructor hands back through its out-parameter.
fn adopt_created(
    call: &'static str,
    create: impl FnOnce(&mut *mut AVBufferRef) -> i32,
) -> Result<HardwareContext, RecordError> {
    let mut created = ptr::null_mut();
    check(create(&mut created), call)?;
    let owned = HardwareContext::share(created);
    // SAFETY: `created` is the constructor's reference, which `owned` now duplicates.
    unsafe { av_buffer_unref(&mut created) };
    owned
}

/// The Vulkan mapping of one ring slot, valid until the ring is reallocated.
struct SlotMapping {
    generation: u64,
    frame: frame::Video,
}

/// The mapping and conversion state for one buffer size and format.
pub(super) struct VulkanInput {
    width: u32,
    height: u32,
    fourcc: u32,
    kept: PixelRect,
    slots: Vec<Option<SlotMapping>>,
    pushed: frame::Video,
    filters: FilterChain,
    drm_frames: HardwareContext,
    vulkan_frames: HardwareContext,
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
        let vulkan_frames = adopt_created("av_hwframe_ctx_create_derived", |derived| {
            // SAFETY: `derived` receives a new reference on success; both contexts are valid.
            unsafe {
                av_hwframe_ctx_create_derived(
                    derived,
                    AVPixelFormat::AV_PIX_FMT_VULKAN,
                    vulkan.as_ptr(),
                    drm_frames.as_ptr(),
                    AV_HWFRAME_MAP_READ as i32,
                )
            }
        })?;
        let filters = FilterChain::new(
            AVPixelFormat::AV_PIX_FMT_VULKAN,
            dmabuf.width,
            dmabuf.height,
            Some(&vulkan_frames),
            &format!(
                "setparams=colorspace=bt709:range=pc,\
                 scale_vulkan=w={output_width}:h={output_height}:format=nv12:out_range=tv"
            ),
        )?;
        Ok(Self {
            width: dmabuf.width,
            height: dmabuf.height,
            fourcc: dmabuf.fourcc,
            kept: converted_rect(crop, dmabuf.width, dmabuf.height),
            slots: Vec::new(),
            pushed: frame::Video::empty(),
            filters,
            drm_frames,
            vulkan_frames,
        })
    }

    pub fn accepts(&self, dmabuf: &DmabufFrame) -> bool {
        (self.width, self.height, self.fourcc) == (dmabuf.width, dmabuf.height, dmabuf.fourcc)
    }

    /// Feeds the frame in `captured`'s slot to the scaler, stamped `timestamp`.
    pub fn push(
        &mut self,
        captured: &CapturedFrame<'_>,
        dmabuf: &DmabufFrame,
        timestamp: i64,
    ) -> Result<(), RecordError> {
        wait_for_writer(dmabuf)?;
        let mapped = self.mapping(captured.slot, captured.generation, dmabuf)?;
        // SAFETY: `pushed` is released before it takes a new reference to the cached mapping,
        // which stays alive in `self.slots`.
        unsafe {
            let pushed = self.pushed.as_mut_ptr();
            av_frame_unref(pushed);
            check(av_frame_ref(pushed, mapped), "av_frame_ref")?;
            (*pushed).pts = timestamp;
        }
        self.filters.push(&self.pushed)
    }

    pub fn pull(&mut self, output: &mut frame::Video) -> Result<bool, RecordError> {
        self.filters.pull(output)
    }

    pub fn output_frames(&mut self) -> Result<HardwareContext, RecordError> {
        self.filters.output_frames()
    }

    fn mapping(
        &mut self,
        slot: usize,
        generation: u64,
        dmabuf: &DmabufFrame,
    ) -> Result<*const AVFrame, RecordError> {
        if self.slots.len() <= slot {
            self.slots.resize_with(slot + 1, || None);
        }
        let cached = &mut self.slots[slot];
        let current = match cached.take() {
            Some(mapping) if mapping.generation == generation => mapping,
            _ => SlotMapping {
                generation,
                frame: map_dmabuf(&self.drm_frames, &self.vulkan_frames, dmabuf, self.kept)?,
            },
        };
        // SAFETY: the frame stays cached in `self.slots` while the caller references it.
        Ok(unsafe { cached.insert(current).frame.as_ptr() })
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

/// Waits until the compositor has finished rendering into `dmabuf`. FFmpeg only waits on the
/// buffer's implicit fence when it maps it, and each mapping serves many frames.
fn wait_for_writer(dmabuf: &DmabufFrame) -> Result<(), RecordError> {
    let mut fds = [PollFd::new(&dmabuf.fd, PollFlags::IN)];
    poll(&mut fds, Some(&WRITER_TIMEOUT)).map_err(std::io::Error::from)?;
    Ok(())
}

/// Maps `dmabuf` into a Vulkan image showing the `kept` part of it.
fn map_dmabuf(
    drm_frames: &HardwareContext,
    vulkan_frames: &HardwareContext,
    dmabuf: &DmabufFrame,
    kept: PixelRect,
) -> Result<frame::Video, RecordError> {
    let descriptor = Box::into_raw(Box::new(drm_descriptor(dmabuf)?));
    let mut drm = frame::Video::empty();
    let mut vulkan = frame::Video::empty();
    // SAFETY: the buffer wrapping `descriptor` owns it and frees it once the last reference to
    // the DRM frame, held by the Vulkan mapping, is gone; both frames are freshly allocated.
    unsafe {
        let source = drm.as_mut_ptr();
        (*source).buf[0] = av_buffer_create(
            descriptor.cast(),
            size_of::<AVDRMFrameDescriptor>(),
            Some(free_descriptor),
            ptr::null_mut(),
            0,
        );
        if (*source).buf[0].is_null() {
            drop(Box::from_raw(descriptor));
            return Err(RecordError::FfmpegCall {
                call: "av_buffer_create",
                code: -1,
            });
        }
        (*source).data[0] = descriptor.cast();
        (*source).format = AVPixelFormat::AV_PIX_FMT_DRM_PRIME as i32;
        (*source).width = dmabuf.width as i32;
        (*source).height = dmabuf.height as i32;
        (*source).hw_frames_ctx = drm_frames.new_reference();
        let mapped = vulkan.as_mut_ptr();
        (*mapped).format = AVPixelFormat::AV_PIX_FMT_VULKAN as i32;
        (*mapped).hw_frames_ctx = vulkan_frames.new_reference();
        check(
            av_hwframe_map(mapped, source, AV_HWFRAME_MAP_READ as i32),
            "av_hwframe_map",
        )?;
        (*mapped).crop_left = kept.x as usize;
        (*mapped).crop_top = kept.y as usize;
        (*mapped).crop_right = (dmabuf.width - kept.x - kept.width) as usize;
        (*mapped).crop_bottom = (dmabuf.height - kept.y - kept.height) as usize;
    }
    Ok(vulkan)
}

unsafe extern "C" fn free_descriptor(_: *mut c_void, descriptor: *mut u8) {
    // SAFETY: the buffer was created around a descriptor leaked from a Box in `map_dmabuf`.
    drop(unsafe { Box::from_raw(descriptor.cast::<AVDRMFrameDescriptor>()) });
}

fn drm_descriptor(frame: &DmabufFrame) -> Result<AVDRMFrameDescriptor, RecordError> {
    let object_size = seek(frame.fd.as_fd(), SeekFrom::End(0)).map_err(std::io::Error::from)?;
    // SAFETY: AVDRMFrameDescriptor is plain data for which all zeroes is valid.
    let mut descriptor: AVDRMFrameDescriptor = unsafe { std::mem::zeroed() };
    descriptor.nb_objects = 1;
    descriptor.objects[0].fd = frame.fd.as_raw_fd();
    descriptor.objects[0].size = object_size as usize;
    descriptor.objects[0].format_modifier = frame.modifier;
    descriptor.nb_layers = 1;
    let layer = &mut descriptor.layers[0];
    layer.format = frame.fourcc;
    for (target, plane) in layer.planes.iter_mut().zip(&frame.planes) {
        target.object_index = 0;
        target.offset = plane.offset as isize;
        target.pitch = plane.stride as isize;
        layer.nb_planes += 1;
    }
    Ok(descriptor)
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
