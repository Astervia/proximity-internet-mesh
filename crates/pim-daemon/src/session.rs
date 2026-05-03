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
    pub(crate) fn encrypt_frame(&self, mut plaintext: bytes::BytesMut) -> Result<TransportFrame> {
        // PERFORMANCE: Encrypting the payload in-place inside the `BytesMut` buffer avoids
        // a runtime memory allocation for the ciphertext on hot network paths.
        let (nonce, tag) = self.send.encrypt_in_place_detached(&mut plaintext)?;

        Ok(TransportFrame {
            frame_type: FrameType::Data,
            nonce,
            payload: plaintext.freeze(), // zero-copy
            tag,
        })
    }

    pub(crate) fn decrypt_frame(&self, mut frame: TransportFrame) -> Result<TransportFrame> {
        // PERFORMANCE: Decrypt directly over the uniquely owned `BytesMut` buffer whenever possible
        // to avoid runtime memory allocation for the plaintext slice.
        let mut payload_buf = match frame.payload.try_into_mut() {
            Ok(buf) => buf,
            Err(payload) => bytes::BytesMut::from(payload.as_ref()), // fallback allocation if shared
        };
        self.recv
            .decrypt_in_place_detached(&frame.nonce, &mut payload_buf, &frame.tag)?;

        frame.payload = payload_buf.freeze(); // zero-copy
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
