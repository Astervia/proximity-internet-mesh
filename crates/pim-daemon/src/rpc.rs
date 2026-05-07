//! JSON-RPC 2.0 server for `pim-daemon`.
//!
//! Spec: [`docs/RPC.md`] in this repo.
//!
//! Wire details:
//!   * Newline-delimited JSON messages over a Unix domain stream socket.
//!   * Path: `runtime_paths::rpc_socket_path()` — Linux user-runtime,
//!     macOS `$TMPDIR`, override via `$PIM_RPC_SOCKET`.
//!   * Mode `0660` so the daemon (or `pim` group) and the local UI can
//!     connect; everyone else is blocked by the filesystem.
//!
//! Architecture: one tokio task owns the listener and accepts. Each
//! accepted connection gets its own task. Per-connection state lives
//! on the stack of that task; cross-connection state (subscription
//! counters, etc.) lives behind `Arc` on `DaemonState`.
//!
//! Response handlers introspect `Arc<DaemonState>` directly — no
//! channels, no command pattern — because every relevant piece of
//! daemon state is already wrapped in interior-mutability primitives
//! (`Mutex`, `RwLock`, `Atomic*`). RPC handlers are read-mostly and
//! cheap; long-running mutations (like `config.save`) reach into the
//! same primitives the rest of the daemon uses.
//!
//! ## Method coverage
//!
//! Mirrors `RpcMethodMap` in `pim-ui/src/lib/rpc-types.ts`:
//!
//!   §2.1  rpc.hello                                — full
//!   §5.1  status, status.subscribe/unsubscribe     — full / id-only
//!   §5.2  peers.list/add_static/remove/discovered  — list/discovered full
//!         peers.pair, peers.subscribe/unsubscribe  — id-only / -32603 stub
//!   §5.3  route.table, route.set_split_default     — table full / no-op
//!   §5.4  gateway.preflight/enable/disable/status  — preflight + status full
//!         gateway.subscribe/unsubscribe            — id-only
//!   §5.5  config.get, config.save                  — full (atomic file IO)
//!   §5.6  logs.subscribe/unsubscribe               — id-only
//!
//! `*.subscribe` returns a unique subscription_id so the UI can store
//! it; this version does NOT yet stream notifications back. Adding
//! periodic `status.event` / `peers.event` / `logs.event` emission is
//! a follow-up — the wire format is the same `Notification` JSON-RPC
//! shape (no `id`, just `method` + `params`).

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, info, warn};

use pim_core::NodeId;
use pim_routing::RouteTableEntry;

use super::logs_subscriber;
use super::DaemonState;

/// Channel sender used by per-connection background tasks (subscription
/// forwarders) to enqueue notification frames without contending with
/// the request handler for socket-write access.
type WriteTx = mpsc::UnboundedSender<Vec<u8>>;

const RPC_VERSION: u64 = 1;

/// JSON-RPC error codes per docs/RPC.md §3.
mod codes {
    pub(super) const PARSE_ERROR: i32 = -32700;
    pub(super) const INVALID_REQUEST: i32 = -32600;
    pub(super) const METHOD_NOT_FOUND: i32 = -32601;
    pub(super) const INVALID_PARAMS: i32 = -32602;
    pub(super) const INTERNAL_ERROR: i32 = -32603;
    pub(super) const RPC_VERSION_MISMATCH: i32 = -32001;
    pub(super) const GATEWAY_NOT_SUPPORTED: i32 = -32031;
    #[cfg(feature = "messaging")]
    pub(super) const MESSAGE_PEER_UNKNOWN: i32 = -32060;
    #[cfg(feature = "messaging")]
    pub(super) const MESSAGE_BODY_TOO_LARGE: i32 = -32061;
    pub(super) const MESSAGE_STORAGE_ERROR: i32 = -32062;
    /// `peers.import_identity` was asked to overwrite an existing
    /// `peers_seen` row whose cached `x25519_pubkey` does not match the
    /// supplied value. Caller should confirm with the user before
    /// removing the stale entry and re-issuing the import.
    pub(super) const PEER_IDENTITY_MISMATCH: i32 = -32040;
    /// `peers.set_broadcast_config` was given an `outgoing_interval_s`
    /// below `BroadcastConfig::MIN_OUTGOING_INTERVAL_S` (currently 30).
    pub(super) const PEER_BROADCAST_INTERVAL_TOO_SMALL: i32 = -32041;
}

/// Format 32 raw bytes (e.g. an X25519 static public key) as 64-char
/// lowercase hex — the wire-encoding used everywhere we surface key
/// material on the JSON-RPC surface.
fn hex32(bytes: &[u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Module-private subscription-id allocator. Stable across reconnects
/// of the same daemon process; client side persists ids only for the
/// lifetime of one connection (per docs/RPC.md §4 — closing the
/// connection unsubscribes everything).
static SUB_COUNTER: AtomicU64 = AtomicU64::new(0);

fn new_subscription_id() -> String {
    let n = SUB_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("rpc-sub-{n}")
}

// ────────────────────────────────────────────────────────────────────────
// Listener / accept loop.
// ────────────────────────────────────────────────────────────────────────

pub(crate) async fn run_rpc_server(state: Arc<DaemonState>, socket_path: PathBuf) {
    // Best-effort cleanup of a stale socket from a previous run.
    let _ = std::fs::remove_file(&socket_path);

    if let Some(parent) = socket_path.parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                warn!(
                    "rpc: create parent dir {} failed: {e} — listener may fail to bind",
                    parent.display()
                );
            }
        }
    }

    let listener = match UnixListener::bind(&socket_path) {
        Ok(l) => l,
        Err(e) => {
            warn!("rpc: bind {} failed: {e}", socket_path.display());
            return;
        }
    };

    // Make the socket world-rw-for-its-group so user-mode UIs can connect
    // without being root. The directory permissions still gate access on
    // the system-daemon path; on the user-daemon path the dir is the
    // user's `$XDG_RUNTIME_DIR` / `$TMPDIR`, already private.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) =
            std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o660))
        {
            warn!("rpc: chmod 0660 socket failed: {e}");
        }
    }

    info!(path = %socket_path.display(), "rpc listening");

    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let st = state.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, st).await {
                        debug!("rpc connection ended: {e}");
                    }
                });
            }
            Err(e) => {
                warn!("rpc accept failed: {e}; sleeping 100ms before retrying");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

// ────────────────────────────────────────────────────────────────────────
// Per-connection handler.
// ────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct RpcRequest {
    #[allow(dead_code)]
    jsonrpc: Option<String>,
    id: Option<Value>,
    method: String,
    params: Option<Value>,
}

async fn handle_connection(stream: UnixStream, state: Arc<DaemonState>) -> Result<()> {
    let (rd, wr) = stream.into_split();
    // Single-writer task: serializes ALL bytes that go to the socket.
    // Both the request handler (responses) and any subscription forwarder
    // tasks (notifications) push onto this channel; the writer pops and
    // performs the actual `write_all`. Without this, two tokio tasks
    // would race on the OwnedWriteHalf and produce interleaved frames.
    let (write_tx, mut write_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let writer_handle = tokio::spawn(async move {
        let mut wr = wr;
        while let Some(bytes) = write_rx.recv().await {
            if wr.write_all(&bytes).await.is_err() {
                break;
            }
            let _ = wr.flush().await;
        }
    });

    // ONE status-event forwarder per connection. Spawned at connect
    // time and lives for the lifetime of the connection. We do NOT
    // spawn one per `status.subscribe` call — UIs that subscribe
    // multiple times (re-mount of components, reseed-after-reconnect,
    // etc.) used to spawn N forwarders that all receive the same
    // broadcast and write N duplicates to the socket. With ~50
    // accumulated forwarders, a single `route.set_split_default`
    // wrote 50 copies of `status.event` and saturated the IPC; the
    // pim-ui WebView froze for seconds processing the duplicates.
    //
    // status.subscribe / unsubscribe in the dispatch layer now just
    // hand out / discard subscription_ids without touching the
    // forwarder. The UI side filters on its end via the per-event
    // handler set in use-daemon-state.ts.
    let status_write_tx = write_tx.clone();
    let mut status_rx = state.status_events_tx.subscribe();
    let status_forwarder = tokio::spawn(async move {
        loop {
            match status_rx.recv().await {
                Ok(notif) => {
                    if push_value(&status_write_tx, &notif).is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    debug!(lagged = n, "status forwarder lagged; resyncing");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // ONE logs forwarder per connection. Same shape as the status
    // forwarder above: subscribe to the global `logs_subscriber`
    // broadcast and pump every event onto this connection's write
    // channel as a `logs.event` notification. UIs filter by level /
    // source on their side.
    //
    // History replay is owned separately by `logs.subscribe` (see
    // `start_logs_subscription`) and gated by the per-connection
    // `logs_history_replayed` AtomicBool below so the daemon emits the
    // historical buffer exactly once per connection no matter how many
    // times the UI calls `logs.subscribe` (StrictMode double-mount,
    // Vite HMR module replacement, simple↔advanced shell remount, …).
    // Before this refactor each `logs.subscribe` call spawned its own
    // forwarder + history-replay task — N calls per connection meant
    // every log line arrived N times in the UI buffer.
    let logs_history_replayed = Arc::new(AtomicBool::new(false));
    let logs_forwarder = match logs_subscriber::live_subscribe() {
        Some(mut logs_rx) => {
            let logs_write_tx = write_tx.clone();
            Some(tokio::spawn(async move {
                loop {
                    match logs_rx.recv().await {
                        Ok(event) => {
                            let notif = json!({
                                "jsonrpc": "2.0",
                                "method": "logs.event",
                                "params": event,
                            });
                            if push_value(&logs_write_tx, &notif).is_err() {
                                break;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            debug!(lagged = n, "logs forwarder lagged; resyncing");
                            continue;
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            }))
        }
        None => {
            warn!("logs_subscriber not initialised; logs.event will be silent on this connection");
            None
        }
    };

    // Per-connection messaging forwarder — only present when the
    // `messaging` feature is compiled in. Subscribes to the messaging
    // plugin's broadcast channel and pumps every event onto the
    // connection's write channel as a `messages.event` notification.
    #[cfg(feature = "messaging")]
    let messaging_forwarder = {
        let messaging_write_tx = write_tx.clone();
        let messaging_svc = state
            .messaging
            .get()
            .expect("messaging feature on but service not initialized")
            .clone();
        let mut messaging_rx = messaging_svc.subscribe();
        tokio::spawn(async move {
            loop {
                match messaging_rx.recv().await {
                    Ok(event) => {
                        let notif = match serde_json::to_value(&event) {
                            Ok(v) => v,
                            Err(e) => {
                                warn!("rpc: serialize messages.event failed: {e}");
                                continue;
                            }
                        };
                        let payload = json!({
                            "jsonrpc": "2.0",
                            "method": "messages.event",
                            "params": notif,
                        });
                        if push_value(&messaging_write_tx, &payload).is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        debug!(lagged = n, "messaging forwarder lagged; resyncing");
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        })
    };

    // Per-connection peer-directory forwarder — always on. Translates
    // [`pim_plugin::PeerDirectoryEvent`] into the `messages.event`
    // `peer_seen` JSON shape so the UI's existing subscriber sees
    // identity changes regardless of whether the messaging plugin is
    // compiled in.
    let peers_write_tx = write_tx.clone();
    let mut peers_rx = state.peer_directory.subscribe();
    let peers_forwarder = tokio::spawn(async move {
        use pim_plugin::{PeerDirectoryEvent, PeerInfoSource};
        loop {
            match peers_rx.recv().await {
                Ok(PeerDirectoryEvent::Seen {
                    node_id, name, via, ..
                }) => {
                    let payload = json!({
                        "jsonrpc": "2.0",
                        "method": "messages.event",
                        "params": {
                            "kind": "peer_seen",
                            "peer_node_id": crate::app::peer_directory::hex_node_id(&node_id),
                            "name": name,
                            "x25519_known": true,
                            "via": match via {
                                PeerInfoSource::Direct => "direct",
                                PeerInfoSource::Routed => "routed",
                            },
                        },
                    });
                    if push_value(&peers_write_tx, &payload).is_err() {
                        break;
                    }
                }
                Ok(PeerDirectoryEvent::Forgotten { node_id }) => {
                    let payload = json!({
                        "jsonrpc": "2.0",
                        "method": "messages.event",
                        "params": {
                            "kind": "peer_seen",
                            "peer_node_id": crate::app::peer_directory::hex_node_id(&node_id),
                            "name": "",
                            "x25519_known": false,
                            "via": "direct",
                        },
                    });
                    if push_value(&peers_write_tx, &payload).is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    debug!(lagged = n, "peers forwarder lagged; resyncing");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    let mut lines = BufReader::new(rd).lines();
    let mut handshake_done = false;

    while let Some(line) = lines.next_line().await.transpose() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                debug!("rpc read error: {e}");
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }

        let req: RpcRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                let resp = error_response(
                    Value::Null,
                    codes::PARSE_ERROR,
                    &format!("parse error: {e}"),
                    None,
                );
                if push_value(&write_tx, &resp).is_err() {
                    break;
                }
                continue;
            }
        };

        let id = req.id.clone().unwrap_or(Value::Null);
        debug!(method = %req.method, id = %id, "rpc request");

        // Enforce rpc.hello-first, per docs/RPC.md §1.4.
        if !handshake_done && req.method != "rpc.hello" {
            let resp = error_response(
                id,
                codes::INVALID_REQUEST,
                "rpc.hello must be the first request",
                None,
            );
            if push_value(&write_tx, &resp).is_err() {
                break;
            }
            continue;
        }

        let outcome = dispatch(&state, &req, &write_tx, &logs_history_replayed).await;
        let response = match outcome {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err((code, message, data)) => error_response(id, code, &message, data),
        };
        if push_value(&write_tx, &response).is_err() {
            break;
        }

        if req.method == "rpc.hello" {
            handshake_done = true;
        }
    }
    // Reader closed → drop the sender so the writer task exits, then
    // wait briefly for it to drain its queue.
    drop(write_tx);
    status_forwarder.abort();
    peers_forwarder.abort();
    #[cfg(feature = "messaging")]
    messaging_forwarder.abort();
    if let Some(handle) = logs_forwarder {
        handle.abort();
    }
    let _ = writer_handle.await;
    Ok(())
}

/// Serialize a Value to a newline-delimited byte buffer and enqueue
/// onto the per-connection writer channel. Returns Err only when the
/// writer task has already exited (peer hung up).
fn push_value(tx: &WriteTx, value: &Value) -> std::result::Result<(), ()> {
    let mut bytes = match serde_json::to_vec(value) {
        Ok(b) => b,
        Err(e) => {
            warn!("rpc: serialize failed: {e}");
            return Ok(());
        }
    };
    bytes.push(b'\n');
    tx.send(bytes).map_err(|_| ())
}

fn error_response(id: Value, code: i32, message: &str, data: Option<Value>) -> Value {
    let mut error_obj = json!({"code": code, "message": message});
    if let Some(d) = data {
        error_obj["data"] = d;
    }
    json!({"jsonrpc": "2.0", "id": id, "error": error_obj})
}

// ────────────────────────────────────────────────────────────────────────
// Dispatch + method handlers.
// ────────────────────────────────────────────────────────────────────────

type RpcResult = std::result::Result<Value, (i32, String, Option<Value>)>;

async fn dispatch(
    state: &Arc<DaemonState>,
    req: &RpcRequest,
    write_tx: &WriteTx,
    logs_history_replayed: &Arc<AtomicBool>,
) -> RpcResult {
    match req.method.as_str() {
        // §2.1
        "rpc.hello" => method_hello(req.params.as_ref()),

        // §5.1 status
        "status" => Ok(build_status(state).await),
        // status.event per pim-ui rpc-types.ts is a DISCRIMINATED KIND
        // union (interface_up/_down, gateway_selected/_lost, route_on/_off,
        // role_changed, kill_switch). The forwarder below subscribes to
        // `state.status_events_tx` and pumps every notification onto
        // this connection until the writer closes. Today only
        // `route.set_split_default` emits `route_on`/`route_off`; other
        // lifecycle events (interface_up/_down, gateway_selected/_lost,
        // role_changed, kill_switch) are TODO — when those daemon-
        // internal hooks gain wiring they push onto the same channel
        // and arrive here automatically.
        // Single forwarder is spawned per connection in handle_connection;
        // status.subscribe just hands out a subscription_id. UI side
        // filters incoming notifications via its per-event handler set.
        "status.subscribe" => Ok(json!({ "subscription_id": new_subscription_id() })),
        "status.unsubscribe" => Ok(Value::Null),

        // §5.2 peers
        "peers.list" => Ok(build_peer_list(state).await),
        "peers.add_static" => method_peers_add_static(state, req.params.as_ref()).await,
        "peers.connect_dynamic" => method_peers_connect_dynamic(state, req.params.as_ref()).await,
        "peers.remove" => method_peers_remove(state, req.params.as_ref()).await,
        "peers.discovered" => Ok(build_peers_discovered(state).await),
        "peers.pair" => Err((
            codes::INTERNAL_ERROR,
            "peers.pair: not yet implemented in this RPC server".into(),
            None,
        )),
        "peers.import_identity" => method_peers_import_identity(state, req.params.as_ref()).await,
        "peers.forget" => method_peers_forget(state, req.params.as_ref()).await,
        "peers.broadcast_identity_now" => method_peers_broadcast_identity_now(state).await,
        "peers.set_broadcast_config" => {
            method_peers_set_broadcast_config(state, req.params.as_ref()).await
        }
        "peers.get_broadcast_state" => Ok(build_broadcast_state(state).await),
        "peers.subscribe" => Ok(json!({ "subscription_id": new_subscription_id() })),
        "peers.unsubscribe" => Ok(Value::Null),

        // §5.3 routing
        "route.set_split_default" => method_route_set_split_default(state, req.params.as_ref()),
        "route.table" => Ok(build_route_table(state).await),

        // §5.4 gateway
        "gateway.preflight" => Ok(build_gateway_preflight(state)),
        "gateway.enable" => method_gateway_enable(state, req.params.as_ref()).await,
        "gateway.disable" => method_gateway_disable(state).await,
        "gateway.status" => Ok(build_gateway_status(state)),
        "gateway.subscribe" => Ok(json!({ "subscription_id": new_subscription_id() })),
        "gateway.unsubscribe" => Ok(Value::Null),

        // §5.5 config
        "config.get" => method_config_get(state, req.params.as_ref()).await,
        "config.save" => method_config_save(state, req.params.as_ref()).await,

        // §5.5b mesh — read-only status of the local node's mesh
        // membership. Returns `{ mode, mesh_id, fingerprint }`. The
        // passphrase itself is never returned. Mutating the mesh
        // (set/clear passphrase, change mode) goes through `config.save`
        // because it requires a daemon restart to re-derive the
        // Argon2id-stretched key.
        "mesh.status" => Ok(build_mesh_status(state).await),

        // §5.6 logs — `logs.event` notifications are pumped by the
        // per-connection forwarder spawned in `handle_connection`.
        // `logs.subscribe` only triggers the one-shot history replay
        // (gated by `logs_history_replayed` so it fires exactly once
        // per connection no matter how often the UI re-subscribes);
        // `logs.unsubscribe` is a no-op because the live forwarder is
        // tied to the connection's lifetime, not to subscription IDs.
        "logs.subscribe" => Ok(start_logs_subscription(
            req.params.as_ref(),
            write_tx.clone(),
            logs_history_replayed,
        )),
        "logs.unsubscribe" => Ok(Value::Null),

        // §5.7 messages — gated on the `messaging` Cargo feature so a
        // daemon built without it returns the standard "method not
        // found" error to UI clients.
        #[cfg(feature = "messaging")]
        "messages.list_conversations" => method_messages_list_conversations(state).await,
        #[cfg(feature = "messaging")]
        "messages.history" => method_messages_history(state, req.params.as_ref()).await,
        #[cfg(feature = "messaging")]
        "messages.send" => method_messages_send(state, req.params.as_ref()).await,
        #[cfg(feature = "messaging")]
        "messages.mark_read" => method_messages_mark_read(state, req.params.as_ref()).await,
        #[cfg(feature = "messaging")]
        "messages.delete_conversation" => {
            method_messages_delete_conversation(state, req.params.as_ref()).await
        }
        #[cfg(feature = "messaging")]
        "messages.delete_all" => method_messages_delete_all(state).await,
        #[cfg(feature = "messaging")]
        "messages.subscribe" => Ok(json!({ "subscription_id": new_subscription_id() })),
        #[cfg(feature = "messaging")]
        "messages.unsubscribe" => Ok(Value::Null),

        unknown => Err((
            codes::METHOD_NOT_FOUND,
            format!("method not found: {unknown}"),
            None,
        )),
    }
}

// ────────────────────────────────────────────────────────────────────────
// Subscription forwarders.
// ────────────────────────────────────────────────────────────────────────

/// Allocate a `subscription_id` and, on the FIRST call per connection,
/// replay the daemon's log history buffer onto this connection.
///
/// Live `logs.event` notifications are NOT spawned here — they're
/// pumped by the per-connection forwarder in `handle_connection`, so
/// extra `logs.subscribe` calls from the same connection (StrictMode
/// double-mount, Vite HMR, AppShell↔SimpleShell remount, ...) collapse
/// to a single forwarder + at most one history replay. Before this
/// refactor each call spawned its own forwarder + replay task, so
/// every log line arrived N times in the UI buffer.
///
/// The history replay covers the daemon's full startup sequence
/// ("daemon starting" → "TUN up" → "transport listening" → "rpc
/// listening" → ...) so the UI's Logs view is populated immediately
/// even though it subscribes well after those events fired.
///
/// The legacy `LogsSubscribeParams` filter (`min_level` / `levels` /
/// `sources`) is intentionally ignored: there is now ONE forwarder per
/// connection and per-call filters would need to multiplex the stream
/// at the wire level, which the UI doesn't actually use (it filters
/// client-side in `useLogsStream`). The CLI's `pim logs` command tails
/// the on-disk log file directly and never goes through this RPC.
fn start_logs_subscription(
    _params: Option<&Value>,
    write_tx: WriteTx,
    history_replayed: &Arc<AtomicBool>,
) -> Value {
    let id = new_subscription_id();
    if history_replayed.swap(true, Ordering::SeqCst) {
        // Already replayed history on this connection — no extra work.
        return json!({ "subscription_id": id });
    }
    let history = match logs_subscriber::history_snapshot() {
        Some(h) => h,
        None => {
            warn!("logs.subscribe: logs_subscriber not initialised; history replay skipped");
            return json!({ "subscription_id": id });
        }
    };
    tokio::spawn(async move {
        for event in history {
            let notif = json!({
                "jsonrpc": "2.0",
                "method": "logs.event",
                "params": event,
            });
            if push_value(&write_tx, &notif).is_err() {
                return;
            }
        }
    });
    json!({ "subscription_id": id })
}

// ── §2.1 ─────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct HelloParams {
    #[allow(dead_code)]
    client: Option<String>,
    rpc_version: Option<u64>,
}

fn method_hello(params: Option<&Value>) -> RpcResult {
    let parsed: HelloParams = match params {
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| {
            (
                codes::INVALID_PARAMS,
                format!("rpc.hello: invalid params: {e}"),
                None,
            )
        })?,
        None => HelloParams {
            client: None,
            rpc_version: None,
        },
    };
    if let Some(ver) = parsed.rpc_version {
        if ver != RPC_VERSION {
            return Err((
                codes::RPC_VERSION_MISMATCH,
                format!("rpc.hello: client wants rpc_version={ver}, daemon speaks {RPC_VERSION}"),
                None,
            ));
        }
    }
    Ok(json!({
        "daemon": format!("pim-daemon/{}", env!("CARGO_PKG_VERSION")),
        "rpc_version": RPC_VERSION,
        "features": [],
    }))
}

// ── §5.1 status ──────────────────────────────────────────────────────────

async fn build_status(state: &Arc<DaemonState>) -> Value {
    let mesh_ip_u32 = state.mesh_ip.load(Ordering::Relaxed);
    let mesh_ip = Ipv4Addr::from(mesh_ip_u32);
    let prefix_len = state.mesh_prefix_len.load(Ordering::Relaxed);
    let mesh_ip_cidr = format!("{mesh_ip}/{prefix_len}");

    let routing = state.routing.lock().await;
    let route_count = routing.route_count();
    let selected_gateway_id = routing.nearest_gateway_route().map(|(gid, _)| gid.to_hex());
    drop(routing);

    let uptime_s = state.start_time.elapsed().unwrap_or_default().as_secs();
    let started_at_iso = system_time_to_iso8601(state.start_time);

    let role = if state.is_gateway {
        json!(["client", "relay", "gateway"])
    } else {
        json!(["client"])
    };

    let listen_port = state.transport.listen_addr.port();
    let interface_name = state.tun.name();

    let x25519_pubkey_self = hex32(&state.own_x25519_pub);

    json!({
        "node": state.node_name,
        "node_id": state.self_id.to_hex(),
        "node_id_short": state.self_id.to_string(),
        "x25519_pubkey": x25519_pubkey_self,
        "mesh_ip": mesh_ip_cidr,
        "interface": {
            "name": interface_name,
            "up": true,
            "mtu": 1400,
        },
        "role": role,
        "transport": {
            "tcp": { "port": listen_port },
        },
        "peers": peer_summaries(state).await,
        "routes": {
            "active": route_count,
            "expired": 0,
            "selected_gateway": selected_gateway_id,
        },
        "stats": {
            "forwarded_bytes": state.bytes_forwarded.load(Ordering::Relaxed),
            "forwarded_packets": state.packets_forwarded.load(Ordering::Relaxed),
            "dropped": state.packets_dropped.load(Ordering::Relaxed),
            "dropped_reason": null,
            "congestion_drops": state.congestion_drops.load(Ordering::Relaxed),
            "conntrack_size": match &state.gw_engine {
                Some(gw) => gw.conntrack_size().await,
                None => 0,
            },
        },
        "uptime_s": uptime_s,
        "route_on": state.route_on.load(Ordering::SeqCst),
        "started_at": started_at_iso,
    })
}

fn system_time_to_iso8601(t: std::time::SystemTime) -> String {
    use std::time::UNIX_EPOCH;
    let secs = t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    // Minimal RFC-3339 formatter — avoids pulling chrono just for this.
    // Format: YYYY-MM-DDThh:mm:ssZ.
    let days_since_epoch = secs / 86_400;
    let secs_today = secs % 86_400;
    let hh = secs_today / 3600;
    let mm = (secs_today % 3600) / 60;
    let ss = secs_today % 60;
    let (y, mo, d) = days_to_ymd(days_since_epoch as i64);
    format!("{y:04}-{mo:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Convert days-since-1970-01-01 to (year, month, day). Pulled from the
/// canonical "Howard Hinnant" date algorithms (public domain) so we can
/// avoid a chrono dep — everything else in this module is std-only.
fn days_to_ymd(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i32 + era as i32 * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year, m, d)
}

// ── §5.2 peers ───────────────────────────────────────────────────────────

async fn peer_summaries(state: &Arc<DaemonState>) -> Vec<Value> {
    let sessions = state.sessions.read().await;

    // Snapshot the routing table once and index by destination NodeId
    // so each peer entry can read its current `is_gateway` / `hops` /
    // `rtt_ms` without holding the routing lock for the whole loop.
    let routing = state.routing.lock().await;
    let routes_by_id: HashMap<NodeId, RouteTableEntry> =
        routing.routes_snapshot().into_iter().collect();
    drop(routing);

    // Bulk-load every known peer's X25519 pubkey in one read so each
    // summary entry can attach its own key without a per-peer
    // round-trip from inside the async loop below.
    let x25519_by_node_hex: HashMap<String, String> = state
        .peer_directory
        .list_known_x25519_pubs()
        .await
        .unwrap_or_default();

    let mut out = Vec::with_capacity(sessions.len());
    for (peer_id, _session) in sessions.iter() {
        let info = state.reconnect.peer_info(peer_id).await;
        let (addr, mechanism, configured, _discovered) = match info {
            Some((target, configured, discovered)) => (
                target.addr().to_string(),
                target.mechanism_name().to_string(),
                configured,
                discovered,
            ),
            None => (String::new(), "tcp".to_string(), false, false),
        };
        let last_hb = state.peer_last_hb.lock().await.get(peer_id).copied();
        let last_seen_s = last_hb.map(|t| t.elapsed().as_secs()).unwrap_or(0);

        // Routing-table lookup: a peer that advertises gateway capability
        // ends up in our routing table with `is_gateway = true` once the
        // first RouteUpdate arrives. Direct neighbours always have
        // hops=1 in the snapshot, but reading from the table is more
        // honest than hard-coding it (transient updates, etc.) and is
        // a single HashMap lookup.
        let route = routes_by_id.get(peer_id);
        let is_gateway = route.map(|e| e.is_gateway).unwrap_or(false);
        let route_hops = route.map(|e| e.hops).unwrap_or(1);
        let latency_ms = route.and_then(|e| e.rtt_ms);

        let peer_hex = peer_id.to_hex();
        let x25519_pubkey = x25519_by_node_hex.get(&peer_hex).cloned();

        out.push(json!({
            "node_id": peer_hex,
            "node_id_short": peer_id.to_string(),
            "label": null,
            "mesh_ip": addr,
            "transport": mechanism,
            "state": "active",
            "route_hops": route_hops,
            "last_seen_s": last_seen_s,
            "latency_ms": latency_ms,
            "is_gateway": is_gateway,
            "static": configured,
            "x25519_pubkey": x25519_pubkey,
        }));
    }
    out
}

async fn build_peer_list(state: &Arc<DaemonState>) -> Value {
    Value::Array(peer_summaries(state).await)
}

async fn build_peers_discovered(state: &Arc<DaemonState>) -> Value {
    let table = match &state.discovery_peer_table {
        Some(t) => t,
        None => return Value::Array(vec![]),
    };
    let table = table.lock().await;
    let mut out: Vec<Value> = Vec::new();
    for record in table.all() {
        let last_seen_s = record.last_seen.elapsed().as_secs();
        let first_seen_s = record.last_seen.elapsed().as_secs(); // best effort
        out.push(json!({
            "node_id": record.node_id.to_hex(),
            "address": record.listen_addr.to_string(),
            "mechanism": "broadcast",
            "first_seen_s": first_seen_s,
            "last_seen_s": last_seen_s,
            "label_announced": null,
        }));
    }
    Value::Array(out)
}

#[derive(Debug, Deserialize)]
struct PeersAddStaticParams {
    address: String,
    mechanism: Option<String>,
    label: Option<String>,
}

async fn method_peers_add_static(_state: &Arc<DaemonState>, params: Option<&Value>) -> RpcResult {
    let p: PeersAddStaticParams = match params {
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| {
            (
                codes::INVALID_PARAMS,
                format!("peers.add_static: invalid params: {e}"),
                None,
            )
        })?,
        None => {
            return Err((
                codes::INVALID_PARAMS,
                "peers.add_static: params required".into(),
                None,
            ))
        }
    };
    // v0: we don't yet round-trip into the live config + reconnect
    // manager, so this is a stub-success that lets the UI complete its
    // optimistic add flow. The next iteration wires this into
    // `state.reconnect.add_target(...)`.
    Ok(json!({
        "node_id": null,
        "config_entry_id": format!("entry-{}-{}", p.mechanism.unwrap_or_else(|| "tcp".to_string()), p.address),
        "_label": p.label,
    }))
}

#[derive(Debug, Deserialize)]
struct PeersConnectDynamicParams {
    /// 32-character lowercase hex of the peer's 16-byte NodeId.
    node_id: String,
    /// Socket address to dial — typically `127.0.0.1:<port>` of a
    /// Bluetooth bridge, but any valid `SocketAddr` is accepted.
    address: String,
}

/// Open a TCP transport connection to the given address and identify
/// the remote peer with the supplied NodeId. Used by the UI to wire a
/// Bluetooth-discovered peer into the mesh: the `pim-ui` Tauri side
/// learns of the peer + its loopback bridge port from the BT sidecar
/// and asks the daemon to dial it as if it were a normal TCP peer.
async fn method_peers_connect_dynamic(
    state: &Arc<DaemonState>,
    params: Option<&Value>,
) -> RpcResult {
    let p: PeersConnectDynamicParams = match params {
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| {
            (
                codes::INVALID_PARAMS,
                format!("peers.connect_dynamic: invalid params: {e}"),
                None,
            )
        })?,
        None => {
            return Err((
                codes::INVALID_PARAMS,
                "peers.connect_dynamic: params required".into(),
                None,
            ))
        }
    };
    let bytes = parse_node_id_hex(&p.node_id).ok_or_else(|| {
        (
            codes::INVALID_PARAMS,
            format!(
                "peers.connect_dynamic: node_id must be 32 hex chars (got {})",
                p.node_id.len()
            ),
            None,
        )
    })?;
    let node_id = NodeId::from_bytes(bytes);
    let addr: std::net::SocketAddr = p.address.parse().map_err(|e: std::net::AddrParseError| {
        (
            codes::INVALID_PARAMS,
            format!("peers.connect_dynamic: address parse error: {e}"),
            None,
        )
    })?;
    use pim_transport::Transport;
    let peer = pim_transport::PeerAddress { node_id, addr };
    state.transport.connect(&peer).await.map_err(|e| {
        (
            codes::INTERNAL_ERROR,
            format!("peers.connect_dynamic: connect failed: {e}"),
            None,
        )
    })?;
    Ok(json!({
        "node_id": node_id.to_hex(),
        "address": p.address,
        "status": "connected",
    }))
}

fn parse_node_id_hex(s: &str) -> Option<[u8; 16]> {
    if s.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for i in 0..16 {
        out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

#[derive(Debug, Deserialize)]
struct GatewayEnableParams {
    nat_interface: String,
    #[allow(dead_code)]
    max_connections: Option<u32>,
}

/// Persist `[gateway].enabled = true` and `[gateway].nat_interface =
/// <iface>` into the live config file, then ask the UI to restart the
/// daemon. We don't yet hot-apply the gateway engine + iptables rules
/// at runtime — that's substantial work behind GatewayEngine — so the
/// UI-facing contract is: save now, restart daemon to actually serve.
/// The result still mirrors `GatewayEnableResult` with `active: false`
/// so the existing UI flow (which calls `onEnabled` on success) can
/// detect the save vs an actual live toggle.
async fn method_gateway_enable(state: &Arc<DaemonState>, params: Option<&Value>) -> RpcResult {
    if !gateway_supported_on_this_platform() {
        return Err((
            codes::GATEWAY_NOT_SUPPORTED,
            "gateway.enable: gateway mode requires linux or macos".into(),
            None,
        ));
    }
    let p: GatewayEnableParams = match params {
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| {
            (
                codes::INVALID_PARAMS,
                format!("gateway.enable: invalid params: {e}"),
                None,
            )
        })?,
        None => {
            return Err((
                codes::INVALID_PARAMS,
                "gateway.enable: params required".into(),
                None,
            ))
        }
    };
    if p.nat_interface.is_empty() {
        return Err((
            codes::INVALID_PARAMS,
            "gateway.enable: nat_interface required".into(),
            None,
        ));
    }
    let path = state.config_path.clone();
    let current = match tokio::fs::read_to_string(&path).await {
        Ok(s) => s,
        Err(e) => {
            return Err((
                codes::INTERNAL_ERROR,
                format!("gateway.enable: read config {}: {e}", path.display()),
                None,
            ))
        }
    };
    let mut doc: toml::Value = match toml::from_str(&current) {
        Ok(v) => v,
        Err(e) => {
            return Err((
                codes::INTERNAL_ERROR,
                format!("gateway.enable: parse current toml: {e}"),
                None,
            ))
        }
    };
    {
        let table = doc.as_table_mut().ok_or((
            codes::INTERNAL_ERROR,
            "gateway.enable: config root not a table".to_string(),
            None,
        ))?;
        let gw = table
            .entry("gateway".to_string())
            .or_insert_with(|| toml::Value::Table(Default::default()));
        let gw_table = gw.as_table_mut().ok_or((
            codes::INTERNAL_ERROR,
            "gateway.enable: [gateway] not a table".to_string(),
            None,
        ))?;
        gw_table.insert("enabled".to_string(), toml::Value::Boolean(true));
        gw_table.insert(
            "nat_interface".to_string(),
            toml::Value::String(p.nat_interface.clone()),
        );
        if let Some(max) = p.max_connections {
            gw_table.insert(
                "max_connections".to_string(),
                toml::Value::Integer(max as i64),
            );
        }
    }
    let new_toml = match toml::to_string_pretty(&doc) {
        Ok(s) => s,
        Err(e) => {
            return Err((
                codes::INTERNAL_ERROR,
                format!("gateway.enable: serialize toml: {e}"),
                None,
            ))
        }
    };
    if let Err(e) = super::fs_util::atomic_write(&path, new_toml.as_bytes()).await {
        return Err((
            codes::INTERNAL_ERROR,
            format!("gateway.enable: write {}: {e}", path.display()),
            None,
        ));
    }
    // active=false is intentional — config saved, but the running
    // daemon still has is_gateway=false until restart. UI mirrors this
    // via the requires_restart hint (extra field; rpc-types ignores
    // unknown JSON fields gracefully).
    Ok(json!({
        "active": false,
        "nat_interface": p.nat_interface,
        "advertised_routes": Vec::<String>::new(),
        "requires_restart": true,
        "written_to": path.to_string_lossy(),
    }))
}

/// Persist `[gateway].enabled = false` into the live config and
/// signal the UI that a restart is needed for the running daemon to
/// actually drop the gateway role. Same caveat as `gateway.enable`:
/// no in-place hot-toggle yet; the toml mutation just makes the next
/// startup pick up the new state.
async fn method_gateway_disable(state: &Arc<DaemonState>) -> RpcResult {
    if !gateway_supported_on_this_platform() {
        return Err((
            codes::GATEWAY_NOT_SUPPORTED,
            "gateway.disable: gateway mode requires linux or macos".into(),
            None,
        ));
    }
    let path = state.config_path.clone();
    let current = match tokio::fs::read_to_string(&path).await {
        Ok(s) => s,
        Err(e) => {
            return Err((
                codes::INTERNAL_ERROR,
                format!("gateway.disable: read config {}: {e}", path.display()),
                None,
            ))
        }
    };
    let mut doc: toml::Value = match toml::from_str(&current) {
        Ok(v) => v,
        Err(e) => {
            return Err((
                codes::INTERNAL_ERROR,
                format!("gateway.disable: parse current toml: {e}"),
                None,
            ))
        }
    };
    {
        let table = doc.as_table_mut().ok_or((
            codes::INTERNAL_ERROR,
            "gateway.disable: config root not a table".to_string(),
            None,
        ))?;
        let gw = table
            .entry("gateway".to_string())
            .or_insert_with(|| toml::Value::Table(Default::default()));
        let gw_table = gw.as_table_mut().ok_or((
            codes::INTERNAL_ERROR,
            "gateway.disable: [gateway] not a table".to_string(),
            None,
        ))?;
        gw_table.insert("enabled".to_string(), toml::Value::Boolean(false));
    }
    let new_toml = match toml::to_string_pretty(&doc) {
        Ok(s) => s,
        Err(e) => {
            return Err((
                codes::INTERNAL_ERROR,
                format!("gateway.disable: serialize toml: {e}"),
                None,
            ))
        }
    };
    if let Err(e) = super::fs_util::atomic_write(&path, new_toml.as_bytes()).await {
        return Err((
            codes::INTERNAL_ERROR,
            format!("gateway.disable: write {}: {e}", path.display()),
            None,
        ));
    }
    Ok(json!({
        "active": false,
        "requires_restart": true,
        "written_to": path.to_string_lossy(),
    }))
}

#[derive(Debug, Deserialize)]
struct PeersRemoveParams {
    node_id: Option<String>,
    config_entry_id: Option<String>,
}

/// `peers.import_identity` params — see docs/RPC.md §5.2.
#[derive(Debug, Deserialize)]
struct PeersImportIdentityParams {
    /// 32-char lowercase hex NodeId of the peer being imported.
    node_id: String,
    /// 64-char lowercase hex of the peer's X25519 static public key.
    x25519_pubkey: String,
    /// Optional friendly label — when omitted or empty, the existing
    /// label (if any) is preserved.
    #[serde(default)]
    friendly_name: Option<String>,
}

fn parse_x25519_hex(s: &str) -> Result<[u8; 32], (i32, String, Option<Value>)> {
    if s.len() != 64 {
        return Err((
            codes::INVALID_PARAMS,
            format!("x25519_pubkey must be 64 hex characters, got {}", s.len()),
            None,
        ));
    }
    let mut out = [0u8; 32];
    for (idx, chunk) in s.as_bytes().chunks_exact(2).enumerate() {
        let pair = std::str::from_utf8(chunk).map_err(|_| {
            (
                codes::INVALID_PARAMS,
                "x25519_pubkey contains non-utf8 bytes".into(),
                None,
            )
        })?;
        out[idx] = u8::from_str_radix(pair, 16).map_err(|_| {
            (
                codes::INVALID_PARAMS,
                format!("x25519_pubkey contains invalid hex: {pair}"),
                None,
            )
        })?;
    }
    Ok(out)
}

async fn method_peers_import_identity(
    state: &Arc<DaemonState>,
    params: Option<&Value>,
) -> RpcResult {
    let parsed: PeersImportIdentityParams = match params {
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| {
            (
                codes::INVALID_PARAMS,
                format!("peers.import_identity: invalid params: {e}"),
                None,
            )
        })?,
        None => {
            return Err((
                codes::INVALID_PARAMS,
                "peers.import_identity: params required".into(),
                None,
            ))
        }
    };

    let node_id: NodeId = parsed.node_id.parse().map_err(|e| {
        (
            codes::INVALID_PARAMS,
            format!("peers.import_identity: invalid node_id: {e}"),
            None,
        )
    })?;
    let x25519 = parse_x25519_hex(&parsed.x25519_pubkey)?;
    let friendly_name = parsed
        .friendly_name
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    let outcome = state
        .peer_directory
        .import_identity_if_compatible(node_id, x25519, friendly_name, now_ms)
        .await
        .map_err(|e| {
            (
                codes::MESSAGE_STORAGE_ERROR,
                format!("peers.import_identity: {e}"),
                None,
            )
        })?;

    match outcome {
        crate::app::peer_directory::ImportOutcome::Inserted => Ok(json!({
            "node_id": node_id.to_hex(),
            "node_id_short": node_id.to_string(),
            "imported": true,
        })),
        crate::app::peer_directory::ImportOutcome::Refreshed => Ok(json!({
            "node_id": node_id.to_hex(),
            "node_id_short": node_id.to_string(),
            "imported": false,
        })),
        crate::app::peer_directory::ImportOutcome::KeyMismatch {
            existing_x25519_hex,
        } => Err((
            codes::PEER_IDENTITY_MISMATCH,
            format!(
                "peers.import_identity: node_id {} is already cached with a different x25519 key",
                node_id.to_hex()
            ),
            Some(json!({
                "node_id": node_id.to_hex(),
                "supplied_x25519_pubkey": parsed.x25519_pubkey,
                "existing_x25519_pubkey": existing_x25519_hex,
            })),
        )),
    }
}

// ── Delete utilities ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct PeersForgetParams {
    /// 32-char lowercase hex NodeId.
    node_id: String,
}

async fn method_peers_forget(state: &Arc<DaemonState>, params: Option<&Value>) -> RpcResult {
    let parsed: PeersForgetParams = match params {
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| {
            (
                codes::INVALID_PARAMS,
                format!("peers.forget: invalid params: {e}"),
                None,
            )
        })?,
        None => {
            return Err((
                codes::INVALID_PARAMS,
                "peers.forget: params required".into(),
                None,
            ))
        }
    };
    let node_id: NodeId = parsed.node_id.parse().map_err(|e| {
        (
            codes::INVALID_PARAMS,
            format!("peers.forget: invalid node_id: {e}"),
            None,
        )
    })?;
    let outcome = state.peer_directory.forget(node_id).await.map_err(|e| {
        (
            codes::MESSAGE_STORAGE_ERROR,
            format!("peers.forget: {e}"),
            None,
        )
    })?;

    // Notify every loaded plugin so they can wipe per-peer state of
    // their own (e.g. messaging deletes the message history). Plugins
    // are responsible for emitting their own follow-up events
    // (`history_cleared` etc.) on whatever channels they own.
    if outcome.forgot_identity {
        if let Some(plugins) = state.plugins.get() {
            for plugin in plugins {
                plugin.on_peer_forgotten(node_id).await;
            }
        }
    }

    Ok(json!({
        "forgot_identity": outcome.forgot_identity,
    }))
}

#[cfg(feature = "messaging")]
#[derive(Debug, Deserialize)]
struct MessagesDeleteConversationParams {
    peer_node_id: String,
}

#[cfg(feature = "messaging")]
async fn method_messages_delete_conversation(
    state: &Arc<DaemonState>,
    params: Option<&Value>,
) -> RpcResult {
    let parsed: MessagesDeleteConversationParams = match params {
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| {
            (
                codes::INVALID_PARAMS,
                format!("messages.delete_conversation: invalid params: {e}"),
                None,
            )
        })?,
        None => {
            return Err((
                codes::INVALID_PARAMS,
                "messages.delete_conversation: params required".into(),
                None,
            ))
        }
    };
    let node_id: NodeId = parsed.peer_node_id.parse().map_err(|e| {
        (
            codes::INVALID_PARAMS,
            format!("messages.delete_conversation: invalid peer_node_id: {e}"),
            None,
        )
    })?;
    let (deleted_messages, deleted_conversation) = messaging_service(state)
        .delete_conversation(node_id)
        .await
        .map_err(|e| {
            (
                codes::MESSAGE_STORAGE_ERROR,
                format!("messages.delete_conversation: {e}"),
                None,
            )
        })?;
    Ok(json!({
        "deleted_messages": deleted_messages,
        "deleted_conversation": deleted_conversation,
    }))
}

#[cfg(feature = "messaging")]
async fn method_messages_delete_all(state: &Arc<DaemonState>) -> RpcResult {
    let (deleted_messages, deleted_conversations) = messaging_service(state)
        .delete_all_messages()
        .await
        .map_err(|e| {
            (
                codes::MESSAGE_STORAGE_ERROR,
                format!("messages.delete_all: {e}"),
                None,
            )
        })?;
    Ok(json!({
        "deleted_messages": deleted_messages,
        "deleted_conversations": deleted_conversations,
    }))
}

/// Borrow the messaging service handle. Panics when the `messaging`
/// feature is enabled but the plugin failed to initialize at startup —
/// that's a programmer error, since `app::run` aborts on plugin start
/// failure.
#[cfg(feature = "messaging")]
fn messaging_service(state: &Arc<DaemonState>) -> &Arc<pim_messaging::MessagingService> {
    state
        .messaging
        .get()
        .expect("messaging service initialized at daemon startup")
}

// ── Broadcast control ────────────────────────────────────────────────────

async fn build_broadcast_state(state: &Arc<DaemonState>) -> Value {
    let cfg = state.broadcast_config.read().await.clone();
    let last_ms = state.last_broadcast_ms.load(Ordering::Relaxed);
    let last_recipients = state.last_broadcast_recipients.load(Ordering::Relaxed);
    let last_broadcast_ms = if last_ms == i64::MIN {
        Value::Null
    } else {
        json!(last_ms)
    };
    let last_recipient_count = if last_ms == i64::MIN {
        Value::Null
    } else {
        json!(last_recipients)
    };
    json!({
        "outgoing_interval_s": cfg.outgoing_interval_s,
        "watch_incoming": cfg.watch_incoming,
        "min_peer_interval_s": cfg.min_peer_interval_s,
        "last_broadcast_ms": last_broadcast_ms,
        "last_recipient_count": last_recipient_count,
    })
}

async fn method_peers_broadcast_identity_now(state: &Arc<DaemonState>) -> RpcResult {
    let outcome = crate::app::identity_broadcast::run_broadcast_cycle(state).await;
    Ok(json!({
        "recipients": outcome.recipients,
        "sent_at_ms": state.last_broadcast_ms.load(Ordering::Relaxed),
    }))
}

#[derive(Debug, Deserialize, Default)]
struct PeersSetBroadcastConfigParams {
    /// `Some(None)` disables; `Some(Some(secs))` sets; `None` leaves
    /// unchanged. We model the "unset vs explicitly null" distinction
    /// with `Option<Option<u64>>` because TOML/JSON null is meaningful
    /// here (it's the way the UI expresses "disable broadcasts").
    #[serde(default, deserialize_with = "deserialize_some_option")]
    outgoing_interval_s: Option<Option<u64>>,
    #[serde(default)]
    watch_incoming: Option<bool>,
    #[serde(default)]
    min_peer_interval_s: Option<u64>,
}

/// Deserialize `Option<Option<T>>` so a missing key stays `None` (no
/// change) but an explicit `null` becomes `Some(None)` (clear). serde's
/// default `Option` deserializer collapses both cases to `None`, which
/// would make "disable" indistinguishable from "leave unchanged".
fn deserialize_some_option<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

async fn method_peers_set_broadcast_config(
    state: &Arc<DaemonState>,
    params: Option<&Value>,
) -> RpcResult {
    let parsed: PeersSetBroadcastConfigParams = match params {
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| {
            (
                codes::INVALID_PARAMS,
                format!("peers.set_broadcast_config: invalid params: {e}"),
                None,
            )
        })?,
        None => PeersSetBroadcastConfigParams::default(),
    };

    // Validate the proposed outgoing interval before touching state.
    if let Some(Some(secs)) = parsed.outgoing_interval_s {
        if secs < pim_core::BroadcastConfig::MIN_OUTGOING_INTERVAL_S {
            return Err((
                codes::PEER_BROADCAST_INTERVAL_TOO_SMALL,
                format!(
                    "peers.set_broadcast_config: outgoing_interval_s must be >= {} (got {secs})",
                    pim_core::BroadcastConfig::MIN_OUTGOING_INTERVAL_S,
                ),
                None,
            ));
        }
    }
    if let Some(min) = parsed.min_peer_interval_s {
        if min == 0 {
            return Err((
                codes::INVALID_PARAMS,
                "peers.set_broadcast_config: min_peer_interval_s must be > 0".into(),
                None,
            ));
        }
    }

    // Apply in-memory immediately.
    {
        let mut guard = state.broadcast_config.write().await;
        if let Some(v) = parsed.outgoing_interval_s {
            guard.outgoing_interval_s = v;
        }
        if let Some(v) = parsed.watch_incoming {
            guard.watch_incoming = v;
        }
        if let Some(v) = parsed.min_peer_interval_s {
            guard.min_peer_interval_s = v;
        }
    }
    // Wake the broadcast task so a freshly-enabled / freshly-disabled
    // schedule takes effect on the next loop iteration instead of
    // waiting for the current sleep to finish.
    state.broadcast_notify.notify_one();

    // Persist to pim.toml so the change survives restart. Loses
    // user-written comments (current `Config::to_toml_string()` is a
    // serde-rountdrip writer). Acceptable for now; a comment-preserving
    // editor can land later via `toml_edit`.
    if let Err(e) = persist_broadcast_config_to_disk(state).await {
        warn!("peers.set_broadcast_config: in-memory updated, but persistence failed: {e}");
        // Don't fail the RPC — the user's intent is honoured for this
        // session; the only loss is restart-survival.
    }

    Ok(build_broadcast_state(state).await)
}

/// Re-read pim.toml, splice in the current in-memory broadcast config,
/// and atomic-write it back. Called after `set_broadcast_config`.
async fn persist_broadcast_config_to_disk(state: &Arc<DaemonState>) -> anyhow::Result<()> {
    use anyhow::{anyhow, Context};

    let path = state.config_path.clone();
    let snapshot = state.broadcast_config.read().await.clone();
    // Hop into spawn_blocking so std::fs is fine and we don't hold any
    // tokio executor-side state across the read+parse+write window.
    let path_for_task = path.clone();
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let mut config = pim_core::Config::load(&path_for_task).map_err(|e| {
            anyhow!(
                "re-parse {} for broadcast persistence: {e}",
                path_for_task.display()
            )
        })?;
        config.messaging.broadcast = snapshot;
        let serialized = config
            .to_toml_string()
            .map_err(|e| anyhow!("serialize config for broadcast persistence: {e}"))?;
        // The async fs_util::atomic_write requires the tokio runtime; use
        // its sync sibling here. There isn't one yet, so do a plain
        // tempfile + rename inline — semantics match atomic_write.
        let parent = path_for_task
            .parent()
            .ok_or_else(|| anyhow!("config path has no parent"))?;
        let tmp = tempfile_in_same_dir(parent, &path_for_task)?;

        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }

        {
            use std::io::Write;
            let mut file = options
                .open(&tmp)
                .with_context(|| format!("create tmp {}", tmp.display()))?;
            file.write_all(serialized.as_bytes())
                .with_context(|| format!("write tmp {}", tmp.display()))?;
            file.sync_all()
                .with_context(|| format!("sync tmp {}", tmp.display()))?;
        }

        std::fs::rename(&tmp, &path_for_task)
            .with_context(|| format!("rename {} -> {}", tmp.display(), path_for_task.display()))?;
        Ok(())
    })
    .await
    .map_err(|e| anyhow!("persist join: {e}"))??;
    let _ = path;
    Ok(())
}

/// Build a sibling tempfile path next to `final_path` for the inline
/// rename-into-place writer used by `persist_broadcast_config_to_disk`.
fn tempfile_in_same_dir(
    parent: &std::path::Path,
    final_path: &std::path::Path,
) -> anyhow::Result<std::path::PathBuf> {
    use anyhow::anyhow;
    let stem = final_path
        .file_name()
        .ok_or_else(|| anyhow!("config path has no file name"))?
        .to_string_lossy()
        .to_string();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    Ok(parent.join(format!(".{stem}.tmp.{nonce}")))
}

async fn method_peers_remove(_state: &Arc<DaemonState>, params: Option<&Value>) -> RpcResult {
    let p: PeersRemoveParams = match params {
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| {
            (
                codes::INVALID_PARAMS,
                format!("peers.remove: invalid params: {e}"),
                None,
            )
        })?,
        None => {
            return Err((
                codes::INVALID_PARAMS,
                "peers.remove: params required".into(),
                None,
            ))
        }
    };
    if p.node_id.is_none() && p.config_entry_id.is_none() {
        return Err((
            codes::INVALID_PARAMS,
            "peers.remove: one of node_id or config_entry_id required".into(),
            None,
        ));
    }
    // v0: stub-success. Real implementation would remove from
    // `state.reconnect`, drop the session, and persist the config.
    Ok(Value::Null)
}

// ── §5.3 routing ─────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Default)]
struct RouteSetSplitDefaultParams {
    #[serde(default)]
    on: bool,
}

/// Toggle split-default routing.
///
/// Three side-effects (in order):
///   1. Store `parsed.on` into `state.route_on` atomically.
///   2. Wake the `route_installer` background task so it reconciles
///      the kernel's `0.0.0.0/1`/`128.0.0.0/1` routes within one async
///      hop instead of waiting for its 2 s tick. The installer reads
///      the same atomic + the routing table's selected gateway and
///      decides what to install/remove — no per-RPC `ip route` call
///      from this handler so concurrent RPC calls can't race against
///      each other.
///   3. Broadcast a discriminated `status.event` (`route_on` /
///      `route_off`) so subscribed UIs flip the toggle badge without
///      a follow-up `status` poll. SendError on no-subscribers is fine
///      — late connections pick up the state via the next `status`.
fn method_route_set_split_default(state: &Arc<DaemonState>, params: Option<&Value>) -> RpcResult {
    info!(?params, "route.set_split_default invoked");
    let parsed: RouteSetSplitDefaultParams = match params {
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| {
            (
                codes::INVALID_PARAMS,
                format!("route.set_split_default params: {e}"),
                None,
            )
        })?,
        None => RouteSetSplitDefaultParams::default(),
    };
    info!(on = parsed.on, "route.set_split_default parsed; storing");

    state
        .route_on
        .store(parsed.on, std::sync::atomic::Ordering::SeqCst);
    state.route_install_notify.notify_one();

    let kind = if parsed.on { "route_on" } else { "route_off" };
    let send_result = state.status_events_tx.send(json!({
        "jsonrpc": "2.0",
        "method": "status.event",
        "params": { "kind": kind },
    }));
    match &send_result {
        Ok(n) => info!(kind, subscribers = n, "status.event broadcast sent"),
        Err(_) => warn!(
            kind,
            "status.event broadcast had ZERO subscribers — UIs won't see the flip"
        ),
    }

    Ok(json!({
        "on": parsed.on,
        "via_gateway_id": null,
    }))
}

async fn build_route_table(state: &Arc<DaemonState>) -> Value {
    use pim_routing::gateway_score;
    let routing = state.routing.lock().await;
    let entries = routing.routes_snapshot();
    let selected_gateway_id = routing.nearest_gateway_route().map(|(gid, _)| gid);

    let mut routes: Vec<Value> = Vec::with_capacity(entries.len());
    let mut gateways: Vec<Value> = Vec::new();
    for (dst, entry) in entries {
        let mesh_ip = entry.mesh_ip.map(|ip| ip.to_string());
        let age_s = entry.last_seen.elapsed().as_secs();
        routes.push(json!({
            "destination": mesh_ip.clone().unwrap_or_else(|| dst.to_hex()),
            "via": entry.next_hop.to_hex(),
            "hops": entry.hops,
            "learned_from": entry.learned_from.to_hex(),
            "is_gateway": entry.is_gateway,
            "load": entry.gateway_load,
            "age_s": age_s,
        }));
        if entry.is_gateway {
            gateways.push(json!({
                "node_id": dst.to_hex(),
                "via": entry.next_hop.to_hex(),
                "hops": entry.hops,
                "score": gateway_score(entry.hops, entry.gateway_load, entry.rtt_ms),
                "selected": selected_gateway_id == Some(dst),
            }));
        }
    }
    json!({ "routes": routes, "gateways": gateways })
}

// ── §5.4 gateway ─────────────────────────────────────────────────────────

/// Whether this build of the daemon can act as a gateway. Linux + macOS
/// only — Windows / other targets lack the NAT engine. Used both by
/// `gateway.preflight` (to advertise capability) and by
/// `gateway.{enable,disable}` (to short-circuit with
/// `GATEWAY_NOT_SUPPORTED` instead of writing a config the daemon will
/// never honor).
fn gateway_supported_on_this_platform() -> bool {
    cfg!(any(target_os = "linux", target_os = "macos"))
}

fn build_gateway_preflight(_state: &Arc<DaemonState>) -> Value {
    let supported = gateway_supported_on_this_platform();
    let platform = if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "other"
    };
    json!({
        "supported": supported,
        "platform": platform,
        "checks": [
            {
                "name": "platform_supported",
                "ok": supported,
                "detail": if supported {
                    "linux + macos support gateway mode in this build"
                } else {
                    "gateway mode requires linux or macos"
                },
            }
        ],
        "suggested_nat_interfaces": list_nat_candidate_interfaces(),
    })
}

/// Enumerate physical-ish network interfaces that a user might pick as
/// the upstream NAT egress. Uses the cheapest per-platform listing
/// (`ifconfig -l` on macOS, `/sys/class/net` on Linux) and filters out
/// obviously-virtual or kernel-managed interfaces (loopback, utun,
/// PIM's own TUN, bridges, docker, awdl/llw/anpi on macOS, etc.). The
/// UI picker calls this via `gateway.preflight`.
fn list_nat_candidate_interfaces() -> Vec<String> {
    let names: Vec<String> = {
        #[cfg(target_os = "macos")]
        {
            std::process::Command::new("ifconfig")
                .arg("-l")
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| {
                    String::from_utf8_lossy(&o.stdout)
                        .split_whitespace()
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default()
        }
        #[cfg(target_os = "linux")]
        {
            std::fs::read_dir("/sys/class/net")
                .ok()
                .map(|rd| {
                    rd.filter_map(|e| e.ok())
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default()
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            Vec::<String>::new()
        }
    };
    names
        .into_iter()
        .filter(is_nat_candidate)
        .filter(|name| interface_has_ipv4(name))
        .collect()
}

/// Returns true iff the interface currently has at least one IPv4
/// address bound. Filters out interfaces that are UP-but-unconfigured
/// (Thunderbolt bridges, USB-C dongles without DHCP, secondary Wi-Fi
/// adapters with no association). Without this check, the picker
/// surfaces interfaces that crash the gateway runtime with
/// `failed to resolve IPv4 address for <iface>`.
fn interface_has_ipv4(name: &str) -> bool {
    // `ifconfig <iface>` works on both macOS and Linux; we just need
    // any line containing "inet " (IPv4 only — `inet6` is too loose
    // since most interfaces have a link-local v6).
    let out = match std::process::Command::new("ifconfig").arg(name).output() {
        Ok(o) if o.status.success() => o,
        _ => return false,
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    stdout.lines().any(|l| l.trim_start().starts_with("inet "))
}

/// Heuristic for whether a kernel-reported interface is plausibly a
/// real upstream NAT egress vs. a virtual/internal interface. Pulled
/// out for unit testability.
fn is_nat_candidate(name: &String) -> bool {
    if name.is_empty() {
        return false;
    }
    // Loopback (Linux: lo, macOS: lo0).
    if name == "lo" || name == "lo0" {
        return false;
    }
    // PIM's own TUN, the macOS userspace tunnel framework, and Apple's
    // peer-to-peer / nearby-discovery interfaces — never useful as
    // egress.
    if name.starts_with("utun")
        || name.starts_with("pim")
        || name.starts_with("awdl")
        || name.starts_with("llw")
        || name.starts_with("anpi")
        || name == "ap1"
    {
        return false;
    }
    // Linux: bridges, container/VM virtual NICs, tunnels.
    if name.starts_with("br-")
        || name.starts_with("bridge")
        || name.starts_with("docker")
        || name.starts_with("veth")
        || name.starts_with("vmnet")
        || name.starts_with("tun")
        || name.starts_with("tap")
    {
        return false;
    }
    // gif / stf are macOS legacy IPv6/IPv4 transition interfaces.
    if name.starts_with("gif") || name.starts_with("stf") {
        return false;
    }
    true
}

fn build_gateway_status(state: &Arc<DaemonState>) -> Value {
    json!({
        "active": state.is_gateway,
        "nat_interface": state.gateway_nat_interface,
        "advertised_routes": Vec::<String>::new(),
    })
}

// ── §5.5b mesh ────────────────────────────────────────────────────────────

/// Read-only snapshot of the local node's mesh membership.
///
/// Returns `{ mode: "open"|"private", mesh_id: string|null,
/// fingerprint: hex|null }`. Never includes the passphrase or any
/// derived key bytes — `fingerprint` is the
/// `pim_crypto::MeshSecret::fingerprint` 8-byte value, intended for
/// UI confirmation that two nodes share a mesh.
async fn build_mesh_status(state: &Arc<DaemonState>) -> Value {
    let (mode, fingerprint) = match (&state.mesh_handshake_key, &state.mesh_fingerprint) {
        (Some(_), Some(fp)) => {
            let mut hex = String::with_capacity(16);
            for b in fp {
                hex.push_str(&format!("{b:02x}"));
            }
            ("private", Some(hex))
        }
        _ => ("open", None),
    };
    json!({
        "mode": mode,
        "mesh_id": state.mesh_id,
        "fingerprint": fingerprint,
    })
}

// ── §5.5 config ──────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ConfigGetParams {
    format: Option<String>,
}

async fn method_config_get(state: &Arc<DaemonState>, params: Option<&Value>) -> RpcResult {
    let format = params
        .and_then(|v| serde_json::from_value::<ConfigGetParams>(v.clone()).ok())
        .and_then(|p| p.format)
        .unwrap_or_else(|| "toml".to_string());
    let path = state.config_path.clone();
    let content = match tokio::fs::read_to_string(&path).await {
        Ok(s) => s,
        Err(e) => {
            return Err((
                codes::INTERNAL_ERROR,
                format!("config.get: read {}: {e}", path.display()),
                None,
            ))
        }
    };
    // Stat for last_modified — UI's ConfigGetResult requires this field.
    // Best-effort: any failure falls back to the daemon start time.
    let last_modified_iso = match tokio::fs::metadata(&path).await {
        Ok(meta) => match meta.modified() {
            Ok(t) => system_time_to_iso8601(t),
            Err(_) => system_time_to_iso8601(state.start_time),
        },
        Err(_) => system_time_to_iso8601(state.start_time),
    };
    // Field names match pim-ui's `ConfigGetResult` (rpc-types.ts §5.5):
    //   format, config, source_path, last_modified.
    Ok(json!({
        "format": format,
        "config": content,
        "source_path": path.to_string_lossy(),
        "last_modified": last_modified_iso,
    }))
}

#[derive(Debug, Deserialize)]
struct ConfigSaveParams {
    /// Raw document content. Field name mirrors pim-ui's
    /// `ConfigSaveParams.config` (rpc-types.ts §5.5).
    config: String,
    #[allow(dead_code)]
    format: Option<String>,
    /// When true, validate only — do not write to disk.
    #[serde(default)]
    dry_run: bool,
}

async fn method_config_save(state: &Arc<DaemonState>, params: Option<&Value>) -> RpcResult {
    let p: ConfigSaveParams = match params {
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| {
            (
                codes::INVALID_PARAMS,
                format!("config.save: invalid params: {e}"),
                None,
            )
        })?,
        None => {
            return Err((
                codes::INVALID_PARAMS,
                "config.save: params required".into(),
                None,
            ))
        }
    };
    // Validate that the new content parses as TOML before writing —
    // refusing a save here is the right move per docs/RPC.md (the
    // daemon shouldn't end up with a config it can't parse on the
    // next start). We do NOT yet hot-apply; the save persists, the
    // user must restart for changes to take effect.
    if let Err(e) = toml::from_str::<toml::Value>(&p.config) {
        return Err((
            codes::INVALID_PARAMS,
            format!("config.save: TOML parse error: {e}"),
            None,
        ));
    }
    let path = state.config_path.clone();
    if !p.dry_run {
        if let Err(e) = super::fs_util::atomic_write(&path, p.config.as_bytes()).await {
            return Err((
                codes::INTERNAL_ERROR,
                format!("config.save: write {}: {e}", path.display()),
                None,
            ));
        }
    }
    // Field names match pim-ui's `ConfigSaveResult` (rpc-types.ts §5.5):
    //   saved, requires_restart, written_to.
    //
    // We currently mark EVERY config field as requires_restart since the
    // daemon doesn't yet hot-apply any field changes. A future iteration
    // can narrow this list once specific [section.field] live-reload
    // hooks land in the daemon.
    Ok(json!({
        "saved": !p.dry_run,
        "requires_restart": ["*"],
        "written_to": path.to_string_lossy(),
    }))
}

// ────────────────────────────────────────────────────────────────────────
// Tests.
// ────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rpc_hello_with_matching_version_succeeds() {
        let p = json!({ "client": "pim-ui/0.0.1", "rpc_version": 1 });
        let res = method_hello(Some(&p)).expect("hello ok");
        assert_eq!(res["rpc_version"], json!(1));
        assert!(res["daemon"]
            .as_str()
            .expect("daemon str")
            .starts_with("pim-daemon/"));
    }

    #[test]
    fn rpc_hello_with_mismatched_version_errors() {
        let p = json!({ "client": "pim-ui/0.0.1", "rpc_version": 99 });
        let err = method_hello(Some(&p)).expect_err("hello should fail");
        assert_eq!(err.0, codes::RPC_VERSION_MISMATCH);
    }

    #[test]
    fn iso8601_format_basic_round() {
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let s = system_time_to_iso8601(t);
        // 2023-11-14T22:13:20Z is the canonical answer for that epoch.
        assert_eq!(s, "2023-11-14T22:13:20Z");
    }

    #[test]
    fn days_to_ymd_known_pairs() {
        // Day 0 is 1970-01-01.
        assert_eq!(days_to_ymd(0), (1970, 1, 1));
        // Day 1 is 1970-01-02.
        assert_eq!(days_to_ymd(1), (1970, 1, 2));
        // 2000-01-01 is day 10_957.
        assert_eq!(days_to_ymd(10_957), (2000, 1, 1));
    }

    #[test]
    fn new_subscription_id_is_unique() {
        let a = new_subscription_id();
        let b = new_subscription_id();
        assert_ne!(a, b);
        assert!(a.starts_with("rpc-sub-"));
    }
}

// ── §5.7 messages ────────────────────────────────────────────────────────
//
// Methods in this section dispatch into the messaging plugin and are
// only compiled when the `messaging` Cargo feature is enabled.

#[cfg(feature = "messaging")]
#[derive(Debug, Deserialize)]
struct MessagesHistoryParams {
    peer_node_id: String,
    #[serde(default)]
    before_ts_ms: Option<i64>,
    #[serde(default)]
    limit: Option<i64>,
}

#[cfg(feature = "messaging")]
#[derive(Debug, Deserialize)]
struct MessagesSendParams {
    peer_node_id: String,
    body: String,
}

#[cfg(feature = "messaging")]
#[derive(Debug, Deserialize)]
struct MessagesMarkReadParams {
    peer_node_id: String,
    up_to_ts_ms: i64,
}

#[cfg(feature = "messaging")]
fn parse_peer_node_id(hex: &str) -> std::result::Result<NodeId, (i32, String, Option<Value>)> {
    parse_node_id_hex(hex)
        .map(NodeId::from_bytes)
        .ok_or_else(|| {
            (
                codes::INVALID_PARAMS,
                "peer_node_id must be 32 hex characters".into(),
                None,
            )
        })
}

#[cfg(feature = "messaging")]
async fn method_messages_list_conversations(state: &Arc<DaemonState>) -> RpcResult {
    let sessions = state.sessions.read().await;
    let connected: std::collections::HashSet<String> = sessions
        .keys()
        .map(crate::app::peer_directory::hex_node_id)
        .collect();
    drop(sessions);

    let conversations = messaging_service(state)
        .list_conversations()
        .await
        .map_err(|e| {
            (
                codes::MESSAGE_STORAGE_ERROR,
                format!("messages.list_conversations: {e}"),
                None,
            )
        })?;

    let mut out: Vec<Value> = Vec::with_capacity(conversations.len());
    for conv in conversations {
        let is_connected = connected.contains(&conv.peer_node_id);
        out.push(json!({
            "peer_node_id": conv.peer_node_id,
            "peer_node_id_short": conv.peer_node_id_short,
            "name": conv.name,
            "last_message_preview": conv.last_message_preview,
            "last_message_ts_ms": conv.last_message_ts_ms,
            "unread_count": conv.unread_count,
            "is_connected": is_connected,
            "x25519_pubkey": conv.x25519_pubkey,
        }));
    }
    Ok(json!({ "conversations": out }))
}

#[cfg(feature = "messaging")]
async fn method_messages_history(state: &Arc<DaemonState>, params: Option<&Value>) -> RpcResult {
    let parsed: MessagesHistoryParams = match params {
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| {
            (
                codes::INVALID_PARAMS,
                format!("messages.history params: {e}"),
                None,
            )
        })?,
        None => {
            return Err((
                codes::INVALID_PARAMS,
                "messages.history requires peer_node_id".into(),
                None,
            ))
        }
    };

    if parsed.peer_node_id.len() != 32 {
        return Err((
            codes::INVALID_PARAMS,
            "peer_node_id must be 32 hex characters".into(),
            None,
        ));
    }
    let peer_hex = parsed.peer_node_id;
    let limit = parsed.limit.unwrap_or(100).clamp(1, 500);
    let before = parsed.before_ts_ms;
    let storage = messaging_service(state).storage().clone();

    let result = tokio::task::spawn_blocking(move || storage.history(&peer_hex, before, limit))
        .await
        .map_err(|e| {
            (
                codes::INTERNAL_ERROR,
                format!("messages.history join: {e}"),
                None,
            )
        })?
        .map_err(|e| {
            (
                codes::MESSAGE_STORAGE_ERROR,
                format!("messages.history: {e}"),
                None,
            )
        })?;

    let (messages, has_more) = result;
    Ok(json!({
        "messages": messages,
        "has_more": has_more,
    }))
}

#[cfg(feature = "messaging")]
async fn method_messages_send(state: &Arc<DaemonState>, params: Option<&Value>) -> RpcResult {
    let parsed: MessagesSendParams = match params {
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| {
            (
                codes::INVALID_PARAMS,
                format!("messages.send params: {e}"),
                None,
            )
        })?,
        None => {
            return Err((
                codes::INVALID_PARAMS,
                "messages.send requires peer_node_id and body".into(),
                None,
            ))
        }
    };

    if parsed.body.is_empty() {
        return Err((
            codes::INVALID_PARAMS,
            "messages.send body must be non-empty".into(),
            None,
        ));
    }
    if parsed.body.len() > pim_messaging::MAX_BODY_BYTES {
        return Err((
            codes::MESSAGE_BODY_TOO_LARGE,
            format!("body exceeds {} bytes", pim_messaging::MAX_BODY_BYTES),
            None,
        ));
    }

    let peer = parse_peer_node_id(&parsed.peer_node_id)?;
    let body = parsed.body;
    let result = messaging_service(state).send(peer, body).await;
    match result {
        Ok(record) => Ok(json!({
            "id": record.id,
            "timestamp_ms": record.timestamp_ms,
            "status": record.status,
        })),
        Err(e) => {
            let msg = format!("{e}");
            if msg.contains("no x25519") {
                Err((codes::MESSAGE_PEER_UNKNOWN, msg, None))
            } else {
                Err((codes::MESSAGE_STORAGE_ERROR, msg, None))
            }
        }
    }
}

#[cfg(feature = "messaging")]
async fn method_messages_mark_read(state: &Arc<DaemonState>, params: Option<&Value>) -> RpcResult {
    let parsed: MessagesMarkReadParams = match params {
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| {
            (
                codes::INVALID_PARAMS,
                format!("messages.mark_read params: {e}"),
                None,
            )
        })?,
        None => {
            return Err((
                codes::INVALID_PARAMS,
                "messages.mark_read requires peer_node_id and up_to_ts_ms".into(),
                None,
            ))
        }
    };

    if parsed.peer_node_id.len() != 32 {
        return Err((
            codes::INVALID_PARAMS,
            "peer_node_id must be 32 hex characters".into(),
            None,
        ));
    }
    let peer_hex = parsed.peer_node_id;
    let up_to = parsed.up_to_ts_ms;
    let storage = messaging_service(state).storage().clone();

    let unread = tokio::task::spawn_blocking(move || storage.mark_read_up_to(&peer_hex, up_to))
        .await
        .map_err(|e| {
            (
                codes::INTERNAL_ERROR,
                format!("messages.mark_read join: {e}"),
                None,
            )
        })?
        .map_err(|e| {
            (
                codes::MESSAGE_STORAGE_ERROR,
                format!("messages.mark_read: {e}"),
                None,
            )
        })?;

    Ok(json!({ "unread_count": unread }))
}
