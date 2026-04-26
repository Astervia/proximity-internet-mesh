//! IPv4 packet parsing, rewriting, and checksum helpers.

use std::net::Ipv4Addr;

use super::{GatewayError, PROTO_ICMP, PROTO_TCP, PROTO_UDP};

// ── Packet parsing helpers ────────────────────────────────────────────────────

/// Minimum IPv4 header size (no options).
pub(super) const IP_HEADER_MIN: usize = 20;

pub(super) fn ip_ihl(packet: &[u8]) -> usize {
    ((packet[0] & 0x0f) as usize) * 4
}

pub(super) fn ip_protocol(packet: &[u8]) -> Result<u8, GatewayError> {
    if packet.len() < IP_HEADER_MIN {
        return Err(GatewayError::PacketTooShort(packet.len()));
    }
    if (packet[0] >> 4) != 4 {
        return Err(GatewayError::NotIpv4);
    }
    Ok(packet[9])
}

/// Extract (protocol, src_ip, src_port) from an outbound packet.
pub(super) fn parse_flow(packet: &[u8]) -> Result<(u8, Ipv4Addr, u16), GatewayError> {
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
pub(super) fn transport_dst_port(packet: &[u8], proto: u8) -> Result<u16, GatewayError> {
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
pub(super) fn rewrite_src(
    packet: &mut [u8],
    new_src: Ipv4Addr,
    new_port: u16,
) -> Result<(), GatewayError> {
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
pub(super) fn rewrite_dst(
    packet: &mut [u8],
    new_dst: Ipv4Addr,
    new_port: u16,
) -> Result<(), GatewayError> {
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
pub(super) fn ones_complement_sum(data: &[u8]) -> u32 {
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

pub(super) fn fold_checksum(mut sum: u32) -> u16 {
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

/// Recalculate and overwrite the IP header checksum.
pub(super) fn recalc_ip_checksum(packet: &mut [u8]) {
    let ihl = ip_ihl(packet);
    packet[10] = 0;
    packet[11] = 0;
    let sum = fold_checksum(ones_complement_sum(&packet[..ihl]));
    packet[10..12].copy_from_slice(&sum.to_be_bytes());
}

/// Incrementally update the transport-layer checksum after an IP address or
/// port rewrite, using the RFC 1624 incremental update formula.
pub(super) fn recalc_transport_checksum(
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
