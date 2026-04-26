use bytes::{Buf, BufMut, BytesMut};

use pim_core::PimError;

/// Length-delimited framing for stream transports (TCP).
///
/// Each message is prefixed with a 4-byte big-endian length.
/// This codec handles framing only — it doesn't interpret the payload.
pub struct LengthDelimitedCodec;

/// Maximum frame size: 1 MB.
const MAX_FRAME_SIZE: u32 = 1_048_576;

impl LengthDelimitedCodec {
    /// Wrap a message with a 4-byte length prefix.
    pub fn encode(payload: &[u8], buf: &mut BytesMut) {
        buf.put_u32(payload.len() as u32);
        buf.put_slice(payload);
    }

    /// Try to extract a complete framed message from the buffer.
    ///
    /// Returns:
    /// - `Ok(Some(bytes))` if a complete message was extracted
    /// - `Ok(None)` if more data is needed
    /// - `Err(...)` if the frame is invalid
    pub fn decode(buf: &mut BytesMut) -> Result<Option<BytesMut>, PimError> {
        if buf.len() < 4 {
            return Ok(None);
        }

        let length = (&buf[0..4]).get_u32();

        if length > MAX_FRAME_SIZE {
            return Err(PimError::Protocol(format!(
                "frame too large: {length} bytes, max {MAX_FRAME_SIZE}"
            )));
        }

        let total = 4 + length as usize;
        if buf.len() < total {
            return Ok(None); // need more data
        }

        buf.advance(4); // skip length prefix
        let payload = buf.split_to(length as usize);
        Ok(Some(payload))
    }
}

#[cfg(test)]
mod tests;
