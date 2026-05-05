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

// Phase B Android JNI bridge. Only compiled for android targets.
// Provides `Java_org_astervia_pim_PimDaemon_*` extern fns that the
// Kotlin `VpnServicePlugin` and `ForegroundServicePlugin` shims call.
#[cfg(target_os = "android")]
pub mod jni;

/// Re-export of the kernel config schema. Mobile embeddings construct a
/// `Config` from disk (`Config::load`) or memory (`Config::from_toml_str`)
/// before calling [`run_in_process`].
pub use pim_core::Config;

/// Re-export of `tokio_util::sync::CancellationToken`. Mobile embeddings
/// own the token's lifetime, firing it from the host (Java foreground
/// service stop, iOS extension teardown).
pub use tokio_util::sync::CancellationToken;

/// Binary entrypoint preserved for `bin/pim-daemon`.
///
/// Reads CLI args, loads the config, writes a pid file, installs a
/// SIGTERM / SIGINT handler against a fresh `CancellationToken`, and
/// then delegates to [`run_in_process`]. Mobile embeddings should
/// **not** call this — call [`run_in_process`] directly.
pub async fn run_binary() -> anyhow::Result<()> {
    app::run().await
}

/// Library entrypoint for in-process daemon embeddings.
///
/// Phase B's Android JNI bridge calls this from a Foreground Service:
/// the Java side reads the config from `getFilesDir()/pim.toml`,
/// installs a logcat-bound tracing subscriber, then runs the daemon
/// to completion against a `CancellationToken` that the service stops.
///
/// The function does **not** install a signal handler, parse argv,
/// touch a pid file, or initialise `tracing_subscriber`. The caller is
/// responsible for all four — see [`run_binary`] for the desktop
/// implementation as a reference.
pub async fn run_in_process(
    config: Config,
    config_path: std::path::PathBuf,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    app::run_in_process(config, config_path, cancel).await
}
