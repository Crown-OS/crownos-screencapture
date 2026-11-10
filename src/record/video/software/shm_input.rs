use std::ffi::c_void;
use std::sync::Arc;

use ffmpeg_next::ffi::{AV_BUFFER_FLAG_READONLY, av_buffer_create};
use ffmpeg_next::{format, frame};

use crate::capture::ShmFrame;
use crate::record::RecordError;

unsafe extern "C" fn release_frame(holder: *mut c_void, _: *mut u8) {
    // SAFETY: `holder` is the strong reference `share_pixels` leaked into the buffer, and FFmpeg
    // calls this once, when the last reference to that buffer goes.
    drop(unsafe { Arc::from_raw(holder.cast_const().cast::<ShmFrame>()) });
}

/// A read-only frame pointing straight into the mapping of `shm`, without copying it. The frame
/// holds `shm` until FFmpeg drops its last reference to the pixels, so the capture stream does
/// not hand the slot back to the compositor before then. Mappings without
/// [`ShmFrame::READ_PADDING`] after the pixels are refused, since swscale reads past the end.
pub(super) fn share_pixels(
    shm: &Arc<ShmFrame>,
    pixel: format::Pixel,
) -> Result<frame::Video, RecordError> {
    if !shm.has_read_padding() {
        return Err(RecordError::WrongBuffer);
    }
    let stride = i32::try_from(shm.stride).map_err(std::io::Error::other)?;
    let bytes = shm.bytes();
    let holder = Arc::into_raw(Arc::clone(shm));
    let mut video = frame::Video::empty();
    video.set_format(pixel);
    video.set_width(shm.width);
    video.set_height(shm.height);
    // SAFETY: the buffer wraps the whole mapping, padding included, so reads up to
    // `READ_PADDING` bytes past the last pixel stay inside it. It is read-only and owns the
    // strong reference in `holder`, released by `release_frame`, so the mapping outlives every
    // FFmpeg reference to it and is never written through. Should the buffer not be created, the
    // reference is taken back here.
    // `video` owns a freshly allocated AVFrame, which takes the buffer and unreferences it when
    // dropped.
    unsafe {
        let buffer = av_buffer_create(
            bytes.as_ptr().cast_mut(),
            bytes.len(),
            Some(release_frame),
            holder.cast_mut().cast(),
            AV_BUFFER_FLAG_READONLY,
        );
        if buffer.is_null() {
            drop(Arc::from_raw(holder));
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
