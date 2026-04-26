use std::sync::Arc;

use pim_core::NodeId;
use pim_transport::{PeerAddress, Transport, TransportError};
use tokio::sync::mpsc;
use tracing::{info, warn};

use super::handshake::handshake_initiator;
use super::ip_control::{
    clear_pending_outbound, register_pending_outbound, take_cancelled_outbound,
};
use super::reconnect::{backoff_duration, ConnectTarget};
use super::DaemonState;

/// Background task that reconnects to a configured peer using exponential
/// backoff + jitter. Runs a full handshake after each successful TCP connect
/// so a fresh session key is established.
pub(crate) async fn run_reconnect_task(state: Arc<DaemonState>, target: ConnectTarget) {
    let addr = target.addr();
    let mut attempt = 0u32;
    loop {
        if attempt >= state.max_reconnect_attempts {
            warn!(
                %addr,
                mechanism = target.mechanism_name(),
                attempt,
                max_reconnect_attempts = state.max_reconnect_attempts,
                "reconnect stopped: reached configured attempt limit"
            );
            state.reconnect.end_reconnect(target).await;
            return;
        }

        let delay = backoff_duration(attempt);
        info!(%addr, mechanism = target.mechanism_name(), attempt, delay_ms = delay.as_millis(), "reconnect scheduled");

        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = state.cancel.cancelled() => {
                state.reconnect.end_reconnect(target).await;
                return;
            }
        }

        info!(%addr, mechanism = target.mechanism_name(), attempt, "attempting reconnect");

        if let Some(peer_id) = state.reconnect.peer_id_for_target(&target).await {
            if state.sessions.read().await.contains_key(&peer_id) {
                info!(
                    %addr, %peer_id,
                    mechanism = target.mechanism_name(),
                    "reconnect cancelled: session already active"
                );
                state.reconnect.end_reconnect(target).await;
                return;
            }
        }

        let transport_key = NodeId::from_bytes(rand::random::<[u8; 16]>());
        register_pending_outbound(&state, target, transport_key).await;

        let connect_result = tokio::time::timeout(
            state.outbound_connect_timeout,
            state.transport.connect(&PeerAddress {
                node_id: transport_key,
                addr,
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
            warn!(%addr, mechanism = target.mechanism_name(), attempt, "reconnect TCP connect failed: {e}");
            attempt += 1;
            continue;
        }

        let (tx, rx) = mpsc::channel(8);
        state.hs_channels.lock().await.insert(transport_key, tx);

        match handshake_initiator(&state, transport_key, rx).await {
            Ok(peer_id) => {
                clear_pending_outbound(&state, target, transport_key).await;
                state.reconnect.register(peer_id, target).await;
                state.hs_channels.lock().await.remove(&transport_key);
                info!(%peer_id, %addr, mechanism = target.mechanism_name(), "reconnect succeeded (new session key)");
                state.reconnect.end_reconnect(target).await;
                return;
            }
            Err(e) => {
                clear_pending_outbound(&state, target, transport_key).await;
                if take_cancelled_outbound(&state, transport_key).await {
                    info!(
                        %addr,
                        mechanism = target.mechanism_name(),
                        attempt,
                        "reconnect attempt cancelled after simultaneous inbound handshake"
                    );
                    state.reconnect.end_reconnect(target).await;
                    return;
                }
                warn!(%addr, mechanism = target.mechanism_name(), attempt, "reconnect handshake failed: {e}");
                state.hs_channels.lock().await.remove(&transport_key);
                state.transport.disconnect(&transport_key).await.ok();
            }
        }

        attempt += 1;
    }
}
