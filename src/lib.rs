//! crownos-screenshot — screenshots and screen recordings for CrownOS and other Wayland desktops.
//!
//! The `protocol` module is the wire contract of the running recorder; with
//! `default-features = false` a client builds only that.

pub(crate) mod protocol;

pub use protocol::protocol::{Client, SERVICE};

#[cfg(feature = "app")]
pub mod app;
#[cfg(feature = "app")]
pub mod area;
#[cfg(feature = "app")]
pub mod capture;
#[cfg(feature = "app")]
pub mod cli;
#[cfg(feature = "app")]
pub mod config;
#[cfg(feature = "app")]
pub mod image;
#[cfg(feature = "app")]
pub mod notify;
#[cfg(feature = "app")]
pub mod preview;
#[cfg(feature = "app")]
pub mod record;
#[cfg(feature = "app")]
pub mod screenshot;
#[cfg(feature = "app")]
pub mod ui;
#[cfg(feature = "app")]
pub mod util;
