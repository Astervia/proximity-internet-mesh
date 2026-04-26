//! IP address pool for automatic mesh-join address assignment.
//!
//! The gateway allocates one IP address per client `NodeId` from a configured
//! subnet, tracks leases, and re-assigns the same address on reconnect.
//!
//! # Example
//!
//! ```
//! use std::net::Ipv4Addr;
//! use pim_gateway::IpPool;
//!
//! let mut pool = IpPool::new(Ipv4Addr::new(10, 77, 0, 0), 24);
//! // First host (.1) is reserved for the gateway itself.
//! let (ip, _) = pool.allocate([1u8; 16]).unwrap();
//! assert_eq!(ip, Ipv4Addr::new(10, 77, 0, 2));
//! ```

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

// ── Error ─────────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
/// Errors returned by gateway IP lease allocation.
pub enum IpPoolError {
    /// No client address remained free in the configured subnet.
    #[error("address pool exhausted (all /{prefix_len} addresses in use)")]
    PoolExhausted {
        /// Prefix length of the exhausted subnet.
        prefix_len: u8,
    },
    /// The prefix length leaves too few usable host addresses for clients.
    #[error("invalid subnet: prefix_len {0} must be <= 30")]
    InvalidSubnet(u8),
}

// ── Lease ─────────────────────────────────────────────────────────────────────

/// A single active IP lease.
#[derive(Debug, Clone)]
pub struct Lease {
    /// IPv4 address assigned to the client.
    pub assigned_ip: Ipv4Addr,
    /// Node identifier that owns this lease.
    pub node_id: [u8; 16],
    /// Lease duration in seconds from `granted_at`.
    pub lease_seconds: u32,
    /// Instant when the current lease term started.
    pub granted_at: Instant,
}

/// Snapshot of the network configuration returned to a client for a lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IpAssignment {
    /// Assigned mesh IPv4 address.
    pub assigned_ip: Ipv4Addr,
    /// Mesh IPv4 address of the serving gateway.
    pub gateway_ip: Ipv4Addr,
    /// CIDR prefix length for the assigned subnet.
    pub subnet_mask: u8,
    /// Lease duration in seconds.
    pub lease_seconds: u32,
}

impl Lease {
    /// Return `true` once the lease duration has elapsed.
    pub fn is_expired(&self) -> bool {
        self.granted_at.elapsed().as_secs() >= self.lease_seconds as u64
    }
}

// ── IpPool ────────────────────────────────────────────────────────────────────

/// Gateway IP address pool.
///
/// Allocates addresses from `network_addr/prefix_len`, skipping:
/// - The network address (offset 0)
/// - The first host address (offset 1) — reserved for the gateway
/// - The broadcast address
pub struct IpPool {
    /// Base network address (e.g. `10.77.0.0`).
    network: u32,
    /// Subnet prefix length (e.g. 24).
    prefix_len: u8,
    /// Highest usable host offset (exclusive). For /24 this is 254.
    max_host: u32,
    /// Default lease duration.
    pub lease_duration: Duration,
    /// node_id → current lease.
    by_node: HashMap<[u8; 16], Lease>,
    /// assigned_ip (as u32 offset) → node_id for reverse look-up.
    by_ip: HashMap<u32, [u8; 16]>,
    /// Next offset to try when allocating (wraps around).
    next_offset: u32,
}

impl IpPool {
    /// Create a pool for `network/prefix_len`.
    ///
    /// The gateway's own address is assumed to be `.1` (offset 1) and is
    /// never allocated to clients.
    pub fn new(network_addr: Ipv4Addr, prefix_len: u8) -> Self {
        let host_bits = 32 - prefix_len as u32;
        let mask: u32 = if prefix_len == 0 {
            0
        } else {
            !((1u32 << host_bits) - 1)
        };
        let network = u32::from(network_addr) & mask;
        // max_host = 2^host_bits - 1 (broadcast is excluded, so last usable = max_host - 1)
        let max_host = (1u32 << host_bits) - 1;

        Self {
            network,
            prefix_len,
            max_host,
            lease_duration: Duration::from_secs(3600),
            by_node: HashMap::new(),
            by_ip: HashMap::new(),
            next_offset: 2, // start at offset 2 (offset 1 = gateway)
        }
    }

    /// Override the default lease duration.
    pub fn with_lease_duration(mut self, duration: Duration) -> Self {
        self.lease_duration = duration;
        self
    }

    /// Allocate (or renew) an IP for `node_id`.
    ///
    /// Returns `(assigned_ip, lease_seconds)`.  If the node already has a
    /// valid lease, its existing IP is returned.  If all slots are taken but
    /// some leases have expired they are freed before retrying once.
    pub fn allocate(&mut self, node_id: [u8; 16]) -> Result<(Ipv4Addr, u32), IpPoolError> {
        if self.prefix_len > 30 {
            return Err(IpPoolError::InvalidSubnet(self.prefix_len));
        }

        // Return existing or renew
        if let Some(existing) = self.by_node.get_mut(&node_id) {
            existing.granted_at = Instant::now();
            return Ok((existing.assigned_ip, existing.lease_seconds));
        }

        // usable client offsets: 2 .. max_host (exclusive broadcast)
        let usable = self.max_host.saturating_sub(2);
        if usable == 0 {
            return Err(IpPoolError::PoolExhausted {
                prefix_len: self.prefix_len,
            });
        }

        let lease_secs = self.lease_duration.as_secs() as u32;

        // Walk up to `usable` slots starting at next_offset
        for _ in 0..usable {
            let offset = self.next_offset;
            // Advance with wrap
            self.next_offset += 1;
            if self.next_offset >= self.max_host {
                self.next_offset = 2;
            }

            if self.by_ip.contains_key(&offset) {
                continue;
            }

            let ip = Ipv4Addr::from(self.network + offset);
            let lease = Lease {
                assigned_ip: ip,
                node_id,
                lease_seconds: lease_secs,
                granted_at: Instant::now(),
            };
            self.by_node.insert(node_id, lease);
            self.by_ip.insert(offset, node_id);
            return Ok((ip, lease_secs));
        }

        // All slots taken — try expiring and retry once
        if self.expire_leases() > 0 {
            return self.allocate(node_id);
        }
        Err(IpPoolError::PoolExhausted {
            prefix_len: self.prefix_len,
        })
    }

    /// Allocate (or renew) an IP and return the full client-facing lease config.
    pub fn allocate_assignment(&mut self, node_id: [u8; 16]) -> Result<IpAssignment, IpPoolError> {
        let (assigned_ip, lease_seconds) = self.allocate(node_id)?;
        Ok(IpAssignment {
            assigned_ip,
            gateway_ip: self.gateway_ip(),
            subnet_mask: self.prefix_len(),
            lease_seconds,
        })
    }

    /// Release a lease by node_id (called on Goodbye or explicit release).
    pub fn release(&mut self, node_id: &[u8; 16]) {
        if let Some(lease) = self.by_node.remove(node_id) {
            let offset = u32::from(lease.assigned_ip) - self.network;
            self.by_ip.remove(&offset);
        }
    }

    /// Look up the current lease for a node.
    pub fn get_lease(&self, node_id: &[u8; 16]) -> Option<&Lease> {
        self.by_node.get(node_id)
    }

    /// Number of active leases.
    pub fn len(&self) -> usize {
        self.by_node.len()
    }

    /// Return `true` if no active leases are present.
    pub fn is_empty(&self) -> bool {
        self.by_node.is_empty()
    }

    /// Remove expired leases.  Returns how many were removed.
    pub fn expire_leases(&mut self) -> usize {
        let expired: Vec<[u8; 16]> = self
            .by_node
            .values()
            .filter(|l| l.is_expired())
            .map(|l| l.node_id)
            .collect();
        for id in &expired {
            self.release(id);
        }
        expired.len()
    }

    /// Return the gateway's IP address (offset 1 in the subnet).
    pub fn gateway_ip(&self) -> Ipv4Addr {
        Ipv4Addr::from(self.network + 1)
    }

    /// Subnet prefix length.
    pub fn prefix_len(&self) -> u8 {
        self.prefix_len
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
