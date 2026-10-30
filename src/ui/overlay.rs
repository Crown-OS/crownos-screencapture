use std::io::Write;
use std::os::fd::{FromRawFd, OwnedFd};

use anyhow::{Context, Result, bail};
use crownui::AppConfig;
use crownui::prelude::{
    Anchor, CornerRadii, CursorIcon, KeyboardInteractivity, Layer, LayerOptions, Listeners, Point,
    Rect, Size, Styled, TRANSPARENT, View, canvas, component, image, zstack,
};

use super::escape_handler;
use super::palette::{DIM, SELECTION};
use crate::capture::{Region, ShmFrame};
use crate::cli::SelectAreaArgs;

const NAMESPACE: &str = "crownos-screenshot-area";
const BORDER: f32 = 1.5;
const MINIMUM_SIZE: f32 = 4.0;
const CANCELLED: i32 = 1;

/// A frozen frame decoded for display.
struct Backdrop {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

/// Covers the focused output, lets the user drag out a region, prints it and exits.
pub fn run_overlay(arguments: SelectAreaArgs) -> Result<()> {
    let backdrop = backdrop(&arguments)?;
    let layer = LayerOptions::new(NAMESPACE, Anchor::ALL)
        .layer(Layer::Overlay)
        .exclusive_zone(-1)
        .keyboard(KeyboardInteractivity::Exclusive);
    let config = AppConfig::new("Select area")
        .app_id(NAMESPACE)
        .clear_color(TRANSPARENT)
        .accessibility(false)
        .layer(layer);
    crownui::run(root(backdrop), config).context("the selection overlay stopped")
}

fn backdrop(arguments: &SelectAreaArgs) -> Result<Option<Backdrop>> {
    let (Some(fd), Some(width), Some(height), Some(stride), Some(format)) = (
        arguments.frame_fd,
        arguments.width,
        arguments.height,
        arguments.stride,
        arguments.format,
    ) else {
        return Ok(None);
    };
    if fd < 3 {
        bail!("frame descriptor {fd} is a standard stream");
    }
    // SAFETY: the parent passed this descriptor to this process for exactly this frame.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let frame = ShmFrame::from_fd(fd, width, height, stride, format)?;
    Ok(Some(Backdrop {
        width,
        height,
        rgba: frame.to_rgba(),
    }))
}

fn root(backdrop: Option<Backdrop>) -> impl View {
    let mut backdrop = backdrop;
    component(move |cx| {
        let drag = cx.signal(None::<(Point, Point)>);
        let frozen = backdrop.take().map(|frame| {
            image(frame.width, frame.height, frame.rgba)
                .absolute()
                .inset(0.0)
                .w_frac(1.0)
                .h_frac(1.0)
        });
        zstack((
            frozen,
            canvas(move |runtime, size, list| {
                let selection = drag.get(runtime).map(|(start, end)| span(start, end));
                for shade in shades(size, selection) {
                    list.fill(shade, CornerRadii::default(), DIM);
                }
                if let Some(selection) = selection {
                    list.stroke(selection, CornerRadii::default(), BORDER, SELECTION);
                }
            })
            .absolute()
            .inset(0.0),
            escape_handler(|_| std::process::exit(CANCELLED)),
        ))
        .w_frac(1.0)
        .h_frac(1.0)
        .cursor(CursorIcon::Crosshair)
        .on_pointer_down(move |cx, event| drag.set(cx, Some((event.position, event.position))))
        .on_pointer_move(move |cx, event| {
            if let Some((start, _)) = drag.get(cx) {
                drag.set(cx, Some((start, event.position)));
            }
        })
        .on_pointer_up(move |cx, event| {
            let Some((start, _)) = drag.get(cx) else {
                return;
            };
            match region(span(start, event.position)) {
                Some(region) => finish(region),
                None => drag.set(cx, None),
            }
        })
    })
}

fn span(start: Point, end: Point) -> Rect {
    let origin = Point::new(start.x.min(end.x), start.y.min(end.y));
    Rect::new(
        origin,
        Size::new((start.x - end.x).abs(), (start.y - end.y).abs()),
    )
}

/// The dimmed parts of the screen: everything, or the four bands around the selection.
fn shades(size: Size, selection: Option<Rect>) -> Vec<Rect> {
    let Some(selected) = selection else {
        return vec![Rect::new(Point::ZERO, size)];
    };
    let (left, top) = (selected.origin.x, selected.origin.y);
    let (right, bottom) = (left + selected.size.width, top + selected.size.height);
    vec![
        Rect::from_xywh(0.0, 0.0, size.width, top),
        Rect::from_xywh(0.0, bottom, size.width, size.height - bottom),
        Rect::from_xywh(0.0, top, left, bottom - top),
        Rect::from_xywh(right, top, size.width - right, bottom - top),
    ]
}

fn region(rect: Rect) -> Option<Region> {
    (rect.size.width >= MINIMUM_SIZE && rect.size.height >= MINIMUM_SIZE).then(|| Region {
        x: rect.origin.x.max(0.0).round() as u32,
        y: rect.origin.y.max(0.0).round() as u32,
        width: rect.size.width.round() as u32,
        height: rect.size.height.round() as u32,
    })
}

fn finish(region: Region) -> ! {
    let mut stdout = std::io::stdout().lock();
    let printed = writeln!(stdout, "{region}").and_then(|()| stdout.flush());
    std::process::exit(if printed.is_ok() { 0 } else { CANCELLED })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalises_reversed_drags() {
        let rect = span(Point::new(50.0, 80.0), Point::new(10.0, 20.0));
        assert_eq!(rect, Rect::from_xywh(10.0, 20.0, 40.0, 60.0));
    }

    #[test]
    fn ignores_clicks_without_drag() {
        assert_eq!(region(Rect::from_xywh(5.0, 5.0, 1.0, 1.0)), None);
        assert_eq!(
            region(Rect::from_xywh(5.4, 5.6, 10.0, 20.0)),
            Some(Region {
                x: 5,
                y: 6,
                width: 10,
                height: 20
            })
        );
    }

    #[test]
    fn dims_around_selection() {
        let bands = shades(
            Size::new(100.0, 100.0),
            Some(Rect::from_xywh(10.0, 20.0, 30.0, 40.0)),
        );
        let dimmed: f32 = bands
            .iter()
            .map(|band| band.size.width * band.size.height)
            .sum();
        assert_eq!(dimmed, 100.0 * 100.0 - 30.0 * 40.0);
    }
}
