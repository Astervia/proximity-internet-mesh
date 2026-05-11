use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use pim_core::{
    DebugDiscoveredPeerSnapshot, DebugGatewaySnapshot, DebugNodeSnapshot, DebugPeerSnapshot,
    DebugRouteSnapshot, DebugSnapshot, DebugStatsSnapshot, NodeId,
};
use pim_routing::gateway_score;
use tracing::debug;

use super::fs_util::atomic_write;
use super::runtime_paths::{debug_snapshot_path, stats_path};
use super::DaemonState;

pub(crate) struct StatsSnapshot {
    pub(crate) peers: usize,
    pub(crate) routes: usize,
    pub(crate) packets_forwarded: u64,
    pub(crate) bytes_forwarded: u64,
    pub(crate) packets_dropped: u64,
    pub(crate) congestion_drops: u64,
    pub(crate) conntrack_size: usize,
    pub(crate) uptime_secs: u64,
}

async fn collect_stats(state: &Arc<DaemonState>) -> StatsSnapshot {
    let peers = state.sessions.read().await.len();
    let routes = state.routing.lock().await.route_count();
    let packets_forwarded = state.packets_forwarded.load(Ordering::Relaxed);
    let bytes_forwarded = state.bytes_forwarded.load(Ordering::Relaxed);
    let packets_dropped = state.packets_dropped.load(Ordering::Relaxed);
    let congestion_drops = state.congestion_drops.load(Ordering::Relaxed);
    let conntrack_size = match &state.gw_engine {
        Some(gw) => gw.conntrack_size().await,
        None => 0,
    };
    let uptime_secs = state.start_time.elapsed().unwrap_or_default().as_secs();

    StatsSnapshot {
        peers,
        routes,
        packets_forwarded,
        bytes_forwarded,
        packets_dropped,
        congestion_drops,
        conntrack_size,
        uptime_secs,
    }
}

/// Collect current metrics and format them as a newline-delimited key=value string.
pub(crate) fn format_stats(stats: &StatsSnapshot) -> String {
    format!(
        "peers={peers}\n\
         routes={routes}\n\
         packets_forwarded={packets_forwarded}\n\
         bytes_forwarded={bytes_forwarded}\n\
         packets_dropped={packets_dropped}\n\
         congestion_drops={congestion_drops}\n\
         conntrack_size={conntrack_size}\n\
         uptime_secs={uptime_secs}\n",
        peers = stats.peers,
        routes = stats.routes,
        packets_forwarded = stats.packets_forwarded,
        bytes_forwarded = stats.bytes_forwarded,
        packets_dropped = stats.packets_dropped,
        congestion_drops = stats.congestion_drops,
        conntrack_size = stats.conntrack_size,
        uptime_secs = stats.uptime_secs,
    )
}

pub(crate) async fn run_stats_writer(state: Arc<DaemonState>) {
    let path = stats_path();
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    loop {
        interval.tick().await;
        let stats = collect_stats(&state).await;
        let content = format_stats(&stats);
        if let Err(e) = atomic_write(&path, content.as_bytes()).await {
            debug!("stats write failed ({}): {e}", path.display());
        }
    }
}

async fn build_debug_snapshot(state: &Arc<DaemonState>) -> DebugSnapshot {
    let stats = collect_stats(state).await;
    let sessions: Vec<NodeId> = state.sessions.read().await.keys().copied().collect();
    let peer_last_hb = state.peer_last_hb.lock().await.clone();
    let direct_peers = {
        let routing = state.routing.lock().await;
        routing.direct_peers().clone()
    };

    let mut peers = Vec::with_capacity(sessions.len());
    for peer_id in sessions {
        let info = state.reconnect.peer_info(&peer_id).await;
        let (addr, mechanism, configured, discovered) = match info {
            Some((target, configured, discovered)) => (
                Some(target.addr().to_string()),
                Some(target.mechanism_name().to_string()),
                configured,
                discovered,
            ),
            None => (None, None, false, false),
        };
        peers.push(DebugPeerSnapshot {
            node_id: peer_id.to_hex(),
            short_id: peer_id.to_string(),
            addr,
            mechanism,
            direct: direct_peers.contains(&peer_id),
            configured,
            discovered,
            last_heartbeat_age_ms: peer_last_hb
                .get(&peer_id)
                .map(|seen| seen.elapsed().as_millis().min(u64::MAX as u128) as u64),
        });
    }
    peers.sort_by(|a, b| a.short_id.cmp(&b.short_id));

    let routing = state.routing.lock().await;
    let route_entries = routing.routes_snapshot();
    let selected_gateway = routing.nearest_gateway_route().map(|(gw_id, _)| gw_id);
    let mut routes = Vec::with_capacity(route_entries.len());
    let mut gateways = Vec::new();
    for (dst, entry) in route_entries {
        let mesh_ip = entry.mesh_ip.map(|ip| ip.to_string());
        let age_ms = entry.last_seen.elapsed().as_millis().min(u64::MAX as u128) as u64;
        let next_hop_blacklisted = routing.is_blacklisted(&entry.next_hop);
        routes.push(DebugRouteSnapshot {
            destination_id: dst.to_hex(),
            destination_short_id: dst.to_string(),
            next_hop_id: entry.next_hop.to_hex(),
            next_hop_short_id: entry.next_hop.to_string(),
            learned_from_id: entry.learned_from.to_hex(),
            learned_from_short_id: entry.learned_from.to_string(),
            hops: entry.hops,
            is_gateway: entry.is_gateway,
            gateway_load: entry.gateway_load,
            rtt_ms: entry.rtt_ms,
            mesh_ip: mesh_ip.clone(),
            age_ms,
            next_hop_blacklisted,
        });

        if entry.is_gateway {
            gateways.push(DebugGatewaySnapshot {
                node_id: dst.to_hex(),
                short_id: dst.to_string(),
                next_hop_id: entry.next_hop.to_hex(),
                next_hop_short_id: entry.next_hop.to_string(),
                hops: entry.hops,
                gateway_load: entry.gateway_load,
                rtt_ms: entry.rtt_ms,
                score: gateway_score(entry.hops, entry.gateway_load, entry.rtt_ms),
                selected: selected_gateway == Some(dst),
                mesh_ip,
            });
        }
    }
    drop(routing);

    routes.sort_by(|a, b| a.destination_short_id.cmp(&b.destination_short_id));
    gateways.sort_by_key(|gateway| {
        (
            u8::from(!gateway.selected),
            gateway.score,
            gateway.short_id.clone(),
        )
    });

    let discovered_peers = match &state.discovery_peer_table {
        Some(peer_table) => {
            let peer_table = peer_table.lock().await;
            let mut peers: Vec<DebugDiscoveredPeerSnapshot> = peer_table
                .all()
                .map(|record| DebugDiscoveredPeerSnapshot {
                    node_id: record.node_id.to_hex(),
                    short_id: record.node_id.to_string(),
                    addr: record.listen_addr.to_string(),
                    is_client: record.capabilities.is_client(),
                    is_relay: record.capabilities.is_relay(),
                    is_gateway: record.capabilities.is_gateway(),
                    last_seen_age_ms: record.last_seen.elapsed().as_millis().min(u64::MAX as u128)
                        as u64,
                })
                .collect();
            peers.sort_by(|a, b| a.short_id.cmp(&b.short_id));
            peers
        }
        None => Vec::new(),
    };

    DebugSnapshot {
        version: 1,
        generated_at_unix_secs: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        node: DebugNodeSnapshot {
            name: state.node_name.clone(),
            node_id: state.self_id.to_hex(),
            short_id: state.self_id.to_string(),
            is_gateway: state.is_gateway,
            mesh_ip: state.mesh_ipv4.to_string(),
            mesh_prefix_len: state.mesh_ipv4_prefix.prefix_len,
        },
        stats: DebugStatsSnapshot {
            peers: stats.peers,
            routes: stats.routes,
            packets_forwarded: stats.packets_forwarded,
            bytes_forwarded: stats.bytes_forwarded,
            packets_dropped: stats.packets_dropped,
            congestion_drops: stats.congestion_drops,
            conntrack_size: stats.conntrack_size,
            uptime_secs: stats.uptime_secs,
        },
        peers,
        discovered_peers,
        routes,
        gateways,
    }
}

pub(crate) async fn run_debug_snapshot_writer(state: Arc<DaemonState>) {
    let path = debug_snapshot_path();
    let mut interval = tokio::time::interval(Duration::from_secs(2));
    loop {
        interval.tick().await;
        match serde_json::to_vec_pretty(&build_debug_snapshot(&state).await) {
            Ok(content) => {
                if let Err(e) = atomic_write(&path, &content).await {
                    debug!("debug snapshot write failed ({}): {e}", path.display());
                }
            }
            Err(e) => debug!("debug snapshot serialize failed: {e}"),
        }
    }
}
