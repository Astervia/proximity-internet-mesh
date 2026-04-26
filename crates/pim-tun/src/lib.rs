//! Cross-platform TUN interface management used by the daemon dataplane.

#![warn(missing_docs)]

mod interface;
mod platform;
mod route;

pub use interface::{TunError, TunInterface};
