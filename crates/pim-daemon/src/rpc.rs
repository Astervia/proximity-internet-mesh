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
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

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
        // role_changed, kill_switch), NOT a Status snapshot. We don't yet
        // wire those internal lifecycle hooks into the broadcast layer,
        // so subscribers receive an id and zero events for now — the
        // UI's switch defaults to no-op for kinds it doesn't recognise,
        // so silence is the only correct behaviour until events ship.
        "status.subscribe" => Ok(json!({ "subscription_id": new_subscription_id() })),
        "status.unsubscribe" => Ok(Value::Null),

        // §5.2 peers
        "peers.list" => Ok(build_peer_list(state).await),
        "peers.add_static" => method_peers_add_static(state, req.params.as_ref()).await,
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
        "route.set_split_default" => Ok(json!({
            "on": false,
            "via_gateway_id": null,
        })),
        "route.table" => Ok(build_route_table(state).await),

        // §5.4 gateway
        "gateway.preflight" => Ok(build_gateway_preflight(state)),
        "gateway.enable" => Err((
            codes::GATEWAY_NOT_SUPPORTED,
            "gateway.enable: in-place toggle requires daemon restart in this version".into(),
            None,
        )),
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
        "route_on": false,
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
        out.push(json!({
            "node_id": peer_id.to_hex(),
            "node_id_short": peer_id.to_string(),
            "label": null,
            "mesh_ip": addr,
            "transport": mechanism,
            "state": "active",
            "route_hops": 1,
            "last_seen_s": last_seen_s,
            "latency_ms": null,
            "is_gateway": false,
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
        "suggested_nat_interfaces": Vec::<String>::new(),
    })
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
