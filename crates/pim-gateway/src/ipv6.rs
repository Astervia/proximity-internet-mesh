use std::collections::{HashMap, HashSet};
use std::net::Ipv6Addr;
use std::time::{Duration, Instant};

use pim_core::NodeId;
use tokio::sync::Mutex;
use tracing::{debug, trace};

use crate::{GatewayError, PROTO_TCP, PROTO_UDP};

/// ICMPv6 next-header value used for echo traffic through the IPv6 gateway path.
pub const PROTO_ICMPV6: u8 = 58;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ConntrackKeyV6 {
    src_id: NodeId,
    proto: u8,
    orig_src: Ipv6Addr,
    orig_port: u16,
}

#[derive(Debug, Clone)]
struct ConntrackEntryV6 {
    src_id: NodeId,
    ext_port: u16,
    last_seen: Instant,
    orig_src: Ipv6Addr,
    orig_port: u16,
    proto: u8,
}

impl ConntrackEntryV6 {
    fn idle_timeout(&self) -> Duration {
        match self.proto {
            PROTO_TCP => Duration::from_secs(300),
            PROTO_UDP => Duration::from_secs(30),
            PROTO_ICMPV6 => Duration::from_secs(10),
            _ => Duration::from_secs(30),
        }
    }

    fn is_expired(&self) -> bool {
        self.last_seen.elapsed() > self.idle_timeout()
    }
}

struct PortPoolV6 {
    next: u16,
    in_use: HashSet<u16>,
}

impl PortPoolV6 {
    fn new() -> Self {
        Self {
            next: crate::PORT_MIN,
            in_use: HashSet::new(),
        }
    }

    fn allocate(&mut self) -> Option<u16> {
        let total = (crate::PORT_MAX - crate::PORT_MIN + 1) as usize;
        for _ in 0..total {
            let port = self.next;
            self.next = if self.next >= crate::PORT_MAX {
                crate::PORT_MIN
            } else {
                self.next + 1
            };
            if self.in_use.insert(port) {
                return Some(port);
            }
        }
        None
    }

    fn release(&mut self, port: u16) {
        self.in_use.remove(&port);
    }
}

struct InnerV6 {
    forward: HashMap<ConntrackKeyV6, ConntrackEntryV6>,
    reverse: HashMap<(u8, u16), ConntrackKeyV6>,
    ports: PortPoolV6,
}

/// Userspace NAT66 engine for internet-bound IPv6 traffic.
pub struct GatewayEngineV6 {
    external_ip: Ipv6Addr,
    internet_iface: String,
    inner: Mutex<InnerV6>,
}

impl GatewayEngineV6 {
    /// Create a new IPv6 gateway engine bound to `internet_iface`.
    pub fn new(external_ip: Ipv6Addr, internet_iface: impl Into<String>) -> Self {
        Self {
            external_ip,
            internet_iface: internet_iface.into(),
            inner: Mutex::new(InnerV6 {
                forward: HashMap::new(),
                reverse: HashMap::new(),
                ports: PortPoolV6::new(),
            }),
        }
    }

    /// Rewrite an outbound IPv6 packet to the gateway's external IPv6 address.
    pub async fn translate_outbound(
        &self,
        packet: &mut [u8],
        src_id: NodeId,
    ) -> Result<u16, GatewayError> {
        let (proto, src_ip, src_port) = parse_flow_v6(packet)?;
        let key = ConntrackKeyV6 {
            src_id,
            proto,
            orig_src: src_ip,
            orig_port: src_port,
        };

        let mut inner = self.inner.lock().await;
        let ext_port = if let Some(entry) = inner.forward.get_mut(&key) {
            entry.last_seen = Instant::now();
            entry.ext_port
        } else {
            let ext_port = inner.ports.allocate().ok_or(GatewayError::ConntrackFull)?;
            let entry = ConntrackEntryV6 {
                src_id,
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

        rewrite_src_v6(packet, self.external_ip, ext_port)?;
        trace!(
            src_id = %src_id,
            orig_src = %src_ip,
            orig_port = src_port,
            ext_ip = %self.external_ip,
            ext_port = ext_port,
            proto = proto,
            "NAT66 outbound"
        );
        Ok(ext_port)
    }

    /// Rewrite an inbound IPv6 response packet back to the original mesh client.
    pub async fn translate_inbound(&self, packet: &mut [u8]) -> Result<NodeId, GatewayError> {
        let proto = ipv6_next_header(packet)?;
        let dst_port = transport_dst_port_v6(packet, proto)?;

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
        rewrite_dst_v6(packet, entry.orig_src, entry.orig_port)?;

        trace!(
            src_id = %entry.src_id,
            orig_dst = %entry.orig_src,
            orig_port = entry.orig_port,
            ext_port = dst_port,
            proto = proto,
            "NAT66 inbound"
        );
        Ok(entry.src_id)
    }

    /// Expire stale IPv6 conntrack entries and release their reserved ports.
    pub async fn cleanup_expired(&self) {
        let mut inner = self.inner.lock().await;
        let expired: Vec<ConntrackKeyV6> = inner
            .forward
            .iter()
            .filter(|(_, entry)| entry.is_expired())
            .map(|(key, _)| key.clone())
            .collect();

        for key in expired {
            if let Some(entry) = inner.forward.remove(&key) {
                inner.reverse.remove(&(entry.proto, entry.ext_port));
                inner.ports.release(entry.ext_port);
                debug!(
                    src_id = %entry.src_id,
                    proto = entry.proto,
                    orig_src = %entry.orig_src,
                    ext_port = entry.ext_port,
                    "IPv6 conntrack entry expired"
                );
            }
        }
    }

    /// Return the number of active IPv6 conntrack entries.
    pub async fn conntrack_size(&self) -> usize {
        self.inner.lock().await.forward.len()
    }

    /// Install host rules needed by the IPv6 userspace NAT path.
    pub fn setup_masquerade(&self) -> Result<(), GatewayError> {
        #[cfg(target_os = "linux")]
        {
            if let Err(e) = crate::run_cmd("sysctl", &["-w", "net.ipv6.conf.all.forwarding=1"]) {
                tracing::warn!("sysctl failed (ignoring): {e}");
            }

            for proto in ["tcp", "udp"] {
                let drop_args = input_drop_args_v6(proto, &self.internet_iface);
                let drop_check = {
                    let mut args = drop_args.to_vec();
                    args[0] = "-C";
                    args
                };
                if !crate::check_cmd_quiet("ip6tables", &drop_check)? {
                    crate::run_cmd("ip6tables", &drop_args)?;
                }
            }

            Ok(())
        }

        #[cfg(not(target_os = "linux"))]
        {
            Ok(())
        }
    }

    /// Reverse of `setup_masquerade`: removes the `ip6tables` INPUT DROP
    /// rules. Best-effort; errors are logged, not propagated. Safe to call
    /// even when setup was never run.
    pub fn teardown_masquerade(&self) {
        #[cfg(target_os = "linux")]
        {
            for proto in ["tcp", "udp"] {
                let drop_args = input_drop_args_v6(proto, &self.internet_iface);
                crate::iptables_delete_if_present("ip6tables", &drop_args);
            }
            debug!(iface = %self.internet_iface, "ip6tables INPUT DROP removed");
        }
    }
}

#[cfg(target_os = "linux")]
fn input_drop_args_v6<'a>(proto: &'a str, iface: &'a str) -> [&'a str; 10] {
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

const IPV6_HEADER_LEN: usize = 40;

fn ipv6_next_header(packet: &[u8]) -> Result<u8, GatewayError> {
    if packet.len() < IPV6_HEADER_LEN {
        return Err(GatewayError::PacketTooShort(packet.len()));
    }
    if (packet[0] >> 4) != 6 {
        return Err(GatewayError::CommandFailed("not an IPv6 packet".into()));
    }
    Ok(packet[6])
}

fn parse_flow_v6(packet: &[u8]) -> Result<(u8, Ipv6Addr, u16), GatewayError> {
    let proto = ipv6_next_header(packet)?;
    let src_ip = ipv6_addr(&packet[8..24]);
    let src_port = match proto {
        PROTO_TCP | PROTO_UDP => {
            if packet.len() < IPV6_HEADER_LEN + 4 {
                return Err(GatewayError::PacketTooShort(packet.len()));
            }
            u16::from_be_bytes([packet[40], packet[41]])
        }
        PROTO_ICMPV6 => icmpv6_id(packet)?,
        other => return Err(GatewayError::UnsupportedProtocol(other)),
    };
    Ok((proto, src_ip, src_port))
}

fn transport_dst_port_v6(packet: &[u8], proto: u8) -> Result<u16, GatewayError> {
    match proto {
        PROTO_TCP | PROTO_UDP => {
            if packet.len() < IPV6_HEADER_LEN + 4 {
                return Err(GatewayError::PacketTooShort(packet.len()));
            }
            Ok(u16::from_be_bytes([packet[42], packet[43]]))
        }
        PROTO_ICMPV6 => icmpv6_id(packet),
        other => Err(GatewayError::UnsupportedProtocol(other)),
    }
}

fn icmpv6_id(packet: &[u8]) -> Result<u16, GatewayError> {
    if packet.len() < IPV6_HEADER_LEN + 8 {
        return Err(GatewayError::PacketTooShort(packet.len()));
    }
    Ok(u16::from_be_bytes([packet[44], packet[45]]))
}

fn rewrite_src_v6(packet: &mut [u8], new_src: Ipv6Addr, new_port: u16) -> Result<(), GatewayError> {
    let proto = ipv6_next_header(packet)?;
    packet[8..24].copy_from_slice(&new_src.octets());
    rewrite_transport_src(packet, proto, new_port)?;
    recalc_transport_checksum_v6(packet, proto)?;
    Ok(())
}

fn rewrite_dst_v6(packet: &mut [u8], new_dst: Ipv6Addr, new_port: u16) -> Result<(), GatewayError> {
    let proto = ipv6_next_header(packet)?;
    packet[24..40].copy_from_slice(&new_dst.octets());
    rewrite_transport_dst(packet, proto, new_port)?;
    recalc_transport_checksum_v6(packet, proto)?;
    Ok(())
}

fn rewrite_transport_src(packet: &mut [u8], proto: u8, new_port: u16) -> Result<(), GatewayError> {
    let bytes = new_port.to_be_bytes();
    match proto {
        PROTO_TCP | PROTO_UDP => {
            if packet.len() < IPV6_HEADER_LEN + 4 {
                return Err(GatewayError::PacketTooShort(packet.len()));
            }
            packet[40..42].copy_from_slice(&bytes);
        }
        PROTO_ICMPV6 => {
            if packet.len() < IPV6_HEADER_LEN + 8 {
                return Err(GatewayError::PacketTooShort(packet.len()));
            }
            packet[44..46].copy_from_slice(&bytes);
        }
        other => return Err(GatewayError::UnsupportedProtocol(other)),
    }
    Ok(())
}

fn rewrite_transport_dst(packet: &mut [u8], proto: u8, new_port: u16) -> Result<(), GatewayError> {
    let bytes = new_port.to_be_bytes();
    match proto {
        PROTO_TCP | PROTO_UDP => {
            if packet.len() < IPV6_HEADER_LEN + 4 {
                return Err(GatewayError::PacketTooShort(packet.len()));
            }
            packet[42..44].copy_from_slice(&bytes);
        }
        PROTO_ICMPV6 => {
            if packet.len() < IPV6_HEADER_LEN + 8 {
                return Err(GatewayError::PacketTooShort(packet.len()));
            }
            packet[44..46].copy_from_slice(&bytes);
        }
        other => return Err(GatewayError::UnsupportedProtocol(other)),
    }
    Ok(())
}

fn recalc_transport_checksum_v6(packet: &mut [u8], proto: u8) -> Result<(), GatewayError> {
    if packet.len() < IPV6_HEADER_LEN {
        return Err(GatewayError::PacketTooShort(packet.len()));
    }

    let checksum_offset = match proto {
        PROTO_TCP => IPV6_HEADER_LEN + 16,
        PROTO_UDP => IPV6_HEADER_LEN + 6,
        PROTO_ICMPV6 => IPV6_HEADER_LEN + 2,
        other => return Err(GatewayError::UnsupportedProtocol(other)),
    };
    if packet.len() < checksum_offset + 2 {
        return Err(GatewayError::PacketTooShort(packet.len()));
    }

    packet[checksum_offset] = 0;
    packet[checksum_offset + 1] = 0;

    let payload_len = packet.len() - IPV6_HEADER_LEN;
    let mut pseudo = Vec::with_capacity(40 + payload_len);
    pseudo.extend_from_slice(&packet[8..24]);
    pseudo.extend_from_slice(&packet[24..40]);
    pseudo.extend_from_slice(&(payload_len as u32).to_be_bytes());
    pseudo.extend_from_slice(&[0, 0, 0, proto]);
    pseudo.extend_from_slice(&packet[IPV6_HEADER_LEN..]);
    let checksum = fold_checksum(ones_complement_sum(&pseudo));
    packet[checksum_offset..checksum_offset + 2].copy_from_slice(&checksum.to_be_bytes());
    Ok(())
}

fn ones_complement_sum(data: &[u8]) -> u32 {
    let mut sum = 0u32;
    let mut i = 0usize;
    while i + 1 < data.len() {
        sum += u16::from_be_bytes([data[i], data[i + 1]]) as u32;
        i += 2;
    }
    if i < data.len() {
        sum += (data[i] as u32) << 8;
    }
    sum
}

fn fold_checksum(mut sum: u32) -> u16 {
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

fn ipv6_addr(bytes: &[u8]) -> Ipv6Addr {
    let mut octets = [0u8; 16];
    octets.copy_from_slice(bytes);
    Ipv6Addr::from(octets)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLIENT_IP: Ipv6Addr = Ipv6Addr::new(0xfd77, 0, 0, 0, 0, 0, 0, 0x5);
    const GW_EXT_IP: Ipv6Addr = Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 0x1);
    const REMOTE_IP: Ipv6Addr = Ipv6Addr::new(0x2606, 0x4700, 0x4700, 0, 0, 0, 0, 0x1111);

    fn engine() -> GatewayEngineV6 {
        GatewayEngineV6::new(GW_EXT_IP, "eno1")
    }

    fn udp_packet_v6(
        src: Ipv6Addr,
        dst: Ipv6Addr,
        src_port: u16,
        dst_port: u16,
        payload: &[u8],
    ) -> Vec<u8> {
        let udp_len = 8 + payload.len();
        let mut pkt = vec![0u8; IPV6_HEADER_LEN + udp_len];
        pkt[0] = 0x60;
        pkt[4..6].copy_from_slice(&(udp_len as u16).to_be_bytes());
        pkt[6] = PROTO_UDP;
        pkt[7] = 64;
        pkt[8..24].copy_from_slice(&src.octets());
        pkt[24..40].copy_from_slice(&dst.octets());
        pkt[40..42].copy_from_slice(&src_port.to_be_bytes());
        pkt[42..44].copy_from_slice(&dst_port.to_be_bytes());
        pkt[44..46].copy_from_slice(&(udp_len as u16).to_be_bytes());
        pkt[48..].copy_from_slice(payload);
        recalc_transport_checksum_v6(&mut pkt, PROTO_UDP).unwrap();
        pkt
    }

    #[tokio::test]
    async fn outbound_and_inbound_round_trip() {
        let gw = engine();
        let src_id = NodeId::from_bytes([7; 16]);
        let mut outbound = udp_packet_v6(CLIENT_IP, REMOTE_IP, 50000, 53, b"query");
        let ext_port = gw.translate_outbound(&mut outbound, src_id).await.unwrap();
        assert_eq!(ipv6_addr(&outbound[8..24]), GW_EXT_IP);

        let mut inbound = udp_packet_v6(REMOTE_IP, GW_EXT_IP, 53, ext_port, b"reply");
        let dst_id = gw.translate_inbound(&mut inbound).await.unwrap();
        assert_eq!(dst_id, src_id);
        assert_eq!(ipv6_addr(&inbound[24..40]), CLIENT_IP);
    }
}
