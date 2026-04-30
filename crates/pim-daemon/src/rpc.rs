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
use std::sync::atomic::{AtomicU64, Ordering};
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
    pub(super) const MESSAGE_PEER_UNKNOWN: i32 = -32060;
    pub(super) const MESSAGE_BODY_TOO_LARGE: i32 = -32061;
    pub(super) const MESSAGE_STORAGE_ERROR: i32 = -32062;
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

    // Single per-connection messaging forwarder. Same shape as the status
    // forwarder above: subscribe to the messaging broadcast channel and
    // pump every event onto the write channel as a `messages.event`
    // notification. UIs filter by event `kind` on their side.
    let messaging_write_tx = write_tx.clone();
    let mut messaging_rx = state.messaging.subscribe();
    let messaging_forwarder = tokio::spawn(async move {
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

        let outcome = dispatch(&state, &req, &write_tx).await;
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
    messaging_forwarder.abort();
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

async fn dispatch(state: &Arc<DaemonState>, req: &RpcRequest, write_tx: &WriteTx) -> RpcResult {
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
        "peers.subscribe" => Ok(json!({ "subscription_id": new_subscription_id() })),
        "peers.unsubscribe" => Ok(Value::Null),

        // §5.3 routing
        "route.set_split_default" => method_route_set_split_default(state, req.params.as_ref()),
        "route.table" => Ok(build_route_table(state).await),

        // §5.4 gateway
        "gateway.preflight" => Ok(build_gateway_preflight(state)),
        "gateway.enable" => method_gateway_enable(state, req.params.as_ref()).await,
        "gateway.disable" => Ok(json!({ "active": false })),
        "gateway.status" => Ok(build_gateway_status(state)),
        "gateway.subscribe" => Ok(json!({ "subscription_id": new_subscription_id() })),
        "gateway.unsubscribe" => Ok(Value::Null),

        // §5.5 config
        "config.get" => method_config_get(state, req.params.as_ref()).await,
        "config.save" => method_config_save(state, req.params.as_ref()).await,

        // §5.6 logs
        "logs.subscribe" => Ok(start_logs_subscription(
            req.params.as_ref(),
            write_tx.clone(),
        )),
        "logs.unsubscribe" => Ok(Value::Null),

        // §5.7 messages
        "messages.list_conversations" => method_messages_list_conversations(state).await,
        "messages.history" => method_messages_history(state, req.params.as_ref()).await,
        "messages.send" => method_messages_send(state, req.params.as_ref()).await,
        "messages.mark_read" => method_messages_mark_read(state, req.params.as_ref()).await,
        "messages.subscribe" => Ok(json!({ "subscription_id": new_subscription_id() })),
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

#[derive(Debug, Deserialize, Default)]
struct LogsSubscribeParams {
    /// Optional minimum level. One of `trace`, `debug`, `info`, `warn`,
    /// `error`. Events at levels BELOW this are dropped before sending.
    /// Used for back-compat / "warn and up" simplicity. If `levels` is
    /// also set, `levels` wins (explicit beats threshold).
    min_level: Option<String>,
    /// Explicit allow-list of levels. When `Some(non_empty)`, ONLY events
    /// at one of these levels are forwarded. Empty/missing falls back to
    /// `min_level` semantics. Lets the UI pick e.g. `[info, error]`
    /// without including warn.
    levels: Option<Vec<String>>,
    /// Source-prefix allow-list (matches `event.source` via
    /// `starts_with`). Empty / missing = any source. Lets the UI pick
    /// e.g. `["pim_daemon", "pim_transport"]` to focus on specific
    /// crates while ignoring noise from `tao`, `mio`, etc.
    #[serde(default)]
    sources: Vec<String>,
}

fn level_rank(level: &str) -> u8 {
    match level {
        "trace" => 0,
        "debug" => 1,
        "info" => 2,
        "warn" => 3,
        "error" => 4,
        _ => 2, // unknown → treat as info
    }
}

/// Allocate a `subscription_id`, atomically snapshot the daemon's log
/// history AND get a live broadcast receiver, replay history first
/// (filtered) and then forward live events (filtered) as `logs.event`
/// notifications onto this connection. Closes when the writer channel
/// closes (peer hung up).
///
/// The history replay covers the daemon's full startup sequence
/// ("daemon starting" → "TUN up" → "transport listening" → "rpc
/// listening" → ...) so the UI's Logs view is populated immediately
/// even though it subscribes well after those events fired.
fn start_logs_subscription(params: Option<&Value>, write_tx: WriteTx) -> Value {
    let id = new_subscription_id();
    let parsed: LogsSubscribeParams = params
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let min_rank = parsed
        .min_level
        .as_deref()
        .map(|l| level_rank(&l.to_lowercase()))
        .unwrap_or(0);
    // Normalise `levels` to a lowercase HashSet for O(1) lookup. Empty
    // vec is treated as "no level filter" (allow all) so the UI can
    // distinguish "select nothing → show nothing" client-side from
    // server-side filtering.
    let level_allow_list: Option<std::collections::HashSet<String>> = parsed
        .levels
        .map(|v| v.into_iter().map(|s| s.to_lowercase()).collect())
        .filter(|s: &std::collections::HashSet<String>| !s.is_empty());
    let sources = parsed.sources;

    let (history, mut rx) = match logs_subscriber::subscribe_with_history() {
        Some(pair) => pair,
        None => {
            warn!("logs.subscribe: logs_subscriber not initialised; subscription will be silent");
            return json!({ "subscription_id": id });
        }
    };

    tokio::spawn(async move {
        // Replay history first.
        for event in history {
            if !passes_filter(&event, min_rank, level_allow_list.as_ref(), &sources) {
                continue;
            }
            let notif = json!({
                "jsonrpc": "2.0",
                "method": "logs.event",
                "params": event,
            });
            if push_value(&write_tx, &notif).is_err() {
                return;
            }
        }
        // Then live stream.
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if !passes_filter(&event, min_rank, level_allow_list.as_ref(), &sources) {
                        continue;
                    }
                    let notif = json!({
                        "jsonrpc": "2.0",
                        "method": "logs.event",
                        "params": event,
                    });
                    if push_value(&write_tx, &notif).is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    debug!("logs.subscribe: lagged, dropped {n} events; resyncing");
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    json!({ "subscription_id": id })
}

/// Apply level + source filter to an event. Returns true if it should
/// be forwarded.
///
/// Level rules:
///   - If `level_allow_list` is `Some`, the event level must be IN the
///     set (explicit allow-list, no min-level threshold applies).
///   - If `level_allow_list` is `None`, the event level must be `>=`
///     `min_rank`.
///
/// Source rules: prefix-match against `sources`. Empty `sources` =
/// allow any source.
fn passes_filter(
    event: &Value,
    min_rank: u8,
    level_allow_list: Option<&std::collections::HashSet<String>>,
    sources: &[String],
) -> bool {
    if let Some(level) = event.get("level").and_then(|v| v.as_str()) {
        match level_allow_list {
            Some(allow) => {
                if !allow.contains(level) {
                    return false;
                }
            }
            None => {
                if level_rank(level) < min_rank {
                    return false;
                }
            }
        }
    }
    if !sources.is_empty() {
        let source = event
            .get("source")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if !sources.iter().any(|prefix| source.starts_with(prefix)) {
            return false;
        }
    }
    true
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

    json!({
        "node": state.node_name,
        "node_id": state.self_id.to_hex(),
        "node_id_short": state.self_id.to_string(),
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

        out.push(json!({
            "node_id": peer_id.to_hex(),
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

#[derive(Debug, Deserialize)]
struct PeersRemoveParams {
    node_id: Option<String>,
    config_entry_id: Option<String>,
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

/// Toggle split-default routing. Currently a state-only operation —
/// the daemon's IP forwarder doesn't yet observe `state.route_on`, so
/// this RPC's job is to (a) keep an authoritative atomic flag, (b)
/// broadcast the corresponding `status.event` so subscribed UIs flip
/// without re-polling, and (c) return the new state to the caller.
/// Wiring the actual default-route mutation through the forwarder is
/// a separate follow-up.
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

    // Broadcast the discriminated `status.event` per pim-ui's rpc-types
    // contract. SendError on no-subscribers is fine — UIs that connect
    // later see the current state via the next `status` RPC.
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

fn build_gateway_preflight(_state: &Arc<DaemonState>) -> Value {
    let supported = cfg!(any(target_os = "linux", target_os = "macos"));
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

#[derive(Debug, Deserialize)]
struct MessagesHistoryParams {
    peer_node_id: String,
    #[serde(default)]
    before_ts_ms: Option<i64>,
    #[serde(default)]
    limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct MessagesSendParams {
    peer_node_id: String,
    body: String,
}

#[derive(Debug, Deserialize)]
struct MessagesMarkReadParams {
    peer_node_id: String,
    up_to_ts_ms: i64,
}

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

async fn method_messages_list_conversations(state: &Arc<DaemonState>) -> RpcResult {
    let storage = state.messaging.storage().clone();
    let sessions = state.sessions.read().await;
    let connected: std::collections::HashSet<String> = sessions
        .keys()
        .map(crate::app::messaging::hex_node_id)
        .collect();
    drop(sessions);

    let conversations =
        match tokio::task::spawn_blocking(move || storage.list_conversations()).await {
            Ok(Ok(c)) => c,
            Ok(Err(e)) => {
                return Err((
                    codes::MESSAGE_STORAGE_ERROR,
                    format!("messages.list_conversations: {e}"),
                    None,
                ))
            }
            Err(e) => {
                return Err((
                    codes::INTERNAL_ERROR,
                    format!("messages.list_conversations join: {e}"),
                    None,
                ))
            }
        };

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
        }));
    }
    Ok(json!({ "conversations": out }))
}

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
    let storage = state.messaging.storage().clone();

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
    if parsed.body.len() > crate::app::messaging::MAX_BODY_BYTES {
        return Err((
            codes::MESSAGE_BODY_TOO_LARGE,
            format!(
                "body exceeds {} bytes",
                crate::app::messaging::MAX_BODY_BYTES
            ),
            None,
        ));
    }

    let peer = parse_peer_node_id(&parsed.peer_node_id)?;
    let body = parsed.body;
    let result = crate::app::messaging::dispatch::send_user_message(state, peer, body).await;
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
    let storage = state.messaging.storage().clone();

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
