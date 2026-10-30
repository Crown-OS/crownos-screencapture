//! What each command does, end to end.

mod announce;
mod record;
mod shot;
mod stop;
mod toolbar;

use anyhow::Result;

use crate::cli::{Cli, Command};
use crate::ui;

pub fn run(cli: Cli) -> Result<()> {
    match cli.command {
        None => stop::stop_recording_or(toolbar::open),
        Some(Command::Shot(arguments)) => shot::run(arguments),
        Some(Command::Record(arguments)) => record::run(arguments),
        Some(Command::Voice(arguments)) => record::run_voice(arguments),
        Some(Command::Stop) => stop::stop_recording(),
        Some(Command::SelectArea(arguments)) => ui::run_overlay(arguments),
    }
}
