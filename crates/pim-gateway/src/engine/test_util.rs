//! Test packet builders and helpers shared across gateway unit tests.

use std::net::Ipv4Addr;

use super::packet::{fold_checksum, ip_ihl, ones_complement_sum, recalc_ip_checksum};
use super::{PROTO_ICMP, PROTO_TCP, PROTO_UDP};

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
