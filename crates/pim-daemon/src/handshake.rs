use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use bytes::BytesMut;
use pim_core::{FrameCodec, NodeId};
use pim_crypto::{HandshakeConfirm, HandshakeInit, HandshakeResponse, Handshaker, SessionCipher};
use pim_protocol::{FrameType, HandshakeFrameType, HandshakeWireFrame, TransportFrame};
use pim_transport::{TcpTransport, Transport};
use tokio::sync::mpsc;
use tracing::info;

use super::auth::AuthorizationDecision;
use super::ip_control::request_dynamic_ip_from_peer;
use super::peer_tasks::flush_send_buffer;
use super::session::{nonce_prefix, Session};
use super::DaemonState;

pub(crate) async fn send_handshake(
    transport: &Arc<TcpTransport>,
    peer: &NodeId,
    wire: HandshakeWireFrame,
) -> Result<()> {
    let mut buf = BytesMut::new();
    wire.encode(&mut buf);
    transport
        .send(
            peer,
            TransportFrame {
                frame_type: FrameType::Handshake,
                nonce: [0; 12],
                payload: buf.freeze(),
                tag: [0; 16],
            },
        )
        .await?;
    Ok(())
}

pub(crate) fn decode_handshake_wire(frame: &TransportFrame) -> Result<HandshakeWireFrame> {
    if frame.frame_type != FrameType::Handshake {
        bail!("expected Handshake frame, got {:?}", frame.frame_type);
    }
    let mut buf = BytesMut::from(&frame.payload[..]);
    Ok(HandshakeWireFrame::decode(&mut buf)?)
}

/// Initiator task: send Init, receive Response, send Confirm.
///
/// `transport_key` is the NodeId under which the transport currently tracks
/// this connection (may be a random placeholder). After the handshake the
/// real peer NodeId is derived from the Response's `sender_pub` and the
/// transport entry is renamed accordingly.
///
/// Returns the real peer NodeId on success.
pub(crate) async fn handshake_initiator(
    state: &Arc<DaemonState>,
    transport_key: NodeId,
    mut rx: mpsc::Receiver<HandshakeWireFrame>,
) -> Result<NodeId> {
    let mut hs = Handshaker::new(&state.identity);
    let init = hs.initiate();

    send_handshake(
        &state.transport,
        &transport_key,
        HandshakeWireFrame::InitOrResponse {
            handshake_type: HandshakeFrameType::Init,
            sender_pub: init.sender_pub,
            ephemeral_pub: init.ephemeral_pub,
            nonce: init.nonce,
            signature: init.signature,
        },
    )
    .await?;
    info!(%transport_key, "sent HandshakeInit");

    let wire = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .context("handshake response timeout")?
        .context("handshake channel closed")?;
    let (response, sender_pub) = match wire {
        HandshakeWireFrame::InitOrResponse {
            handshake_type: HandshakeFrameType::Response,
            sender_pub,
            ephemeral_pub,
            nonce,
            signature,
        } => {
            let sp = sender_pub;
            (
                HandshakeResponse {
                    sender_pub: sp,
                    ephemeral_pub,
                    nonce,
                    signature,
                },
                sp,
            )
        }
        _ => bail!("expected HandshakeResponse from {transport_key}"),
    };

    let peer_id = NodeId::from_public_key(&sender_pub);
    match state
        .authorization
        .authorize_authenticated_peer(peer_id)
        .await?
    {
        AuthorizationDecision::Allowed => {}
        AuthorizationDecision::TrustedOnFirstUse => {
            info!(%peer_id, "peer trusted on first use");
        }
        AuthorizationDecision::Rejected => {
            bail!("peer {peer_id} rejected by authorization policy");
        }
    }

    hs.finalize_initiator(&response)
        .context("handshake finalize")?;

    let confirm = hs.make_confirm().context("make_confirm")?;
    send_handshake(
        &state.transport,
        &transport_key,
        HandshakeWireFrame::Confirm { hmac: confirm.hmac },
    )
    .await?;
    info!(%peer_id, "handshake complete (initiator)");

    let key = *hs.session_key().context("missing session key")?.as_bytes();
    let session = Arc::new(Session {
        peer_id,
        send: SessionCipher::new(&key, nonce_prefix(&key, true)),
        recv: SessionCipher::new(&key, nonce_prefix(&key, false)),
    });

    if transport_key != peer_id {
        state.transport.rename_peer(transport_key, peer_id).await;
    }

    state.sessions.write().await.insert(peer_id, session);
    state.peer_pubkeys.write().await.insert(peer_id, sender_pub);
    state.routing.lock().await.add_peer(peer_id);
    state.routing.lock().await.unblacklist_peer(&peer_id);
    state.reputation.lock().await.pardon(&peer_id);
    state
        .peer_last_hb
        .lock()
        .await
        .insert(peer_id, Instant::now());
    info!(%peer_id, "session established (initiator)");

    flush_send_buffer(state, peer_id).await;
    request_dynamic_ip_from_peer(state, peer_id).await;
    crate::app::identity_broadcast::send_peer_info(state, peer_id).await;
    Ok(peer_id)
}

/// Responder task: receive Init (already parsed), send Response, wait for Confirm.
pub(crate) async fn handshake_responder(
    state: &Arc<DaemonState>,
    peer_id: NodeId,
    init_wire: HandshakeWireFrame,
    mut rx: mpsc::Receiver<HandshakeWireFrame>,
) -> Result<()> {
    let init = match init_wire {
        HandshakeWireFrame::InitOrResponse {
            handshake_type: HandshakeFrameType::Init,
            sender_pub,
            ephemeral_pub,
            nonce,
            signature,
        } => HandshakeInit {
            sender_pub,
            ephemeral_pub,
            nonce,
            signature,
        },
        _ => bail!("expected HandshakeInit"),
    };

    let mut hs = Handshaker::new(&state.identity);
    let presented_peer_id = NodeId::from_public_key(&init.sender_pub);
    match state
        .authorization
        .authorize_authenticated_peer(presented_peer_id)
        .await?
    {
        AuthorizationDecision::Allowed => {}
        AuthorizationDecision::TrustedOnFirstUse => {
            info!(%presented_peer_id, "peer trusted on first use");
        }
        AuthorizationDecision::Rejected => {
            bail!("peer {presented_peer_id} rejected by authorization policy");
        }
    }
    let response = hs.respond(&init).context("handshake respond")?;

    send_handshake(
        &state.transport,
        &peer_id,
        HandshakeWireFrame::InitOrResponse {
            handshake_type: HandshakeFrameType::Response,
            sender_pub: response.sender_pub,
            ephemeral_pub: response.ephemeral_pub,
            nonce: response.nonce,
            signature: response.signature,
        },
    )
    .await?;
    info!(%peer_id, "sent HandshakeResponse");

    let wire = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .context("confirm timeout")?
        .context("channel closed")?;
    let confirm = match wire {
        HandshakeWireFrame::Confirm { hmac } => HandshakeConfirm { hmac },
        _ => bail!("expected HandshakeConfirm from {peer_id}"),
    };
    hs.verify_confirm(&confirm)
        .context("confirm verification")?;
    info!(%peer_id, "handshake complete (responder)");

    let key = *hs.session_key().context("missing session key")?.as_bytes();
    let session = Arc::new(Session {
        peer_id,
        send: SessionCipher::new(&key, nonce_prefix(&key, false)),
        recv: SessionCipher::new(&key, nonce_prefix(&key, true)),
    });
    state.sessions.write().await.insert(peer_id, session);
    state
        .peer_pubkeys
        .write()
        .await
        .insert(peer_id, init.sender_pub);
    state.routing.lock().await.add_peer(peer_id);
    state.routing.lock().await.unblacklist_peer(&peer_id);
    state.reputation.lock().await.pardon(&peer_id);
    state
        .peer_last_hb
        .lock()
        .await
        .insert(peer_id, Instant::now());
    info!(%peer_id, "session established (responder)");

    flush_send_buffer(state, peer_id).await;
    request_dynamic_ip_from_peer(state, peer_id).await;
    crate::app::identity_broadcast::send_peer_info(state, peer_id).await;
    Ok(())
}
