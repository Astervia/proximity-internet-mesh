use super::super::*;

fn node(b: u8) -> NodeId {
    NodeId::from_bytes([b; 16])
}

fn mk_frame(ft: FrameType) -> TransportFrame {
    TransportFrame {
        frame_type: ft,
        nonce: [0; 12],
        payload: bytes::Bytes::new(),
        tag: [0; 16],
    }
}

fn ctrl() -> TransportFrame {
    mk_frame(FrameType::Control)
}
fn route() -> TransportFrame {
    mk_frame(FrameType::RouteUpdate)
}
fn data() -> TransportFrame {
    mk_frame(FrameType::Data)
}

// ── Priority ──────────────────────────────────────────────────────────────

#[test]
fn priority_ordering_correct() {
    assert!(Priority::Control < Priority::Route);
    assert!(Priority::Route < Priority::Data);
    assert!(Priority::Control < Priority::Data);
}

#[test]
fn priority_classified_by_frame_type() {
    assert_eq!(Priority::of(FrameType::Control), Priority::Control);
    assert_eq!(Priority::of(FrameType::Handshake), Priority::Control);
    assert_eq!(Priority::of(FrameType::RouteUpdate), Priority::Route);
    assert_eq!(Priority::of(FrameType::Data), Priority::Data);
    assert_eq!(Priority::of(FrameType::Heartbeat), Priority::Data);
}

// ── PeerBuffer ────────────────────────────────────────────────────────────

#[test]
fn peer_buffer_drain_returns_highest_priority_first() {
    let mut buf = PeerBuffer::new(16, Duration::from_secs(60));
    buf.push(Priority::Data, data());
    buf.push(Priority::Route, route());
    buf.push(Priority::Control, ctrl());
    let frames = buf.drain();
    assert_eq!(frames.len(), 3);
    assert_eq!(frames[0].frame_type, FrameType::Control);
    assert_eq!(frames[1].frame_type, FrameType::RouteUpdate);
    assert_eq!(frames[2].frame_type, FrameType::Data);
}

#[test]
fn peer_buffer_same_priority_is_fifo() {
    let mut buf = PeerBuffer::new(16, Duration::from_secs(60));
    // Use payload to distinguish frames
    let mut f1 = data();
    f1.payload = bytes::Bytes::from(vec![1]);
    let mut f2 = data();
    f2.payload = bytes::Bytes::from(vec![2]);
    buf.push(Priority::Data, f1);
    buf.push(Priority::Data, f2);
    let frames = buf.drain();
    assert_eq!(frames[0].payload, bytes::Bytes::from(vec![1]));
    assert_eq!(frames[1].payload, bytes::Bytes::from(vec![2]));
}

#[test]
fn peer_buffer_overflow_drops_incoming_lower_priority() {
    let mut buf = PeerBuffer::new(2, Duration::from_secs(60));
    buf.push(Priority::Control, ctrl());
    buf.push(Priority::Route, route());
    // At capacity; incoming Data is lower priority than both → dropped
    buf.push(Priority::Data, data());
    assert_eq!(buf.len(), 2, "buffer should stay at capacity");
    let frames = buf.drain();
    assert!(frames.iter().all(|f| f.frame_type != FrameType::Data));
}

#[test]
fn peer_buffer_overflow_evicts_worst_for_higher_priority_incoming() {
    let mut buf = PeerBuffer::new(2, Duration::from_secs(60));
    buf.push(Priority::Route, route());
    buf.push(Priority::Data, data());
    // At capacity; incoming Control evicts Data (lowest priority)
    buf.push(Priority::Control, ctrl());
    assert_eq!(buf.len(), 2);
    let frames = buf.drain();
    assert!(frames.iter().any(|f| f.frame_type == FrameType::Control));
    assert!(frames
        .iter()
        .any(|f| f.frame_type == FrameType::RouteUpdate));
    assert!(!frames.iter().any(|f| f.frame_type == FrameType::Data));
}

#[test]
fn peer_buffer_expire_removes_stale_frames() {
    let mut buf = PeerBuffer::new(16, Duration::from_millis(1));
    buf.push(Priority::Data, data());
    std::thread::sleep(Duration::from_millis(10));
    let removed = buf.expire();
    assert_eq!(removed, 1);
    assert!(buf.is_empty());
}

#[test]
fn peer_buffer_drain_filters_expired() {
    let mut buf = PeerBuffer::new(16, Duration::from_millis(1));
    buf.push(Priority::Control, ctrl());
    std::thread::sleep(Duration::from_millis(10));
    let frames = buf.drain();
    assert!(frames.is_empty(), "expired frame must not be delivered");
}

// ── SendBuffer ────────────────────────────────────────────────────────────

#[tokio::test]
async fn send_buffer_push_and_drain_priority_order() {
    let sb = SendBuffer::new(16, Duration::from_secs(60));
    let peer = node(1);
    sb.push(peer, Priority::Data, data()).await;
    sb.push(peer, Priority::Control, ctrl()).await;
    sb.push(peer, Priority::Route, route()).await;
    let frames = sb.drain(&peer).await;
    assert_eq!(frames.len(), 3);
    assert_eq!(frames[0].frame_type, FrameType::Control);
    assert_eq!(frames[1].frame_type, FrameType::RouteUpdate);
    assert_eq!(frames[2].frame_type, FrameType::Data);
}

#[tokio::test]
async fn send_buffer_drain_removes_peer_entry() {
    let sb = SendBuffer::new(16, Duration::from_secs(60));
    let peer = node(1);
    sb.push(peer, Priority::Data, data()).await;
    sb.drain(&peer).await;
    assert_eq!(
        sb.peer_count().await,
        0,
        "peer entry should be removed after drain"
    );
}

#[tokio::test]
async fn send_buffer_drain_unknown_peer_returns_empty() {
    let sb = SendBuffer::new(16, Duration::from_secs(60));
    let frames = sb.drain(&node(99)).await;
    assert!(frames.is_empty());
}

#[tokio::test]
async fn send_buffer_expire_all_clears_stale() {
    let sb = SendBuffer::new(16, Duration::from_millis(1));
    sb.push(node(1), Priority::Data, data()).await;
    sb.push(node(2), Priority::Control, ctrl()).await;
    tokio::time::sleep(Duration::from_millis(10)).await;
    let removed = sb.expire_all().await;
    assert_eq!(removed, 2);
    assert_eq!(sb.peer_count().await, 0);
}

#[tokio::test]
async fn send_buffer_multiple_peers_independent() {
    let sb = SendBuffer::new(16, Duration::from_secs(60));
    sb.push(node(1), Priority::Data, data()).await;
    sb.push(node(2), Priority::Control, ctrl()).await;
    assert_eq!(sb.frame_count(&node(1)).await, 1);
    assert_eq!(sb.frame_count(&node(2)).await, 1);
    sb.drain(&node(1)).await;
    assert_eq!(sb.frame_count(&node(1)).await, 0);
    assert_eq!(sb.frame_count(&node(2)).await, 1);
}

#[tokio::test]
async fn send_buffer_respects_capacity_per_peer() {
    let sb = SendBuffer::new(2, Duration::from_secs(60));
    let peer = node(1);
    sb.push(peer, Priority::Control, ctrl()).await;
    sb.push(peer, Priority::Route, route()).await;
    sb.push(peer, Priority::Data, data()).await; // should be dropped (lowest)
    assert_eq!(sb.frame_count(&peer).await, 2);
}
