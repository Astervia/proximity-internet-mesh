use pim_core::PimError;

/// Identifies the type of payload inside a TransportFrame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameType {
    Handshake = 0x01,
    Data = 0x02,
    RouteUpdate = 0x03,
    Heartbeat = 0x04,
    Control = 0x05,
    Fragment = 0x06,
}

impl FrameType {
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
