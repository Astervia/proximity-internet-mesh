//! Routed control-frame send + reconnect-cancellation helpers.
//!
//! This module used to host the dynamic mesh-IP allocation protocol
//! (`IpRequest` / `IpAssign`, gateway pool round-trips). All of that
//! is gone now that mesh addresses are derived deterministically from
//! each node's `NodeId` (see `pim_core::derive_mesh_ipv4`). What
//! remains is the small surface that other daemon modules still rely
//! on: a routed control-frame sender (used by the identity broadcast
//! task and by the plugin host) plus the `pending_outbound`
//! cancellation bookkeeping that protects the reconnect path against
//! double-dial races.

use std::net::IpAddr;
use std::sync::Arc;

use bytes::BytesMut;
use pim_core::{FrameCodec, NodeId};
use pim_protocol::{ControlFrame, DataFlags};
use pim_transport::Transport;
use tracing::warn;

use super::data_plane::send_single_mesh;
use super::reconnect::ConnectTarget;
use super::DaemonState;

#[derive(Debug, Clone, Copy)]
pub(crate) struct PendingOutbound {
    pub(crate) transport_key: NodeId,
    pub(crate) target: ConnectTarget,
}

pub(crate) async fn register_pending_outbound(
    state: &Arc<DaemonState>,
    target: ConnectTarget,
    transport_key: NodeId,
) {
    state.pending_outbound.lock().await.insert(
        target.addr().ip(),
        PendingOutbound {
            transport_key,
            target,
        },
    );
}

pub(crate) async fn clear_pending_outbound(
    state: &Arc<DaemonState>,
    target: ConnectTarget,
    transport_key: NodeId,
) {
    let mut pending = state.pending_outbound.lock().await;
    if pending
        .get(&target.addr().ip())
        .is_some_and(|entry| entry.transport_key == transport_key)
    {
        pending.remove(&target.addr().ip());
    }
}

fn encode_control_frame(cf: ControlFrame) -> bytes::Bytes {
    let mut buf = BytesMut::new();
    cf.encode(&mut buf);
    buf.freeze()
}

pub(crate) async fn send_routed_control_via(
    state: &Arc<DaemonState>,
    next_hop: NodeId,
    dst_id: NodeId,
    cf: ControlFrame,
) -> bool {
    let session = state.sessions.read().await.get(&next_hop).cloned();
    let Some(session) = session else {
        warn!(%next_hop, %dst_id, "no session for routed control next hop");
        return false;
    };
    let payload = encode_control_frame(cf);
    send_single_mesh(
        state,
        &session,
        state.self_id,
        dst_id,
        8,
        DataFlags::IS_CONTROL,
        payload,
    )
    .await;
    true
}

pub(crate) async fn send_routed_control(
    state: &Arc<DaemonState>,
    dst_id: NodeId,
    cf: ControlFrame,
) -> bool {
    let next_hop = state.routing.lock().await.lookup(dst_id);
    let Some(next_hop) = next_hop else {
        warn!(%dst_id, "no route for routed control");
        return false;
    };
    send_routed_control_via(state, next_hop, dst_id, cf).await
}

pub(crate) async fn cancel_pending_outbound_for_ip(
    state: &Arc<DaemonState>,
    remote_ip: IpAddr,
) -> Option<PendingOutbound> {
    let entry = state.pending_outbound.lock().await.remove(&remote_ip);
    let entry = entry?;

    state
        .cancelled_outbounds
        .lock()
        .await
        .insert(entry.transport_key);
    state.hs_channels.lock().await.remove(&entry.transport_key);
    state.transport.disconnect(&entry.transport_key).await.ok();
    state.reconnect.end_reconnect(entry.target).await;
    Some(entry)
}

pub(crate) async fn take_cancelled_outbound(
    state: &Arc<DaemonState>,
    transport_key: NodeId,
) -> bool {
    state
        .cancelled_outbounds
        .lock()
        .await
        .remove(&transport_key)
}
