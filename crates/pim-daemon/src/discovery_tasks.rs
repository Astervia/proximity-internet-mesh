use std::net::SocketAddr;
use std::sync::Arc;

use pim_core::NodeId;
use pim_discovery::PeerRecord;
use pim_transport::{PeerAddress, Transport, TransportError};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::handshake::handshake_initiator;
use super::ip_control::{
    clear_pending_outbound, register_pending_outbound, take_cancelled_outbound,
};
use super::reconnect::ConnectTarget;
use super::reconnect_task::run_reconnect_task;
use super::DaemonState;

/// Initiate an outbound TCP connection and handshake to `target`.
///
/// Uses a random temporary `NodeId` as the transport key so that concurrent
/// connect attempts don't collide in the transport map. The real `NodeId` is
/// learned from the handshake response and registered in `reconnect` on success.
pub(crate) async fn initiate_peer_connection(state: Arc<DaemonState>, target: ConnectTarget) {
    let peer_addr = target.addr();
    let transport_key = NodeId::from_bytes(rand::random::<[u8; 16]>());
    register_pending_outbound(&state, target, transport_key).await;

    let connect_result = tokio::time::timeout(
        state.outbound_connect_timeout,
        state.transport.connect(&PeerAddress {
            node_id: transport_key,
            addr: peer_addr,
        }),
    )
    .await;

    if let Err(e) = match connect_result {
        Ok(result) => result,
        Err(_) => Err(TransportError::ConnectionFailed(format!(
            "connect timeout after {}ms",
            state.outbound_connect_timeout.as_millis()
        ))),
    } {
        clear_pending_outbound(&state, target, transport_key).await;
        warn!(%peer_addr, mechanism = target.mechanism_name(), "connect failed: {e}; reconnect will retry");
        if state.reconnect.begin_reconnect(target).await {
            let st = state.clone();
            tokio::spawn(run_reconnect_task(st, target));
        }
        return;
    }
    info!(%peer_addr, mechanism = target.mechanism_name(), "connected to peer");

    let (tx, rx) = mpsc::channel(8);
    state.hs_channels.lock().await.insert(transport_key, tx);
    let st = state.clone();
    tokio::spawn(async move {
        match handshake_initiator(&st, transport_key, rx).await {
            Ok(peer_id) => {
                clear_pending_outbound(&st, target, transport_key).await;
                st.reconnect.register(peer_id, target).await;
                info!(%peer_id, %peer_addr, mechanism = target.mechanism_name(), "peer connected");
            }
            Err(e) => {
                clear_pending_outbound(&st, target, transport_key).await;
                if take_cancelled_outbound(&st, transport_key).await {
                    info!(
                        %peer_addr,
                        mechanism = target.mechanism_name(),
                        "outbound handshake cancelled after simultaneous inbound handshake"
                    );
                    st.hs_channels.lock().await.remove(&transport_key);
                    return;
                }
                warn!(%peer_addr, mechanism = target.mechanism_name(), "handshake failed: {e}; reconnect will retry");
                st.transport.disconnect(&transport_key).await.ok();
                if st.reconnect.begin_reconnect(target).await {
                    tokio::spawn(run_reconnect_task(st.clone(), target));
                }
            }
        }
        st.hs_channels.lock().await.remove(&transport_key);
    });
}

/// Consume new-peer notifications from `DiscoveryService` and initiate
/// connections to discovered relays and gateways.
pub(crate) async fn run_discovery_consumer(
    state: Arc<DaemonState>,
    mut new_peer_rx: mpsc::Receiver<PeerRecord>,
) {
    loop {
        tokio::select! {
            Some(record) = new_peer_rx.recv() => {
                if record.node_id == state.self_id {
                    continue;
                }

                if !state.authorization.authorize_discovered_peer(record.node_id).await {
                    info!(
                        peer_id = %record.node_id,
                        "discovered peer rejected by authorization policy"
                    );
                    continue;
                }

                if state.sessions.read().await.contains_key(&record.node_id) {
                    debug!(%record.node_id, "discovery: already connected, skipping");
                    continue;
                }

                let caps = record.capabilities;
                if !caps.is_relay() && !caps.is_gateway() {
                    debug!(%record.node_id, "discovery: client-only peer, skipping");
                    continue;
                }

                let cfg = &state.discovery_config;
                if caps.is_gateway() && !cfg.connect_gateways {
                    debug!(%record.node_id, "discovery: connect_gateways=false, skipping gateway");
                    continue;
                }
                if caps.is_relay() && !caps.is_gateway() && !cfg.connect_relays {
                    debug!(%record.node_id, "discovery: connect_relays=false, skipping relay");
                    continue;
                }

                info!(
                    peer_id = %record.node_id,
                    addr = %record.listen_addr,
                    is_gateway = caps.is_gateway(),
                    is_relay = caps.is_relay(),
                    "discovered peer - initiating connection",
                );

                let target = ConnectTarget::Tcp(record.listen_addr);
                state.reconnect.register_discovered(target).await;
                initiate_peer_connection(state.clone(), target).await;
            }
            _ = state.cancel.cancelled() => break,
        }
    }
}

/// Consume `SocketAddr` notifications from Wi-Fi Direct discovery and initiate
/// connections to the indicated peers.
pub(crate) async fn run_wifidirect_consumer(
    state: Arc<DaemonState>,
    mut addr_rx: mpsc::Receiver<SocketAddr>,
) {
    loop {
        tokio::select! {
            Some(addr) = addr_rx.recv() => {
                let target = ConnectTarget::Tcp(addr);
                info!(%addr, mechanism = target.mechanism_name(), "Wi-Fi Direct: new peer addr - initiating connection");
                state.reconnect.register_discovered(target).await;
                initiate_peer_connection(state.clone(), target).await;
            }
            _ = state.cancel.cancelled() => break,
        }
    }
}

/// Consume `SocketAddr` notifications from Bluetooth discovery and initiate
/// connections to the indicated peers.
pub(crate) async fn run_bluetooth_consumer(
    state: Arc<DaemonState>,
    mut addr_rx: mpsc::Receiver<SocketAddr>,
) {
    loop {
        tokio::select! {
            Some(addr) = addr_rx.recv() => {
                let target = ConnectTarget::BluetoothPan(addr);
                info!(%addr, mechanism = target.mechanism_name(), "Bluetooth PAN: peer addr ready — initiating connection");
                state.reconnect.register_discovered(target).await;
                initiate_peer_connection(state.clone(), target).await;
            }
            _ = state.cancel.cancelled() => break,
        }
    }
}
