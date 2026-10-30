//! Capturing one still image and writing it to disk.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use crownos_config::schema::ImageFormat;

use crate::area::pick_region;
use crate::capture::{CaptureTarget, CursorMode, PixelRect, ScreenCapture, ShmFrame};
use crate::cli::Selection;
use crate::config::Settings;
use crate::image::encoder_for;
use crate::util::paths::timestamped_file;

const FILE_PREFIX: &str = "Screenshot";

/// Captures `selection` and saves it, returning the file, or `None` when the user cancelled.
pub fn take(
    capture: &mut impl ScreenCapture,
    selection: &Selection,
    destination: Option<PathBuf>,
    settings: &Settings,
) -> Result<Option<PathBuf>> {
    let Some((frame, rect)) = grab(capture, selection, settings.cursor())? else {
        return Ok(None);
    };
    let format = destination
        .as_deref()
        .and_then(format_from_extension)
        .unwrap_or(settings.0.image_format);
    let encoder = encoder_for(format);
    let path = destination.unwrap_or_else(|| {
        timestamped_file(
            &settings.screenshot_directory(),
            FILE_PREFIX,
            encoder.extension(),
        )
    });
    if let Some(directory) = path.parent() {
        std::fs::create_dir_all(directory)
            .with_context(|| format!("cannot create {}", directory.display()))?;
    }
    let mut file = BufWriter::new(
        File::create(&path).with_context(|| format!("cannot write {}", path.display()))?,
    );
    encoder.encode(&frame, rect, &mut file)?;
    file.flush()?;
    Ok(Some(path))
}

fn grab(
    capture: &mut impl ScreenCapture,
    selection: &Selection,
    cursor: CursorMode,
) -> Result<Option<(ShmFrame, PixelRect)>> {
    let whole = |frame: ShmFrame| {
        let rect = frame.full_rect();
        Some((frame, rect))
    };
    Ok(match selection {
        Selection::Display(name) => {
            let output = capture.resolve_output(name.as_deref())?;
            whole(capture.capture(&CaptureTarget::Output(output.name), cursor)?)
        }
        Selection::Window(identifier) => {
            whole(capture.capture(&CaptureTarget::Window(identifier.clone()), cursor)?)
        }
        Selection::Region { output, region } => {
            let output = capture.resolve_output(output.as_deref())?;
            let frame = capture.capture(&CaptureTarget::Output(output.name), cursor)?;
            let rect = region.to_pixels(
                output.logical_width,
                output.logical_height,
                frame.width,
                frame.height,
            );
            Some((frame, rect))
        }
        Selection::Area => {
            let output = capture.focused_output()?;
            let frame = capture.capture(&CaptureTarget::Output(output.name), cursor)?;
            pick_region(Some(&frame))?.map(|region| {
                let rect = region.to_pixels(
                    output.logical_width,
                    output.logical_height,
                    frame.width,
                    frame.height,
                );
                (frame, rect)
            })
        }
    })
}

fn format_from_extension(path: &Path) -> Option<ImageFormat> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => Some(ImageFormat::Png),
        "jpg" | "jpeg" => Some(ImageFormat::Jpeg),
        _ => None,
    }
}
