use crownui::kit;
use crownui::prelude::{Cx, Listeners, Semantic, Spring, Styled, View, text, vstack};

use super::palette::{FOREGROUND, TILE, TILE_HOVER};

const WIDTH: f32 = 104.0;
const HEIGHT: f32 = 84.0;
const RADIUS: f32 = 16.0;
const ICON_SIZE: f32 = 28.0;
const ICON_STROKE: f32 = 1.75;
const DISABLED_OPACITY: f32 = 0.35;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    Enabled,
    Disabled,
}

/// A large icon-over-label button, as in the capture toolbar.
pub fn tile(
    icon: &'static str,
    label: &'static str,
    availability: Availability,
    mut on_activate: impl FnMut(&mut Cx) + 'static,
) -> impl View {
    let enabled = availability == Availability::Enabled;
    vstack((
        kit::icon_colored(icon, ICON_STROKE, |_| FOREGROUND).size(ICON_SIZE),
        text(label)
            .text_size(13.0)
            .font_semibold()
            .text_color(FOREGROUND),
    ))
    .w(WIDTH)
    .h(HEIGHT)
    .gap(8.0)
    .items_center()
    .justify_center()
    .rounded(RADIUS)
    .bg(TILE)
    .opacity(if enabled { 1.0 } else { DISABLED_OPACITY })
    .disabled(!enabled)
    .hover(|style| {
        if enabled {
            style.bg(TILE_HOVER).scale(1.04)
        } else {
            style
        }
    })
    .active(|style| if enabled { style.scale(0.97) } else { style })
    .transition(Spring::snappy())
    .on_click(move |cx, _| {
        if enabled {
            on_activate(cx);
        }
    })
}
