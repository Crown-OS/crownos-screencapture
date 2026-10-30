use std::rc::Rc;

use anyhow::{Context, Result};
use crownui::AppConfig;
use crownui::kit::{self, Theme, ToggleItem, icons, provide_theme, toggle_group};
use crownui::prelude::{
    Anchor, AnyView, Color, Cx, KeyboardInteractivity, Layer, LayerExtent, LayerMargin,
    LayerOptions, Listeners, Material, MaterialBlur, Shadow, Signal, Styled, TRANSPARENT, View,
    bind, component, hstack, keyed, spacer, vstack,
};

use super::escape_handler;
use super::launch::{CaptureMode, Launch};
use super::palette::{PANEL, PANEL_BORDER};
use super::picker::{Choice, picker};
use super::tile::{Availability, tile};
use crate::capture::{OutputInfo, WindowInfo};

const NAMESPACE: &str = "crownos-screenshot";
const SURFACE_WIDTH: u32 = 640;
const SURFACE_HEIGHT: u32 = 380;
const TOP_MARGIN: i32 = 48;
const RADIUS: f32 = 24.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Tiles,
    Displays,
    Windows,
}

/// What the toolbar offers to capture, read once before it opens.
#[derive(Debug, Default)]
struct Targets {
    outputs: Vec<OutputInfo>,
    windows: Vec<WindowInfo>,
}

pub fn run_toolbar(outputs: Vec<OutputInfo>, windows: Vec<WindowInfo>) -> Result<()> {
    let layer = LayerOptions::new(NAMESPACE, Anchor::TOP)
        .layer(Layer::Overlay)
        .width(LayerExtent::Fixed(SURFACE_WIDTH))
        .height(LayerExtent::Fixed(SURFACE_HEIGHT))
        .margin(LayerMargin {
            top: TOP_MARGIN,
            ..LayerMargin::default()
        })
        .keyboard(KeyboardInteractivity::Exclusive);
    let config = AppConfig::new("Screenshot")
        .app_id(NAMESPACE)
        .size(SURFACE_WIDTH, SURFACE_HEIGHT)
        .clear_color(TRANSPARENT)
        .font(kit::inter_font().to_vec(), Some(kit::INTER_FAMILY))
        .layer(layer);
    let targets = Rc::new(Targets { outputs, windows });
    crownui::run(root(targets), config).context("the toolbar stopped")
}

fn root(targets: Rc<Targets>) -> impl View {
    component(move |cx| {
        provide_theme(cx, Theme::default());
        let mode = cx.signal(CaptureMode::Screenshot.index());
        let page = cx.signal(Page::Tiles);
        let capabilities = cx.platform_capabilities();
        let targets = Rc::clone(&targets);
        let panel = vstack((
            escape_handler(|_| std::process::exit(0)),
            header(mode),
            keyed(
                move |runtime| (page.get(runtime), mode.get(runtime)),
                move |_, (current, mode)| {
                    content(&targets, current, CaptureMode::from_index(mode), page)
                },
            ),
        ))
        .p(14.0)
        .gap(14.0)
        .rounded(RADIUS)
        .border(1.0)
        .border_color(PANEL_BORDER)
        .material(glass())
        .shadow(Shadow::xl())
        .bg(bind(move |runtime| {
            if capabilities.get(runtime).materials {
                TRANSPARENT
            } else {
                PANEL
            }
        }))
        .pointer_events_auto();
        hstack(panel)
            .justify_center()
            .items_start()
            .w_frac(1.0)
            .h_frac(1.0)
            .pointer_events_none()
    })
}

fn header(mode: Signal<usize>) -> impl View {
    let modes = [
        ToggleItem::icon(icons::CIRCLE_DOT, "Record"),
        ToggleItem::icon(icons::CAMERA, "Screenshot"),
    ];
    hstack((
        toggle_group(modes, mode),
        spacer(),
        kit::icon_button(icons::SETTINGS, "Settings").on_click(|_, _| Launch::Settings.run()),
    ))
    .items_center()
}

fn content(
    targets: &Rc<Targets>,
    page: Page,
    mode: CaptureMode,
    navigate: Signal<Page>,
) -> AnyView {
    let back = move |cx: &mut Cx| navigate.set(cx, Page::Tiles);
    match page {
        Page::Tiles => tiles(targets, mode, navigate).boxed(),
        Page::Displays => picker(
            "Choose a display",
            display_choices(&targets.outputs, mode),
            back,
        )
        .boxed(),
        Page::Windows => picker(
            "Choose a window",
            window_choices(&targets.windows, mode),
            back,
        )
        .boxed(),
    }
}

fn tiles(targets: &Rc<Targets>, mode: CaptureMode, navigate: Signal<Page>) -> impl View {
    let single_output = match targets.outputs.as_slice() {
        [] => Some(None),
        [only] => Some(Some(only.name.clone())),
        _ => None,
    };
    let display = tile(
        icons::MONITOR,
        "Display",
        Availability::Enabled,
        move |cx| match &single_output {
            Some(output) => Launch::Display {
                mode,
                output: output.clone(),
            }
            .run(),
            None => navigate.set(cx, Page::Displays),
        },
    );
    let window = tile(
        icons::APP_WINDOW,
        "Window",
        Availability::Enabled,
        move |cx| navigate.set(cx, Page::Windows),
    );
    let area = tile(icons::CROP, "Area", Availability::Enabled, move |_| {
        Launch::Area { mode }.run()
    });
    let recording = mode == CaptureMode::Record;
    let camera = recording.then(|| tile(icons::VIDEO, "Camera", Availability::Disabled, |_| {}));
    let voice = recording.then(|| {
        tile(icons::AUDIO_LINES, "Voice", Availability::Enabled, |_| {
            Launch::Voice.run()
        })
    });
    hstack((display, window, area, camera, voice))
        .gap(12.0)
        .justify_center()
}

fn display_choices(outputs: &[OutputInfo], mode: CaptureMode) -> Vec<Choice> {
    outputs
        .iter()
        .map(|output| Choice {
            title: output.name.clone(),
            detail: format!(
                "{} · {}×{}",
                output.description, output.logical_width, output.logical_height
            ),
            launch: Launch::Display {
                mode,
                output: Some(output.name.clone()),
            },
        })
        .collect()
}

fn window_choices(windows: &[WindowInfo], mode: CaptureMode) -> Vec<Choice> {
    windows
        .iter()
        .map(|window| Choice {
            title: if window.title.is_empty() {
                window.app_id.clone()
            } else {
                window.title.clone()
            },
            detail: window.app_id.clone(),
            launch: Launch::Window {
                mode,
                identifier: window.identifier.clone(),
            },
        })
        .collect()
}

/// Dark frosted glass drawn by the compositor behind the panel. The shadow is drawn by the
/// panel itself: a compositor shadow would fall across the whole surface.
fn glass() -> Material {
    Material {
        corner_radius: RADIUS,
        border_width: 1.0,
        blur: Some(MaterialBlur {
            radius: 40.0,
            tint: Color::new(0.04, 0.04, 0.05, 0.78),
            saturation: 1.4,
        }),
        shadow: None,
    }
}
