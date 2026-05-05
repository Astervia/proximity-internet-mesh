#![allow(missing_docs)]
//! RFCOMM auto-discovery service (Phase 7).
//!
//! Linux-only port of the Python spike at
//! `spikes/bt-rfcomm/linux/pim-bt-rfcomm-linux.py`. Same wire protocol
//! (4-byte BE length-prefix + JSON), same Hello/HelloAck handshake,
//! same default channel (22), so a Mac running the
//! `pim-bt-rfcomm-mac` Swift sidecar pairs with a Linux running this
//! service without any further coordination.
//!
//! Architecture: one `RfcommService` per daemon instance, owning two
//! tokio tasks:
//!
//!   * **acceptor** — binds RFCOMM channel `cfg.channel`, accepts inbound
//!     connections from any paired peer, runs the handshake, emits
//!     `RfcommEvent::Discovered` on success.
//!   * **outbound discovery** — every `cfg.poll_interval` polls
//!     `bluetoothctl devices Paired`, filters peers by `cfg.prefix`,
//!     dials each one (skipping addresses that already have an active
//!     session), runs the handshake.
//!
//! Both tasks feed the same `mpsc::Sender<RfcommEvent>` so the daemon
//! consumes a single stream regardless of who initiated.
//!
//! macOS / Windows / other: types compile but `RfcommService::start`
//! returns `RfcommError::UnsupportedPlatform` — those platforms speak
//! RFCOMM via the Tauri sidecar in `pim-ui` instead.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[cfg(target_os = "linux")]
mod bridge;
#[cfg(target_os = "linux")]
mod listener;
#[cfg(target_os = "linux")]
mod outbound;
#[cfg(target_os = "linux")]
mod session;
#[cfg(target_os = "linux")]
mod socket;

// Phase B note for android: a real backend lives in
// `pim-bluetooth/src/rfcomm/android.rs` (not yet present — needs
// Tauri Android plugin from Phase B Step 4 to ship the Java
// `BluetoothSocket` byte stream over JNI). The current Phase A stub
// returns `UnsupportedPlatform` from `RfcommService::start` on
// non-linux; that contract stays the same when the android backend
// arrives, just the constructor body changes. Existing `frame.rs`
// codec is platform-agnostic and shared.

mod frame;

#[cfg(test)]
mod tests;

pub use frame::{decode_frame, encode_frame, FrameError, MAX_FRAME_PAYLOAD};

/// 6-byte Bluetooth Device Address. Big-endian "AA:BB:CC:DD:EE:FF" on the
/// wire, but stored little-endian to match the Linux kernel's `bdaddr_t`
/// layout (the kernel reverses on input/output, so the in-memory bytes
/// are the reverse of the human-readable string).
pub type BdAddr = [u8; 6];

/// Default RFCOMM channel matches the spike (`spikes/bt-rfcomm/PROTOCOL.md`).
/// Channel 1 is the SPP convention BUT BlueZ's bluetoothd reserves it
/// for the built-in SPP profile, so we use 22 (in the dynamic range
/// 1–30 but far from common conflicts).
pub const DEFAULT_CHANNEL: u8 = 22;

/// Default name prefix for the paired-device scan filter.
pub const DEFAULT_PREFIX: &str = "PIM-";

/// Wire-protocol version. Mismatch → both sides send `error` and close.
pub const HELLO_VERSION: u8 = 1;

/// Local identity carried in the Hello / HelloAck frames. Provided by
/// the daemon (typically `pim-core::NodeId` on Linux, formatted via
/// `to_hex()`).
#[derive(Clone)]
pub struct LocalIdentity {
    /// Pre-formatted hex node id. Kernel `NodeId` is 16 bytes / 32 hex
    /// chars; spike Mac side advertises 64 hex chars (random 32 bytes).
    /// Either is fine on the wire — this field accepts whatever the
    /// daemon hands in.
    pub node_id_hex: String,
    /// Local advertised name; the peer-side prefix filter expects
    /// `cfg.prefix` (default `PIM-`). The daemon owns the format.
    pub name: String,
    /// Capability flags advertised in Hello payload (`mesh-v1`,
    /// `gateway-v1`, …).
    pub caps: Vec<String>,
    /// 32-byte handshake key for the local mesh, or `None` for the open
    /// mesh. `Some` → we attach a `mesh_tag` to our outgoing Hello and
    /// require the peer to send a matching one; `None` → we send no
    /// `mesh_tag` and reject any peer that does. This is what blocks an
    /// open node from talking to a private one (and vice-versa) at the
    /// RFCOMM layer, before the TCP-bridge handshake task is spawned.
    pub mesh_handshake_key: Option<[u8; 32]>,
}

impl std::fmt::Debug for LocalIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never include `mesh_handshake_key` bytes in Debug output —
        // it's the same secret that protects discovery encryption.
        f.debug_struct("LocalIdentity")
            .field("node_id_hex", &self.node_id_hex)
            .field("name", &self.name)
            .field("caps", &self.caps)
            .field(
                "mesh_handshake_key",
                &self.mesh_handshake_key.map(|_| "<redacted>"),
            )
            .finish()
    }
}

/// Configuration for `RfcommService`. The daemon constructs this from
/// the `[bluetooth_rfcomm]` config section.
#[derive(Debug, Clone)]
pub struct RfcommConfig {
    /// Whether to start at all. Daemon honors `[bluetooth_rfcomm].enabled`.
    pub enabled: bool,
    /// RFCOMM channel to bind / dial.
    pub channel: u8,
    /// Name prefix used by the outbound scan to filter paired devices.
    pub prefix: String,
    /// Interval between outbound discovery ticks.
    pub poll_interval: Duration,
    /// Whether the outbound (paired-device-scan) loop is active.
    /// Disable when this node is acceptor-only (e.g. embedded gateway
    /// with no UI to scan from).
    pub outbound_enabled: bool,
    /// `bluetoothctl` command used by outbound paired-device scans.
    pub bluetoothctl_command: PathBuf,
    /// Local TCP loopback address that post-handshake RFCOMM bytes are
    /// bridged onto. Should match the `pim-transport` TCP listener.
    /// `None` disables the bridge — discovery still emits `Discovered`
    /// events but the channel will not carry mesh frames.
    pub local_bridge_addr: Option<SocketAddr>,
}

impl Default for RfcommConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            channel: DEFAULT_CHANNEL,
            prefix: DEFAULT_PREFIX.to_string(),
            poll_interval: Duration::from_secs(30),
            outbound_enabled: true,
            bluetoothctl_command: PathBuf::from("bluetoothctl"),
            local_bridge_addr: None,
        }
    }
}

/// Discovery / lifecycle events emitted by the service. The daemon
/// converts each `Discovered` into a `pim-discovery::PeerTable` entry
/// with transport label `"bluetooth-rfcomm"`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum RfcommEvent {
    /// Service-up signal. Useful for the UI / supervisor.
    Listening { channel: u8 },
    /// Identity exchange completed. Either inbound (peer dialed us) or
    /// outbound (we dialed peer); both produce the same event.
    Discovered {
        bd_addr: String,
        node_id: String,
        name: String,
        platform: String,
        caps: Vec<String>,
        /// Whether we dialed the peer (`true`) or accepted them (`false`).
        initiator: bool,
        /// ISO-8601 timestamp the channel opened.
        since: String,
    },
    /// Channel closed. `bd_addr` mirrors the `Discovered` event so the
    /// daemon can mark the corresponding peer offline.
    Lost { bd_addr: String, reason: String },
    /// Outbound dial that did not produce a working channel (peer not
    /// listening, host down, etc). Useful for UI noise filtering.
    OpenFailed {
        bd_addr: String,
        name: String,
        reason: String,
    },
    /// Catch-all error. `code` mirrors the `-33000..=-33099` range
    /// reserved for `bluetooth-coc` in the Phase 7 spec doc; same
    /// numbering applies to RFCOMM since they share the proto subsystem.
    Error { code: i32, message: String },
}

/// Public errors. The daemon converts these into `RfcommEvent::Error`
/// or surfaces them at start time (`enabled = true` but bind failed).
#[derive(Debug, thiserror::Error)]
pub enum RfcommError {
    #[error("rfcomm not supported on this platform")]
    UnsupportedPlatform,

    #[error("bind RFCOMM channel {channel}: {source}")]
    BindFailed { channel: u8, source: std::io::Error },

    #[error("connect to {bd_addr} channel {channel}: {source}")]
    ConnectFailed {
        bd_addr: String,
        channel: u8,
        source: std::io::Error,
    },

    #[error("frame error: {0}")]
    Frame(#[from] FrameError),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("malformed peer payload: {0}")]
    MalformedPayload(String),

    #[error("peer reported version mismatch (theirs={their_v}, ours={our_v})")]
    VersionMismatch { their_v: u8, our_v: u8 },
}

/// Service handle. `start` spawns the background tasks; dropping the
/// handle cancels them.
pub struct RfcommService {
    cancel: CancellationToken,
}

impl RfcommService {
    /// Spawn the service. On non-Linux platforms returns
    /// `UnsupportedPlatform`; the daemon should not call this on macOS.
    #[allow(unused_variables)]
    pub fn start(
        cfg: RfcommConfig,
        identity: LocalIdentity,
        events_tx: mpsc::Sender<RfcommEvent>,
    ) -> Result<Self, RfcommError> {
        if !cfg.enabled {
            return Err(RfcommError::UnsupportedPlatform);
        }
        #[cfg(target_os = "linux")]
        {
            let cancel = CancellationToken::new();
            // Shared session-dedup set: a single Arc<Mutex<HashSet<BdAddr>>>
            // checked atomically by both listener and outbound before they
            // spawn a session for a given peer. Without this, both sides
            // can simultaneously dial each other and both can also accept
            // each other's inbound — yielding 2 sessions per pair, where
            // the second `register_peer` clobbers the first and the first
            // session's bridge dies with `Connection reset by peer`. With
            // it, whichever side acts first (outbound dial OR inbound
            // accept) wins the slot and the other path skips.
            let active: std::sync::Arc<tokio::sync::Mutex<std::collections::HashSet<BdAddr>>> =
                std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashSet::new()));
            listener::spawn(
                cfg.clone(),
                identity.clone(),
                events_tx.clone(),
                cancel.clone(),
                active.clone(),
            )?;
            if cfg.outbound_enabled {
                outbound::spawn(cfg, identity, events_tx, cancel.clone(), active);
            }
            Ok(Self { cancel })
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(RfcommError::UnsupportedPlatform)
        }
    }

    /// Cancel the listener + outbound tasks. Idempotent.
    pub fn shutdown(&self) {
        self.cancel.cancel();
    }
}

impl Drop for RfcommService {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// Format a 6-byte BD_ADDR as `"AA:BB:CC:DD:EE:FF"`.
pub fn format_bdaddr(addr: &BdAddr) -> String {
    format!(
        "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        addr[5], addr[4], addr[3], addr[2], addr[1], addr[0],
    )
}

/// Parse `"AA:BB:CC:DD:EE:FF"` → 6-byte BD_ADDR (kernel little-endian).
pub fn parse_bdaddr(s: &str) -> Option<BdAddr> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 6 {
        return None;
    }
    let mut out = [0u8; 6];
    for (i, p) in parts.iter().enumerate() {
        out[5 - i] = u8::from_str_radix(p, 16).ok()?;
    }
    Some(out)
}

/// ISO-8601 timestamp for the current instant.
pub fn now_iso() -> String {
    let now = SystemTime::now();
    let secs = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Minimal ISO-8601 in UTC; we don't need full chrono here.
    let (year, month, day, hour, min, sec) = ymdhms_from_unix(secs);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hour, min, sec
    )
}

/// Convert a Unix timestamp in seconds to (Y, M, D, h, m, s) UTC.
/// Tiny date-arithmetic implementation (no chrono dep). Range:
/// 1970..2100; outside, output is best-effort.
fn ymdhms_from_unix(t: u64) -> (u32, u32, u32, u32, u32, u32) {
    let sec = (t % 60) as u32;
    let min = ((t / 60) % 60) as u32;
    let hour = ((t / 3600) % 24) as u32;
    let mut days = (t / 86_400) as i64;

    let mut year = 1970i64;
    loop {
        let days_in = if is_leap(year) { 366 } else { 365 };
        if days < days_in {
            break;
        }
        days -= days_in;
        year += 1;
    }
    let leap = is_leap(year);
    let month_days: [i64; 12] = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 1u32;
    for d in month_days {
        if days < d {
            break;
        }
        days -= d;
        month += 1;
    }
    (year as u32, month, (days + 1) as u32, hour, min, sec)
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}
