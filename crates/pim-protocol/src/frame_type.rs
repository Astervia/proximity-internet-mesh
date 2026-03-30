//! Shared discriminator for the outer transport envelope.

use pim_core::PimError;

/// Identifies the type of payload inside a TransportFrame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameType {
    /// Authenticated handshake message.
    Handshake = 0x01,
    /// Mesh data payload.
    Data = 0x02,
    /// Route advertisement payload.
    RouteUpdate = 0x03,
    /// Periodic heartbeat payload.
    Heartbeat = 0x04,
    /// Control-plane request or response payload.
    Control = 0x05,
    /// Packet fragment payload.
    Fragment = 0x06,
}

impl FrameType {
    /// Decode a raw frame-type tag from the wire.
    pub fn from_u8(value: u8) -> Result<Self, PimError> {
        match value {
            0x01 => Ok(Self::Handshake),
            0x02 => Ok(Self::Data),
            0x03 => Ok(Self::RouteUpdate),
            0x04 => Ok(Self::Heartbeat),
            0x05 => Ok(Self::Control),
            0x06 => Ok(Self::Fragment),
            other => Err(PimError::Protocol(format!(
                "unknown frame type: 0x{other:02x}"
            ))),
        }
    }
}
