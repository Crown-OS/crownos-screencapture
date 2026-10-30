use std::ffi::OsStr;
use std::io;
use std::process::{Child, Command, Stdio};

const CROWNOS_DESKTOP: &str = "CrownOS";

/// Whether the session is a CrownOS desktop, per `XDG_CURRENT_DESKTOP`.
pub fn is_crownos() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|desktops| {
        desktops
            .split(':')
            .any(|desktop| desktop.eq_ignore_ascii_case(CROWNOS_DESKTOP))
    })
}

/// Starts `program` without tying its lifetime or output to this process.
pub fn spawn_detached<I, S>(program: impl AsRef<OsStr>, args: I) -> io::Result<Child>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
}

/// Starts this same binary with `args`.
pub fn spawn_self<I, S>(args: I) -> io::Result<Child>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    spawn_detached(std::env::current_exe()?, args)
}
