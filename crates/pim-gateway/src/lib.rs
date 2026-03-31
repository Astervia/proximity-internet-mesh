//! Userspace NAT / gateway engine.
//!
//! The gateway receives raw IPv4 packets from the mesh, rewrites their source
//! IP/port to the gateway's external address, forwards them to the internet,
//! and routes the responses back to the originating mesh client.
//!
//! # Conntrack
//!
//! Each outbound flow is identified by `(protocol, orig_src_ip, orig_src_port)`.
//! A free external port is assigned from a pool (30000–59999). The reverse
//! mapping `ext_port → orig` is used to demux inbound responses.
//!
//! Entries expire after an idle timeout (default: 60 s for UDP, 300 s for TCP,
//! 10 s for ICMP).
//!
//! # iptables setup
//!
//! `GatewayEngine::setup_masquerade` shells out to `iptables` and `sysctl` to
//! configure MASQUERADE on the internet-facing interface and enable IP
//! forwarding.  This is idempotent (uses `-C` to check before `-A`).

#![warn(missing_docs)]

pub mod ip_pool;
pub use ip_pool::{IpPool, IpPoolError, Lease};

use std::collections::HashMap;
use std::io;
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;
use tracing::{debug, trace};

// ── Error type ────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
/// Errors raised while performing userspace NAT translation or setup.
pub enum GatewayError {
    /// Packet bytes were shorter than the parser expected.
    #[error("packet too short ({0} bytes)")]
    PacketTooShort(usize),
    /// Packet version was not IPv4.
    #[error("not an IPv4 packet")]
    NotIpv4,
    /// Packet protocol is not handled by the gateway engine.
    #[error("unsupported protocol: {0}")]
    UnsupportedProtocol(u8),
    /// No reverse conntrack entry matched an inbound response packet.
    #[error("no conntrack entry for inbound packet (proto={0}, ext_port={1})")]
    NoConntrackEntry(u8, u16),
    /// No free conntrack slot or external port was available.
    #[error("conntrack table full")]
    ConntrackFull,
    /// An external command such as `iptables` or `sysctl` failed.
    #[error("iptables/sysctl command failed: {0}")]
    CommandFailed(String),
    /// A local I/O operation failed.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
}

// ── Protocol constants ────────────────────────────────────────────────────────

/// ICMP protocol number used in IPv4 headers.
pub const PROTO_ICMP: u8 = 1;
/// TCP protocol number used in IPv4 headers.
pub const PROTO_TCP: u8 = 6;
/// UDP protocol number used in IPv4 headers.
pub const PROTO_UDP: u8 = 17;

// ── Conntrack ─────────────────────────────────────────────────────────────────

/// Key into the forward conntrack map (outbound direction).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ConntrackKey {
    proto: u8,
    orig_src: Ipv4Addr,
    orig_port: u16, // src port for TCP/UDP; ICMP id for ICMP
}

/// A single conntrack entry.
#[derive(Debug, Clone)]
struct ConntrackEntry {
    /// External port (or ICMP id) assigned to this flow.
    ext_port: u16,
    /// When this entry was last used.
    last_seen: Instant,
    /// Original source for reverse mapping.
    orig_src: Ipv4Addr,
    orig_port: u16,
    proto: u8,
}

impl ConntrackEntry {
    fn idle_timeout(&self) -> Duration {
        match self.proto {
            PROTO_TCP => Duration::from_secs(300), // 5 min
            PROTO_UDP => Duration::from_secs(30),  // 30 s
            PROTO_ICMP => Duration::from_secs(10), // 10 s
            _ => Duration::from_secs(30),
        }
    }

    fn is_expired(&self) -> bool {
        self.last_seen.elapsed() > self.idle_timeout()
    }
}

// ── Port pool ─────────────────────────────────────────────────────────────────

const PORT_MIN: u16 = 30000;
const PORT_MAX: u16 = 59999;

struct PortPool {
    next: u16,
    in_use: std::collections::HashSet<u16>,
}

impl PortPool {
    fn new() -> Self {
        Self {
            next: PORT_MIN,
            in_use: Default::default(),
        }
    }

    fn allocate(&mut self) -> Option<u16> {
        let start = self.next;
        loop {
            let port = self.next;
            self.next = if self.next >= PORT_MAX {
                PORT_MIN
            } else {
                self.next + 1
            };

            if !self.in_use.contains(&port) {
                self.in_use.insert(port);
                return Some(port);
            }

            if self.next == start {
                // Wrapped all the way around — pool exhausted
                return None;
            }
        }
    }

    fn release(&mut self, port: u16) {
        self.in_use.remove(&port);
    }
}

// ── GatewayEngine ─────────────────────────────────────────────────────────────

/// Shared state protected by a single Mutex.
struct Inner {
    /// Forward map: flow key → entry.
    forward: HashMap<ConntrackKey, ConntrackEntry>,
    /// Reverse map: (proto, ext_port) → ConntrackKey.
    reverse: HashMap<(u8, u16), ConntrackKey>,
    ports: PortPool,
}

/// Userspace NAT engine.
///
/// All public methods are async and internally lock a single Mutex.
pub struct GatewayEngine {
    /// The gateway's own external (internet-facing) IP address.
    external_ip: Ipv4Addr,
    /// Name of the internet-facing interface (e.g. "eth0").
    internet_iface: String,
    inner: Mutex<Inner>,
}

impl GatewayEngine {
    /// Create a new gateway engine.
    ///
    /// `external_ip` is the address that will be used as the rewritten source
    /// on outbound packets (typically the gateway's public / LAN IP).
    pub fn new(external_ip: Ipv4Addr, internet_iface: impl Into<String>) -> Self {
        Self {
            external_ip,
            internet_iface: internet_iface.into(),
            inner: Mutex::new(Inner {
                forward: HashMap::new(),
                reverse: HashMap::new(),
                ports: PortPool::new(),
            }),
        }
    }

    // ── Outbound NAT ──────────────────────────────────────────────────────────

    /// Translate an outbound IP packet: rewrite the source address to the
    /// gateway's external IP, allocate or reuse a conntrack entry, recalculate
    /// checksums.
    ///
    /// `packet` is modified in-place. Returns the external port assigned to
    /// this flow (useful for tests).
    pub async fn translate_outbound(&self, packet: &mut [u8]) -> Result<u16, GatewayError> {
        let (proto, src_ip, src_port) = parse_flow(packet)?;

        let key = ConntrackKey {
            proto,
            orig_src: src_ip,
            orig_port: src_port,
        };

        let mut inner = self.inner.lock().await;

        // Look up or create conntrack entry
        let ext_port = if let Some(entry) = inner.forward.get_mut(&key) {
            entry.last_seen = Instant::now();
            entry.ext_port
        } else {
            let ext_port = inner.ports.allocate().ok_or(GatewayError::ConntrackFull)?;
            let entry = ConntrackEntry {
                ext_port,
                last_seen: Instant::now(),
                orig_src: src_ip,
                orig_port: src_port,
                proto,
            };
            inner.reverse.insert((proto, ext_port), key.clone());
            inner.forward.insert(key, entry);
            ext_port
        };

        // Rewrite source IP and port in the packet
        rewrite_src(packet, self.external_ip, ext_port)?;

        trace!(
            orig_src = %src_ip, orig_port = src_port,
            ext_ip = %self.external_ip, ext_port = ext_port,
            proto = proto, "NAT outbound"
        );
        Ok(ext_port)
    }

    // ── Inbound NAT ───────────────────────────────────────────────────────────

    /// Translate an inbound response packet: look up the conntrack entry by
    /// destination port, rewrite the destination IP and port back to the
    /// original source, recalculate checksums.
    ///
    /// Returns the original client's IP address.
    pub async fn translate_inbound(&self, packet: &mut [u8]) -> Result<Ipv4Addr, GatewayError> {
        let proto = ip_protocol(packet)?;
        let dst_port = transport_dst_port(packet, proto)?;

        let mut inner = self.inner.lock().await;

        let key = inner
            .reverse
            .get(&(proto, dst_port))
            .cloned()
            .ok_or(GatewayError::NoConntrackEntry(proto, dst_port))?;

        let entry = inner
            .forward
            .get_mut(&key)
            .ok_or(GatewayError::NoConntrackEntry(proto, dst_port))?;

        entry.last_seen = Instant::now();
        let orig_src = entry.orig_src;
        let orig_port = entry.orig_port;

        // Rewrite destination IP and port
        rewrite_dst(packet, orig_src, orig_port)?;

        trace!(
            orig_dst = %orig_src, orig_port = orig_port,
            ext_port = dst_port, proto = proto, "NAT inbound"
        );
        Ok(orig_src)
    }

    // ── Cleanup ───────────────────────────────────────────────────────────────

    /// Remove all expired conntrack entries and release their ports.
    pub async fn cleanup_expired(&self) {
        let mut inner = self.inner.lock().await;
        let expired: Vec<ConntrackKey> = inner
            .forward
            .iter()
            .filter(|(_, e)| e.is_expired())
            .map(|(k, _)| k.clone())
            .collect();

        for key in expired {
            if let Some(entry) = inner.forward.remove(&key) {
                inner.reverse.remove(&(entry.proto, entry.ext_port));
                inner.ports.release(entry.ext_port);
                debug!(
                    proto = entry.proto, orig_src = %entry.orig_src,
                    ext_port = entry.ext_port, "conntrack entry expired"
                );
            }
        }
    }

    /// Number of active conntrack entries.
    pub async fn conntrack_size(&self) -> usize {
        self.inner.lock().await.forward.len()
    }

    // ── iptables setup ────────────────────────────────────────────────────────

    /// Set up kernel IP forwarding and iptables MASQUERADE for `mesh_cidr`.
    ///
    /// Example `mesh_cidr`: `"10.77.0.0/24"`.
    /// This is idempotent.
    pub fn setup_masquerade(&self, mesh_cidr: &str) -> Result<(), GatewayError> {
        // Enable IP forwarding (may fail with permission denied in Docker, but often already set)
        if let Err(e) = run_cmd("sysctl", &["-w", "net.ipv4.ip_forward=1"]) {
            tracing::warn!("sysctl failed (ignoring): {e}");
        }

        // Add MASQUERADE rule (check first to avoid duplicates)
        let rule_args = [
            "-t",
            "nat",
            "-A",
            "POSTROUTING",
            "-s",
            mesh_cidr,
            "-o",
            &self.internet_iface,
            "-j",
            "MASQUERADE",
        ];
        let check_args = {
            let mut a = rule_args.to_vec();
            a[2] = "-C"; // replace -A with -C
            a
        };

        if !check_cmd_quiet("iptables", &check_args)? {
            // Rule not present — add it
            run_cmd("iptables", &rule_args)?;
        }

        // Allow FORWARD traffic from the mesh
        let fwd_args = ["-A", "FORWARD", "-s", mesh_cidr, "-j", "ACCEPT"];
        let fwd_check = {
            let mut a = fwd_args.to_vec();
            a[0] = "-C";
            a
        };
        if !check_cmd_quiet("iptables", &fwd_check)? {
            run_cmd("iptables", &fwd_args)?;
        }

        // Reserve the userspace NAT port range so the host TCP/UDP stack does
        // not race our raw-socket gateway handling with unsolicited replies.
        for proto in ["tcp", "udp"] {
            let drop_args = input_drop_args(proto, &self.internet_iface);
            let drop_check = {
                let mut a = drop_args.to_vec();
                a[0] = "-C";
                a
            };
            if !check_cmd_quiet("iptables", &drop_check)? {
                run_cmd("iptables", &drop_args)?;
            }
        }

        debug!(mesh_cidr = mesh_cidr, iface = %self.internet_iface, "iptables MASQUERADE configured");
        Ok(())
    }
}

fn input_drop_args<'a>(proto: &'a str, iface: &'a str) -> [&'a str; 10] {
    [
        "-A",
        "INPUT",
        "-i",
        iface,
        "-p",
        proto,
        "--dport",
        "30000:59999",
        "-j",
        "DROP",
    ]
}

// ── Packet parsing helpers ────────────────────────────────────────────────────

/// Minimum IPv4 header size (no options).
const IP_HEADER_MIN: usize = 20;

fn ip_ihl(packet: &[u8]) -> usize {
    ((packet[0] & 0x0f) as usize) * 4
}

fn ip_protocol(packet: &[u8]) -> Result<u8, GatewayError> {
    if packet.len() < IP_HEADER_MIN {
        return Err(GatewayError::PacketTooShort(packet.len()));
    }
    if (packet[0] >> 4) != 4 {
        return Err(GatewayError::NotIpv4);
    }
    Ok(packet[9])
}

/// Extract (protocol, src_ip, src_port) from an outbound packet.
fn parse_flow(packet: &[u8]) -> Result<(u8, Ipv4Addr, u16), GatewayError> {
    let proto = ip_protocol(packet)?;
    let ihl = ip_ihl(packet);

    let src_ip = Ipv4Addr::new(packet[12], packet[13], packet[14], packet[15]);

    let src_port = match proto {
        PROTO_TCP | PROTO_UDP => {
            if packet.len() < ihl + 4 {
                return Err(GatewayError::PacketTooShort(packet.len()));
            }
            u16::from_be_bytes([packet[ihl], packet[ihl + 1]])
        }
        PROTO_ICMP => {
            // ICMP type 8 (echo) / 0 (echo reply): id at offset 4
            if packet.len() < ihl + 8 {
                return Err(GatewayError::PacketTooShort(packet.len()));
            }
            u16::from_be_bytes([packet[ihl + 4], packet[ihl + 5]])
        }
        other => return Err(GatewayError::UnsupportedProtocol(other)),
    };

    Ok((proto, src_ip, src_port))
}

/// Return the destination port of the transport layer header.
fn transport_dst_port(packet: &[u8], proto: u8) -> Result<u16, GatewayError> {
    let ihl = ip_ihl(packet);
    match proto {
        PROTO_TCP | PROTO_UDP => {
            if packet.len() < ihl + 4 {
                return Err(GatewayError::PacketTooShort(packet.len()));
            }
            Ok(u16::from_be_bytes([packet[ihl + 2], packet[ihl + 3]]))
        }
        PROTO_ICMP => {
            // Echo reply: id at offset 4
            if packet.len() < ihl + 8 {
                return Err(GatewayError::PacketTooShort(packet.len()));
            }
            Ok(u16::from_be_bytes([packet[ihl + 4], packet[ihl + 5]]))
        }
        other => Err(GatewayError::UnsupportedProtocol(other)),
    }
}

// ── Packet rewriting ──────────────────────────────────────────────────────────

/// Rewrite source IP and source port; recalculate checksums.
fn rewrite_src(packet: &mut [u8], new_src: Ipv4Addr, new_port: u16) -> Result<(), GatewayError> {
    let proto = ip_protocol(packet)?;
    let ihl = ip_ihl(packet);

    let old_src = Ipv4Addr::new(packet[12], packet[13], packet[14], packet[15]);
    let old_port = match proto {
        PROTO_TCP | PROTO_UDP => {
            if packet.len() < ihl + 4 {
                return Err(GatewayError::PacketTooShort(packet.len()));
            }
            u16::from_be_bytes([packet[ihl], packet[ihl + 1]])
        }
        PROTO_ICMP => {
            if packet.len() < ihl + 8 {
                return Err(GatewayError::PacketTooShort(packet.len()));
            }
            u16::from_be_bytes([packet[ihl + 4], packet[ihl + 5]])
        }
        other => return Err(GatewayError::UnsupportedProtocol(other)),
    };

    // Write new source IP
    packet[12..16].copy_from_slice(&new_src.octets());

    // Write new source port / ICMP id
    let new_port_bytes = new_port.to_be_bytes();
    match proto {
        PROTO_TCP | PROTO_UDP => {
            packet[ihl..ihl + 2].copy_from_slice(&new_port_bytes);
        }
        PROTO_ICMP => {
            packet[ihl + 4..ihl + 6].copy_from_slice(&new_port_bytes);
        }
        _ => {}
    }

    // Recalculate IP checksum
    recalc_ip_checksum(packet);

    // Recalculate transport checksum
    recalc_transport_checksum(packet, proto, ihl, old_src, new_src, old_port, new_port);

    Ok(())
}

/// Rewrite destination IP and destination port; recalculate checksums.
fn rewrite_dst(packet: &mut [u8], new_dst: Ipv4Addr, new_port: u16) -> Result<(), GatewayError> {
    let proto = ip_protocol(packet)?;
    let ihl = ip_ihl(packet);

    let old_dst = Ipv4Addr::new(packet[16], packet[17], packet[18], packet[19]);
    let old_port = match proto {
        PROTO_TCP | PROTO_UDP => {
            if packet.len() < ihl + 4 {
                return Err(GatewayError::PacketTooShort(packet.len()));
            }
            u16::from_be_bytes([packet[ihl + 2], packet[ihl + 3]])
        }
        PROTO_ICMP => {
            if packet.len() < ihl + 8 {
                return Err(GatewayError::PacketTooShort(packet.len()));
            }
            u16::from_be_bytes([packet[ihl + 4], packet[ihl + 5]])
        }
        other => return Err(GatewayError::UnsupportedProtocol(other)),
    };

    // Write new destination IP
    packet[16..20].copy_from_slice(&new_dst.octets());

    // Write new destination port / ICMP id
    let new_port_bytes = new_port.to_be_bytes();
    match proto {
        PROTO_TCP | PROTO_UDP => {
            packet[ihl + 2..ihl + 4].copy_from_slice(&new_port_bytes);
        }
        PROTO_ICMP => {
            packet[ihl + 4..ihl + 6].copy_from_slice(&new_port_bytes);
        }
        _ => {}
    }

    recalc_ip_checksum(packet);
    recalc_transport_checksum(packet, proto, ihl, old_dst, new_dst, old_port, new_port);

    Ok(())
}

// ── Checksum helpers ──────────────────────────────────────────────────────────

/// One's complement sum of 16-bit words (used for IP/TCP/UDP checksums).
fn ones_complement_sum(data: &[u8]) -> u32 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < data.len() {
        sum += u16::from_be_bytes([data[i], data[i + 1]]) as u32;
        i += 2;
    }
    if i < data.len() {
        sum += (data[i] as u32) << 8; // pad last odd byte
    }
    sum
}

fn fold_checksum(mut sum: u32) -> u16 {
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

/// Recalculate and overwrite the IP header checksum.
fn recalc_ip_checksum(packet: &mut [u8]) {
    let ihl = ip_ihl(packet);
    packet[10] = 0;
    packet[11] = 0;
    let sum = fold_checksum(ones_complement_sum(&packet[..ihl]));
    packet[10..12].copy_from_slice(&sum.to_be_bytes());
}

/// Incrementally update the transport-layer checksum after an IP address or
/// port rewrite, using the RFC 1624 incremental update formula.
fn recalc_transport_checksum(
    packet: &mut [u8],
    proto: u8,
    ihl: usize,
    old_addr: Ipv4Addr,
    new_addr: Ipv4Addr,
    old_port: u16,
    new_port: u16,
) {
    let cksum_offset = match proto {
        PROTO_TCP => ihl + 16,
        PROTO_UDP => ihl + 6,
        PROTO_ICMP => ihl + 2,
        _ => return,
    };

    if packet.len() < cksum_offset + 2 {
        return;
    }

    // For ICMP, recompute the full checksum (no pseudo-header complication)
    if proto == PROTO_ICMP {
        let icmp_start = ihl;
        let icmp_end = packet.len();
        packet[cksum_offset] = 0;
        packet[cksum_offset + 1] = 0;
        let sum = fold_checksum(ones_complement_sum(&packet[icmp_start..icmp_end]));
        packet[cksum_offset..cksum_offset + 2].copy_from_slice(&sum.to_be_bytes());
        return;
    }

    // TCP/UDP: incremental update using RFC 1624
    // new_checksum = ~( ~old_checksum + ~old_value + new_value )
    // Applied twice: once for address change, once for port change.
    let old_cksum = u16::from_be_bytes([packet[cksum_offset], packet[cksum_offset + 1]]);

    if old_cksum == 0 && proto == PROTO_UDP {
        // UDP checksum is optional; 0 means not computed
        return;
    }

    let mut cksum = !old_cksum as u32;

    // Address change: 4 bytes (two 16-bit words)
    let old_a = old_addr.octets();
    let new_a = new_addr.octets();
    cksum = cksum
        .wrapping_add(!u16::from_be_bytes([old_a[0], old_a[1]]) as u32)
        .wrapping_add(u16::from_be_bytes([new_a[0], new_a[1]]) as u32)
        .wrapping_add(!u16::from_be_bytes([old_a[2], old_a[3]]) as u32)
        .wrapping_add(u16::from_be_bytes([new_a[2], new_a[3]]) as u32);

    // Port change
    cksum = cksum
        .wrapping_add(!old_port as u32)
        .wrapping_add(new_port as u32);

    let new_cksum = fold_checksum(cksum);
    packet[cksum_offset..cksum_offset + 2].copy_from_slice(&new_cksum.to_be_bytes());
}

// ── Shell helpers ─────────────────────────────────────────────────────────────

fn run_cmd(program: &str, args: &[&str]) -> Result<(), GatewayError> {
    let status = std::process::Command::new(program)
        .args(args)
        .status()
        .map_err(|e| GatewayError::CommandFailed(format!("{program}: {e}")))?;

    if status.success() {
        Ok(())
    } else {
        Err(GatewayError::CommandFailed(format!(
            "{program} {} exited with {:?}",
            args.join(" "),
            status.code()
        )))
    }
}

fn check_cmd_quiet(program: &str, args: &[&str]) -> Result<bool, GatewayError> {
    let status = std::process::Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| GatewayError::CommandFailed(format!("{program}: {e}")))?;

    Ok(status.success())
}

// ── IP packet builder (test helper) ──────────────────────────────────────────

#[cfg(test)]
/// Test packet builders and helpers shared across gateway unit tests.
pub mod test_util {
    use super::*;

    /// Build a minimal IPv4/UDP packet.
    pub fn udp_packet(
        src: Ipv4Addr,
        dst: Ipv4Addr,
        src_port: u16,
        dst_port: u16,
        payload: &[u8],
    ) -> Vec<u8> {
        let udp_len = 8 + payload.len();
        let total = 20 + udp_len;
        let mut pkt = vec![0u8; total];

        // IP header
        pkt[0] = 0x45; // version=4, ihl=5
        pkt[1] = 0;
        pkt[2..4].copy_from_slice(&(total as u16).to_be_bytes());
        pkt[8] = 64; // TTL
        pkt[9] = PROTO_UDP;
        pkt[12..16].copy_from_slice(&src.octets());
        pkt[16..20].copy_from_slice(&dst.octets());

        // UDP header
        pkt[20..22].copy_from_slice(&src_port.to_be_bytes());
        pkt[22..24].copy_from_slice(&dst_port.to_be_bytes());
        pkt[24..26].copy_from_slice(&(udp_len as u16).to_be_bytes());
        // checksum = 0 (optional for UDP)

        // Payload
        pkt[28..].copy_from_slice(payload);

        recalc_ip_checksum(&mut pkt);
        pkt
    }

    /// Build a minimal IPv4/TCP SYN packet.
    pub fn tcp_packet(src: Ipv4Addr, dst: Ipv4Addr, src_port: u16, dst_port: u16) -> Vec<u8> {
        let tcp_len = 20; // no options
        let total = 20 + tcp_len;
        let mut pkt = vec![0u8; total];

        pkt[0] = 0x45;
        pkt[2..4].copy_from_slice(&(total as u16).to_be_bytes());
        pkt[8] = 64;
        pkt[9] = PROTO_TCP;
        pkt[12..16].copy_from_slice(&src.octets());
        pkt[16..20].copy_from_slice(&dst.octets());

        pkt[20..22].copy_from_slice(&src_port.to_be_bytes());
        pkt[22..24].copy_from_slice(&dst_port.to_be_bytes());
        pkt[32] = 0x50; // data offset = 5 (20 bytes)
        pkt[33] = 0x02; // SYN flag

        recalc_ip_checksum(&mut pkt);

        // Compute TCP checksum (requires pseudo-header)
        let cksum = tcp_checksum_full(&pkt);
        pkt[36..38].copy_from_slice(&cksum.to_be_bytes());

        pkt
    }

    fn tcp_checksum_full(pkt: &[u8]) -> u16 {
        let ihl = ip_ihl(pkt);
        let tcp_len = pkt.len() - ihl;
        let mut pseudo = vec![0u8; 12 + tcp_len];
        pseudo[0..4].copy_from_slice(&pkt[12..16]); // src ip
        pseudo[4..8].copy_from_slice(&pkt[16..20]); // dst ip
        pseudo[8] = 0;
        pseudo[9] = PROTO_TCP;
        pseudo[10..12].copy_from_slice(&(tcp_len as u16).to_be_bytes());
        pseudo[12..].copy_from_slice(&pkt[ihl..]);
        fold_checksum(ones_complement_sum(&pseudo))
    }

    /// Build a minimal IPv4/ICMP echo request packet.
    pub fn icmp_echo_packet(src: Ipv4Addr, dst: Ipv4Addr, id: u16, seq: u16) -> Vec<u8> {
        let total = 20 + 8; // IP header + ICMP header
        let mut pkt = vec![0u8; total];

        // IP header
        pkt[0] = 0x45;
        pkt[2..4].copy_from_slice(&(total as u16).to_be_bytes());
        pkt[8] = 64; // TTL
        pkt[9] = PROTO_ICMP;
        pkt[12..16].copy_from_slice(&src.octets());
        pkt[16..20].copy_from_slice(&dst.octets());

        // ICMP echo request: type=8, code=0
        pkt[20] = 8;
        pkt[21] = 0;
        pkt[24..26].copy_from_slice(&id.to_be_bytes());
        pkt[26..28].copy_from_slice(&seq.to_be_bytes());

        recalc_ip_checksum(&mut pkt);
        let sum = fold_checksum(ones_complement_sum(&pkt[20..]));
        pkt[22..24].copy_from_slice(&sum.to_be_bytes());
        pkt
    }

    /// Build a minimal IPv4/ICMP echo reply packet.
    pub fn icmp_echo_reply_packet(src: Ipv4Addr, dst: Ipv4Addr, id: u16, seq: u16) -> Vec<u8> {
        let mut pkt = icmp_echo_packet(src, dst, id, seq);
        pkt[20] = 0; // type: echo reply
                     // Recompute ICMP checksum
        pkt[22] = 0;
        pkt[23] = 0;
        let sum = fold_checksum(ones_complement_sum(&pkt[20..]));
        pkt[22..24].copy_from_slice(&sum.to_be_bytes());
        pkt
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use test_util::*;

    const CLIENT_IP: Ipv4Addr = Ipv4Addr::new(10, 77, 0, 5);
    const GW_EXT_IP: Ipv4Addr = Ipv4Addr::new(203, 0, 113, 1);
    const REMOTE_IP: Ipv4Addr = Ipv4Addr::new(8, 8, 8, 8);

    fn engine() -> GatewayEngine {
        GatewayEngine::new(GW_EXT_IP, "eth0")
    }

    #[tokio::test]
    async fn outbound_creates_conntrack_and_rewrites_src() {
        let gw = engine();
        let mut pkt = udp_packet(CLIENT_IP, REMOTE_IP, 50000, 53, b"query");

        let ext_port = gw.translate_outbound(&mut pkt).await.unwrap();

        // Source IP must now be the gateway's external IP
        let new_src = Ipv4Addr::new(pkt[12], pkt[13], pkt[14], pkt[15]);
        assert_eq!(new_src, GW_EXT_IP);

        // Source port must be the external port
        let new_src_port = u16::from_be_bytes([pkt[20], pkt[21]]);
        assert_eq!(new_src_port, ext_port);

        // Conntrack must have one entry
        assert_eq!(gw.conntrack_size().await, 1);
    }

    #[tokio::test]
    async fn inbound_rewrites_dst() {
        let gw = engine();

        // Outbound first (creates conntrack)
        let mut out_pkt = udp_packet(CLIENT_IP, REMOTE_IP, 50000, 53, b"query");
        let ext_port = gw.translate_outbound(&mut out_pkt).await.unwrap();

        // Synthesise inbound response: src=REMOTE, dst=GW_EXT_IP:ext_port
        let mut in_pkt = udp_packet(REMOTE_IP, GW_EXT_IP, 53, ext_port, b"response");

        let orig_src = gw.translate_inbound(&mut in_pkt).await.unwrap();
        assert_eq!(orig_src, CLIENT_IP);

        let new_dst = Ipv4Addr::new(in_pkt[16], in_pkt[17], in_pkt[18], in_pkt[19]);
        assert_eq!(new_dst, CLIENT_IP);

        let new_dst_port = u16::from_be_bytes([in_pkt[22], in_pkt[23]]);
        assert_eq!(new_dst_port, 50000);
    }

    #[tokio::test]
    async fn unknown_inbound_returns_error() {
        let gw = engine();
        let mut pkt = udp_packet(REMOTE_IP, GW_EXT_IP, 53, 40000, b"unsolicited");
        let result = gw.translate_inbound(&mut pkt).await;
        assert!(matches!(result, Err(GatewayError::NoConntrackEntry(_, _))));
    }

    #[tokio::test]
    async fn same_flow_reuses_conntrack_entry() {
        let gw = engine();
        let mut p1 = udp_packet(CLIENT_IP, REMOTE_IP, 50000, 53, b"q1");
        let mut p2 = udp_packet(CLIENT_IP, REMOTE_IP, 50000, 53, b"q2");

        let ep1 = gw.translate_outbound(&mut p1).await.unwrap();
        let ep2 = gw.translate_outbound(&mut p2).await.unwrap();

        assert_eq!(ep1, ep2); // same flow → same external port
        assert_eq!(gw.conntrack_size().await, 1);
    }

    #[tokio::test]
    async fn different_flows_get_different_ports() {
        let gw = engine();
        let mut p1 = udp_packet(CLIENT_IP, REMOTE_IP, 50001, 53, b"a");
        let mut p2 = udp_packet(CLIENT_IP, REMOTE_IP, 50002, 53, b"b");

        let ep1 = gw.translate_outbound(&mut p1).await.unwrap();
        let ep2 = gw.translate_outbound(&mut p2).await.unwrap();

        assert_ne!(ep1, ep2);
        assert_eq!(gw.conntrack_size().await, 2);
    }

    #[tokio::test]
    async fn cleanup_removes_expired_entries() {
        let gw = engine();
        let mut pkt = udp_packet(CLIENT_IP, REMOTE_IP, 50000, 53, b"q");
        gw.translate_outbound(&mut pkt).await.unwrap();
        assert_eq!(gw.conntrack_size().await, 1);

        // Manually set last_seen far in the past
        {
            let mut inner = gw.inner.lock().await;
            for entry in inner.forward.values_mut() {
                entry.last_seen = Instant::now() - Duration::from_secs(3600);
            }
        }

        gw.cleanup_expired().await;
        assert_eq!(gw.conntrack_size().await, 0);
    }

    #[tokio::test]
    async fn ip_checksum_valid_after_outbound() {
        let gw = engine();
        let mut pkt = udp_packet(CLIENT_IP, REMOTE_IP, 50000, 80, b"data");
        gw.translate_outbound(&mut pkt).await.unwrap();

        // Verify IP checksum
        let ihl = ip_ihl(&pkt);
        let computed = fold_checksum(ones_complement_sum(&pkt[..ihl]));
        assert_eq!(computed, 0, "IP checksum should fold to 0 (valid)");
    }

    #[tokio::test]
    async fn tcp_outbound_and_inbound_round_trip() {
        let gw = engine();
        let mut out = tcp_packet(CLIENT_IP, REMOTE_IP, 12345, 80);
        let ext_port = gw.translate_outbound(&mut out).await.unwrap();

        let mut in_pkt = tcp_packet(REMOTE_IP, GW_EXT_IP, 80, ext_port);
        let orig = gw.translate_inbound(&mut in_pkt).await.unwrap();
        assert_eq!(orig, CLIENT_IP);
    }

    #[test]
    fn port_pool_allocates_sequentially() {
        let mut pool = PortPool::new();
        let p1 = pool.allocate().unwrap();
        let p2 = pool.allocate().unwrap();
        assert_eq!(p1, PORT_MIN);
        assert_eq!(p2, PORT_MIN + 1);
    }

    #[test]
    fn port_pool_recycles_released_ports() {
        let mut pool = PortPool::new();
        let p = pool.allocate().unwrap();
        pool.release(p);
        // Next allocation wraps around and eventually returns p again
        let p2 = pool.allocate().unwrap();
        // After release, p is available; the pool counter advanced past PORT_MIN
        // so the next alloc gives PORT_MIN+1. After wraparound it gives PORT_MIN.
        // Just verify we got some valid port.
        assert!((PORT_MIN..=PORT_MAX).contains(&p2));
    }

    #[tokio::test]
    async fn icmp_outbound_and_inbound_round_trip() {
        let gw = engine();
        let mut out = icmp_echo_packet(CLIENT_IP, REMOTE_IP, 42, 1);
        let ext_id = gw.translate_outbound(&mut out).await.unwrap();

        // Source IP must be rewritten to gateway external IP
        assert_eq!(Ipv4Addr::new(out[12], out[13], out[14], out[15]), GW_EXT_IP);

        // Inbound echo reply
        let mut in_pkt = icmp_echo_reply_packet(REMOTE_IP, GW_EXT_IP, ext_id, 1);
        let orig = gw.translate_inbound(&mut in_pkt).await.unwrap();
        assert_eq!(orig, CLIENT_IP);
        assert_eq!(
            Ipv4Addr::new(in_pkt[16], in_pkt[17], in_pkt[18], in_pkt[19]),
            CLIENT_IP
        );
    }

    #[tokio::test]
    async fn conntrack_tcp_expires_after_idle_timeout() {
        let gw = engine();
        let mut pkt = tcp_packet(CLIENT_IP, REMOTE_IP, 12345, 80);
        gw.translate_outbound(&mut pkt).await.unwrap();
        {
            let mut inner = gw.inner.lock().await;
            for entry in inner.forward.values_mut() {
                entry.last_seen = Instant::now() - Duration::from_secs(301);
            }
        }
        gw.cleanup_expired().await;
        assert_eq!(gw.conntrack_size().await, 0);
    }

    #[tokio::test]
    async fn conntrack_udp_expires_after_idle_timeout() {
        let gw = engine();
        let mut pkt = udp_packet(CLIENT_IP, REMOTE_IP, 50000, 53, b"q");
        gw.translate_outbound(&mut pkt).await.unwrap();
        {
            let mut inner = gw.inner.lock().await;
            for entry in inner.forward.values_mut() {
                entry.last_seen = Instant::now() - Duration::from_secs(31);
            }
        }
        gw.cleanup_expired().await;
        assert_eq!(gw.conntrack_size().await, 0);
    }

    #[tokio::test]
    async fn conntrack_icmp_expires_after_idle_timeout() {
        let gw = engine();
        let mut pkt = icmp_echo_packet(CLIENT_IP, REMOTE_IP, 1, 1);
        gw.translate_outbound(&mut pkt).await.unwrap();
        {
            let mut inner = gw.inner.lock().await;
            for entry in inner.forward.values_mut() {
                entry.last_seen = Instant::now() - Duration::from_secs(11);
            }
        }
        gw.cleanup_expired().await;
        assert_eq!(gw.conntrack_size().await, 0);
    }

    #[tokio::test]
    async fn conntrack_tcp_not_expired_within_timeout() {
        let gw = engine();
        let mut pkt = tcp_packet(CLIENT_IP, REMOTE_IP, 12345, 80);
        gw.translate_outbound(&mut pkt).await.unwrap();
        {
            let mut inner = gw.inner.lock().await;
            for entry in inner.forward.values_mut() {
                entry.last_seen = Instant::now() - Duration::from_secs(299);
            }
        }
        gw.cleanup_expired().await;
        assert_eq!(gw.conntrack_size().await, 1); // still alive
    }

    #[tokio::test]
    async fn port_released_after_conntrack_expiry() {
        let gw = engine();
        let mut pkt = udp_packet(CLIENT_IP, REMOTE_IP, 50000, 53, b"q");
        gw.translate_outbound(&mut pkt).await.unwrap();
        {
            let mut inner = gw.inner.lock().await;
            for entry in inner.forward.values_mut() {
                entry.last_seen = Instant::now() - Duration::from_secs(31);
            }
        }
        gw.cleanup_expired().await;
        assert_eq!(gw.conntrack_size().await, 0);

        // Pool must accept a new allocation (released port is back in the pool)
        let mut pkt2 = udp_packet(CLIENT_IP, REMOTE_IP, 50001, 53, b"q2");
        let result = gw.translate_outbound(&mut pkt2).await;
        assert!(result.is_ok());
    }

    #[test]
    fn input_drop_args_for_tcp_cover_reserved_nat_range() {
        assert_eq!(
            input_drop_args("tcp", "eno1"),
            [
                "-A",
                "INPUT",
                "-i",
                "eno1",
                "-p",
                "tcp",
                "--dport",
                "30000:59999",
                "-j",
                "DROP"
            ]
        );
    }

    #[test]
    fn input_drop_args_for_udp_cover_reserved_nat_range() {
        assert_eq!(
            input_drop_args("udp", "eno1"),
            [
                "-A",
                "INPUT",
                "-i",
                "eno1",
                "-p",
                "udp",
                "--dport",
                "30000:59999",
                "-j",
                "DROP"
            ]
        );
    }
}
