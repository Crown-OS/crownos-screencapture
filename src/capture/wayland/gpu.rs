use std::fs::{File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use gbm::{BufferObject, BufferObjectFlags};
use rustix::fs::{major, minor};

use crate::capture::{CaptureError, DmabufFrame, PixelFormat, PlaneLayout};

const DRM_FORMAT_MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;

/// Allocates capture buffers on the GPU the compositor renders with.
pub(super) struct GpuAllocator {
    device: gbm::Device<File>,
    pub node: PathBuf,
}

impl GpuAllocator {
    pub fn open(device_number: u64) -> Result<Self, CaptureError> {
        let node = render_node(device_number).ok_or(CaptureError::NoRenderNode(device_number))?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(rustix::fs::OFlags::CLOEXEC.bits() as i32)
            .open(&node)?;
        Ok(Self {
            device: gbm::Device::new(file)?,
            node,
        })
    }

    pub fn allocate(
        &self,
        width: u32,
        height: u32,
        format: PixelFormat,
        modifiers: &[u64],
    ) -> Result<(BufferObject<()>, DmabufFrame), CaptureError> {
        let fourcc =
            gbm::Format::try_from(format.drm_fourcc()).map_err(|_| CaptureError::NoBufferFormat)?;
        let explicit: Vec<_> = modifiers
            .iter()
            .copied()
            .filter(|&modifier| modifier != DRM_FORMAT_MOD_INVALID)
            .collect();
        let buffer = if explicit.is_empty() {
            self.device.create_buffer_object::<()>(
                width,
                height,
                fourcc,
                BufferObjectFlags::RENDERING,
            )?
        } else {
            self.device.create_buffer_object_with_modifiers2::<()>(
                width,
                height,
                fourcc,
                explicit.into_iter().map(gbm::Modifier::from),
                BufferObjectFlags::RENDERING,
            )?
        };
        let fd = buffer
            .fd()
            .map_err(|_| CaptureError::Failed("GBM buffer has no dmabuf"))?;
        let planes = (0..buffer.plane_count() as i32)
            .map(|plane| PlaneLayout {
                offset: buffer.offset(plane),
                stride: buffer.stride_for_plane(plane),
            })
            .collect();
        let frame = DmabufFrame {
            fd,
            width,
            height,
            fourcc: format.drm_fourcc(),
            modifier: buffer.modifier().into(),
            planes,
        };
        Ok((buffer, frame))
    }
}

/// The `/dev/dri/renderD*` node of the GPU whose primary or render node is `device_number`.
fn render_node(device_number: u64) -> Option<PathBuf> {
    let sysfs = format!(
        "/sys/dev/char/{}:{}/device/drm",
        major(device_number),
        minor(device_number)
    );
    std::fs::read_dir(sysfs)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .find(|name| name.to_string_lossy().starts_with("renderD"))
        .map(|name| PathBuf::from("/dev/dri").join(name))
}
