//! Userspace NAT / gateway engine.
//!
//! The gateway receives raw IPv4 packets from the mesh, rewrites their source
//! IP/port to the gateway's external address, forwards them to the internet,
//! and routes the responses back to the originating mesh client.

#![warn(missing_docs)]

mod engine;
pub mod ip_pool;
mod ipv6;

pub use engine::{GatewayEngine, GatewayError, PROTO_ICMP, PROTO_TCP, PROTO_UDP};
pub use ip_pool::{IpPool, IpPoolError, Lease};
pub use ipv6::{GatewayEngineV6, PROTO_ICMPV6};

pub(crate) use engine::{run_cmd, PORT_MAX, PORT_MIN};

#[cfg(target_os = "linux")]
pub(crate) use engine::{check_cmd_quiet, iptables_delete_if_present};

#[cfg(test)]
pub use engine::test_util;
