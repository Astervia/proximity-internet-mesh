use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use bytes::BytesMut;
use pim_core::{FrameCodec, NodeId};
use pim_protocol::{
    fragment_packet, DataFlags, FragmentFrame, FrameType, HeartbeatFrame, MeshDataFrame,
    TransportFrame,
};
use pim_routing::signing::sign_route_update;
use pim_transport::Transport;
use tracing::{debug, warn};

use super::peer_tasks::send_frame_buffered;
use super::session::Session;
use super::DaemonState;

/// Send one or more MeshDataFrames (after fragmentation) to `dst_session`.
pub(crate) async fn send_mesh_data(
    state: &Arc<DaemonState>,
    session: &Arc<Session>,
    src_id: NodeId,
    dst_id: NodeId,
    ttl: u8,
    flags: DataFlags,
    payload: &[u8],
) {
    let threshold = pim_protocol::MAX_FRAGMENT_PAYLOAD.saturating_sub(40); // minus mesh header
    if payload.len() > threshold {
        let frag_id = state.next_frag_id();
        for frag in fragment_packet(payload, frag_id) {
            let frag_bytes = frag.serialize();
            let mesh_flags = flags | DataFlags::IS_FRAGMENT;
            send_single_mesh(state, session, src_id, dst_id, ttl, mesh_flags, &frag_bytes).await;
        }
    } else {
        send_single_mesh(state, session, src_id, dst_id, ttl, flags, payload).await;
    }
}

pub(crate) async fn send_single_mesh(
    state: &Arc<DaemonState>,
    session: &Arc<Session>,
    src_id: NodeId,
    dst_id: NodeId,
    ttl: u8,
    flags: DataFlags,
    payload: &[u8],
) {
    let mut mesh_buf = BytesMut::new();
    MeshDataFrame {
        src_id,
        dst_id,
        session_id: 0,
        ttl,
        flags,
        payload: payload.to_vec(),
    }
    .encode(&mut mesh_buf);

    match session.encrypt_frame(&mesh_buf) {
        Ok(frame) => send_frame_buffered(state, &session.peer_id, frame).await,
        Err(e) => warn!(%dst_id, "encrypt failed: {e}"),
    }
}

/// Reassemble a fragment or deliver a whole packet. Returns the IP packet when
/// ready, or `None` if more fragments are needed.
pub(crate) async fn reassemble_or_deliver(
    state: &Arc<DaemonState>,
    src_id: NodeId,
    flags: DataFlags,
    payload: &[u8],
) -> Option<Vec<u8>> {
    if flags.contains(DataFlags::IS_FRAGMENT) {
        let frag = FragmentFrame::deserialize(payload)?;
        let mut reassemblers = state.reassemblers.lock().await;
        let r = reassemblers.entry(src_id).or_default();
        r.insert(frag)
    } else {
        Some(payload.to_vec())
    }
}

/// Periodically expire stale reassembly buffers.
pub(crate) async fn run_reassembly_gc(state: Arc<DaemonState>) {
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    loop {
        interval.tick().await;
        let mut reassemblers = state.reassemblers.lock().await;
        for r in reassemblers.values_mut() {
            r.expire_stale();
        }
        reassemblers.retain(|_, r| r.buffer_count() > 0);
    }
}

/// Periodically send route advertisements to all connected peers.
pub(crate) async fn run_route_advertisements(state: Arc<DaemonState>) {
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    loop {
        interval.tick().await;
        let adverts = state.routing.lock().await.generate_all_advertisements();
        for (peer_id, mut update) in adverts {
            sign_route_update(&mut update, state.identity.signing_key());
            let mut buf = BytesMut::new();
            update.encode(&mut buf);
            send_frame_buffered(
                &state,
                &peer_id,
                TransportFrame {
                    frame_type: FrameType::RouteUpdate,
                    nonce: [0; 12],
                    payload: buf.to_vec(),
                    tag: [0; 16],
                },
            )
            .await;
            debug!(%peer_id, "sent route advertisement");
        }
    }
}

/// Periodically send heartbeats to all connected peers.
///
/// The `load` field is computed as the packet-forwarding rate over the last
/// heartbeat interval, normalized to 0-255 (2,000 packets/interval ~= 255).
pub(crate) async fn run_heartbeats(state: Arc<DaemonState>) {
    let mut interval = tokio::time::interval(Duration::from_secs(10));
    let mut last_fwd: u64 = 0;
    loop {
        interval.tick().await;
        let cur_fwd = state.packets_forwarded.load(Ordering::Relaxed);
        let delta = cur_fwd.saturating_sub(last_fwd);
        last_fwd = cur_fwd;
        // Normalize: >=2,000 pkts/interval -> load=255; 0 pkts -> load=0.
        let load = (delta.min(2000) * 255 / 2000) as u8;

        let peers = state.transport.connected_peers();
        let gateway_hops: u8 = if state.is_gateway {
            0
        } else {
            state
                .routing
                .lock()
                .await
                .nearest_gateway()
                .map(|(_, hops)| hops)
                .unwrap_or(0xFF)
        };
        let hb = HeartbeatFrame {
            sender_id: state.self_id,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            gateway_hops,
            load,
            gw_x25519_pub: if state.is_gateway {
                state.own_x25519_pub
            } else {
                [0u8; 32]
            },
        };
        let mut buf = BytesMut::new();
        hb.encode(&mut buf);
        let tf = TransportFrame {
            frame_type: FrameType::Heartbeat,
            nonce: [0; 12],
            payload: buf.to_vec(),
            tag: [0; 16],
        };
        for peer in &peers {
            state.transport.send(peer, tf.clone()).await.ok();
        }
    }
}
