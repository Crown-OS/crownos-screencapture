use std::ffi::CString;
use std::ptr;

use ffmpeg_next::ffi::{
    AVPixelFormat, AVRational, av_buffersink_get_hw_frames_ctx, av_buffersrc_parameters_alloc,
    av_buffersrc_parameters_set, av_free, avfilter_get_by_name, avfilter_graph_alloc_filter,
    avfilter_init_str,
};
use ffmpeg_next::{filter, frame};

use super::hardware::HardwareContext;
use crate::record::RecordError;
use crate::record::clock::RecordingClock;
use crate::record::error::check;

const SOURCE: &str = "in";
const SINK: &str = "out";

/// A `buffer → … → buffersink` filter graph converting captured frames for an encoder.
pub struct FilterChain {
    graph: filter::Graph,
}

// SAFETY: the graph is owned by one encoder and never shared.
unsafe impl Send for FilterChain {}

impl FilterChain {
    /// Builds `source → filters → sink` for frames of `format` at `width`×`height`, attached to
    /// `frames` when the frames live on a hardware device.
    pub fn new(
        format: AVPixelFormat,
        width: u32,
        height: u32,
        frames: Option<&HardwareContext>,
        filters: &str,
    ) -> Result<Self, RecordError> {
        let mut graph = filter::Graph::new();
        add_source(&mut graph, format, width, height, frames)?;
        graph.add(&find("buffersink")?, SINK, "")?;
        graph.output(SOURCE, 0)?.input(SINK, 0)?.parse(filters)?;
        graph.validate()?;
        Ok(Self { graph })
    }

    pub fn push(&mut self, input: &frame::Video) -> Result<(), RecordError> {
        let mut source = self
            .graph
            .get(SOURCE)
            .ok_or(RecordError::Ffmpeg(ffmpeg_next::Error::FilterNotFound))?;
        source.source().add(input)?;
        Ok(())
    }

    /// The next converted frame, or `false` when the graph needs more input.
    pub fn pull(&mut self, output: &mut frame::Video) -> Result<bool, RecordError> {
        let mut sink = self
            .graph
            .get(SINK)
            .ok_or(RecordError::Ffmpeg(ffmpeg_next::Error::FilterNotFound))?;
        match sink.sink().frame(output) {
            Ok(()) => Ok(true),
            Err(
                ffmpeg_next::Error::Other {
                    errno: ffmpeg_next::util::error::EAGAIN,
                }
                | ffmpeg_next::Error::Eof,
            ) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    /// The hardware frame pool the graph outputs into.
    pub fn output_frames(&mut self) -> Result<HardwareContext, RecordError> {
        let mut sink = self
            .graph
            .get(SINK)
            .ok_or(RecordError::Ffmpeg(ffmpeg_next::Error::FilterNotFound))?;
        // SAFETY: the sink belongs to a configured graph; the result is borrowed, so it is re-referenced.
        HardwareContext::share(unsafe { av_buffersink_get_hw_frames_ctx(sink.as_mut_ptr()) })
    }
}

/// Adds the `buffer` source, configured before it initialises: a hardware pixel format is only
/// accepted once its frame pool is attached.
fn add_source(
    graph: &mut filter::Graph,
    format: AVPixelFormat,
    width: u32,
    height: u32,
    frames: Option<&HardwareContext>,
) -> Result<(), RecordError> {
    let name = CString::new(SOURCE).map_err(std::io::Error::other)?;
    // SAFETY: the filter is allocated inside `graph`, which owns it; the parameters are freshly
    // allocated, only read by the call, then freed; the source takes its own frames reference.
    unsafe {
        let source = avfilter_graph_alloc_filter(
            graph.as_mut_ptr(),
            avfilter_get_by_name(c"buffer".as_ptr()),
            name.as_ptr(),
        );
        if source.is_null() {
            return Err(RecordError::NoEncoder("buffer"));
        }
        let parameters = av_buffersrc_parameters_alloc();
        if parameters.is_null() {
            return Err(RecordError::FfmpegCall {
                call: "av_buffersrc_parameters_alloc",
                code: -1,
            });
        }
        (*parameters).format = format as i32;
        (*parameters).width = width as i32;
        (*parameters).height = height as i32;
        (*parameters).time_base = AVRational {
            num: 1,
            den: RecordingClock::MICROSECONDS,
        };
        (*parameters).sample_aspect_ratio = AVRational { num: 1, den: 1 };
        (*parameters).hw_frames_ctx = frames.map_or(ptr::null_mut(), HardwareContext::as_ptr);
        let code = av_buffersrc_parameters_set(source, parameters);
        av_free(parameters.cast());
        check(code, "av_buffersrc_parameters_set")?;
        check(avfilter_init_str(source, ptr::null()), "avfilter_init_str")?;
    }
    Ok(())
}

fn find(name: &'static str) -> Result<filter::Filter, RecordError> {
    filter::find(name).ok_or(RecordError::NoEncoder(name))
}
