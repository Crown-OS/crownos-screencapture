use std::os::fd::{AsFd, AsRawFd};
use std::path::Path;

use crownos_config::schema::VideoCodec;
use ffmpeg_next::ffi::{AVDRMFrameDescriptor, AVHWDeviceType, AVPixelFormat, av_buffer_create};
use ffmpeg_next::{Dictionary, encoder, format, frame};
use rustix::fs::{SeekFrom, fstat, seek};

use super::filter::FilterChain;
use super::hardware::HardwareContext;
use super::{Crop, VideoBackend, VideoEncoder, VideoSettings, drain, flush, open_encoder};
use crate::capture::{BufferKind, CapturedFrame, DmabufFrame, FramePixels, PixelFormat};
use crate::record::{EncodedStream, PacketSink, RecordError};

/// Hardware encoding through VA-API, importing the compositor's dmabufs without a copy.
#[derive(Debug, Default, Clone, Copy)]
pub struct VaapiBackend;

impl VideoBackend for VaapiBackend {
    fn name(&self) -> &'static str {
        "VA-API"
    }

    fn buffer_kind(&self) -> BufferKind {
        BufferKind::Gpu
    }

    fn open(
        &self,
        first: &CapturedFrame<'_>,
        render_node: Option<&Path>,
        settings: &VideoSettings,
    ) -> Result<Box<dyn VideoEncoder>, RecordError> {
        let FramePixels::Gpu(dmabuf) = first.pixels else {
            return Err(RecordError::WrongBuffer);
        };
        let device = HardwareContext::device(AVHWDeviceType::AV_HWDEVICE_TYPE_DRM, render_node)?;
        let output = settings.crop.within(dmabuf.width, dmabuf.height);
        let output_size = (output.width, output.height);
        let mut input = VaapiInput::new(&device, dmabuf, output_size)?;
        let frames = input.filters.output_frames()?;
        let encoder = open_encoder(
            encoder_name(settings.codec),
            format::Pixel::VAAPI,
            output.width,
            output.height,
            settings,
            // SAFETY: the encoder context takes ownership of the new reference.
            |video| unsafe { (*video.as_mut_ptr()).hw_frames_ctx = frames.new_reference() },
            Dictionary::new(),
        )?;
        Ok(Box::new(VaapiEncoder {
            device,
            input,
            encoder,
            descriptors: Vec::new(),
            crop: settings.crop,
            output_size,
            converted: frame::Video::empty(),
        }))
    }
}

fn encoder_name(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::H264 => "h264_vaapi",
        VideoCodec::Hevc => "hevc_vaapi",
        VideoCodec::Av1 => "av1_vaapi",
    }
}

/// The conversion graph for one input size and format.
struct VaapiInput {
    width: u32,
    height: u32,
    fourcc: u32,
    frames: HardwareContext,
    filters: FilterChain,
}

impl VaapiInput {
    fn new(
        device: &HardwareContext,
        frame: &DmabufFrame,
        (width, height): (u32, u32),
    ) -> Result<Self, RecordError> {
        let format = PixelFormat::from_drm_fourcc(frame.fourcc).ok_or(RecordError::WrongBuffer)?;
        let frames = device.frames(
            AVPixelFormat::AV_PIX_FMT_DRM_PRIME,
            software_format(format),
            frame.width,
            frame.height,
        )?;
        let filters = FilterChain::new(
            AVPixelFormat::AV_PIX_FMT_DRM_PRIME,
            frame.width,
            frame.height,
            Some(&frames),
            &format!(
                "hwmap=mode=direct:derive_device=vaapi,\
                 scale_vaapi=w={width}:h={height}:format=nv12:out_range=tv:out_color_matrix=bt709"
            ),
        )?;
        Ok(Self {
            width: frame.width,
            height: frame.height,
            fourcc: frame.fourcc,
            frames,
            filters,
        })
    }

    fn accepts(&self, frame: &DmabufFrame) -> bool {
        (self.width, self.height, self.fourcc) == (frame.width, frame.height, frame.fourcc)
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

/// The DRM description of a ring buffer, kept alive while FFmpeg maps it.
struct SlotDescriptor {
    identity: (u64, u64),
    descriptor: Box<AVDRMFrameDescriptor>,
}

struct VaapiEncoder {
    device: HardwareContext,
    input: VaapiInput,
    encoder: encoder::Video,
    descriptors: Vec<Option<SlotDescriptor>>,
    crop: Crop,
    output_size: (u32, u32),
    converted: frame::Video,
}

// SAFETY: every FFmpeg object here is owned by the encoder and used from one thread at a time.
unsafe impl Send for VaapiEncoder {}

impl VaapiEncoder {
    fn descriptor(
        &mut self,
        slot: usize,
        frame: &DmabufFrame,
    ) -> Result<*mut AVDRMFrameDescriptor, RecordError> {
        let stat = fstat(frame.fd.as_fd()).map_err(std::io::Error::from)?;
        let identity = (stat.st_dev, stat.st_ino);
        if self.descriptors.len() <= slot {
            self.descriptors.resize_with(slot + 1, || None);
        }
        let cached = &mut self.descriptors[slot];
        if cached
            .as_ref()
            .is_none_or(|cached| cached.identity != identity)
        {
            *cached = Some(SlotDescriptor {
                identity,
                descriptor: Box::new(drm_descriptor(frame)?),
            });
        }
        cached
            .as_mut()
            .map(|cached| std::ptr::from_mut(cached.descriptor.as_mut()))
            .ok_or(RecordError::WrongBuffer)
    }

    fn drm_frame(
        &mut self,
        captured: &CapturedFrame<'_>,
        dmabuf: &DmabufFrame,
        timestamp: i64,
    ) -> Result<frame::Video, RecordError> {
        let descriptor = self.descriptor(captured.slot, dmabuf)?;
        let crop = self.crop.within(dmabuf.width, dmabuf.height);
        let mut drm = frame::Video::empty();
        // SAFETY: the descriptor is boxed in `self.descriptors` and outlives every mapping of
        // its slot; the buffer wrapping it never frees it.
        unsafe {
            let raw = drm.as_mut_ptr();
            (*raw).format = AVPixelFormat::AV_PIX_FMT_DRM_PRIME as i32;
            (*raw).width = dmabuf.width as i32;
            (*raw).height = dmabuf.height as i32;
            (*raw).data[0] = descriptor.cast();
            (*raw).buf[0] = av_buffer_create(
                descriptor.cast(),
                size_of::<AVDRMFrameDescriptor>(),
                Some(keep_descriptor),
                std::ptr::null_mut(),
                0,
            );
            (*raw).hw_frames_ctx = self.input.frames.new_reference();
            (*raw).pts = timestamp;
            (*raw).crop_left = crop.x as usize;
            (*raw).crop_top = crop.y as usize;
            (*raw).crop_right = (dmabuf.width - crop.x - crop.width) as usize;
            (*raw).crop_bottom = (dmabuf.height - crop.y - crop.height) as usize;
        }
        Ok(drm)
    }
}

unsafe extern "C" fn keep_descriptor(_: *mut std::ffi::c_void, _: *mut u8) {}

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

impl EncodedStream for VaapiEncoder {
    fn context(&self) -> &ffmpeg_next::codec::Context {
        &self.encoder
    }
}

impl VideoEncoder for VaapiEncoder {
    fn encode(
        &mut self,
        frame: &CapturedFrame<'_>,
        timestamp: i64,
        sink: &mut dyn PacketSink,
    ) -> Result<(), RecordError> {
        let FramePixels::Gpu(dmabuf) = frame.pixels else {
            return Err(RecordError::WrongBuffer);
        };
        if !self.input.accepts(dmabuf) {
            self.input = VaapiInput::new(&self.device, dmabuf, self.output_size)?;
        }
        let drm = self.drm_frame(frame, dmabuf, timestamp)?;
        self.input.filters.push(&drm)?;
        while self.input.filters.pull(&mut self.converted)? {
            self.encoder.send_frame(&self.converted)?;
            drain(&mut self.encoder, sink)?;
        }
        Ok(())
    }

    fn finish(&mut self, sink: &mut dyn PacketSink) -> Result<(), RecordError> {
        flush(&mut self.encoder, sink)
    }
}
