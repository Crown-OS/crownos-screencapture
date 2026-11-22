//! Captures every toolbar and area-selection state for the CrownOS-Design Figma file. Run both
//! appearances with `../crownui/capture/scripts/capture-app.sh .`; output lands in
//! `$CROWN_DESIGN_DIR`.

use anyhow::{Result, bail};
use capture::{CaptureApp, Carrier};
use crownos_screenshot::capture::{OutputInfo, WindowInfo};
use crownos_screenshot::ui::{
    Backdrop, overlay_config, overlay_root, toolbar_config, toolbar_root,
};
use crowntest::{Query, Role, role};

const OUTPUT: (f32, f32) = (1920.0, 1080.0);
const RESTING_POINTER: (f32, f32) = (2.0, 370.0);
const TOOLBAR: &str = "Current build · Toolbar";
const PICKERS: &str = "Current build · Target pickers";
const AREA: &str = "Current build · Area selection";
const SELECTION_START: (f32, f32) = (560.0, 300.0);
const WINDOW_LIST_SCROLL: f32 = 240.0;
const SELECTION_END: (f32, f32) = (1360.0, 780.0);

fn outputs() -> Vec<OutputInfo> {
    [
        ("DP-1", "Dell Inc. DELL U2723QE", 2560, 1440),
        ("HDMI-A-1", "LG Electronics LG ULTRAGEAR", 1920, 1080),
        ("eDP-1", "BOE Built-in display", 1920, 1200),
    ]
    .into_iter()
    .map(|(name, description, width, height)| OutputInfo {
        name: name.into(),
        description: description.into(),
        logical_width: width,
        logical_height: height,
    })
    .collect()
}

fn windows() -> Vec<WindowInfo> {
    [
        ("Home — Files", "crownos-files"),
        ("~/Projects/aurora — Terminal", "crownos-terminal"),
        ("Release planning — Firefox", "firefox"),
        ("Display — Settings", "crownsettings"),
        ("notes.md — Text Editor", "crownos-editor"),
        ("", "crownos-preview"),
        ("Now Playing — Music", "crownos-music"),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (title, app_id))| WindowInfo {
        identifier: format!("toplevel-{index}"),
        title: title.into(),
        app_id: app_id.into(),
    })
    .collect()
}

fn toolbar(windows: Vec<WindowInfo>) -> Result<CaptureApp> {
    let mut app = capture::mount_app(toolbar_root(outputs(), windows), &toolbar_config(), None)?;
    rest_pointer(&mut app);
    Ok(app)
}

fn rest_pointer(app: &mut CaptureApp) {
    app.pointer_move(RESTING_POINTER.0, RESTING_POINTER.1)
        .settle();
}

fn centre_of(app: &mut CaptureApp, query: Query) -> Result<(f32, f32)> {
    let tree = app.ui().semantic_tree();
    let Some(bounds) = query.find_all(tree).first().map(|node| node.bounds) else {
        bail!("nothing matches {query:?}\n{tree}");
    };
    Ok((
        bounds.origin.x + bounds.size.width / 2.0,
        bounds.origin.y + bounds.size.height / 2.0,
    ))
}

fn press(app: &mut CaptureApp, query: Query) -> Result<()> {
    let (x, y) = centre_of(app, query)?;
    app.pointer_move(x, y).pointer_down(x, y).pointer_up(x, y);
    rest_pointer(app);
    Ok(())
}

fn tile_label(label: &str) -> Query {
    role(Role::Button).name(label)
}

fn capture_toolbar(carrier: &mut Carrier) -> Result<()> {
    {
        let mut app = toolbar(windows())?;
        carrier.capture_state(
            &mut app,
            TOOLBAR,
            "Screenshot",
            "Screenshot mode: Display, Window and Area tiles",
        )?;
    }
    {
        let mut app = toolbar(windows())?;
        let (x, y) = centre_of(&mut app, tile_label("Window"))?;
        app.pointer_move(x, y).settle();
        carrier.capture_state(
            &mut app,
            TOOLBAR,
            "Hovered tile",
            "The pointer over the Window tile: lighter fill, scaled up",
        )?;
    }
    {
        let mut app = toolbar(windows())?;
        press(&mut app, role(Role::RadioButton).name("Record"))?;
        carrier.capture_state(
            &mut app,
            TOOLBAR,
            "Record",
            "Record mode adds Camera (unavailable) and Voice",
        )?;
    }
    Ok(())
}

fn capture_pickers(carrier: &mut Carrier) -> Result<()> {
    {
        let mut app = toolbar(windows())?;
        press(&mut app, tile_label("Display"))?;
        carrier.capture_state(
            &mut app,
            PICKERS,
            "Choose a display",
            "Several outputs: Display asks which one",
        )?;
    }
    {
        let mut app = toolbar(windows())?;
        press(&mut app, tile_label("Window"))?;
        carrier.capture_state(
            &mut app,
            PICKERS,
            "Choose a window",
            "Open windows by title and app id; the list scrolls",
        )?;
    }
    {
        let mut app = toolbar(windows())?;
        press(&mut app, tile_label("Window"))?;
        app.scroll(role(Role::Button).text("Firefox"), WINDOW_LIST_SCROLL)
            .settle();
        carrier.capture_state(
            &mut app,
            PICKERS,
            "Choose a window scrolled",
            "The list scrolled to its end; an untitled window shows its app id",
        )?;
    }
    {
        let mut app = toolbar(Vec::new())?;
        press(&mut app, tile_label("Window"))?;
        carrier.capture_state(&mut app, PICKERS, "No windows", "Nothing open to capture")?;
    }
    Ok(())
}

/// A stand-in desktop frame: a dusk gradient with a soft glow, never the user's screen.
fn sample_frame() -> Backdrop {
    let (width, height) = (OUTPUT.0 as u32, OUTPUT.1 as u32);
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let across = x as f32 / width as f32;
            let down = y as f32 / height as f32;
            let glow = (1.0 - ((across - 0.7).powi(2) + (down - 0.35).powi(2)).sqrt() * 2.2)
                .clamp(0.0, 1.0);
            let channel = |from: f32, to: f32, lift: f32| {
                ((from + (to - from) * (across * 0.4 + down * 0.6) + lift * glow) * 255.0)
                    .clamp(0.0, 255.0) as u8
            };
            rgba.extend([
                channel(0.16, 0.05, 0.55),
                channel(0.22, 0.08, 0.30),
                channel(0.48, 0.20, 0.25),
                255,
            ]);
        }
    }
    Backdrop {
        width,
        height,
        rgba,
    }
}

fn overlay(backdrop: Option<Backdrop>) -> Result<CaptureApp> {
    let mut app = capture::mount_app(overlay_root(backdrop), &overlay_config(), Some(OUTPUT))?;
    app.pointer_move(OUTPUT.0 / 2.0, OUTPUT.1 / 2.0).settle();
    Ok(app)
}

fn drag_selection(app: &mut CaptureApp) {
    app.pointer_down(SELECTION_START.0, SELECTION_START.1)
        .pointer_move(SELECTION_END.0, SELECTION_END.1)
        .settle();
}

fn capture_area(carrier: &mut Carrier) -> Result<()> {
    {
        let mut app = overlay(Some(sample_frame()))?;
        carrier.capture_state(
            &mut app,
            AREA,
            "Screenshot area",
            "The frozen screen dimmed, waiting for a drag",
        )?;
    }
    {
        let mut app = overlay(Some(sample_frame()))?;
        drag_selection(&mut app);
        carrier.capture_state(
            &mut app,
            AREA,
            "Screenshot area selecting",
            "Dragging out a region: outside dimmed, outlined selection",
        )?;
    }
    {
        let mut app = overlay(None)?;
        carrier.capture_state(
            &mut app,
            AREA,
            "Recording area",
            "Over the live desktop (transparent surface), waiting for a drag",
        )?;
    }
    {
        let mut app = overlay(None)?;
        drag_selection(&mut app);
        carrier.capture_state(
            &mut app,
            AREA,
            "Recording area selecting",
            "Dragging out the region to record over the live desktop",
        )?;
    }
    Ok(())
}

fn main() -> Result<()> {
    let mut carrier = Carrier::new(
        "Screen Capture",
        "crownos-screencapture: the capture toolbar, its target pickers and the area-selection overlay, captured headlessly from the current source.",
    );
    capture_toolbar(&mut carrier)?;
    capture_pickers(&mut carrier)?;
    capture_area(&mut carrier)?;
    let path = carrier.write(None)?;
    println!("{}", path.display());
    capture::finish()
}
