use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use bytes::BytesMut;
use pim_core::{FrameCodec, NodeId};
use pim_protocol::{ControlFrame, FrameType, TransportFrame};
use pim_routing::signing::sign_route_update;
use pim_transport::{Transport, TransportError};
use tracing::{debug, info, warn};

use super::run_reconnect_task;
use super::send_buffer::Priority;
use super::DaemonState;

/// Returns `true` if a frame should be buffered (not silently dropped) when the
/// peer's send queue is congested. Control and route frames are buffered;
/// data frames are tail-dropped to avoid head-of-line blocking.
pub(crate) fn should_buffer_under_congestion(frame_type: FrameType) -> bool {
    Priority::of(frame_type) < Priority::Data
}

/// Send `frame` to `peer_id`.
///
/// - `PeerNotConnected` -> buffered in send buffer (flushed on reconnect).
/// - `Congested` -> control/route frames buffered for later flush; data frames
///   dropped immediately (priority-based tail drop) and `congestion_drops`
///   counter is incremented.
pub(crate) async fn send_frame_buffered(
    state: &Arc<DaemonState>,
    peer_id: &NodeId,
    frame: TransportFrame,
) {
    match state.transport.send(peer_id, frame.clone()).await {
        Ok(()) => {}
        Err(TransportError::PeerNotConnected(_)) => {
            let priority = Priority::of(frame.frame_type);
            state.send_buffer.push(*peer_id, priority, frame).await;
            debug!(%peer_id, "frame buffered (peer not connected)");
        }
        Err(TransportError::Congested(_)) => {
            if should_buffer_under_congestion(frame.frame_type) {
                let priority = Priority::of(frame.frame_type);
                state.send_buffer.push(*peer_id, priority, frame).await;
                debug!(%peer_id, "control/route frame buffered under congestion");
            } else {
                state.congestion_drops.fetch_add(1, Ordering::Relaxed);
                debug!(%peer_id, "data frame dropped under congestion");
            }
        }
        Err(e) => warn!(%peer_id, "send failed: {e}"),
    }
}

/// Send a `ControlFrame` directly (unencrypted) to `peer` over the transport.
/// Buffers the frame if the peer is temporarily unreachable.
pub(crate) async fn send_control(state: &Arc<DaemonState>, peer: &NodeId, cf: ControlFrame) {
    let mut buf = BytesMut::new();
    cf.encode(&mut buf);
    let tf = TransportFrame {
        frame_type: FrameType::Control,
        nonce: [0; 12],
        payload: buf.to_vec(),
        tag: [0; 16],
    };
    send_frame_buffered(state, peer, tf).await;
}

/// Remove a peer: disconnect transport, clean up session, routing, heartbeat map,
/// send triggered route updates, then schedule reconnect if it was a configured peer.
pub(crate) async fn remove_peer(state: &Arc<DaemonState>, peer_id: NodeId) {
    state.sessions.write().await.remove(&peer_id);
    state.peer_pubkeys.write().await.remove(&peer_id);
    state.rate_limiter.lock().await.remove_peer(&peer_id);
    state.routing.lock().await.remove_peer(peer_id);
    state.peer_last_hb.lock().await.remove(&peer_id);
    state.transport.disconnect(&peer_id).await.ok();
    info!(%peer_id, "peer removed");

    // Triggered route advertisement to remaining peers.
    let adverts = state.routing.lock().await.generate_all_advertisements();
    for (pid, mut update) in adverts {
        sign_route_update(&mut update, state.identity.signing_key());
        let mut buf = BytesMut::new();
        update.encode(&mut buf);
        send_frame_buffered(
            state,
            &pid,
            TransportFrame {
                frame_type: FrameType::RouteUpdate,
                nonce: [0; 12],
                payload: buf.to_vec(),
                tag: [0; 16],
            },
        )
        .await;
    }

    // Schedule reconnect if this was a configured or discovered peer.
    if let Some(target) = state.reconnect.is_reconnectable_target(&peer_id).await {
        if state.reconnect.begin_reconnect(target).await {
            let addr = target.addr();
            info!(%peer_id, mechanism = target.mechanism_name(), %addr, "scheduling reconnect with backoff");
            let st = state.clone();
            tokio::spawn(run_reconnect_task(st, target));
        }
    }
}

/// Background task: check heartbeat liveness, remove peers that have been
/// silent for more than 15 seconds (3 missed x 5 s interval).
pub(crate) async fn run_peer_liveness(state: Arc<DaemonState>) {
    const TIMEOUT: Duration = Duration::from_secs(15);
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    loop {
        interval.tick().await;
        let timed_out: Vec<NodeId> = {
            let hb = state.peer_last_hb.lock().await;
            hb.iter()
                .filter(|(_, last)| last.elapsed() > TIMEOUT)
                .map(|(id, _)| *id)
                .collect()
        };
        for peer_id in timed_out {
            warn!(%peer_id, "peer timed out (no heartbeat for 15s); removing");
            remove_peer(&state, peer_id).await;
            // Record liveness failure; blacklist if threshold reached.
            let newly_blacklisted = state.reputation.lock().await.record_failure(peer_id);
            if newly_blacklisted {
                warn!(%peer_id, "peer reached reputation blacklist threshold; blocking routes");
                state.routing.lock().await.blacklist_peer(peer_id);
            }
        }
    }
}

/// Drain the send buffer for `peer_id` and deliver all non-expired frames.
pub(crate) async fn flush_send_buffer(state: &Arc<DaemonState>, peer_id: NodeId) {
    let buffered = state.send_buffer.drain(&peer_id).await;
    if !buffered.is_empty() {
        info!(%peer_id, count = buffered.len(), "flushing send buffer after reconnect");
        for frame in buffered {
            state.transport.send(&peer_id, frame).await.ok();
        }
    }
}

/// Periodically flush the send buffer for all peers that currently have an
/// active session. This handles congestion recovery and transient send failures.
pub(crate) async fn run_buffer_flush(state: Arc<DaemonState>) {
    // 50 ms gives fast congestion recovery without spinning the CPU.
    let mut interval = tokio::time::interval(Duration::from_millis(50));
    loop {
        interval.tick().await;
        let connected: Vec<NodeId> = state.sessions.read().await.keys().copied().collect();
        for peer_id in connected {
            let buffered = state.send_buffer.drain(&peer_id).await;
            if buffered.is_empty() {
                continue;
            }
            debug!(%peer_id, count = buffered.len(), "periodic buffer flush");
            let mut re_queue: Vec<TransportFrame> = Vec::new();
            let mut congested = false;
            for frame in buffered {
                if congested {
                    // Don't attempt further sends this tick; re-queue remaining frames.
                    re_queue.push(frame);
                    continue;
                }
                match state.transport.send(&peer_id, frame.clone()).await {
                    Ok(()) => {}
                    Err(TransportError::Congested(_)) => {
                        congested = true;
                        re_queue.push(frame);
                    }
                    Err(TransportError::PeerNotConnected(_)) => {
                        // Session entry exists but transport connection is transiently
                        // gone (e.g. mid-reconnect); re-buffer so the frame survives.
                        re_queue.push(frame);
                    }
                    Err(e) => warn!(%peer_id, "flush send failed: {e}"),
                }
            }
            for frame in re_queue {
                let priority = Priority::of(frame.frame_type);
                state.send_buffer.push(peer_id, priority, frame).await;
            }
        }
    }
}

/// Periodically expire stale entries in the send buffer.
pub(crate) async fn run_buffer_gc(state: Arc<DaemonState>) {
    let mut interval = tokio::time::interval(Duration::from_secs(10));
    loop {
        interval.tick().await;
        let removed = state.send_buffer.expire_all().await;
        if removed > 0 {
            debug!(removed, "send buffer: expired stale frames");
        }
    }
}

pub(crate) async fn run_conntrack_gc(state: Arc<DaemonState>) {
    let mut interval = tokio::time::interval(Duration::from_secs(30));
    loop {
        interval.tick().await;
        if let Some(gw) = &state.gw_engine {
            let before = gw.conntrack_size().await;
            gw.cleanup_expired().await;
            let after = gw.conntrack_size().await;
            if before != after {
                debug!(
                    removed = before - after,
                    "conntrack GC: expired entries removed"
                );
            }
        }
        if let Some(gw) = state.gw_engine_v6.read().await.clone() {
            let before = gw.conntrack_size().await;
            gw.cleanup_expired().await;
            let after = gw.conntrack_size().await;
            if before != after {
                debug!(
                    removed = before - after,
                    "IPv6 conntrack GC: expired entries removed"
                );
            }
        }
    }
}
