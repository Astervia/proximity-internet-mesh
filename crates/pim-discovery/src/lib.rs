//! Peer discovery for the proximity-internet mesh.
//!
//! Nodes announce themselves on the local network by broadcasting a compact
//! UDP advertisement packet.  Any node that receives an advertisement from an
//! unknown peer can connect to it via the `pim-transport` layer.
//!
//! # Discovery advertisement wire format
//!
//! ```text
//! magic(4)       — 0x50494D44 ("PIMD")
//! version(1)     — 0x01
//! node_id(16)    — sender's NodeId
//! public_key(32) — sender's Ed25519 public key bytes
//! capabilities(1)— bitfield: 0x01=client 0x02=relay 0x04=gateway
//! listen_port(2) — big-endian u16 TCP listen port
//! ── total: 56 bytes ──
//! ```

pub mod advertisement;
pub mod peer_table;
pub mod service;

pub use advertisement::{DiscoveryAdvertisement, NodeCapabilities};
pub use peer_table::{PeerRecord, PeerTable};
pub use service::DiscoveryService;
