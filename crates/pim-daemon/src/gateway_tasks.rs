use std::sync::Arc;
use std::time::{Duration, Instant};

use pim_core::NodeId;
use pim_gateway::GatewayEngineV6;
use pim_protocol::{ControlFrame, DataFlags};
use tracing::{debug, error, info, warn};

use super::data_plane::send_mesh_data;
use super::net::{find_any_ipv6_uplink, lookup_interface_ipv6, packet_ip_version};
use super::peer_tasks::send_control;
use super::DaemonState;

/// Maximum age of a pending ping before it is discarded as lost.
pub(crate) const PENDING_PING_TTL: Duration = Duration::from_secs(30);

/// Periodically send Ping frames to each directly-connected gateway peer to
/// measure round-trip latency. The matching Pong handler in the event loop
/// calls `update_gateway_rtt` to update the routing table.
///
/// Only direct-peer gateways are probed because `ControlFrame::Ping` is an
/// unrouted transport-layer frame (it is not wrapped inside a `MeshDataFrame`).
pub(crate) async fn run_gateway_probes(state: Arc<DaemonState>) {
    let mut interval = tokio::time::interval(Duration::from_secs(10));
    loop {
        interval.tick().await;

        // Collect directly-connected gateways (hold lock briefly).
        let direct_gateways: Vec<NodeId> = {
            let rt = state.routing.lock().await;
            let direct = rt.direct_peers().clone();
            rt.all_gateways()
                .into_iter()
                .map(|(id, _)| id)
                .filter(|id| direct.contains(id))
                .collect()
        };

        {
            let mut pings = state.pending_pings.lock().await;
            pings.retain(|_, (_, sent_at)| sent_at.elapsed() < PENDING_PING_TTL);
        }

        for gw_id in direct_gateways {
            let nonce: u64 = rand::random();
            state
                .pending_pings
                .lock()
                .await
                .insert(nonce, (gw_id, Instant::now()));
            send_control(&state, &gw_id, ControlFrame::Ping { nonce }).await;
            debug!(%gw_id, nonce, "sent gateway probe Ping");
        }
    }
}

pub(crate) async fn ensure_gateway_ipv6_engine(
    state: &Arc<DaemonState>,
) -> Option<Arc<GatewayEngineV6>> {
    if let Some(gw) = state.gw_engine_v6.read().await.clone() {
        return Some(gw);
    }

    let configured = state.gateway_nat_interface.as_deref()?;
    let (iface, external_ip) = match lookup_interface_ipv6(configured) {
        Ok(ip) => (configured.to_string(), ip),
        Err(e) => {
            // Docker Compose can map the uplink to a different eth index than
            // the config expects, so fall back to scanning other interfaces.
            match find_any_ipv6_uplink(&[configured, "pim0", "lo"]) {
                Some((iface, ip)) => {
                    info!(
                        configured = %configured,
                        detected = %iface,
                        "configured nat_interface has no IPv6; using auto-detected uplink"
                    );
                    (iface, ip)
                }
                None => {
                    debug!(iface = %configured, "IPv6 gateway uplink still unavailable: {e}");
                    return None;
                }
            }
        }
    };

    let gw = Arc::new(GatewayEngineV6::new(external_ip, &iface));
    if let Err(e) = gw.setup_masquerade() {
        warn!("ip6tables setup failed (may need root): {e}");
    }

    let mut slot = state.gw_engine_v6.write().await;
    if let Some(existing) = slot.as_ref() {
        return Some(existing.clone());
    }
    info!(iface = %iface, external_ip = %external_ip, "IPv6 gateway uplink became available");
    *slot = Some(gw.clone());
    Some(gw)
}

/// Gateway task: drain TUN (internet -> mesh), NAT inbound, send back to originators.
pub(crate) async fn run_gateway_return(state: Arc<DaemonState>) {
    if !state.is_gateway {
        return;
    }
    let Some(link) = state.internet_link.as_ref().cloned() else {
        return;
    };
    let gw = state.gw_engine.as_ref().cloned();
    let gw_v6 = state.gw_engine_v6.read().await.clone();
    if gw.is_none() && gw_v6.is_none() {
        return;
    }
    let mut buf = vec![0u8; 65536];

    loop {
        tokio::select! {
            res = link.recv_packet(&mut buf) => {
                let n = match res {
                    Ok(n) => n,
                    Err(e) => {
                        error!("gateway internet recv: {e:#}");
                        break;
                    }
                };
                let mut pkt = buf[..n].to_vec();
                let Some(version) = packet_ip_version(&pkt) else {
                    continue;
                };

                if version == 4 {
                    let Some(gw) = gw.as_ref() else {
                        continue;
                    };
                    if pkt.len() < 20 {
                        continue;
                    }
                    let dest_ip = match gw.translate_inbound(&mut pkt).await {
                        Ok(dest_ip) => dest_ip,
                        Err(_) => continue,
                    };

                    if let Some((dst_id, next_hop)) = state.routing.lock().await.lookup_mesh_ip(dest_ip) {
                        let session = state.sessions.read().await.get(&next_hop).cloned();
                        if let Some(session) = session {
                            send_mesh_data(&state, &session, state.self_id, dst_id, 8, DataFlags::IS_INTERNET, bytes::Bytes::from(pkt)).await;
                        }
                    }
                } else if version == 6 {
                    let Some(gw_v6) = gw_v6.as_ref() else {
                        continue;
                    };
                    let dst_id = match gw_v6.translate_inbound(&mut pkt).await {
                        Ok(dst_id) => dst_id,
                        Err(_) => continue,
                    };
                    if let Some(next_hop) = state.routing.lock().await.lookup(dst_id) {
                        let session = state.sessions.read().await.get(&next_hop).cloned();
                        if let Some(session) = session {
                            send_mesh_data(&state, &session, state.self_id, dst_id, 8, DataFlags::IS_INTERNET, bytes::Bytes::from(pkt)).await;
                        }
                    }
                }
            }
            _ = state.cancel.cancelled() => break,
        }
    }
}
