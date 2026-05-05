//! Userspace NAT / gateway engine.
//!
//! The gateway receives raw IPv4 packets from the mesh, rewrites their source
//! IP/port to the gateway's external address, forwards them to the internet,
//! and routes the responses back to the originating mesh client.

#![warn(missing_docs)]
// Phase A android note: gateway/NAT requires `iptables` (linux) or
// `pfctl` (macos). Neither exists on android, so the gateway role is
// not available there in v1. The engine still compiles to keep the
// workspace building for `aarch64-linux-android`; at runtime
// `setup_masquerade` falls into the explicit unsupported error path
// in `engine.rs` and `gateway.enabled = true` produces a clear
// "platform unsupported" error in logs.
#![cfg_attr(
    not(any(target_os = "linux", target_os = "macos")),
    allow(dead_code, unused_imports, unused_variables)
)]

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
