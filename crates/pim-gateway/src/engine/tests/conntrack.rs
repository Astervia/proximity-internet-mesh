use super::super::test_util::*;
use super::super::*;

const CLIENT_IP: Ipv4Addr = Ipv4Addr::new(10, 77, 0, 5);
const GW_EXT_IP: Ipv4Addr = Ipv4Addr::new(203, 0, 113, 1);
const REMOTE_IP: Ipv4Addr = Ipv4Addr::new(8, 8, 8, 8);

fn engine() -> GatewayEngine {
    GatewayEngine::new(GW_EXT_IP, "eth0")
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
