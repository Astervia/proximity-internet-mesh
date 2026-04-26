use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use bytes::BytesMut;
use pim_core::{FrameCodec, NodeId};
use pim_protocol::{ControlFrame, DataFlags};
use pim_transport::Transport;
use tracing::{debug, info, warn};

use super::data_plane::send_single_mesh;
use super::peer_tasks::send_control;
use super::reconnect::ConnectTarget;
use super::DaemonState;

#[derive(Debug, Clone, Copy)]
pub(crate) struct PendingOutbound {
    pub(crate) transport_key: NodeId,
    pub(crate) target: ConnectTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IpRequestDisposition {
    Process,
    DuplicateInFlight,
    SpoofedRequester,
}

pub(crate) fn classify_ip_request(
    pending: &mut HashSet<NodeId>,
    requester_id: NodeId,
    from_peer: NodeId,
) -> IpRequestDisposition {
    if requester_id != from_peer {
        return IpRequestDisposition::SpoofedRequester;
    }
    if !pending.insert(requester_id) {
        return IpRequestDisposition::DuplicateInFlight;
    }
    IpRequestDisposition::Process
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

fn has_dynamic_mesh_ip(state: &DaemonState) -> bool {
    state.mesh_ip.load(Ordering::Relaxed) != 0
}

fn encode_control_frame(cf: ControlFrame) -> Vec<u8> {
    let mut buf = BytesMut::new();
    cf.encode(&mut buf);
    buf.to_vec()
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
        &payload,
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

pub(crate) async fn maybe_request_dynamic_ip(state: &Arc<DaemonState>) {
    if !state.request_dynamic_ip || has_dynamic_mesh_ip(state) {
        return;
    }

    let Some((gateway_id, next_hop)) = state.routing.lock().await.nearest_gateway_route() else {
        return;
    };

    {
        let pending = state.pending_dynamic_ip_gateway.lock().await;
        if *pending == Some(gateway_id) {
            return;
        }
    }

    if send_routed_control_via(
        state,
        next_hop,
        gateway_id,
        ControlFrame::IpRequest {
            requester_id: state.self_id,
        },
    )
    .await
    {
        *state.pending_dynamic_ip_gateway.lock().await = Some(gateway_id);
        debug!(%gateway_id, via = %next_hop, "sent routed IpRequest");
    }
}

pub(crate) async fn request_dynamic_ip_from_peer(state: &Arc<DaemonState>, peer_id: NodeId) {
    if !state.request_dynamic_ip || has_dynamic_mesh_ip(state) {
        return;
    }

    let is_direct_gateway = {
        let rt = state.routing.lock().await;
        rt.lookup(peer_id) == Some(peer_id)
            && rt
                .all_gateways()
                .into_iter()
                .any(|(gateway_id, hops)| gateway_id == peer_id && hops == 1)
    };

    if is_direct_gateway {
        send_control(
            state,
            &peer_id,
            ControlFrame::IpRequest {
                requester_id: state.self_id,
            },
        )
        .await;
        *state.pending_dynamic_ip_gateway.lock().await = Some(peer_id);
        debug!(%peer_id, "sent direct IpRequest");
        return;
    }

    maybe_request_dynamic_ip(state).await;
}

pub(crate) async fn apply_dynamic_ip_assignment(
    state: &Arc<DaemonState>,
    assigned_ip: [u8; 4],
    subnet_mask: u8,
    gateway_ip: [u8; 4],
) {
    let ip = Ipv4Addr::from(assigned_ip);
    let gw = Ipv4Addr::from(gateway_ip);
    info!(%ip, prefix = subnet_mask, %gw, "received IP assignment");
    if let Err(e) = state.tun.set_ip(ip, subnet_mask) {
        warn!("TUN set_ip failed: {e}");
    }
    state.mesh_ip.store(u32::from(ip), Ordering::Relaxed);
    state.mesh_prefix_len.store(subnet_mask, Ordering::Relaxed);
    state.routing.lock().await.set_self_mesh_ip(ip);
    *state.pending_dynamic_ip_gateway.lock().await = None;
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
