//! Wire-level representation of the authenticated peer handshake.

use bytes::{Buf, BufMut, BytesMut};

use pim_core::{FrameCodec, PimError};

/// Sub-type of handshake frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum HandshakeFrameType {
    /// Initiator sends its ephemeral key and signature.
    Init = 0,
    /// Responder sends its ephemeral key and signature.
    Response = 1,
    /// Final transcript confirmation MAC.
    Confirm = 2,
}

impl HandshakeFrameType {
    /// Decode a raw handshake subtype from the wire.
    pub fn from_u8(v: u8) -> Result<Self, PimError> {
        match v {
            0 => Ok(Self::Init),
            1 => Ok(Self::Response),
            2 => Ok(Self::Confirm),
            other => Err(PimError::Protocol(format!(
                "unknown handshake type: {other}"
            ))),
        }
    }
}

/// Wire representation of a handshake message.
///
/// For Init/Response: handshake_type(1) + sender_pub(32) + ephemeral_pub(32) + nonce(32) + signature(64) = 161
/// For Confirm: handshake_type(1) + hmac(32) = 33
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandshakeWireFrame {
    /// Init or response frame sharing the same binary layout.
    InitOrResponse {
        /// Whether this frame is an init or a response.
        handshake_type: HandshakeFrameType,
        /// Sender's Ed25519 public key.
        sender_pub: [u8; 32],
        /// Sender's ephemeral X25519 public key.
        ephemeral_pub: [u8; 32],
        /// Random nonce mixed into transcript-derived keys.
        nonce: [u8; 32],
        /// Signature over the ephemeral key and nonce.
        signature: [u8; 64],
    },
    /// Final transcript-confirmation frame.
    Confirm {
        /// HMAC of the negotiated handshake transcript.
        hmac: [u8; 32],
    },
}

const INIT_RESPONSE_SIZE: usize = 1 + 32 + 32 + 32 + 64; // 161
const CONFIRM_SIZE: usize = 1 + 32; // 33

impl FrameCodec for HandshakeWireFrame {
    fn encode(&self, buf: &mut BytesMut) {
        match self {
            HandshakeWireFrame::InitOrResponse {
                handshake_type,
                sender_pub,
                ephemeral_pub,
                nonce,
                signature,
            } => {
                buf.put_u8(*handshake_type as u8);
                buf.put_slice(sender_pub);
                buf.put_slice(ephemeral_pub);
                buf.put_slice(nonce);
                buf.put_slice(signature);
            }
            HandshakeWireFrame::Confirm { hmac } => {
                buf.put_u8(HandshakeFrameType::Confirm as u8);
                buf.put_slice(hmac);
            }
        }
    }

    fn decode(buf: &mut BytesMut) -> Result<Self, PimError> {
        if buf.is_empty() {
            return Err(PimError::Protocol("handshake frame empty".into()));
        }

        let handshake_type = HandshakeFrameType::from_u8(buf[0])?;

        match handshake_type {
            HandshakeFrameType::Init | HandshakeFrameType::Response => {
                if buf.len() < INIT_RESPONSE_SIZE {
                    return Err(PimError::Protocol(format!(
                        "handshake init/response too short: need {INIT_RESPONSE_SIZE}, have {}",
                        buf.len()
                    )));
                }
                let mut sender_pub = [0u8; 32];
                sender_pub.copy_from_slice(&buf[1..33]);
                let mut ephemeral_pub = [0u8; 32];
                ephemeral_pub.copy_from_slice(&buf[33..65]);
                let mut nonce = [0u8; 32];
                nonce.copy_from_slice(&buf[65..97]);
                let mut signature = [0u8; 64];
                signature.copy_from_slice(&buf[97..161]);

                buf.advance(INIT_RESPONSE_SIZE);

                Ok(HandshakeWireFrame::InitOrResponse {
                    handshake_type,
                    sender_pub,
                    ephemeral_pub,
                    nonce,
                    signature,
                })
            }
            HandshakeFrameType::Confirm => {
                if buf.len() < CONFIRM_SIZE {
                    return Err(PimError::Protocol(format!(
                        "handshake confirm too short: need {CONFIRM_SIZE}, have {}",
                        buf.len()
                    )));
                }
                let mut hmac = [0u8; 32];
                hmac.copy_from_slice(&buf[1..33]);

                buf.advance(CONFIRM_SIZE);

                Ok(HandshakeWireFrame::Confirm { hmac })
            }
        }
    }
}

#[cfg(test)]
mod tests;
