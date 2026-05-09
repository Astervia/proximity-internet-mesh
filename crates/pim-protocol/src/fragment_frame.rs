//! Fragmentation of large mesh data payloads.
//!
//! Large IP packets are split into `FragmentFrame`s before being encapsulated
//! in a `MeshDataFrame` so that each hop stays within the mesh MTU.
//!
//! # Wire format
//!
//! ```text
//! fragment_id   (4 bytes, big-endian u32)
//! fragment_offset (2 bytes, big-endian u16)  — byte offset in original packet
//! total_length  (2 bytes, big-endian u16)   — total original packet length
//! payload       (variable)
//! ```

/// Maximum payload bytes per fragment (leaves room for outer headers).
pub const MAX_FRAGMENT_PAYLOAD: usize = 1200;

/// Size of the fixed fragment header in bytes.
pub const FRAGMENT_HEADER_SIZE: usize = 8;

/// A single fragment of a larger IP packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FragmentFrame {
    /// Identifies the original packet being fragmented (monotonically increasing
    /// per sender; unique enough within the reassembly timeout window).
    pub fragment_id: u32,
    /// Byte offset of `payload` within the original packet.
    pub fragment_offset: u16,
    /// Total byte length of the original (unfragmented) packet.
    pub total_length: u16,
    /// Fragment payload.
    pub payload: bytes::Bytes,
}

impl FragmentFrame {
    /// Serialize to wire bytes.
    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(FRAGMENT_HEADER_SIZE + self.payload.len());
        buf.extend_from_slice(&self.fragment_id.to_be_bytes());
        buf.extend_from_slice(&self.fragment_offset.to_be_bytes());
        buf.extend_from_slice(&self.total_length.to_be_bytes());
        buf.extend_from_slice(&self.payload);
        buf
    }

    /// Deserialize from wire bytes.  Returns `None` if the buffer is too short
    /// or the contained offsets are nonsensical.
    pub fn deserialize(buf: &[u8]) -> Option<Self> {
        if buf.len() < FRAGMENT_HEADER_SIZE {
            return None;
        }
        let fragment_id = u32::from_be_bytes(buf[0..4].try_into().ok()?);
        let fragment_offset = u16::from_be_bytes(buf[4..6].try_into().ok()?);
        let total_length = u16::from_be_bytes(buf[6..8].try_into().ok()?);
        let payload = bytes::Bytes::copy_from_slice(&buf[8..]);

        // Basic sanity checks.
        let end = (fragment_offset as usize).checked_add(payload.len())?;
        if end > total_length as usize {
            return None;
        }

        Some(Self {
            fragment_id,
            fragment_offset,
            total_length,
            payload,
        })
    }

    /// `true` if this is the last fragment (its data reaches the end of the
    /// original packet).
    pub fn is_last(&self) -> bool {
        self.fragment_offset as usize + self.payload.len() == self.total_length as usize
    }
}

/// Split `data` into [`FragmentFrame`]s using `fragment_id` as the shared
/// identifier.  Each fragment carries at most [`MAX_FRAGMENT_PAYLOAD`] bytes.
///
/// Returns an empty `Vec` only when `data` is empty.
pub fn fragment_packet(data: bytes::Bytes, fragment_id: u32) -> Vec<FragmentFrame> {
    if data.is_empty() {
        return Vec::new();
    }

    let total_length = data.len() as u16;
    let mut frames = Vec::new();
    let mut offset = 0usize;

    while offset < data.len() {
        let end = (offset + MAX_FRAGMENT_PAYLOAD).min(data.len());
        frames.push(FragmentFrame {
            fragment_id,
            fragment_offset: offset as u16,
            total_length,
            payload: data.slice(offset..end),
        });
        offset = end;
    }

    frames
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
