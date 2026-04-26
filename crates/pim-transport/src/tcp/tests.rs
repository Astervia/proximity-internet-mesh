use super::*;
use pim_protocol::FrameType;

mod error_paths;
mod lifecycle;
mod send_recv;

fn make_frame(data: &[u8]) -> TransportFrame {
    TransportFrame {
        frame_type: FrameType::Data,
        nonce: [0; 12],
        payload: data.to_vec(),
        tag: [0; 16],
    }
}
