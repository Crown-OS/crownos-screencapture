use std::ffi::c_void;
use std::os::fd::{AsFd, AsRawFd};

use ffmpeg_next::ffi::{AVDRMFrameDescriptor, AVPixelFormat, av_buffer_create};
use ffmpeg_next::frame;
use rustix::fs::{SeekFrom, seek};

use super::super::hardware::HardwareContext;
use crate::capture::DmabufFrame;
use crate::record::RecordError;

/// A DRM_PRIME frame in `frames` describing `dmabuf`, owning its descriptor.
pub fn drm_frame(
    dmabuf: &DmabufFrame,
    frames: &HardwareContext,
) -> Result<frame::Video, RecordError> {
    let descriptor = Box::into_raw(Box::new(drm_descriptor(dmabuf)?));
    // SAFETY: the descriptor is a live heap allocation; on success the buffer owns it and
    // releases it through `free_descriptor`, otherwise it is reclaimed here.
    let buffer = unsafe {
        let buffer = av_buffer_create(
            descriptor.cast(),
            size_of::<AVDRMFrameDescriptor>(),
            Some(free_descriptor),
            std::ptr::null_mut(),
            0,
        );
        if buffer.is_null() {
            drop(Box::from_raw(descriptor));
            return Err(RecordError::FfmpegCall {
                call: "av_buffer_create",
                code: -1,
            });
        }
        buffer
    };
    let mut drm = frame::Video::empty();
    // SAFETY: the frame is blank and takes ownership of the buffer and the frames reference.
    unsafe {
        let raw = drm.as_mut_ptr();
        (*raw).format = AVPixelFormat::AV_PIX_FMT_DRM_PRIME as i32;
        (*raw).width = dmabuf.width as i32;
        (*raw).height = dmabuf.height as i32;
        (*raw).buf[0] = buffer;
        (*raw).data[0] = descriptor.cast();
        (*raw).hw_frames_ctx = frames.new_reference();
    }
    Ok(drm)
}

unsafe extern "C" fn free_descriptor(_: *mut c_void, descriptor: *mut u8) {
    // SAFETY: FFmpeg calls this once, when the last reference to the buffer created in
    // `drm_frame` from `Box::into_raw` is released.
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
