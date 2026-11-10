use std::ffi::c_void;
use std::ptr;

use ffmpeg_next::ffi::{AV_BUFFER_FLAG_READONLY, av_buffer_create};
use ffmpeg_next::{format, frame};

use crate::capture::ShmFrame;
use crate::record::RecordError;

unsafe extern "C" fn keep_mapping(_: *mut c_void, _: *mut u8) {}

/// A read-only frame pointing straight into the mapping of `shm`, without copying it.
///
/// # Safety
///
/// The frame and every reference FFmpeg takes to it must be done reading before the mapping of
/// `shm` is unmapped or rewritten.
pub(super) unsafe fn borrow_pixels(
    shm: &ShmFrame,
    pixel: format::Pixel,
) -> Result<frame::Video, RecordError> {
    let stride = i32::try_from(shm.stride).map_err(std::io::Error::other)?;
    let bytes = shm.bytes();
    let mut video = frame::Video::empty();
    video.set_format(pixel);
    video.set_width(shm.width);
    video.set_height(shm.height);
    // SAFETY: the buffer wraps `bytes` read-only with a free callback that leaves the mapping
    // alone, so FFmpeg never writes through it or frees it; `video` owns a freshly allocated
    // AVFrame, which takes the buffer and unreferences it when dropped.
    unsafe {
        let buffer = av_buffer_create(
            bytes.as_ptr().cast_mut(),
            bytes.len(),
            Some(keep_mapping),
            ptr::null_mut(),
            AV_BUFFER_FLAG_READONLY,
        );
        if buffer.is_null() {
            return Err(RecordError::FfmpegCall {
                call: "av_buffer_create",
                code: -1,
            });
        }
        let raw = video.as_mut_ptr();
        (*raw).buf[0] = buffer;
        (*raw).data[0] = (*buffer).data;
        (*raw).linesize[0] = stride;
    }
    Ok(video)
}
