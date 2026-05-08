//! Phase B Android JNI bridge.
//!
//! The Android Tauri shell links `libpim_daemon.so` (the `cdylib`
//! produced from this crate) and calls into the
//! `Java_org_astervia_pim_PimDaemon_*` symbols exported here from a
//! Kotlin Foreground Service.
//!
//! The bridge owns:
//!
//! 1. A multi-thread tokio runtime created on first `nativeStart` and
//!    parked in a global. Avoids one-shot runtimes per call.
//! 2. A handle table keyed by `jlong` so Java can stop a previously
//!    started daemon without exposing raw Rust pointers across the JNI
//!    boundary.
//! 3. A logcat-bound `tracing` subscriber installed once on first
//!    use so kernel `tracing::*` events surface alongside Java logs.
//!
//! The Java contract is intentionally narrow:
//!
//! ```kotlin
//! object PimDaemon {
//!     external fun nativeStart(
//!         configPath: String,
//!         dataDir: String,
//!         tunFd: Int,
//!     ): Long
//!     external fun nativeStop(handle: Long)
//!     external fun nativeProvideProtect(socket: Int): Boolean  // future
//! }
//! ```
//!
//! Phase B end-to-end testing has not yet exercised these symbols on
//! a device — the build target needs an Android NDK clang plus the
//! Tauri Android scaffold (deferred to a session where those are
//! installed). The signatures here are the contract the Kotlin shims
//! are written against; the implementation bodies are minimal but
//! correct enough that a fresh session can wire them up without
//! redesigning the boundary.

use std::sync::OnceLock;

use jni::objects::{JClass, JString};
use jni::sys::{jboolean, jint, jlong};
use jni::JNIEnv;
use tokio::runtime::Runtime;
use tokio_util::sync::CancellationToken;

/// One shared runtime for all `nativeStart` calls. Tauri-android's
/// process model runs at most one daemon per app instance, so a single
/// `Runtime` keyed at first use is sufficient.
static RUNTIME: OnceLock<Runtime> = OnceLock::new();

/// Installed once on first `nativeStart` call. Routes `tracing`
/// events to logcat under the `pim-daemon` tag.
static TRACING_INIT: OnceLock<()> = OnceLock::new();

/// Per-daemon handle returned to Java as an opaque `jlong`. Stop
/// reclaims it via `Box::from_raw`.
struct DaemonHandle {
    cancel: CancellationToken,
    join: tokio::task::JoinHandle<anyhow::Result<()>>,
}

fn ensure_runtime() -> &'static Runtime {
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("pim-daemon")
            .build()
            .expect("failed to build tokio runtime for JNI")
    })
}

fn ensure_tracing() {
    TRACING_INIT.get_or_init(|| {
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;
        let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
        let android_layer =
            tracing_android::layer("pim-daemon").expect("failed to build tracing-android layer");
        tracing_subscriber::registry()
            .with(env_filter)
            .with(android_layer)
            .with(crate::app::logs_subscriber::init())
            .init();
    });
}

fn jstring_to_string(env: &mut JNIEnv, s: &JString) -> anyhow::Result<String> {
    let java_str = env.get_string(s)?;
    Ok(java_str.into())
}

/// Called by Kotlin from a Foreground Service started after the user
/// granted VPN consent. `tun_fd` is the file descriptor returned by
/// `VpnService.Builder::establish()`; the daemon takes ownership and
/// closes it on stop.
///
/// Returns 0 on failure, otherwise an opaque handle to be passed back
/// to [`Java_org_astervia_pim_PimDaemon_nativeStop`].
///
/// # Safety
///
/// Standard JNI: must be called by the JVM on a thread that has
/// already attached to the JNI environment.
#[no_mangle]
pub extern "system" fn Java_org_astervia_pim_PimDaemon_nativeStart(
    mut env: JNIEnv,
    _class: JClass,
    config_path: JString,
    data_dir: JString,
    tun_fd: jint,
) -> jlong {
    ensure_tracing();

    let config_path = match jstring_to_string(&mut env, &config_path) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(?e, "nativeStart: failed to decode config_path");
            return 0;
        }
    };
    let data_dir = match jstring_to_string(&mut env, &data_dir) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(?e, "nativeStart: failed to decode data_dir");
            return 0;
        }
    };

    // Daemon picks up the runtime socket / data dir via env vars on
    // android, since the FHS-style paths it falls back to (`/tmp`,
    // `~/.pim`) are not writable from inside an Android app sandbox.
    // The Java side passes the app's `getFilesDir()` here.
    let socket_path = format!("{}/pim.sock", data_dir);
    if std::env::var_os("PIM_RPC_SOCKET").is_none() {
        std::env::set_var("PIM_RPC_SOCKET", &socket_path);
    }
    std::env::set_var("PIM_DATA_DIR", &data_dir);

    // Stash the TUN fd in the env until pim-tun's android backend
    // reads it. Phase B Step 2 wires the env-var consumer side.
    std::env::set_var("PIM_TUN_FD", tun_fd.to_string());

    let config = match pim_core::Config::load(std::path::Path::new(&config_path)) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(?e, %config_path, "nativeStart: failed to load config");
            return 0;
        }
    };

    let cancel = CancellationToken::new();
    let cancel_for_run = cancel.clone();
    let config_path_buf = std::path::PathBuf::from(&config_path);

    let rt = ensure_runtime();
    let join = rt.spawn(async move {
        let result =
            crate::app::run_in_process(config, config_path_buf, cancel_for_run).await;
        if let Err(e) = &result {
            tracing::error!(error = ?e, "run_in_process exited with error");
        }
        result
    });

    let handle = Box::new(DaemonHandle { cancel, join });
    Box::into_raw(handle) as jlong
}

/// Stop the daemon previously started by `nativeStart`. Idempotent:
/// passing a zero handle is a no-op.
///
/// # Safety
///
/// `handle` must be either zero or a value previously returned by
/// `nativeStart` and not yet stopped.
#[no_mangle]
pub extern "system" fn Java_org_astervia_pim_PimDaemon_nativeStop(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) {
    if handle == 0 {
        return;
    }
    let handle = unsafe { Box::from_raw(handle as *mut DaemonHandle) };
    handle.cancel.cancel();
    if let Some(rt) = RUNTIME.get() {
        // Block briefly waiting for the run loop to wind down. If the
        // daemon doesn't exit within the deadline we drop the join
        // handle anyway — the foreground service is being torn down,
        // so the OS will reap us shortly.
        let _ = rt.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(5), handle.join).await
        });
    }
}

/// Reserved for future use: hand a socket fd to the daemon so its
/// gateway-egress sockets can be `protect()`ed against the VPN. Phase
/// B does NOT use this (Android Phase B ships relay+client only).
/// Returns false unconditionally for now.
///
/// # Safety
///
/// Standard JNI.
#[no_mangle]
pub extern "system" fn Java_org_astervia_pim_PimDaemon_nativeProvideProtect(
    _env: JNIEnv,
    _class: JClass,
    _socket: jint,
) -> jboolean {
    0
}
