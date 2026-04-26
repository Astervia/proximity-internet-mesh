//! Control-plane messages carried inside transport frames.

use bytes::{Buf, BufMut, BytesMut};

use pim_core::{FrameCodec, NodeId, PimError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
/// Discriminator for [`ControlFrame`] payloads.
pub enum ControlType {
    /// Client requests a mesh IPv4 assignment from a gateway.
    IpRequest = 0x01,
    /// Gateway assigns a mesh IPv4 configuration lease to a client.
    IpAssign = 0x02,
    /// Peer is leaving and wants its state cleaned up promptly.
    Goodbye = 0x03,
    /// Session keys should be renegotiated.
    Rekey = 0x04,
    /// RTT probe request.
    Ping = 0x05,
    /// RTT probe response.
    Pong = 0x06,
}

impl ControlType {
    /// Decode a raw control-type tag from the wire.
    pub fn from_u8(v: u8) -> Result<Self, PimError> {
        match v {
            0x01 => Ok(Self::IpRequest),
            0x02 => Ok(Self::IpAssign),
            0x03 => Ok(Self::Goodbye),
            0x04 => Ok(Self::Rekey),
            0x05 => Ok(Self::Ping),
            0x06 => Ok(Self::Pong),
            other => Err(PimError::Protocol(format!(
                "unknown control type: 0x{other:02x}"
            ))),
        }
    }
}

/// Multiplexed control message.
///
/// Layout: control_type(1) + body (variable, depends on type)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlFrame {
    /// Request an address lease from a gateway.
    IpRequest {
        /// Node requesting a mesh IP allocation.
        requester_id: NodeId,
    },
    /// Lease configuration returned by a gateway.
    IpAssign {
        /// Assigned mesh IPv4 address.
        assigned_ip: [u8; 4],
        /// CIDR prefix length for the assigned subnet.
        subnet_mask: u8,
        /// Mesh IPv4 address of the serving gateway.
        gateway_ip: [u8; 4],
        /// Lease duration in seconds.
        lease_seconds: u32,
    },
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
}

impl FrameCodec for ControlFrame {
    fn encode(&self, buf: &mut BytesMut) {
        match self {
            ControlFrame::IpRequest { requester_id } => {
                buf.put_u8(ControlType::IpRequest as u8);
                buf.put_slice(requester_id.as_bytes());
            }
            ControlFrame::IpAssign {
                assigned_ip,
                subnet_mask,
                gateway_ip,
                lease_seconds,
            } => {
                buf.put_u8(ControlType::IpAssign as u8);
                buf.put_slice(assigned_ip);
                buf.put_u8(*subnet_mask);
                buf.put_slice(gateway_ip);
                buf.put_u32(*lease_seconds);
            }
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
        }
    }

    fn decode(buf: &mut BytesMut) -> Result<Self, PimError> {
        if buf.is_empty() {
            return Err(PimError::Protocol("control frame empty".into()));
        }

        let control_type = ControlType::from_u8(buf[0])?;

        match control_type {
            ControlType::IpRequest => {
                if buf.len() < 17 {
                    return Err(PimError::Protocol("IpRequest too short".into()));
                }
                let mut id = [0u8; 16];
                id.copy_from_slice(&buf[1..17]);
                buf.advance(17);
                Ok(ControlFrame::IpRequest {
                    requester_id: NodeId::from_bytes(id),
                })
            }
            ControlType::IpAssign => {
                if buf.len() < 14 {
                    // 1 + 4 + 1 + 4 + 4
                    return Err(PimError::Protocol("IpAssign too short".into()));
                }
                let mut assigned_ip = [0u8; 4];
                assigned_ip.copy_from_slice(&buf[1..5]);
                let subnet_mask = buf[5];
                let mut gateway_ip = [0u8; 4];
                gateway_ip.copy_from_slice(&buf[6..10]);
                let lease_seconds = (&buf[10..14]).get_u32();
                buf.advance(14);
                Ok(ControlFrame::IpAssign {
                    assigned_ip,
                    subnet_mask,
                    gateway_ip,
                    lease_seconds,
                })
            }
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
        }
    }
}

#[cfg(test)]
mod tests;
