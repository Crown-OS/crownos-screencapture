use anyhow::Result;

use super::announce::{Capture, announce};
use crate::capture::WaylandCapture;
use crate::cli::{Selection, ShotArgs};
use crate::config::Settings;
use crate::screenshot;

pub fn run(arguments: ShotArgs) -> Result<()> {
    std::thread::sleep(arguments.delay());
    let settings = Settings::load();
    let selection = Selection::from(arguments.target);
    let saved = {
        let mut capture = WaylandCapture::connect()?;
        screenshot::take(&mut capture, &selection, arguments.path, &settings)?
    };
    let Some(path) = saved else {
        return Ok(());
    };
    println!("{}", path.display());
    if !arguments.no_notify {
        announce(Capture::Screenshot, &path);
    }
    Ok(())
}
