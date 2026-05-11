//! Core shared types, configuration, and error handling for the mesh.

#![warn(missing_docs)]

pub mod config;
pub mod debug;
pub mod error;
pub mod mesh_address;
pub mod types;

pub use config::{
    AuthorizationPolicy, BluetoothConfig, BluetoothRfcommConfig, BroadcastConfig, Config,
    DiscoveryConfig, MeshConfig, MeshKdfConfig, MeshMode, MessagingConfig, PeerCleanupConfig,
    PeerConfig, PeerEndpointConfig, RelayConfig, SecurityConfig, WifiDirectConfig,
};
pub use debug::*;
pub use error::PimError;
pub use mesh_address::{
    derive_mesh_ipv4, derive_mesh_ipv6, verify_mesh_ipv4, verify_mesh_ipv6, Ipv4Prefix, Ipv6Prefix,
    DEFAULT_MESH_IPV4_PREFIX, DEFAULT_MESH_IPV6_PREFIX,
};
pub use types::{FrameCodec, MeshIp, NodeId};
