//! PIM daemon library surface.
//!
//! Historically the daemon was binary-only; `main.rs` declared `mod app;`
//! and dropped the bulk of the runtime under it. Phase A introduces a
//! library-shaped public surface so non-binary callers — notably the
//! Phase B Android in-process embedding under `pim-ui`'s Tauri shell —
//! can link the daemon directly without the binary's CLI argument
//! parsing or `tracing-subscriber` setup.
//!
//! On Linux/macOS the binary at `bin/pim-daemon` keeps its current
//! shape and behaviour: `main.rs` calls [`run_binary`], which is
//! identical to the old `app::run`. There is no behaviour change for
//! existing users.
//!
//! The wider library is currently `pub(crate)` to avoid leaking
//! internals before Phase B firms up the embedding contract. Phase B
//! will promote a small, deliberately-shaped surface (likely
//! `run_in_process(config, cancel)` plus error/event types) to `pub`.

pub(crate) mod app;

/// Binary entrypoint preserved for `bin/pim-daemon`.
///
/// The body is the original `app::run` entrypoint that the binary's
/// `main.rs` used to call directly. Pulling it through the library
/// keeps the binary trivial and gives Phase B a single function to
/// rewrite when it splits out a daemon runtime that does not own
/// argument parsing.
pub async fn run_binary() -> anyhow::Result<()> {
    app::run().await
}
