//! Per-RFCOMM-channel session: handshake + event emission.

#![cfg(target_os = "linux")]

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::frame::{decode_frame, encode_frame, FrameError};
use super::socket::RfcommStream;
use super::{
    format_bdaddr, node_id_to_hex, now_iso, BdAddr, LocalIdentity, RfcommEvent, HELLO_VERSION,
};

#[derive(Debug, Serialize, Deserialize)]
struct HelloMsg<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    v: u8,
    node_id: String,
    name: String,
    platform: &'a str,
    caps: Vec<String>,
}

pub async fn run(
    stream: RfcommStream,
    peer_addr: BdAddr,
    initiator: bool,
    identity: LocalIdentity,
    events_tx: mpsc::Sender<RfcommEvent>,
    cancel: CancellationToken,
) {
    let bd_str = format_bdaddr(&peer_addr);
    if let Err(e) = handshake(&stream, &bd_str, initiator, &identity, &events_tx).await {
        let _ = events_tx
            .send(RfcommEvent::Lost {
                bd_addr: bd_str.clone(),
                reason: format!("handshake_failed: {e}"),
            })
            .await;
        return;
    }

    // Post-handshake idle loop: read frames until the channel closes
    // or shutdown is requested. Phase 7 spike scope is discovery only,
    // so we just consume bytes; production will pump
    // pim-protocol::TransportFrame here.
    let mut buf: Vec<u8> = Vec::with_capacity(8192);
    let mut chunk = [0u8; 4096];
    let close_reason = loop {
        tokio::select! {
            _ = cancel.cancelled() => break "shutdown".to_string(),
            r = stream.read(&mut chunk) => {
                match r {
                    Ok(0) => break "stream_eof".to_string(),
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        match decode_frame(&mut buf) {
                            Ok(frames) => {
                                for f in frames {
                                    match serde_json::from_slice::<Value>(&f) {
                                        Ok(_v) => {
                                            // Future: dispatch frame to pim-protocol
                                            debug!(target: "pim-bluetooth-rfcomm", "rx post-handshake frame ({} bytes)", f.len());
                                        }
                                        Err(e) => warn!(target: "pim-bluetooth-rfcomm", "non-json post-handshake: {e}"),
                                    }
                                }
                            }
                            Err(e) => break format!("frame_error: {e}"),
                        }
                    }
                    Err(e) => break format!("read_error: {e}"),
                }
            }
        }
    };

    let _ = events_tx
        .send(RfcommEvent::Lost {
            bd_addr: bd_str,
            reason: close_reason,
        })
        .await;
}

async fn handshake(
    stream: &RfcommStream,
    bd_str: &str,
    initiator: bool,
    identity: &LocalIdentity,
    events_tx: &mpsc::Sender<RfcommEvent>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let local_caps = identity.caps.clone();
    let local_node_hex = node_id_to_hex(&identity.node_id);
    let local_name = identity.name.clone();

    let send_msg = |kind: &str| -> Result<Vec<u8>, FrameError> {
        let msg = HelloMsg {
            kind,
            v: HELLO_VERSION,
            node_id: local_node_hex.clone(),
            name: local_name.clone(),
            platform: "linux",
            caps: local_caps.clone(),
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
            return Err(format!("version mismatch: peer={} ours={}", their_v, HELLO_VERSION).into());
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

        if !initiator {
            let frame = send_msg("hello-ack")?;
            stream.write_all(&frame).await?;
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
        return Ok(());
    }
}
