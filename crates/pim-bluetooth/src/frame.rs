//! Length-prefixed frame codec — `u32 BE | payload (utf-8 JSON)`.
//! Matches `spikes/bt-rfcomm/PROTOCOL.md` byte-for-byte. Reuses the
//! same wire format as `pim-protocol::LengthDelimitedCodec` (the
//! Phase 7 spec doc cites it explicitly). Shared by both
//! [`crate::rfcomm`] and [`crate::coc`].

#![allow(missing_docs)]

/// Maximum payload size — frames larger than this are rejected with
/// `FrameError::TooLarge`. 65 536 mirrors the Mac sidecar limit.
pub const MAX_FRAME_PAYLOAD: usize = 65_536;

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("frame size {size} exceeds max {max}")]
    TooLarge { size: usize, max: usize },
    #[error("frame size 0 is reserved (kept for future framing extensions)")]
    EmptyFrame,
    #[error("incomplete frame (have {have}B, need {need}B)")]
    Incomplete { have: usize, need: usize },
    #[error("payload not valid utf-8: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
}

/// Encode `payload` with the 4-byte BE length prefix.
pub fn encode_frame(payload: &[u8]) -> Result<Vec<u8>, FrameError> {
    if payload.is_empty() {
        return Err(FrameError::EmptyFrame);
    }
    if payload.len() > MAX_FRAME_PAYLOAD {
        return Err(FrameError::TooLarge {
            size: payload.len(),
            max: MAX_FRAME_PAYLOAD,
        });
    }
    let n = payload.len() as u32;
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&n.to_be_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

/// Read frames from a growing byte buffer. Returns the list of
/// complete payloads (drained from `buf`); leaves any incomplete tail
/// in `buf` for the next call.
pub fn decode_frame(buf: &mut Vec<u8>) -> Result<Vec<Vec<u8>>, FrameError> {
    let mut out = Vec::new();
    loop {
        if buf.len() < 4 {
            break;
        }
        let n = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        if n == 0 {
            return Err(FrameError::EmptyFrame);
        }
        if n > MAX_FRAME_PAYLOAD {
            return Err(FrameError::TooLarge {
                size: n,
                max: MAX_FRAME_PAYLOAD,
            });
        }
        if buf.len() < 4 + n {
            break;
        }
        let payload = buf[4..4 + n].to_vec();
        buf.drain(..4 + n);
        out.push(payload);
    }
    Ok(out)
}

#[cfg(test)]
mod codec_tests {
    use super::*;

    #[test]
    fn roundtrip_small() {
        let p = b"{}";
        let mut buf = encode_frame(p).unwrap();
        let frames = decode_frame(&mut buf).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], p);
        assert!(buf.is_empty());
    }

    #[test]
    fn roundtrip_max() {
        let p = vec![b'x'; MAX_FRAME_PAYLOAD];
        let mut buf = encode_frame(&p).unwrap();
        let frames = decode_frame(&mut buf).unwrap();
        assert_eq!(frames[0].len(), MAX_FRAME_PAYLOAD);
    }

    #[test]
    fn rejects_too_large() {
        let p = vec![0u8; MAX_FRAME_PAYLOAD + 1];
        assert!(matches!(encode_frame(&p), Err(FrameError::TooLarge { .. })));
    }

    #[test]
    fn rejects_empty() {
        assert!(matches!(encode_frame(&[]), Err(FrameError::EmptyFrame)));
    }

    #[test]
    fn fragmented_decode() {
        let p = b"hello world";
        let frame = encode_frame(p).unwrap();
        let mut buf: Vec<u8> = Vec::new();
        // feed byte-at-a-time
        for byte in frame.iter() {
            buf.push(*byte);
            let frames = decode_frame(&mut buf).unwrap();
            if !frames.is_empty() {
                assert_eq!(frames[0], p);
                return;
            }
        }
        panic!("never decoded");
    }

    #[test]
    fn multiple_frames_in_buffer() {
        let mut buf: Vec<u8> = Vec::new();
        buf.extend(encode_frame(b"a").unwrap());
        buf.extend(encode_frame(b"bb").unwrap());
        buf.extend(encode_frame(b"ccc").unwrap());
        let frames = decode_frame(&mut buf).unwrap();
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0], b"a");
        assert_eq!(frames[1], b"bb");
        assert_eq!(frames[2], b"ccc");
    }

    #[test]
    fn rejects_zero_length_in_stream() {
        let mut buf = vec![0u8, 0, 0, 0];
        assert!(matches!(
            decode_frame(&mut buf),
            Err(FrameError::EmptyFrame)
        ));
    }
}
