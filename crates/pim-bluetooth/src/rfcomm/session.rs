//! Per-RFCOMM-channel session: handshake + event emission.

#![cfg(target_os = "linux")]

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::bridge;
use super::socket::RfcommStream;
use super::{format_bdaddr, now_iso, BdAddr, LocalIdentity, RfcommEvent, HELLO_VERSION};
use crate::frame::{decode_frame, encode_frame, FrameError};

#[derive(Debug, Serialize, Deserialize)]
struct HelloMsg<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    v: u8,
    node_id: String,
    name: String,
    platform: &'a str,
    caps: Vec<String>,
    /// Lowercase-hex `HMAC-SHA256(mesh_handshake_key, node_id_hex)`.
    /// Present iff the sender is on a private mesh. See
    /// `pim_crypto::compute_rfcomm_hello_tag`. The receiver requires
    /// this field to be present and matching when the receiver is
    /// itself on a private mesh, and absent when the receiver is on
    /// the open mesh — i.e., open ↔ private is hard-rejected at the
    /// RFCOMM Hello layer, before any TCP bridge is set up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mesh_tag: Option<String>,
}

/// Drive a freshly-accepted/dialed RFCOMM session from handshake
/// completion through teardown.
///
/// `local_bridge_addr`: when `Some`, on successful handshake the
/// channel is bridged to that loopback TCP address (the daemon's
/// `pim-transport` listener) and bytes flow through until either side
/// closes or `cancel` fires. When `None`, the post-handshake side
/// just consumes bytes until the channel closes (discovery-only mode,
/// kept for tests / acceptor-only deployments).
pub async fn run(
    stream: RfcommStream,
    peer_addr: BdAddr,
    initiator: bool,
    identity: LocalIdentity,
    events_tx: mpsc::Sender<RfcommEvent>,
    cancel: CancellationToken,
    local_bridge_addr: Option<std::net::SocketAddr>,
) {
    let bd_str = format_bdaddr(&peer_addr);
    let peer_node_id = match handshake(&stream, &bd_str, initiator, &identity, &events_tx).await {
        Ok(id) => id,
        Err(e) => {
            let _ = events_tx
                .send(RfcommEvent::Lost {
                    bd_addr: bd_str.clone(),
                    reason: format!("handshake_failed: {e}"),
                })
                .await;
            return;
        }
    };

    let stream = Arc::new(stream);
    let close_reason = match local_bridge_addr {
        Some(addr) => {
            debug!(
                target: "pim-bluetooth-rfcomm",
                peer = %bd_str,
                bridge = %addr,
                "session: handshake OK, bridging to loopback TCP",
            );
            // Hand the bridge a CHILD token, not a clone. `bridge::run`
            // calls `cancel.cancel()` from inside its r2t/t2r select to
            // unwind the second pump when the first exits — if that
            // call landed on the parent token (a clone of the listener
            // / outbound token), one peer disconnecting would also
            // cancel the acceptor + outbound poll, and every future
            // dial against this daemon would hit
            // `Connection refused (os error 111)` until restart.
            // Child tokens cascade parent → child cancellation but not
            // child → parent.
            match bridge::run(
                stream.clone(),
                addr,
                bd_str.clone(),
                peer_node_id,
                cancel.child_token(),
            )
            .await
            {
                Ok(()) => "bridge_closed".to_string(),
                Err(e) => format!("bridge_io_error: {e}"),
            }
        }
        None => discovery_only_loop(stream, &bd_str, cancel).await,
    };

    let _ = events_tx
        .send(RfcommEvent::Lost {
            bd_addr: bd_str,
            reason: close_reason,
        })
        .await;
}

/// Discovery-only mode: just consume bytes until the peer closes or
/// the supervisor cancels us. Kept around for tests and acceptor-only
/// deployments that don't run a daemon TCP listener.
async fn discovery_only_loop(
    stream: Arc<RfcommStream>,
    bd_str: &str,
    cancel: CancellationToken,
) -> String {
    let mut buf: Vec<u8> = Vec::with_capacity(8192);
    let mut chunk = [0u8; 4096];
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return "shutdown".to_string(),
            r = stream.read(&mut chunk) => match r {
                Ok(0) => return "stream_eof".to_string(),
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    match decode_frame(&mut buf) {
                        Ok(frames) => {
                            for f in frames {
                                match serde_json::from_slice::<Value>(&f) {
                                    Ok(_v) => debug!(
                                        target: "pim-bluetooth-rfcomm",
                                        peer = %bd_str,
                                        bytes = f.len(),
                                        "rx post-handshake frame (discovery-only)",
                                    ),
                                    Err(e) => warn!(
                                        target: "pim-bluetooth-rfcomm",
                                        peer = %bd_str,
                                        err = %e,
                                        "non-json post-handshake frame",
                                    ),
                                }
                            }
                        }
                        Err(e) => return format!("frame_error: {e}"),
                    }
                }
                Err(e) => return format!("read_error: {e}"),
            }
        }
    }
}

/// Mesh-membership gate enforced on every RFCOMM Hello / HelloAck.
///
/// | local | peer sent tag | result | reason returned |
/// |-------|---------------|--------|-----------------|
/// | private | matching | admit | — |
/// | private | mismatch | reject | "mesh tag mismatch ..." |
/// | private | absent | reject | "mesh tag missing ..." |
/// | open | absent | admit | — |
/// | open | present | reject | "mesh tag present ..." |
///
/// Pure function — no I/O — so the table above is exercised directly
/// in `pim_bluetooth::rfcomm::session::tests` rather than via the
/// async RFCOMM socket plumbing.
pub(super) fn verify_peer_mesh_tag(
    local_mesh_key: Option<&[u8; 32]>,
    peer_mesh_tag_hex: Option<&str>,
    peer_node_id_hex: &str,
) -> Result<(), String> {
    match (local_mesh_key, peer_mesh_tag_hex) {
        (Some(key), Some(tag_hex)) => {
            let expected = pim_crypto::compute_rfcomm_hello_tag(key, peer_node_id_hex);
            let expected_hex = bytes_to_hex(&expected);
            if constant_time_eq(expected_hex.as_bytes(), tag_hex.as_bytes()) {
                Ok(())
            } else {
                Err("mesh tag mismatch (peer is on a different mesh)".to_string())
            }
        }
        (Some(_), None) => {
            Err("mesh tag missing (peer is on the open mesh; this node is private)".to_string())
        }
        (None, Some(_)) => {
            Err("mesh tag present (peer is on a private mesh; this node is open)".to_string())
        }
        (None, None) => Ok(()),
    }
}

/// Lowercase-hex encode a 32-byte tag for the JSON Hello envelope.
fn bytes_to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Constant-time byte-slice equality. Avoids leaking the
/// position-of-first-mismatch through timing for the mesh-tag check.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Decode a 32-char lowercase hex NodeId into 16 bytes. Returns the
/// concrete `String` reason on parse failure so the caller can pass it
/// up the `RfcommEvent::Lost` reason field unchanged.
fn hex_to_node_id(hex: &str, out: &mut [u8; 16]) -> Result<(), String> {
    if hex.len() != 32 {
        return Err(format!("expected 32 hex chars, got {}", hex.len()));
    }
    for (i, chunk) in hex.as_bytes().chunks_exact(2).enumerate() {
        let s = std::str::from_utf8(chunk).map_err(|e| e.to_string())?;
        out[i] = u8::from_str_radix(s, 16).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Run the RFCOMM session-level Hello / HelloAck handshake. Returns
/// the peer's 16-byte raw NodeId on success — the caller forwards it
/// to the bridge so the loopback TCP listener can be told *who* this
/// connection is from without having to send the bytes back over
/// RFCOMM (which would race with the post-handshake byte stream and
/// cause `decode_frame` to choke on binary NodeId bytes that look
/// like absurd length prefixes).
async fn handshake(
    stream: &RfcommStream,
    bd_str: &str,
    initiator: bool,
    identity: &LocalIdentity,
    events_tx: &mpsc::Sender<RfcommEvent>,
) -> Result<[u8; 16], Box<dyn std::error::Error + Send + Sync>> {
    let local_caps = identity.caps.clone();
    let local_node_hex = identity.node_id_hex.clone();
    let local_name = identity.name.clone();
    let local_mesh_key = identity.mesh_handshake_key;
    let local_mesh_tag = local_mesh_key.map(|key| {
        let raw = pim_crypto::compute_rfcomm_hello_tag(&key, &local_node_hex);
        bytes_to_hex(&raw)
    });

    let send_msg = |kind: &str| -> Result<Vec<u8>, FrameError> {
        let msg = HelloMsg {
            kind,
            v: HELLO_VERSION,
            node_id: local_node_hex.clone(),
            name: local_name.clone(),
            platform: "linux",
            caps: local_caps.clone(),
            mesh_tag: local_mesh_tag.clone(),
        };
        let json = serde_json::to_vec(&msg).expect("serialize hello");
        encode_frame(&json)
    };

    if initiator {
        let frame = send_msg("hello")?;
        stream.write_all(&frame).await?;
    }

    // Read until we get exactly one Hello (acceptor) or HelloAck (initiator).
    let want = if initiator { "hello-ack" } else { "hello" };
    let mut buf: Vec<u8> = Vec::with_capacity(2048);
    let mut chunk = [0u8; 1024];
    loop {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Err("eof during handshake".into());
        }
        buf.extend_from_slice(&chunk[..n]);
        let frames = decode_frame(&mut buf)?;
        if frames.is_empty() {
            continue;
        }
        let payload = &frames[0];
        let v: Value = serde_json::from_slice(payload)?;
        let kind = v
            .get("type")
            .and_then(|t| t.as_str())
            .ok_or("missing type")?;
        if kind != want {
            return Err(format!("unexpected handshake msg: {kind}").into());
        }
        let their_v = v.get("v").and_then(|x| x.as_u64()).unwrap_or(0) as u8;
        if their_v != HELLO_VERSION {
            return Err(
                format!("version mismatch: peer={} ours={}", their_v, HELLO_VERSION).into(),
            );
        }
        let their_node = v
            .get("node_id")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let their_name = v
            .get("name")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let their_platform = v
            .get("platform")
            .and_then(|x| x.as_str())
            .unwrap_or("unknown")
            .to_string();
        let their_caps: Vec<String> = v
            .get("caps")
            .and_then(|x| x.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|c| c.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        // Mesh-membership gate. See `verify_peer_mesh_tag`.
        let their_mesh_tag = v
            .get("mesh_tag")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string());
        if let Err(reason) = verify_peer_mesh_tag(
            local_mesh_key.as_ref(),
            their_mesh_tag.as_deref(),
            &their_node,
        ) {
            return Err(reason.into());
        }

        if !initiator {
            let frame = send_msg("hello-ack")?;
            stream.write_all(&frame).await?;
        }
        // Decode the peer's hex NodeId now so the caller can hand the
        // raw 16-byte form to the bridge. Errors here mean the peer
        // sent us a Hello with a malformed `node_id` field; treat as
        // a handshake failure so we don't bridge with garbage.
        let mut peer_node_id = [0u8; 16];
        if let Err(msg) = hex_to_node_id(&their_node, &mut peer_node_id) {
            return Err(format!("invalid peer node_id `{their_node}`: {msg}").into());
        }
        let _ = events_tx
            .send(RfcommEvent::Discovered {
                bd_addr: bd_str.to_string(),
                node_id: their_node,
                name: their_name,
                platform: their_platform,
                caps: their_caps,
                initiator,
                since: now_iso(),
            })
            .await;
        return Ok(peer_node_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PEER_ID: &str = "0123456789abcdef0123456789abcdef";

    fn matching_tag(key: &[u8; 32], node_id_hex: &str) -> String {
        let raw = pim_crypto::compute_rfcomm_hello_tag(key, node_id_hex);
        bytes_to_hex(&raw)
    }

    #[test]
    fn open_to_open_admits() {
        assert!(verify_peer_mesh_tag(None, None, PEER_ID).is_ok());
    }

    #[test]
    fn open_to_private_rejects() {
        let key = [0xABu8; 32];
        let tag = matching_tag(&key, PEER_ID);
        let err = verify_peer_mesh_tag(None, Some(&tag), PEER_ID).unwrap_err();
        assert!(err.contains("private mesh"), "{err}");
    }

    #[test]
    fn private_to_open_rejects() {
        let key = [0xABu8; 32];
        let err = verify_peer_mesh_tag(Some(&key), None, PEER_ID).unwrap_err();
        assert!(err.contains("missing"), "{err}");
    }

    #[test]
    fn private_with_matching_tag_admits() {
        let key = [0xABu8; 32];
        let tag = matching_tag(&key, PEER_ID);
        verify_peer_mesh_tag(Some(&key), Some(&tag), PEER_ID).unwrap();
    }

    #[test]
    fn private_with_wrong_key_rejects() {
        let our_key = [0xABu8; 32];
        let their_key = [0xCDu8; 32];
        let their_tag = matching_tag(&their_key, PEER_ID);
        let err = verify_peer_mesh_tag(Some(&our_key), Some(&their_tag), PEER_ID).unwrap_err();
        assert!(err.contains("mismatch"), "{err}");
    }

    #[test]
    fn private_with_tag_for_different_node_rejects() {
        // Replay defense: peer says they're PEER_ID but signed a tag for
        // a different node id (mounted attack from a captured Hello).
        let key = [0xABu8; 32];
        let tag_for_other = matching_tag(&key, "ffffffffffffffffffffffffffffffff");
        let err = verify_peer_mesh_tag(Some(&key), Some(&tag_for_other), PEER_ID).unwrap_err();
        assert!(err.contains("mismatch"), "{err}");
    }

    #[test]
    fn private_with_garbage_tag_rejects() {
        let key = [0xABu8; 32];
        let err = verify_peer_mesh_tag(Some(&key), Some("not-hex"), PEER_ID).unwrap_err();
        assert!(err.contains("mismatch"), "{err}");
    }

    #[test]
    fn constant_time_eq_correctness() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn bytes_to_hex_round_trip() {
        let bytes = [0x00u8, 0xAB, 0xFF, 0x42];
        assert_eq!(bytes_to_hex(&bytes), "00abff42");
    }
}
