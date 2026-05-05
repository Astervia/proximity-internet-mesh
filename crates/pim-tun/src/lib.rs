//! Cross-platform TUN interface management used by the daemon dataplane.

#![warn(missing_docs)]

mod interface;
mod platform;
// Route helpers are consumed by the linux + macos platform backends. The
// `unsupported` arm (used today for android, ios, windows, etc.) does not
// call them, so gate the module to silence dead-code warnings under
// `clippy -D warnings` when cross-compiling to those targets.
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod route;

pub use interface::{TunError, TunInterface};
