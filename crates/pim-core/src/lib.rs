//! Core shared types, configuration, and error handling for the mesh.

#![warn(missing_docs)]

pub mod config;
pub mod error;
pub mod types;

pub use config::{
    BluetoothConfig, Config, DiscoveryConfig, PeerConfig, PeerEndpointConfig, RelayConfig,
    WifiDirectConfig,
};
pub use error::PimError;
pub use types::{FrameCodec, MeshIp, NodeId};
