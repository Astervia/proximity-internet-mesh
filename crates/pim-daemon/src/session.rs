use anyhow::Result;
use pim_core::NodeId;
use pim_crypto::SessionCipher;
use pim_protocol::{FrameType, TransportFrame};

/// Per-peer crypto session established after the handshake.
pub(crate) struct Session {
    pub(crate) peer_id: NodeId,
    pub(crate) send: SessionCipher,
    /// Persistent receive cipher: carries nonce-replay state across frames.
    pub(crate) recv: SessionCipher,
}

impl Session {
    pub(crate) fn encrypt_frame(&self, plaintext: &[u8]) -> Result<TransportFrame> {
        let mut payload_buf = plaintext.to_vec();
        let (nonce, tag) = self.send.encrypt_in_place_detached(&mut payload_buf)?;

        Ok(TransportFrame {
            frame_type: FrameType::Data,
            nonce,
            payload: bytes::Bytes::from(payload_buf),
            tag,
        })
    }

    pub(crate) fn decrypt_frame(&self, mut frame: TransportFrame) -> Result<TransportFrame> {
        let mut payload_buf = frame.payload.to_vec();
        self.recv
            .decrypt_in_place_detached(&frame.nonce, &mut payload_buf, &frame.tag)?;

        frame.payload = bytes::Bytes::from(payload_buf);
        Ok(frame)
    }
}

pub(crate) fn nonce_prefix(session_key: &[u8; 32], is_initiator: bool) -> [u8; 8] {
    let mut prefix = [0u8; 8];
    if is_initiator {
        prefix.copy_from_slice(&session_key[0..8]);
    } else {
        prefix.copy_from_slice(&session_key[8..16]);
    }
    prefix
}
