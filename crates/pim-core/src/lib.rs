//! Core shared types, configuration, and error handling for the mesh.

#![warn(missing_docs)]

pub mod config;
pub mod debug;
pub mod error;
pub mod types;

pub use config::{
    AuthorizationPolicy, BluetoothConfig, BluetoothRfcommConfig, BroadcastConfig, Config,
    DiscoveryConfig, MessagingConfig, PeerConfig, PeerEndpointConfig, RelayConfig, SecurityConfig,
    WifiDirectConfig,
};
pub use debug::*;
pub use error::PimError;
pub use types::{FrameCodec, MeshIp, NodeId};
