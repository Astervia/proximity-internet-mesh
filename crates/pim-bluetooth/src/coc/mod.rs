#![allow(missing_docs)]
//! L2CAP Connection-Oriented Channel (CoC) auto-discovery service.
//!
//! BLE counterpart to [`super::rfcomm`]. Same Hello/HelloAck JSON
//! envelope, same `verify_peer_mesh_tag` gate, same loopback-TCP
//! bridge — only the underlying socket changes: `AF_BLUETOOTH` +
//! `BTPROTO_L2CAP` with a `sockaddr_l2` carrying `BDADDR_LE_PUBLIC`
//! (or `BDADDR_LE_RANDOM`) so the kernel routes traffic through the LE
//! controller instead of BR/EDR. PSMs live in the LE-only dynamic
//! range `0x0080..=0x00FF`; the daemon default ([`DEFAULT_PSM`]) is
//! `0x0083`. PSMs `0x0001..=0x007F` are SIG-assigned and must not be
//! used.
//!
//! Architecture mirrors [`super::rfcomm`] one-to-one — one
//! `CocService` per daemon instance, owning a listener + (optional)
//! outbound + (optional) inquiry-equivalent.
//!
//! macOS / Windows / android: types compile but `CocService::start`
//! returns `CocError::UnsupportedPlatform`. Future macOS port plugs in
//! a `coc/macos.rs` backend with the same `CocConfig`/`CocEvent`
//! shape via `IOBluetoothL2CAPChannel`.
//!
//! ## Why CoC, not GATT
//!
//! GATT is request/response over a 23–247 B attribute table. L2CAP CoC
//! is a credit-flow-controlled bidirectional byte stream with up to
//! 65 535-B SDUs and a socket API on both Linux and Android — drop-in
//! shape for the existing length-prefix frame codec already shared via
//! [`crate::frame`].
//!
//! ## PIM LE service UUID
//!
//! [`PIM_SERVICE_UUID`] identifies a PIM advertiser in GAP scan
//! results (see Phase 4 — `coc/advertising.rs`). Random UUIDv4
//! generated for this transport; do not reuse for unrelated PIM
//! services.
//!
//! ## Threat-model note
//!
//! GAP advertising is observable in radio range. The advertisement
//! reveals "a PIM node is here" via [`PIM_SERVICE_UUID`] and exposes
//! the dynamic PSM bytes in service-data so the scanner can dial
//! without an SDP lookup. The optional mesh-tag bytes embedded in
//! service-data are HMAC-truncated and do not reveal the mesh secret;
//! they let the scanner pre-filter wrong-mesh peers before opening a
//! connection. This is no worse than RFCOMM's BR/EDR inquiry, which
//! exposes the same `device_name_prefix` name to anyone running a
//! Classic scan.

use std::net::SocketAddr;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[cfg(target_os = "linux")]
mod advertising;
#[cfg(target_os = "linux")]
mod bridge;
#[cfg(target_os = "linux")]
mod listener;
#[cfg(target_os = "linux")]
mod outbound;
#[cfg(target_os = "linux")]
mod scan;
#[cfg(target_os = "linux")]
mod session;
#[cfg(target_os = "linux")]
mod socket;

#[cfg(test)]
mod tests;

pub use crate::frame::{decode_frame, encode_frame, FrameError, MAX_FRAME_PAYLOAD};

/// 6-byte Bluetooth Device Address. Same in-memory layout as
/// `rfcomm::BdAddr` (kernel little-endian reverse of the human-readable
/// string).
pub type BdAddr = [u8; 6];

/// Default L2CAP CoC PSM for PIM (`0x0083`). LE dynamic PSM range is
/// `0x0080..=0x00FF`; SIG-assigned values `0x0001..=0x007F` must not
/// be used. Android's `listenUsingL2capChannel` ignores this value
/// and assigns its own dynamic PSM at listen time — Phase 4's GAP
/// advertising carries the assigned PSM so the initiator dials the
/// right number.
pub const DEFAULT_PSM: u16 = 0x0083;

/// Default name prefix for the paired-device scan filter. Reused
/// verbatim from RFCOMM so a single naming convention identifies a
/// PIM peer across both transports.
pub const DEFAULT_PREFIX: &str = "PIM-";

/// Wire-protocol version. Same value as the RFCOMM Hello version —
/// the JSON envelope is shared, only the transport differs.
pub const HELLO_VERSION: u8 = 1;

/// PIM L2CAP CoC service UUID, advertised in LE GAP scan results.
/// Random UUIDv4 — do not reuse for any other PIM service. Phase 4's
/// `coc/advertising.rs` puts this in the `ServiceUUIDs` field of the
/// outbound advertisement; the scanner filters incoming advertisements
/// on the same UUID before reading service-data.
pub const PIM_SERVICE_UUID: &str = "e5c0d2a4-5b1f-4a3e-9d77-2c0a8b1f1a83";

/// `BDADDR_BREDR` — Classic BR/EDR address-type. Kept here for
/// completeness; the CoC service always uses an LE variant.
pub const BDADDR_BREDR: u8 = 0x00;
/// `BDADDR_LE_PUBLIC` — public LE device address (per Vol 6, Part B,
/// §1.3.2.1). The acceptor binds with this; the initiator picks the
/// peer's address-type at dial time.
pub const BDADDR_LE_PUBLIC: u8 = 0x01;
/// `BDADDR_LE_RANDOM` — random (static/non-resolvable/resolvable) LE
/// device address. Most modern smartphones default to random
/// addresses for privacy.
pub const BDADDR_LE_RANDOM: u8 = 0x02;

/// Local identity carried in the Hello / HelloAck frames. Same shape
/// as `rfcomm::LocalIdentity`.
#[derive(Clone)]
pub struct LocalIdentity {
    /// Pre-formatted hex node id; the daemon owns the format
    /// (typically `pim-core::NodeId::to_hex()`).
    pub node_id_hex: String,
    /// Local advertised name; the peer-side prefix filter expects
    /// `cfg.prefix` (default `PIM-`).
    pub name: String,
    /// Capability flags advertised in Hello payload.
    pub caps: Vec<String>,
    /// 32-byte handshake key for the local mesh, or `None` for the open
    /// mesh. Same semantics as `rfcomm::LocalIdentity::mesh_handshake_key`.
    pub mesh_handshake_key: Option<[u8; 32]>,
}

impl std::fmt::Debug for LocalIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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

/// Configuration for `CocService`. The daemon constructs this from the
/// `[bluetooth_coc]` config section.
#[derive(Debug, Clone)]
pub struct CocConfig {
    /// Whether to start at all. Daemon honors `[bluetooth_coc].enabled`.
    pub enabled: bool,
    /// L2CAP CoC PSM to bind / dial.
    pub psm: u16,
    /// Name prefix used by the outbound scan to filter paired devices.
    pub prefix: String,
    /// Interval between outbound discovery ticks.
    pub poll_interval: Duration,
    /// Whether the outbound (paired-device-scan) loop is active.
    pub outbound_enabled: bool,
    /// `bluetoothctl` command used by outbound paired-device scans.
    pub bluetoothctl_command: std::path::PathBuf,
    /// Local TCP loopback address that post-handshake CoC bytes are
    /// bridged onto. Should match the `pim-transport` TCP listener.
    /// `None` disables the bridge — discovery still emits `Discovered`
    /// events but the channel will not carry mesh frames.
    pub local_bridge_addr: Option<SocketAddr>,
    /// Reserved for the Phase 4 GAP-scan loop. Today the outbound
    /// dialer reads `bluetoothctl devices Paired` exactly like the
    /// RFCOMM path; flipping this on switches the discovery loop to
    /// `Adapter1.StartDiscovery` with `Transport = "le"`. Off by
    /// default until Phase 4 ships.
    pub discovery_enabled: bool,
    /// Cadence between inquiry cycles when `discovery_enabled = true`.
    pub inquiry_interval: Duration,
    /// Bluetooth address-type to use when dialing peers from the
    /// outbound paired-devices loop. BlueZ caches both public and
    /// random addresses by their string form, so the user (or future
    /// auto-detection) tells the dialer which one to feed into
    /// `sockaddr_l2.l2_bdaddr_type`. Most smartphones use random
    /// addresses; most Linux-controller-paired peers use public.
    pub peer_bdaddr_type: u8,
}

impl Default for CocConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            psm: DEFAULT_PSM,
            prefix: DEFAULT_PREFIX.to_string(),
            poll_interval: Duration::from_secs(30),
            outbound_enabled: true,
            bluetoothctl_command: std::path::PathBuf::from("bluetoothctl"),
            local_bridge_addr: None,
            discovery_enabled: true,
            inquiry_interval: Duration::from_secs(60),
            peer_bdaddr_type: BDADDR_LE_PUBLIC,
        }
    }
}

/// Discovery / lifecycle events emitted by the service. Same shape as
/// `RfcommEvent` so the daemon's existing handler dispatches both with
/// a thin translation layer — only the transport label changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum CocEvent {
    /// Service-up signal.
    Listening { psm: u16 },
    /// Identity exchange completed.
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
    /// Channel closed.
    Lost { bd_addr: String, reason: String },
    /// Outbound dial that did not produce a working channel.
    OpenFailed {
        bd_addr: String,
        name: String,
        reason: String,
    },
    /// Catch-all error. `code` lives in the `-33100..=-33199` range —
    /// disjoint from RFCOMM's `-33000..=-33099` so log filters can
    /// distinguish them.
    Error { code: i32, message: String },
}

/// Public errors.
#[derive(Debug, thiserror::Error)]
pub enum CocError {
    #[error("l2cap coc not supported on this platform")]
    UnsupportedPlatform,

    #[error("bind L2CAP CoC psm {psm:#06x}: {source}")]
    BindFailed { psm: u16, source: std::io::Error },

    #[error("connect to {bd_addr} psm {psm:#06x}: {source}")]
    ConnectFailed {
        bd_addr: String,
        psm: u16,
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
pub struct CocService {
    cancel: CancellationToken,
}

impl CocService {
    /// Spawn the service. On non-Linux platforms returns
    /// `UnsupportedPlatform`; the daemon should not call this on macOS
    /// or android (the android backend ships in the UI repo).
    #[allow(unused_variables)]
    pub fn start(
        cfg: CocConfig,
        identity: LocalIdentity,
        events_tx: mpsc::Sender<CocEvent>,
    ) -> Result<Self, CocError> {
        if !cfg.enabled {
            return Err(CocError::UnsupportedPlatform);
        }
        #[cfg(target_os = "linux")]
        {
            let cancel = CancellationToken::new();
            // Shared session-dedup set — same rationale as the RFCOMM
            // `active` HashSet: first arriver (inbound accept or
            // outbound dial) wins the slot so we never end up with two
            // sessions per peer where the second clobbers the first.
            let active: std::sync::Arc<tokio::sync::Mutex<std::collections::HashSet<BdAddr>>> =
                std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashSet::new()));
            listener::spawn(
                cfg.clone(),
                identity.clone(),
                events_tx.clone(),
                cancel.clone(),
                active.clone(),
            )?;
            // Phase 4: LE GAP advertising + scan are opt-in via
            // `discovery_enabled`. Advertising publishes our bound
            // PSM in service-data so remote scanners can dial; scan
            // discovers PIM advertisers and dials each new peer's
            // PSM. The pair forms the LE-native discovery surface
            // that replaces RFCOMM's BR/EDR-inquiry path.
            if cfg.discovery_enabled {
                advertising::spawn(
                    cfg.clone(),
                    identity.clone(),
                    cfg.psm,
                    events_tx.clone(),
                    cancel.clone(),
                );
                scan::spawn(
                    cfg.clone(),
                    identity.clone(),
                    events_tx.clone(),
                    cancel.clone(),
                    active.clone(),
                );
            }
            if cfg.outbound_enabled {
                outbound::spawn(cfg, identity, events_tx, cancel.clone(), active);
            }
            Ok(Self { cancel })
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(CocError::UnsupportedPlatform)
        }
    }

    /// Cancel the listener + outbound tasks. Idempotent.
    pub fn shutdown(&self) {
        self.cancel.cancel();
    }
}

impl Drop for CocService {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// Format a 6-byte BD_ADDR as `"AA:BB:CC:DD:EE:FF"`. Same encoding as
/// `rfcomm::format_bdaddr` — both transports share the wire-string
/// convention used in the JSON Hello and in operator-facing logs.
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

/// ISO-8601 timestamp for the current instant. Same format as
/// `rfcomm::now_iso`.
pub fn now_iso() -> String {
    let now = SystemTime::now();
    let secs = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (year, month, day, hour, min, sec) = ymdhms_from_unix(secs);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hour, min, sec
    )
}

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
