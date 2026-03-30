//! Wire-format frames exchanged across the mesh transport and control planes.
//!
//! These types define the canonical binary layout shared by peers for
//! transport, routing, discovery-related control messages, and packet
//! fragmentation.

#![warn(missing_docs)]

/// Control-plane frames such as IP assignment and keepalive requests.
pub mod control_frame;
/// Encapsulation for mesh-routed IP payloads.
pub mod data_frame;
/// Fragment headers used when a packet exceeds the mesh MTU.
pub mod fragment_frame;
/// Shared discriminator for the outer transport envelope.
pub mod frame_type;
/// Wire representation of the peer handshake messages.
pub mod handshake_frame;
/// Periodic liveness and gateway-metric frames.
pub mod heartbeat_frame;
/// Stream framing used by byte-stream transports such as TCP.
pub mod length_delimited;
pub mod reassembler;
/// Route advertisement frames exchanged between peers.
pub mod route_frame;
/// Authenticated outer transport envelope exchanged on direct links.
pub mod transport_frame;

pub use control_frame::{ControlFrame, ControlType};
pub use data_frame::{DataFlags, MeshDataFrame};
pub use fragment_frame::{fragment_packet, FragmentFrame, MAX_FRAGMENT_PAYLOAD};
pub use frame_type::FrameType;
pub use handshake_frame::{HandshakeFrameType, HandshakeWireFrame};
pub use heartbeat_frame::HeartbeatFrame;
pub use length_delimited::LengthDelimitedCodec;
pub use reassembler::Reassembler;
pub use route_frame::{RouteEntry, RouteUpdateFrame};
pub use transport_frame::TransportFrame;
