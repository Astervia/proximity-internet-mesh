//! Deterministic mesh address derivation from `NodeId`.
//!
//! Every daemon — gateway or not — computes its own mesh IPv4 / IPv6
//! address at boot from `derive_mesh_ipv4(self_id, prefix)` /
//! `derive_mesh_ipv6(self_id, prefix)`. The same function on every
//! node makes self-assignment offline (no `IpRequest` round trip, no
//! gateway-side pool, no leases) and lets the routing table populate
//! its mesh-IP reverse index from a peer's `NodeId` alone.
//!
//! Hashing uses `blake3::keyed_hash` with a 32-byte key derived from
//! a domain-separating context string (`"pim-mesh-v4"` / `"pim-mesh-v6"`).
//! Different contexts yield independent address streams, so v4 and v6
//! collisions are uncorrelated.
//!
//! Reserved hosts (`0` and `all-ones`) are skipped: the network and
//! broadcast addresses inside the configured prefix are nudged to a
//! valid host so the derived address is always usable.

use std::net::{Ipv4Addr, Ipv6Addr};
use std::str::FromStr;

use crate::{NodeId, PimError};

/// Default IPv4 mesh prefix used when no `interface.mesh_ipv4_prefix`
/// is set in `pim.toml`. A `/16` gives ~65k host slots — about 300
/// nodes before a 50% birthday-bound collision; widen via config for
/// denser deployments.
pub const DEFAULT_MESH_IPV4_PREFIX: &str = "10.77.0.0/16";

/// Default IPv6 mesh prefix used when no `interface.mesh_ipv6_prefix`
/// is set in `pim.toml`. `/64` is collision-free at our scale.
pub const DEFAULT_MESH_IPV6_PREFIX: &str = "fd77::/64";

const CONTEXT_V4: &str = "pim-mesh-v4";
const CONTEXT_V6: &str = "pim-mesh-v6";

/// IPv4 mesh-prefix in `(network, prefix_len)` form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Ipv4Prefix {
    /// Network address of the prefix (host bits zeroed).
    pub network: Ipv4Addr,
    /// CIDR prefix length, `0..=32`.
    pub prefix_len: u8,
}

/// IPv6 mesh-prefix in `(network, prefix_len)` form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Ipv6Prefix {
    /// Network address of the prefix (host bits zeroed).
    pub network: Ipv6Addr,
    /// CIDR prefix length, `0..=128`.
    pub prefix_len: u8,
}

impl Ipv4Prefix {
    /// Parse a CIDR string (e.g. `"10.77.0.0/16"`). The IP portion is
    /// canonicalised by zeroing the host bits.
    pub fn parse(s: &str) -> Result<Self, PimError> {
        let (ip_str, prefix_str) = s
            .split_once('/')
            .ok_or_else(|| PimError::Config(format!("invalid IPv4 CIDR (missing `/`): {s}")))?;
        let ip: Ipv4Addr = ip_str
            .parse()
            .map_err(|_| PimError::Config(format!("invalid IPv4 in CIDR: {s}")))?;
        let prefix_len: u8 = prefix_str
            .parse()
            .map_err(|_| PimError::Config(format!("invalid prefix length in CIDR: {s}")))?;
        if prefix_len > 32 {
            return Err(PimError::Config(format!(
                "IPv4 prefix length must be 0..=32: {s}"
            )));
        }
        let mask = ipv4_netmask(prefix_len);
        let network = Ipv4Addr::from(u32::from(ip) & mask);
        Ok(Self {
            network,
            prefix_len,
        })
    }

    /// Format back to `network/prefix_len`.
    pub fn to_cidr_string(&self) -> String {
        format!("{}/{}", self.network, self.prefix_len)
    }
}

impl FromStr for Ipv4Prefix {
    type Err = PimError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl Ipv6Prefix {
    /// Parse a CIDR string (e.g. `"fd77::/64"`). The IP portion is
    /// canonicalised by zeroing the host bits.
    pub fn parse(s: &str) -> Result<Self, PimError> {
        let (ip_str, prefix_str) = s
            .split_once('/')
            .ok_or_else(|| PimError::Config(format!("invalid IPv6 CIDR (missing `/`): {s}")))?;
        let ip: Ipv6Addr = ip_str
            .parse()
            .map_err(|_| PimError::Config(format!("invalid IPv6 in CIDR: {s}")))?;
        let prefix_len: u8 = prefix_str
            .parse()
            .map_err(|_| PimError::Config(format!("invalid prefix length in CIDR: {s}")))?;
        if prefix_len > 128 {
            return Err(PimError::Config(format!(
                "IPv6 prefix length must be 0..=128: {s}"
            )));
        }
        let mask = ipv6_netmask(prefix_len);
        let network = Ipv6Addr::from(u128::from(ip) & mask);
        Ok(Self {
            network,
            prefix_len,
        })
    }

    /// Format back to `network/prefix_len`.
    pub fn to_cidr_string(&self) -> String {
        format!("{}/{}", self.network, self.prefix_len)
    }
}

impl FromStr for Ipv6Prefix {
    type Err = PimError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

fn ipv4_netmask(prefix_len: u8) -> u32 {
    if prefix_len == 0 {
        0
    } else if prefix_len >= 32 {
        u32::MAX
    } else {
        !((1u32 << (32 - prefix_len)) - 1)
    }
}

fn ipv6_netmask(prefix_len: u8) -> u128 {
    if prefix_len == 0 {
        0
    } else if prefix_len >= 128 {
        u128::MAX
    } else {
        !((1u128 << (128 - prefix_len)) - 1)
    }
}

fn keyed_digest(context: &str, node_id: &NodeId) -> [u8; 32] {
    let key = blake3::derive_key(context, b"pim-mesh-derivation");
    blake3::keyed_hash(&key, node_id.as_bytes()).into()
}

/// Derive this node's mesh IPv4 address inside `prefix` from its `NodeId`.
///
/// Two nodes with different `NodeId`s can still collide on the same
/// IPv4 address when the prefix is small (a `/16` has ~300-node 50%
/// birthday bound). Collisions degrade gracefully via the routing
/// table's "first writer wins" reverse-lookup; deployments that care
/// should leave IPv6 enabled (collision-free at 64 bits).
pub fn derive_mesh_ipv4(node_id: &NodeId, prefix: Ipv4Prefix) -> Ipv4Addr {
    let host_bits = 32u32.saturating_sub(prefix.prefix_len as u32);
    let host_mask: u32 = if host_bits == 0 {
        0
    } else if host_bits >= 32 {
        u32::MAX
    } else {
        (1u32 << host_bits) - 1
    };
    if host_bits == 0 {
        return prefix.network;
    }

    let digest = keyed_digest(CONTEXT_V4, node_id);
    let raw = u32::from_be_bytes(digest[..4].try_into().expect("blake3 digest >= 4 bytes"));
    let mut host = raw & host_mask;
    // Reserve host = 0 (network) and host = all-ones (broadcast).
    if host_bits >= 2 {
        if host == 0 {
            host = 1;
        } else if host == host_mask {
            host = host_mask - 1;
        }
    }
    let network = u32::from(prefix.network) & !host_mask;
    Ipv4Addr::from(network | host)
}

/// Derive this node's mesh IPv6 address inside `prefix` from its `NodeId`.
///
/// At a default `/64` prefix the host space is 64 bits, so birthday
/// collisions are not a practical concern at PIM scale.
pub fn derive_mesh_ipv6(node_id: &NodeId, prefix: Ipv6Prefix) -> Ipv6Addr {
    let host_bits = 128u32.saturating_sub(prefix.prefix_len as u32);
    let host_mask: u128 = if host_bits == 0 {
        0
    } else if host_bits >= 128 {
        u128::MAX
    } else {
        (1u128 << host_bits) - 1
    };
    if host_bits == 0 {
        return prefix.network;
    }

    let digest = keyed_digest(CONTEXT_V6, node_id);
    let raw = u128::from_be_bytes(digest[..16].try_into().expect("blake3 digest >= 16 bytes"));
    let mut host = raw & host_mask;
    if host_bits >= 2 {
        if host == 0 {
            host = 1;
        } else if host == host_mask {
            host = host_mask - 1;
        }
    }
    let network = u128::from(prefix.network) & !host_mask;
    Ipv6Addr::from(network | host)
}

/// Returns `true` when `claimed` matches `derive_mesh_ipv4(node_id, prefix)`.
///
/// Used to cross-check a peer's advertised mesh address against the
/// canonical derivation — mismatches are logged at WARN, not fatal.
pub fn verify_mesh_ipv4(node_id: &NodeId, claimed: Ipv4Addr, prefix: Ipv4Prefix) -> bool {
    derive_mesh_ipv4(node_id, prefix) == claimed
}

/// IPv6 counterpart of [`verify_mesh_ipv4`].
pub fn verify_mesh_ipv6(node_id: &NodeId, claimed: Ipv6Addr, prefix: Ipv6Prefix) -> bool {
    derive_mesh_ipv6(node_id, prefix) == claimed
}

#[cfg(test)]
mod tests;
