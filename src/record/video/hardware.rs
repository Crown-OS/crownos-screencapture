use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::ptr::{self, NonNull};

use ffmpeg_next::ffi::{
    AVBufferRef, AVHWDeviceType, AVHWFramesContext, AVPixelFormat, av_buffer_ref, av_buffer_unref,
    av_hwdevice_ctx_create, av_hwframe_ctx_alloc, av_hwframe_ctx_init,
};

use crate::record::{RecordError, error::check};

/// An owned reference to an FFmpeg hardware device or frame pool.
#[derive(Debug)]
pub struct HardwareContext(NonNull<AVBufferRef>);

// SAFETY: FFmpeg buffer references are reference counted atomically and this handle is only
// used through `&self`/`&mut self` from one thread at a time.
unsafe impl Send for HardwareContext {}

impl HardwareContext {
    /// Opens a device of `kind` on `node`, or the default device when `node` is `None`.
    pub fn device(kind: AVHWDeviceType, node: Option<&Path>) -> Result<Self, RecordError> {
        let node = node
            .map(|path| CString::new(path.as_os_str().as_bytes()))
            .transpose()
            .map_err(std::io::Error::other)?;
        let mut device = ptr::null_mut();
        // SAFETY: `device` receives a new reference on success; the node string outlives the call.
        let code = unsafe {
            av_hwdevice_ctx_create(
                &mut device,
                kind,
                node.as_ref().map_or(ptr::null(), |node| node.as_ptr()),
                ptr::null_mut(),
                0,
            )
        };
        check(code, "av_hwdevice_ctx_create")?;
        Self::adopt(device, "av_hwdevice_ctx_create")
    }

    /// A pool description for frames of `format` backed by `software_format` on this device.
    pub fn frames(
        &self,
        format: AVPixelFormat,
        software_format: AVPixelFormat,
        width: u32,
        height: u32,
    ) -> Result<Self, RecordError> {
        // SAFETY: the device reference is valid; the new frames context is configured before init.
        unsafe {
            let frames = Self::adopt(
                av_hwframe_ctx_alloc(self.0.as_ptr()),
                "av_hwframe_ctx_alloc",
            )?;
            let context = (*frames.0.as_ptr()).data.cast::<AVHWFramesContext>();
            (*context).format = format;
            (*context).sw_format = software_format;
            (*context).width = width as i32;
            (*context).height = height as i32;
            check(
                av_hwframe_ctx_init(frames.0.as_ptr()),
                "av_hwframe_ctx_init",
            )?;
            Ok(frames)
        }
    }

    /// Takes a new reference to `buffer`, which stays owned by its holder.
    pub fn share(buffer: *mut AVBufferRef) -> Result<Self, RecordError> {
        // SAFETY: av_buffer_ref accepts any valid buffer reference and returns null on failure.
        Self::adopt(unsafe { av_buffer_ref(buffer) }, "av_buffer_ref")
    }

    /// A new reference for FFmpeg to own.
    pub fn new_reference(&self) -> *mut AVBufferRef {
        // SAFETY: self holds a valid reference.
        unsafe { av_buffer_ref(self.0.as_ptr()) }
    }

    pub fn as_ptr(&self) -> *mut AVBufferRef {
        self.0.as_ptr()
    }

    fn adopt(buffer: *mut AVBufferRef, call: &'static str) -> Result<Self, RecordError> {
        NonNull::new(buffer)
            .map(Self)
            .ok_or(RecordError::FfmpegCall { call, code: -1 })
    }
}

impl Drop for HardwareContext {
    fn drop(&mut self) {
        let mut buffer = self.0.as_ptr();
        // SAFETY: self owns exactly one reference.
        unsafe { av_buffer_unref(&mut buffer) };
    }
}
