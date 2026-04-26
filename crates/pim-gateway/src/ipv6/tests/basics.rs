use super::super::*;

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
