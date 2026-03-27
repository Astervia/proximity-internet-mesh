use bytes::{Buf, BufMut, BytesMut};

use pim_core::{FrameCodec, PimError};

/// Sub-type of handshake frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum HandshakeFrameType {
    Init = 0,
    Response = 1,
    Confirm = 2,
}

impl HandshakeFrameType {
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
    InitOrResponse {
        handshake_type: HandshakeFrameType,
        sender_pub: [u8; 32],
        ephemeral_pub: [u8; 32],
        nonce: [u8; 32],
        signature: [u8; 64],
    },
    Confirm {
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
mod tests {
    use super::*;

    #[test]
    fn init_round_trip() {
        let frame = HandshakeWireFrame::InitOrResponse {
            handshake_type: HandshakeFrameType::Init,
            sender_pub: [0xAA; 32],
            ephemeral_pub: [0xBB; 32],
            nonce: [0xCC; 32],
            signature: [0xDD; 64],
        };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let decoded = HandshakeWireFrame::decode(&mut buf).unwrap();
        assert_eq!(frame, decoded);
    }

    #[test]
    fn response_round_trip() {
        let frame = HandshakeWireFrame::InitOrResponse {
            handshake_type: HandshakeFrameType::Response,
            sender_pub: [0x11; 32],
            ephemeral_pub: [0x22; 32],
            nonce: [0x33; 32],
            signature: [0x44; 64],
        };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let decoded = HandshakeWireFrame::decode(&mut buf).unwrap();
        assert_eq!(frame, decoded);
    }

    #[test]
    fn confirm_round_trip() {
        let frame = HandshakeWireFrame::Confirm { hmac: [0xFF; 32] };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let decoded = HandshakeWireFrame::decode(&mut buf).unwrap();
        assert_eq!(frame, decoded);
    }

    #[test]
    fn reject_truncated_init() {
        let mut buf = BytesMut::from(&[0u8; 50][..]); // too short for init
        assert!(HandshakeWireFrame::decode(&mut buf).is_err());
    }

    #[test]
    fn reject_empty() {
        let mut buf = BytesMut::new();
        assert!(HandshakeWireFrame::decode(&mut buf).is_err());
    }

    #[test]
    fn reject_unknown_type() {
        let mut buf = BytesMut::from(&[0xFF][..]);
        assert!(HandshakeWireFrame::decode(&mut buf).is_err());
    }
}
