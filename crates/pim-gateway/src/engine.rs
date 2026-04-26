//! IPv4 userspace NAT engine.
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
//! # Gateway setup
//!
//! `GatewayEngine::setup_masquerade` performs the host-side setup needed for the
//! userspace NAT path. On Linux it shells out to `iptables` plus `sysctl`.
//! On macOS it enables forwarding and installs a small `pf` anchor that keeps
//! the reserved userspace NAT port range away from the host TCP/UDP stack.

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
    /// An external command such as `iptables`, `pfctl`, or `sysctl` failed.
    #[error("gateway setup command failed: {0}")]
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

pub(crate) const PORT_MIN: u16 = 30000;
pub(crate) const PORT_MAX: u16 = 59999;

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
    /// Original value of `net.ipv4.ip_local_reserved_ports` before setup,
    /// so teardown can restore it. `None` until `setup_masquerade` runs.
    #[cfg(target_os = "linux")]
    reserved_ports_backup: std::sync::Mutex<Option<String>>,
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
            #[cfg(target_os = "linux")]
            reserved_ports_backup: std::sync::Mutex::new(None),
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

    // ── Host gateway setup ────────────────────────────────────────────────────

    /// Set up host forwarding rules for `mesh_cidr`.
    ///
    /// Example `mesh_cidr`: `"10.77.0.0/24"`.
    /// This is idempotent.
    pub fn setup_masquerade(&self, mesh_cidr: &str) -> Result<(), GatewayError> {
        #[cfg(target_os = "linux")]
        {
            // Enable IP forwarding (may fail with permission denied in Docker, but often already set)
            if let Err(e) = run_cmd("sysctl", &["-w", "net.ipv4.ip_forward=1"]) {
                tracing::warn!("sysctl failed (ignoring): {e}");
            }

            // Reserve the NAT port range from the kernel's ephemeral allocator so the
            // host's own outbound connections don't pick source ports in 30000-59999.
            // Without this, the INPUT DROP rule below would black-hole legitimate
            // reply traffic to host-initiated connections. Shared allocator covers
            // IPv4 and IPv6, so this one setting is enough.
            self.reserve_nat_ports_from_kernel();

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

        #[cfg(target_os = "macos")]
        {
            self.setup_pf_anchor(mesh_cidr)
        }

        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = mesh_cidr;
            Err(GatewayError::CommandFailed(
                "gateway setup is not supported on this platform".into(),
            ))
        }
    }

    /// Reverse of `setup_masquerade`: removes the iptables rules and restores
    /// any sysctl values we changed. Best-effort; errors are logged, not
    /// propagated.
    ///
    /// Safe to call even if `setup_masquerade` was never called — each step is
    /// guarded by a "rule present?" check.
    pub fn teardown_masquerade(&self, mesh_cidr: &str) {
        #[cfg(target_os = "linux")]
        {
            // Remove INPUT DROP rules first so the host regains access to the
            // NAT port range as early as possible.
            for proto in ["tcp", "udp"] {
                let drop_args = input_drop_args(proto, &self.internet_iface);
                iptables_delete_if_present("iptables", &drop_args);
            }

            let post_args = [
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
            iptables_delete_if_present("iptables", &post_args);

            let fwd_args = ["-A", "FORWARD", "-s", mesh_cidr, "-j", "ACCEPT"];
            iptables_delete_if_present("iptables", &fwd_args);

            self.restore_nat_ports_from_kernel();

            debug!(mesh_cidr = mesh_cidr, iface = %self.internet_iface, "iptables MASQUERADE removed");
        }

        #[cfg(target_os = "macos")]
        {
            self.teardown_pf_anchor();
        }

        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = mesh_cidr;
        }
    }

    /// Save the kernel's current `ip_local_reserved_ports` and extend it to
    /// include our NAT pool. Idempotent: re-entry is a no-op once set.
    #[cfg(target_os = "linux")]
    fn reserve_nat_ports_from_kernel(&self) {
        const PATH: &str = "/proc/sys/net/ipv4/ip_local_reserved_ports";
        let mut backup = match self.reserved_ports_backup.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if backup.is_some() {
            return; // already applied
        }
        let current = std::fs::read_to_string(PATH)
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let our_range = format!("{PORT_MIN}-{PORT_MAX}");
        let new_value = if current.is_empty() {
            our_range.clone()
        } else if current.split(',').any(|e| e.trim() == our_range) {
            current.clone() // already contains our range verbatim
        } else {
            format!("{current},{our_range}")
        };
        if new_value != current {
            if let Err(e) = run_cmd(
                "sysctl",
                &[
                    "-w",
                    &format!("net.ipv4.ip_local_reserved_ports={new_value}"),
                ],
            ) {
                tracing::warn!(
                    "failed to reserve NAT ports from kernel ephemeral range (ignoring): {e}"
                );
                return;
            }
            debug!(reserved = %new_value, "kernel ephemeral allocator reserved NAT pool");
        }
        *backup = Some(current);
    }

    /// Restore the saved `ip_local_reserved_ports` value, if any.
    #[cfg(target_os = "linux")]
    fn restore_nat_ports_from_kernel(&self) {
        let mut backup = match self.reserved_ports_backup.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let Some(orig) = backup.take() else {
            return;
        };
        // `sysctl -w key=` with an empty RHS is accepted and clears the value.
        if let Err(e) = run_cmd(
            "sysctl",
            &["-w", &format!("net.ipv4.ip_local_reserved_ports={orig}")],
        ) {
            tracing::warn!("failed to restore ip_local_reserved_ports (ignoring): {e}");
        }
    }

    #[cfg(target_os = "macos")]
    fn setup_pf_anchor(&self, mesh_cidr: &str) -> Result<(), GatewayError> {
        const PF_ANCHOR: &str = "com.apple/pim.gateway";

        if let Err(e) = run_cmd("sysctl", &["-w", "net.inet.ip.forwarding=1"]) {
            tracing::warn!("sysctl failed (ignoring): {e}");
        }

        // Enabling PF is idempotent; if it is already active this still succeeds.
        run_cmd("pfctl", &["-E"])?;

        let rules = format!(
            "# PIM userspace NAT on mesh subnet {mesh_cidr}\n\
             block drop in quick on {iface} inet proto {{ tcp udp }} from any to ({iface}) port {port_min}:{port_max}\n",
            iface = self.internet_iface,
            port_min = PORT_MIN,
            port_max = PORT_MAX,
        );
        run_cmd_with_stdin("pfctl", &["-a", PF_ANCHOR, "-f", "-"], &rules)?;

        debug!(mesh_cidr = mesh_cidr, iface = %self.internet_iface, anchor = PF_ANCHOR, "pf gateway rules configured");
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn teardown_pf_anchor(&self) {
        const PF_ANCHOR: &str = "com.apple/pim.gateway";
        // Flush the anchor's rules. Missing anchors are not an error for pfctl -F.
        if let Err(e) = run_cmd("pfctl", &["-a", PF_ANCHOR, "-F", "all"]) {
            tracing::warn!(
                anchor = PF_ANCHOR,
                "pfctl anchor flush failed (ignoring): {e}"
            );
        } else {
            debug!(anchor = PF_ANCHOR, "pf gateway rules flushed");
        }
    }
}

mod firewall;
mod packet;
#[cfg(test)]
pub mod test_util;
#[cfg(test)]
mod tests;

use firewall::input_drop_args;
#[cfg(target_os = "macos")]
use firewall::run_cmd_with_stdin;
pub(crate) use firewall::{check_cmd_quiet, iptables_delete_if_present, run_cmd};
use packet::{ip_protocol, parse_flow, rewrite_dst, rewrite_src, transport_dst_port};
