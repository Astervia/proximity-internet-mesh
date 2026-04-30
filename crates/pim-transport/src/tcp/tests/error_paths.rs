use super::super::*;
use super::make_frame;
use pim_protocol::FrameType;

#[tokio::test]
async fn send_to_disconnected_peer_fails() {
    let node_a = NodeId::from_bytes([0x01; 16]);
    let unknown = NodeId::from_bytes([0xFF; 16]);

    let transport = TcpTransport::new("127.0.0.1:0".parse().unwrap(), node_a)
        .await
        .unwrap();

    let result = transport.send(&unknown, make_frame(b"hello")).await;
    assert!(matches!(result, Err(TransportError::PeerNotConnected(_))));
}

#[tokio::test]
async fn send_returns_congested_when_write_queue_full() {
    // Verify that try_send is used and TransportError::Congested is returned
    // when the per-peer write channel is full.
    //
    // Strategy: create a transport pair, then flood the write side fast
    // enough that the write task can't keep up.  We use a small write-
    // channel capacity (1) via a saturating loop with a stall on the far
    // end (stop consuming incoming frames on the receiver transport so the
    // TCP receive buffer fills and backs up the write task).

    let node_a = NodeId::from_bytes([0x01; 16]);
    let node_b = NodeId::from_bytes([0x02; 16]);

    let transport_a = TcpTransport::new("127.0.0.1:0".parse().unwrap(), node_a)
        .await
        .unwrap();
    let addr_a = transport_a.listen_addr;

    let transport_b = TcpTransport::new("127.0.0.1:0".parse().unwrap(), node_b)
        .await
        .unwrap();

    transport_b
        .connect(&PeerAddress {
            node_id: node_a,
            addr: addr_a,
        })
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Attempt to send a large number of frames very quickly without
    // consuming on transport_a.  Eventually the write channel (cap 64)
    // will fill and try_send will return Full → Congested.
    let payload = vec![0u8; 1024];
    let mut got_congested = false;
    for _ in 0..512 {
        let f = TransportFrame {
            frame_type: FrameType::Data,
            nonce: [0; 12],
            payload: bytes::Bytes::from(payload.clone()),
            tag: [0; 16],
        };
        match transport_b.send(&node_a, f).await {
            Ok(()) => {}
            Err(crate::TransportError::Congested(_)) => {
                got_congested = true;
                break;
            }
            Err(e) => panic!("unexpected error: {e}"),
        }
    }
    assert!(
        got_congested,
        "should hit Congested after saturating write queue"
    );
}
