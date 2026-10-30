use crownui::kit::{self, icons};
use crownui::prelude::{Cx, Listeners, Spring, Styled, View, hstack, scroll_view, text, vstack};

use super::launch::Launch;
use super::palette::{FOREGROUND, MUTED, TILE, TILE_HOVER};

const LIST_HEIGHT: f32 = 236.0;
const ROW_RADIUS: f32 = 12.0;

/// One choice in a picker list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub title: String,
    pub detail: String,
    pub launch: Launch,
}

/// A titled list of choices with a back button.
pub fn picker(
    heading: &'static str,
    choices: Vec<Choice>,
    mut on_back: impl FnMut(&mut Cx) + 'static,
) -> impl View {
    let rows: Vec<_> = choices.into_iter().map(row).collect();
    let empty = rows
        .is_empty()
        .then(|| text("Nothing to capture").text_color(MUTED).p(12.0));
    vstack((
        hstack((
            kit::icon_button(icons::CHEVRON_LEFT, "Back").on_click(move |cx, _| on_back(cx)),
            text(heading)
                .text_size(15.0)
                .font_semibold()
                .text_color(FOREGROUND),
        ))
        .gap(8.0)
        .items_center(),
        scroll_view(vstack((rows, empty)).gap(6.0)).max_h(LIST_HEIGHT),
    ))
    .gap(10.0)
    .w(560.0)
}

fn row(choice: Choice) -> impl View {
    let Choice {
        title,
        detail,
        launch,
    } = choice;
    vstack((
        text(title)
            .text_size(14.0)
            .font_semibold()
            .text_color(FOREGROUND)
            .line_clamp(1),
        text(detail).text_size(12.0).text_color(MUTED).line_clamp(1),
    ))
    .gap(2.0)
    .px(14.0)
    .py(10.0)
    .rounded(ROW_RADIUS)
    .bg(TILE)
    .hover(|style| style.bg(TILE_HOVER))
    .transition(Spring::snappy())
    .on_click(move |_, _| launch.run())
}
