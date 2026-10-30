//! The capture toolbar and the area-selection overlay, both crownui layer surfaces.

mod launch;
mod overlay;
mod palette;
mod picker;
mod tile;
mod toolbar;

pub use launch::{CaptureMode, Launch};
pub use overlay::run_overlay;
pub use toolbar::run_toolbar;

use crownui::ext::{Key, Keybind, Modifiers, NamedKey};
use crownui::prelude::{Cx, Listeners, Styled, View, vstack};

/// An invisible element that runs `on_escape` when Escape is pressed.
fn escape_handler(mut on_escape: impl FnMut(&mut Cx) + 'static) -> impl View {
    vstack(())
        .absolute()
        .size(0.0)
        .shortcut(Keybind {
            key: Key::Named(NamedKey::Escape),
            modifiers: Modifiers::NONE,
        })
        .on_click(move |cx, _| on_escape(cx))
}
