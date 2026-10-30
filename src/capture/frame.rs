use std::fs::File;
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::time::Duration;

use memmap2::Mmap;
use rustix::fs::{MemfdFlags, ftruncate, memfd_create};

use super::PixelRect;

const BYTES_PER_PIXEL: u32 = 4;

/// The byte order of a 32-bit pixel in memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Bgrx,
    Bgra,
    Rgbx,
    Rgba,
}

impl PixelFormat {
    pub const PREFERRED: [Self; 4] = [Self::Bgrx, Self::Rgbx, Self::Bgra, Self::Rgba];

    pub const fn drm_fourcc(self) -> u32 {
        u32::from_le_bytes(*match self {
            Self::Bgrx => b"XR24",
            Self::Bgra => b"AR24",
            Self::Rgbx => b"XB24",
            Self::Rgba => b"AB24",
        })
    }

    pub fn from_drm_fourcc(fourcc: u32) -> Option<Self> {
        Self::PREFERRED
            .into_iter()
            .find(|format| format.drm_fourcc() == fourcc)
    }

    pub const fn is_bgr(self) -> bool {
        matches!(self, Self::Bgrx | Self::Bgra)
    }

    pub const fn has_alpha(self) -> bool {
        matches!(self, Self::Bgra | Self::Rgba)
    }

    pub const fn short_name(self) -> &'static str {
        match self {
            Self::Bgrx => "bgrx",
            Self::Bgra => "bgra",
            Self::Rgbx => "rgbx",
            Self::Rgba => "rgba",
        }
    }

    pub fn from_short_name(name: &str) -> Option<Self> {
        Self::PREFERRED
            .into_iter()
            .find(|format| format.short_name() == name)
    }
}

/// Packed 32-bit pixels in a memfd, shareable with the compositor and child processes.
#[derive(Debug)]
pub struct ShmFrame {
    file: File,
    memory: Mmap,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: PixelFormat,
}

impl ShmFrame {
    pub fn allocate(width: u32, height: u32, format: PixelFormat) -> io::Result<Self> {
        let stride = width * BYTES_PER_PIXEL;
        let fd = memfd_create("crownos-screenshot", MemfdFlags::CLOEXEC)?;
        ftruncate(&fd, u64::from(stride) * u64::from(height))?;
        Self::from_fd(fd, width, height, stride, format)
    }

    pub fn from_fd(
        fd: OwnedFd,
        width: u32,
        height: u32,
        stride: u32,
        format: PixelFormat,
    ) -> io::Result<Self> {
        let file = File::from(fd);
        let length =
            usize::try_from(u64::from(stride) * u64::from(height)).map_err(io::Error::other)?;
        // SAFETY: the memfd is private to this process tree and only written by the compositor
        // before the frame is reported ready.
        let memory = unsafe { memmap2::MmapOptions::new().len(length).map(&file)? };
        Ok(Self {
            file,
            memory,
            width,
            height,
            stride,
            format,
        })
    }

    pub fn byte_len(&self) -> usize {
        self.memory.len()
    }

    /// The pixel bytes from the first pixel of `rect` to the last, rows `stride` apart.
    pub fn bytes_from(&self, rect: PixelRect) -> &[u8] {
        let start =
            rect.y as usize * self.stride as usize + rect.x as usize * BYTES_PER_PIXEL as usize;
        &self.memory[start..]
    }

    /// Rows of `rect`, each `rect.width * 4` bytes long.
    pub fn rows(&self, rect: PixelRect) -> impl Iterator<Item = &[u8]> {
        let stride = self.stride as usize;
        let start = rect.x as usize * BYTES_PER_PIXEL as usize;
        let length = rect.width as usize * BYTES_PER_PIXEL as usize;
        self.memory
            .chunks_exact(stride)
            .skip(rect.y as usize)
            .take(rect.height as usize)
            .map(move |row| &row[start..start + length])
    }

    pub fn full_rect(&self) -> PixelRect {
        PixelRect::full(self.width, self.height)
    }

    /// The pixels as tightly packed RGBA, which is what image widgets take.
    pub fn to_rgba(&self) -> Vec<u8> {
        let mut rgba = Vec::with_capacity(self.width as usize * self.height as usize * 4);
        for row in self.rows(self.full_rect()) {
            for pixel in row.chunks_exact(4) {
                let (red, blue) = if self.format.is_bgr() {
                    (pixel[2], pixel[0])
                } else {
                    (pixel[0], pixel[2])
                };
                let alpha = if self.format.has_alpha() {
                    pixel[3]
                } else {
                    u8::MAX
                };
                rgba.extend_from_slice(&[red, pixel[1], blue, alpha]);
            }
        }
        rgba
    }
}

impl AsFd for ShmFrame {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.file.as_fd()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PlaneLayout {
    pub offset: u32,
    pub stride: u32,
}

/// A GPU buffer the compositor renders captured frames into.
#[derive(Debug)]
pub struct DmabufFrame {
    pub fd: OwnedFd,
    pub width: u32,
    pub height: u32,
    pub fourcc: u32,
    pub modifier: u64,
    pub planes: Vec<PlaneLayout>,
}

#[derive(Debug, Clone, Copy)]
pub enum FramePixels<'frame> {
    Memory(&'frame ShmFrame),
    Gpu(&'frame DmabufFrame),
}

impl FramePixels<'_> {
    pub fn size(&self) -> (u32, u32) {
        match self {
            Self::Memory(frame) => (frame.width, frame.height),
            Self::Gpu(frame) => (frame.width, frame.height),
        }
    }
}

/// One frame of a capture stream.
#[derive(Debug, Clone, Copy)]
pub struct CapturedFrame<'frame> {
    pub pixels: FramePixels<'frame>,
    /// The ring slot holding the frame, stable for the life of the stream.
    pub slot: usize,
    /// When the compositor presented the frame, on `CLOCK_MONOTONIC`.
    pub presented_at: Duration,
}

/// Which kind of buffer a stream should capture into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferKind {
    Memory,
    Gpu,
}
