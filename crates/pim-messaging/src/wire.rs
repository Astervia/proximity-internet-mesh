//! On-wire encoding for messaging payloads carried inside
//! [`pim_protocol::ControlFrame::PluginPayload`].
//!
//! The plugin claims two payload kinds:
//!
//! - [`KIND_MESSAGE`] — `messaging.msg` — carries an end-to-end encrypted
//!   user message.
//! - [`KIND_ACK`] — `messaging.ack` — acknowledges a previously received
//!   `messaging.msg`.
//!
//! Both bodies are length-prefixed binary blobs, decoded by the helpers
//! in this module before [`crate::service::MessagingService`] handles
//! them.

use anyhow::{anyhow, Result};
use bytes::{Buf, BufMut, Bytes, BytesMut};

/// `kind` claimed for outbound and inbound user messages.
pub const KIND_MESSAGE: &str = "messaging.msg";
/// `kind` claimed for delivery / read acknowledgements.
pub const KIND_ACK: &str = "messaging.ack";

/// Wire shape:
///
/// ```text
/// message_id   (16 bytes)
/// timestamp_ms (u64 BE)
/// ciphertext   (rest of payload)
/// ```
pub fn encode_message(message_id: [u8; 16], timestamp_ms: u64, ciphertext: &[u8]) -> Bytes {
    let mut out = BytesMut::with_capacity(16 + 8 + ciphertext.len());
    out.put_slice(&message_id);
    out.put_u64(timestamp_ms);
    out.put_slice(ciphertext);
    out.freeze()
}

/// Decode a `messaging.msg` payload body.
pub fn decode_message(body: &[u8]) -> Result<DecodedMessage> {
    if body.len() < 24 {
        return Err(anyhow!(
            "messaging.msg too short: need ≥ 24, have {}",
            body.len()
        ));
    }
    let mut message_id = [0u8; 16];
    message_id.copy_from_slice(&body[..16]);
    let timestamp_ms = (&body[16..24]).get_u64();
    let ciphertext = body[24..].to_vec();
    Ok(DecodedMessage {
        message_id,
        timestamp_ms,
        ciphertext,
    })
}

/// Wire shape:
///
/// ```text
/// message_id (16 bytes)
/// ack_kind   (u8: 1 = delivered, 2 = read)
/// ```
pub fn encode_ack(message_id: [u8; 16], ack_kind: u8) -> Bytes {
    let mut out = BytesMut::with_capacity(17);
    out.put_slice(&message_id);
    out.put_u8(ack_kind);
    out.freeze()
}

/// Decode a `messaging.ack` payload body.
pub fn decode_ack(body: &[u8]) -> Result<DecodedAck> {
    if body.len() < 17 {
        return Err(anyhow!(
            "messaging.ack too short: need ≥ 17, have {}",
            body.len()
        ));
    }
    let mut message_id = [0u8; 16];
    message_id.copy_from_slice(&body[..16]);
    let ack_kind = body[16];
    Ok(DecodedAck {
        message_id,
        ack_kind,
    })
}

/// Decoded `messaging.msg` payload.
#[derive(Debug, Clone)]
pub struct DecodedMessage {
    /// 16-byte stable identifier (UUIDv4 bytes).
    pub message_id: [u8; 16],
    /// Sender-stamped wall-clock time in milliseconds since epoch.
    pub timestamp_ms: u64,
    /// ECIES ciphertext as produced by `pim_crypto::e2e_encrypt`.
    pub ciphertext: Vec<u8>,
}

/// Decoded `messaging.ack` payload.
#[derive(Debug, Clone, Copy)]
pub struct DecodedAck {
    /// Identifier of the acknowledged message.
    pub message_id: [u8; 16],
    /// Raw ack tag — see [`crate::storage::AckKind::from_u8`].
    pub ack_kind: u8,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_round_trip() {
        let id = [0x55; 16];
        let body = encode_message(id, 1_745_960_000_000, &[0xDE, 0xAD, 0xBE, 0xEF]);
        let decoded = decode_message(&body).unwrap();
        assert_eq!(decoded.message_id, id);
        assert_eq!(decoded.timestamp_ms, 1_745_960_000_000);
        assert_eq!(decoded.ciphertext, vec![0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn ack_round_trip() {
        let id = [0x77; 16];
        let body = encode_ack(id, 2);
        let decoded = decode_ack(&body).unwrap();
        assert_eq!(decoded.message_id, id);
        assert_eq!(decoded.ack_kind, 2);
    }

    #[test]
    fn reject_truncated_message() {
        let body = vec![0u8; 10];
        assert!(decode_message(&body).is_err());
    }

    #[test]
    fn reject_truncated_ack() {
        let body = vec![0u8; 5];
        assert!(decode_ack(&body).is_err());
    }
}
