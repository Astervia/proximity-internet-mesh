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
        let result = crate::app::run_in_process(config, config_path_buf, cancel_for_run).await;
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

// ─── Hello-envelope JNI exports ────────────────────────────────────────
//
// The Android RFCOMM bridge runs in Kotlin (it owns the Java
// `BluetoothSocket`), but the Hello/HelloAck protocol is a kernel
// concern: node identity, capability flags, and the optional
// `mesh_tag` HMAC all derive from data the daemon already loaded.
//
// We expose two helpers so the Kotlin side never sees the raw mesh
// handshake key:
//   * `nativeLocalIdentity(configPath)` — returns the JSON envelope
//     fields the Kotlin side needs to assemble its outgoing Hello.
//   * `nativeComputeMeshTag(configPath, peerNodeIdHex)` — returns the
//     32-hex `HMAC-SHA256(mesh_handshake_key, peer_node_id_hex)` so
//     the Kotlin side can verify an inbound Hello's `mesh_tag`.
//
// Both are stateless: they re-load the config + identity per call.
// That keeps the JNI surface narrow and avoids any cross-thread
// state. The cost is ~1 ms of disk + Argon2 work per call; fine
// because the Kotlin side calls them at most twice per RFCOMM
// session.

#[derive(serde::Serialize)]
struct LocalIdentityJson {
    /// 32-character lowercase hex of the local NodeId.
    node_id: String,
    /// Human-readable node label, with the
    /// `[bluetooth_rfcomm].device_name_prefix` already prepended.
    name: String,
    /// Always `"android"` — included so the JS layer doesn't have
    /// to hardcode it and the wire envelope stays self-describing.
    platform: &'static str,
    /// Capability flags for the Hello payload.
    caps: Vec<String>,
    /// `null` on the open mesh, otherwise the 32-hex
    /// `HMAC-SHA256(mesh_handshake_key, node_id_hex)`.
    mesh_tag: Option<String>,
}

/// Load the config + identity at `config_path` and return the local
/// Hello-envelope fields as a JSON string. Returns `null` on any
/// failure; the cause is logged at `error` level.
///
/// The Kotlin side calls this once per session-startup pass to build
/// its outbound `Hello` JSON. Repeated calls re-do the disk + Argon2
/// work; that's intentional so the Rust side never has to expose the
/// mesh handshake key over JNI or hold it in a global static.
///
/// # Safety
///
/// Standard JNI: must be called by the JVM on a thread that has
/// already attached to the JNI environment.
#[no_mangle]
pub extern "system" fn Java_org_astervia_pim_PimDaemon_nativeLocalIdentity<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    config_path: JString<'local>,
) -> jni::objects::JString<'local> {
    let config_path = match jstring_to_string(&mut env, &config_path) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(?e, "nativeLocalIdentity: failed to decode config_path");
            return jni::objects::JString::default();
        }
    };
    match build_local_identity(&config_path) {
        Ok(json) => match env.new_string(json) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(?e, "nativeLocalIdentity: failed to build JString");
                jni::objects::JString::default()
            }
        },
        Err(e) => {
            tracing::error!(?e, %config_path, "nativeLocalIdentity: failed to build identity");
            jni::objects::JString::default()
        }
    }
}

/// Notify the running daemon that the Kotlin RFCOMM bridge has
/// completed its Hello handshake with `peer_node_id_hex` and stood
/// the byte-bridge up. Calls into [`crate::app::notify_rfcomm_peer_discovered`]
/// which mirrors what the Linux-side `RfcommEvent::Discovered` handler
/// does — spawns the Noise initiator election so one side breaks the
/// responder-vs-responder deadlock that otherwise blocks the loopback
/// TCP transports from completing the Noise handshake.
///
/// No-op when the daemon hasn't booted yet (the Kotlin side may call
/// this from the BT accept loop before `nativeStart` has finished
/// publishing state); the Linux-side path has the same guard.
///
/// # Safety
///
/// Standard JNI.
#[no_mangle]
pub extern "system" fn Java_org_astervia_pim_PimDaemon_nativeNotifyRfcommPeerDiscovered<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    peer_node_id_hex: JString<'local>,
) {
    let peer_hex = match jstring_to_string(&mut env, &peer_node_id_hex) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(
                ?e,
                "nativeNotifyRfcommPeerDiscovered: failed to decode peer_node_id_hex"
            );
            return;
        }
    };
    crate::app::notify_rfcomm_peer_discovered(peer_hex);
}

/// Verify or build a `mesh_tag` for the given peer NodeId hex.
/// Returns the 32-hex tag, or `null` on the open mesh / on failure.
///
/// The Kotlin side calls this when validating an inbound Hello's
/// `mesh_tag` field: compute the expected tag for the peer's
/// `node_id`, then constant-time compare it against the received
/// value.
///
/// # Safety
///
/// Standard JNI.
#[no_mangle]
pub extern "system" fn Java_org_astervia_pim_PimDaemon_nativeComputeMeshTag<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    config_path: JString<'local>,
    peer_node_id_hex: JString<'local>,
) -> jni::objects::JString<'local> {
    let config_path = match jstring_to_string(&mut env, &config_path) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(?e, "nativeComputeMeshTag: failed to decode config_path");
            return jni::objects::JString::default();
        }
    };
    let peer_hex = match jstring_to_string(&mut env, &peer_node_id_hex) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(
                ?e,
                "nativeComputeMeshTag: failed to decode peer_node_id_hex"
            );
            return jni::objects::JString::default();
        }
    };
    match compute_mesh_tag_for(&config_path, &peer_hex) {
        Ok(Some(tag)) => match env.new_string(tag) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(?e, "nativeComputeMeshTag: failed to build JString");
                jni::objects::JString::default()
            }
        },
        Ok(None) => jni::objects::JString::default(),
        Err(e) => {
            tracing::error!(?e, %config_path, "nativeComputeMeshTag: failed to compute tag");
            jni::objects::JString::default()
        }
    }
}

/// Re-load the config at `config_path` and return the routing knobs
/// the Android `VpnService.Builder` needs to wire split-default + DNS
/// before `establish()` freezes the VPN config. Mirrors what the
/// Linux-side `route_installer` reads from `[routing]`.
///
/// The Kotlin side calls this once per `startDaemon` to feed
/// `Builder.addDnsServer(...)` and `Builder.addRoute("0.0.0.0", 1)` +
/// `Builder.addRoute("128.0.0.0", 1)` so the device's default-route
/// traffic flows through the mesh once the daemon is up.
///
/// Returns `null` on any failure; the cause is logged at `error` level.
#[no_mangle]
pub extern "system" fn Java_org_astervia_pim_PimDaemon_nativeRoutingConfig<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    config_path: JString<'local>,
) -> jni::objects::JString<'local> {
    let config_path = match jstring_to_string(&mut env, &config_path) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(?e, "nativeRoutingConfig: failed to decode config_path");
            return jni::objects::JString::default();
        }
    };
    match build_routing_config(&config_path) {
        Ok(json) => match env.new_string(json) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(?e, "nativeRoutingConfig: failed to build JString");
                jni::objects::JString::default()
            }
        },
        Err(e) => {
            tracing::error!(?e, %config_path, "nativeRoutingConfig: failed to load");
            jni::objects::JString::default()
        }
    }
}

#[derive(serde::Serialize)]
struct RoutingConfigJson {
    /// DNS servers from `[routing].dns_servers`. Used by the Kotlin
    /// `VpnService.Builder.addDnsServer(...)` calls so DNS resolution
    /// survives disabling Wi-Fi.
    dns_servers: Vec<String>,
    /// Mesh IPv4 address this node will adopt at boot, derived
    /// deterministically from the identity stored at
    /// `[security].key_file` via `pim_core::derive_mesh_ipv4` inside
    /// the configured `interface.mesh_ipv4_prefix`. The Kotlin side
    /// feeds this straight to `VpnService.Builder.addAddress(tun_ip,
    /// tun_prefix)` so the Android TUN matches the daemon's
    /// `state.mesh_ipv4` from the first packet — no `IpAssign`
    /// round-trip needed.
    tun_ip: String,
    /// CIDR prefix length of the mesh IPv4 prefix (default 16, from
    /// `pim_core::DEFAULT_MESH_IPV4_PREFIX = "10.77.0.0/16"`).
    tun_prefix: u8,
}

fn build_routing_config(config_path: &str) -> anyhow::Result<String> {
    let config = pim_core::Config::load(std::path::Path::new(config_path))?;
    // Resolve the configured mesh prefix (falls back to
    // `pim_core::DEFAULT_MESH_IPV4_PREFIX = 10.77.0.0/16` when unset).
    // Mirrors `app::runtime_config::parse_mesh_ipv4_prefix`, inlined
    // here because that helper is `pub(crate)` inside a private
    // submodule that `crate::app::*` doesn't re-export.
    let prefix_str = config
        .interface
        .mesh_ipv4_prefix
        .as_deref()
        .unwrap_or(pim_core::DEFAULT_MESH_IPV4_PREFIX);
    let prefix = pim_core::Ipv4Prefix::parse(prefix_str)
        .map_err(|e| anyhow::anyhow!("invalid interface.mesh_ipv4_prefix: {e}"))?;
    let key_file_str = config.security.key_file.to_string_lossy();
    let key_path = expand_home(&key_file_str);
    let identity = pim_crypto::Identity::load_or_generate(std::path::Path::new(&key_path))?;
    let mesh_ip = pim_core::derive_mesh_ipv4(&identity.node_id(), prefix);
    let payload = RoutingConfigJson {
        dns_servers: config.routing.dns_servers.clone(),
        tun_ip: mesh_ip.to_string(),
        tun_prefix: prefix.prefix_len,
    };
    Ok(serde_json::to_string(&payload)?)
}

/// Return the `pim-discovery` UDP socket's raw fd, or `0` if the
/// service hasn't bound yet OR `[discovery].enabled = false` in
/// `pim.toml`. The Kotlin side polls this from `PimVpnService` after
/// `nativeStart` and feeds the fd to `VpnService.protect(fd)` so the
/// PIMD broadcasts bypass the VPN TUN — otherwise they leak onto the
/// split-default route, get NAT'd by the gateway, and (in the worst
/// case before defence-in-depth filtering landed) hit raw-sendto
/// EACCES on `255.255.255.255`.
///
/// Returning `0` is the "absent" signal: Unix fds are >0 in practice,
/// and JNI doesn't carry Optional naturally.
#[no_mangle]
pub extern "system" fn Java_org_astervia_pim_PimDaemon_nativeDiscoverySocketFd(
    _env: JNIEnv,
    _class: JClass,
) -> jint {
    crate::app::current_discovery_socket_fd().unwrap_or(0)
}


/// Re-load the config + identity at `config_path` and assemble the
/// local Hello envelope as a JSON string. Pulls `[node].name`,
/// `[security].key_file`, `[bluetooth_rfcomm].device_name_prefix`,
/// `[gateway].enabled`, and `[mesh]` together exactly the way
/// `app::run_in_process` does on Linux — so a Kotlin-driven Hello
/// is byte-compatible with the Linux-driven one.
fn build_local_identity(config_path: &str) -> anyhow::Result<String> {
    let config = pim_core::Config::load(std::path::Path::new(config_path))?;
    let key_file_str = config.security.key_file.to_string_lossy();
    let key_path = expand_home(&key_file_str);
    let identity = pim_crypto::Identity::load_or_generate(std::path::Path::new(&key_path))?;
    let node_id_hex = identity.node_id().to_hex();

    let prefix = if config.bluetooth_rfcomm.device_name_prefix.is_empty() {
        pim_bluetooth::rfcomm::DEFAULT_PREFIX.to_string()
    } else {
        config.bluetooth_rfcomm.device_name_prefix.clone()
    };
    let name = format!("{prefix}{}", config.node.name);

    let mut caps = vec!["mesh-v1".to_string()];
    if config.gateway.enabled {
        caps.push("gateway-v1".to_string());
    }

    let mesh_tag = derive_mesh_handshake_key(&config.mesh)?
        .map(|key| pim_crypto::compute_rfcomm_hello_tag(&key, &node_id_hex))
        .map(|raw| bytes_to_hex(&raw));

    let payload = LocalIdentityJson {
        node_id: node_id_hex,
        name,
        platform: "android",
        caps,
        mesh_tag,
    };
    Ok(serde_json::to_string(&payload)?)
}

/// Compute `HMAC-SHA256(mesh_handshake_key, peer_node_id_hex)` for the
/// given peer. Returns `Ok(None)` when the local node is on the open
/// mesh (no handshake key derived).
fn compute_mesh_tag_for(
    config_path: &str,
    peer_node_id_hex: &str,
) -> anyhow::Result<Option<String>> {
    let config = pim_core::Config::load(std::path::Path::new(config_path))?;
    let key = derive_mesh_handshake_key(&config.mesh)?;
    Ok(key.map(|k| {
        let raw = pim_crypto::compute_rfcomm_hello_tag(&k, peer_node_id_hex);
        bytes_to_hex(&raw)
    }))
}

/// Mirror of `pim_daemon::app::build_mesh_secret` that returns just
/// the 32-byte handshake key. Kept in jni.rs because the daemon-side
/// builder is `pub(crate)` and we don't want to widen its visibility
/// just for this caller.
fn derive_mesh_handshake_key(
    mesh: &pim_core::config::MeshConfig,
) -> anyhow::Result<Option<[u8; 32]>> {
    use pim_core::config::MeshMode;
    match mesh.mode {
        MeshMode::Open => Ok(None),
        MeshMode::Private => {
            let passphrase = mesh
                .passphrase
                .as_deref()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!("[mesh] mode = \"private\" requires a non-empty passphrase")
                })?;
            let kdf = pim_crypto::MeshKdfParams {
                m_cost_kib: mesh.kdf.m_cost_kib,
                t_cost: mesh.kdf.t_cost,
                p_cost: mesh.kdf.p_cost,
            };
            let secret = pim_crypto::MeshSecret::derive(passphrase, mesh.mesh_id.as_deref(), kdf)?;
            Ok(Some(*secret.handshake_key()))
        }
    }
}

/// Lowercase hex encode a byte slice.
fn bytes_to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Mirror of `pim_daemon::auth::expand_tilde` for the JNI bridge —
/// expands `~` and `~user` style paths so a config that points at
/// `~/.pim/node.key` resolves correctly inside the Android sandbox.
fn expand_home(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return std::path::Path::new(&home)
                .join(rest)
                .to_string_lossy()
                .to_string();
        }
    }
    path.to_string()
}
