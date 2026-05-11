//! Control-plane messages carried inside transport frames.

use bytes::{Buf, BufMut, Bytes, BytesMut};

use pim_core::{FrameCodec, NodeId, PimError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
/// Discriminator for [`ControlFrame`] payloads.
///
/// Tag values `0x01` and `0x02` were previously assigned to
/// `IpRequest` / `IpAssign`. They were removed when mesh addresses
/// became deterministic from each node's `NodeId` (no more dynamic
/// allocation handshake). The slots are kept reserved on the wire so
/// any straggling old daemon's frames decode to a clean error rather
/// than aliasing a future tag.
pub enum ControlType {
    /// Peer is leaving and wants its state cleaned up promptly.
    Goodbye = 0x03,
    /// Session keys should be renegotiated.
    Rekey = 0x04,
    /// RTT probe request.
    Ping = 0x05,
    /// RTT probe response.
    Pong = 0x06,
    /// One-shot exchange of node identity metadata after handshake.
    PeerInfo = 0x07,
    /// Generic plugin-defined payload — see [`ControlFrame::PluginPayload`].
    PluginPayload = 0x08,
}

impl ControlType {
    /// Decode a raw control-type tag from the wire.
    pub fn from_u8(v: u8) -> Result<Self, PimError> {
        match v {
            0x03 => Ok(Self::Goodbye),
            0x04 => Ok(Self::Rekey),
            0x05 => Ok(Self::Ping),
            0x06 => Ok(Self::Pong),
            0x07 => Ok(Self::PeerInfo),
            0x08 => Ok(Self::PluginPayload),
            other => Err(PimError::Protocol(format!(
                "unknown control type: 0x{other:02x}"
            ))),
        }
    }
}

/// Multiplexed control message.
///
/// Layout: control_type(1) + body (variable, depends on type).
///
/// Mesh-essential variants (routing, liveness, identity) live here
/// directly. Optional features such as user messaging are carried
/// inside [`ControlFrame::PluginPayload`] so the daemon can be built
/// without those plugins compiled in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlFrame {
    /// Graceful disconnect notification.
    Goodbye {
        /// Node that is departing.
        departing_id: NodeId,
        /// Implementation-defined reason code.
        reason: u8,
    },
    /// Request that the session be rekeyed.
    Rekey,
    /// Ping carrying an opaque nonce.
    Ping {
        /// Opaque value echoed by the corresponding pong.
        nonce: u64,
    },
    /// Pong echoing a ping nonce.
    Pong {
        /// Opaque value copied from the ping request.
        nonce: u64,
    },
    /// Identity metadata exchanged once after the session is established
    /// so that peers can address each other by `NodeId` and end-to-end
    /// encrypt to the recipient's static X25519 key.
    PeerInfo {
        /// X25519 public key derived from the sender's Ed25519 seed.
        x25519_pub: [u8; 32],
        /// Sender's friendly node name as configured locally (UTF-8,
        /// length-prefixed by `u8` — capped at 255 bytes by the codec).
        friendly_name: String,
    },
    /// Generic plugin-defined payload.
    ///
    /// `kind` is an ASCII identifier (≤ 255 bytes) registered by a
    /// [`pim-plugin`](https://crates.io/crates/pim-plugin)-style
    /// plugin (e.g. `"messaging.msg"`). `body` is plugin-private and
    /// opaque to the daemon — typically further encrypted/serialized
    /// according to the plugin's own scheme.
    ///
    /// Wire layout:
    /// ```text
    /// 0x08
    /// kind_len (u8, 1..=255)
    /// kind     (kind_len bytes, ASCII)
    /// body_len (u16 BE)
    /// body     (body_len bytes)
    /// ```
    PluginPayload {
        /// Plugin-namespaced kind identifier.
        kind: String,
        /// Opaque payload bytes — interpretation is up to the plugin
        /// claiming `kind`.
        body: Bytes,
    },
}

impl FrameCodec for ControlFrame {
    fn encode(&self, buf: &mut BytesMut) {
        match self {
            ControlFrame::Goodbye {
                departing_id,
                reason,
            } => {
                buf.put_u8(ControlType::Goodbye as u8);
                buf.put_slice(departing_id.as_bytes());
                buf.put_u8(*reason);
            }
            ControlFrame::Rekey => {
                buf.put_u8(ControlType::Rekey as u8);
            }
            ControlFrame::Ping { nonce } => {
                buf.put_u8(ControlType::Ping as u8);
                buf.put_u64(*nonce);
            }
            ControlFrame::Pong { nonce } => {
                buf.put_u8(ControlType::Pong as u8);
                buf.put_u64(*nonce);
            }
            ControlFrame::PeerInfo {
                x25519_pub,
                friendly_name,
            } => {
                buf.put_u8(ControlType::PeerInfo as u8);
                buf.put_slice(x25519_pub);
                let name_bytes = friendly_name.as_bytes();
                let name_len = name_bytes.len().min(255) as u8;
                buf.put_u8(name_len);
                buf.put_slice(&name_bytes[..name_len as usize]);
            }
            ControlFrame::PluginPayload { kind, body } => {
                buf.put_u8(ControlType::PluginPayload as u8);
                let kind_bytes = kind.as_bytes();
                let kind_len = kind_bytes.len().min(255) as u8;
                buf.put_u8(kind_len);
                buf.put_slice(&kind_bytes[..kind_len as usize]);
                let body_len = body.len().min(u16::MAX as usize) as u16;
                buf.put_u16(body_len);
                buf.put_slice(&body[..body_len as usize]);
            }
        }
    }

    fn decode(buf: &mut BytesMut) -> Result<Self, PimError> {
        if buf.is_empty() {
            return Err(PimError::Protocol("control frame empty".into()));
        }

        let control_type = ControlType::from_u8(buf[0])?;

        match control_type {
            ControlType::Goodbye => {
                if buf.len() < 18 {
                    // 1 + 16 + 1
                    return Err(PimError::Protocol("Goodbye too short".into()));
                }
                let mut id = [0u8; 16];
                id.copy_from_slice(&buf[1..17]);
                let reason = buf[17];
                buf.advance(18);
                Ok(ControlFrame::Goodbye {
                    departing_id: NodeId::from_bytes(id),
                    reason,
                })
            }
            ControlType::Rekey => {
                buf.advance(1);
                Ok(ControlFrame::Rekey)
            }
            ControlType::Ping => {
                if buf.len() < 9 {
                    return Err(PimError::Protocol("Ping too short".into()));
                }
                let nonce = (&buf[1..9]).get_u64();
                buf.advance(9);
                Ok(ControlFrame::Ping { nonce })
            }
            ControlType::Pong => {
                if buf.len() < 9 {
                    return Err(PimError::Protocol("Pong too short".into()));
                }
                let nonce = (&buf[1..9]).get_u64();
                buf.advance(9);
                Ok(ControlFrame::Pong { nonce })
            }
            ControlType::PeerInfo => {
                // 1 (tag) + 32 (x25519) + 1 (name_len) + N (name)
                if buf.len() < 34 {
                    return Err(PimError::Protocol("PeerInfo too short".into()));
                }
                let mut x25519_pub = [0u8; 32];
                x25519_pub.copy_from_slice(&buf[1..33]);
                let name_len = buf[33] as usize;
                let total = 34 + name_len;
                if buf.len() < total {
                    return Err(PimError::Protocol(format!(
                        "PeerInfo truncated: need {total}, have {}",
                        buf.len()
                    )));
                }
                let friendly_name = match std::str::from_utf8(&buf[34..total]) {
                    Ok(s) => s.to_owned(),
                    Err(_) => {
                        return Err(PimError::Protocol(
                            "PeerInfo friendly_name not valid UTF-8".into(),
                        ))
                    }
                };
                buf.advance(total);
                Ok(ControlFrame::PeerInfo {
                    x25519_pub,
                    friendly_name,
                })
            }
            ControlType::PluginPayload => {
                // 1 (tag) + 1 (kind_len) + N (kind) + 2 (body_len) + M (body)
                if buf.len() < 4 {
                    return Err(PimError::Protocol("PluginPayload too short".into()));
                }
                let kind_len = buf[1] as usize;
                let body_len_off = 2 + kind_len;
                if buf.len() < body_len_off + 2 {
                    return Err(PimError::Protocol(format!(
                        "PluginPayload header truncated: need {}, have {}",
                        body_len_off + 2,
                        buf.len()
                    )));
                }
                let kind = match std::str::from_utf8(&buf[2..body_len_off]) {
                    Ok(s) => s.to_owned(),
                    Err(_) => {
                        return Err(PimError::Protocol(
                            "PluginPayload kind not valid UTF-8".into(),
                        ))
                    }
                };
                let body_len = (&buf[body_len_off..body_len_off + 2]).get_u16() as usize;
                let total = body_len_off + 2 + body_len;
                if buf.len() < total {
                    return Err(PimError::Protocol(format!(
                        "PluginPayload truncated: need {total}, have {}",
                        buf.len()
                    )));
                }
                let body = Bytes::copy_from_slice(&buf[body_len_off + 2..total]);
                buf.advance(total);
                Ok(ControlFrame::PluginPayload { kind, body })
            }
        }
    }
}

#[cfg(test)]
mod tests;
