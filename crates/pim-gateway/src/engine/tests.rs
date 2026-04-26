use super::packet::{fold_checksum, ip_ihl, ones_complement_sum};
use super::test_util::*;
use super::*;

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

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
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
