//! Wi-Fi Direct P2P group state and IP resolution.
//!
//! After `wpa_supplicant` completes GO/GC negotiation it creates a new network
//! interface (e.g. `p2p-wlan0-0`).  This module resolves the IP addresses on
//! both sides of the newly formed group so the caller can build a `SocketAddr`
//! and hand it to the connection initiator.

use crate::{wpa_cli::WpaCliController, WifiDirectError};
use std::net::Ipv4Addr;
use tokio::time::{sleep, Duration};

/// The role this node plays in the P2P group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupRole {
    /// Group Owner — acts as the soft AP, runs DHCP for group clients.
    Go,
    /// Group Client — connects to the GO and receives a DHCP-assigned IP.
    Gc,
}

/// Well-known IP address assigned to the Group Owner interface by wpa_supplicant
/// on Linux.  The GC always connects to the GO at this address.
pub const GO_INTERFACE_IP: Ipv4Addr = Ipv4Addr::new(192, 168, 49, 1);

/// Represents a live Wi-Fi Direct P2P group.
#[derive(Debug, Clone)]
pub struct WifiDirectGroup {
    /// Name of the P2P group interface (e.g. `p2p-wlan0-0`).
    pub group_iface: String,
    /// Role this node plays in the group.
    pub role: GroupRole,
    /// IPv4 address assigned to this node's group interface.
    pub own_ip: Ipv4Addr,
    /// IPv4 address of the remote peer in the group, if already known.
    ///
    /// - **GC**: always `Some(GO_INTERFACE_IP)` immediately after group formation.
    /// - **GO**: resolved by reading the ARP table once the GC obtains a DHCP lease.
    pub peer_ip: Option<Ipv4Addr>,
}

impl WifiDirectGroup {
    /// Build a `WifiDirectGroup` from a newly-appeared P2P interface.
    ///
    /// Reads the interface IP via `ip addr show` and, for the GO role, polls
    /// `/proc/net/arp` for up to `arp_timeout` to find the GC's assigned IP.
    pub async fn from_iface(
        ctrl: &WpaCliController,
        iface: &str,
        role: GroupRole,
        arp_timeout: Duration,
    ) -> Result<Self, WifiDirectError> {
        // Wait for the interface to get an IP (DHCP may take a moment for GC).
        let own_ip = wait_for_iface_ip(ctrl, iface, Duration::from_secs(15)).await?;

        let peer_ip = match role {
            GroupRole::Gc => Some(GO_INTERFACE_IP),
            GroupRole::Go => {
                // Poll ARP table until the GC's IP appears.
                poll_arp_for_peer(ctrl, iface, arp_timeout).await
            }
        };

        Ok(Self {
            group_iface: iface.to_string(),
            role,
            own_ip,
            peer_ip,
        })
    }
}

/// Poll `ip addr show <iface>` until an IPv4 address appears or timeout expires.
async fn wait_for_iface_ip(
    ctrl: &WpaCliController,
    iface: &str,
    timeout: Duration,
) -> Result<Ipv4Addr, WifiDirectError> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Ok(Some(addr)) = ctrl.iface_ipv4(iface).await {
            return Ok(addr);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(WifiDirectError::GroupFormation(format!(
                "interface {iface} did not get an IP within {timeout:?}"
            )));
        }
        sleep(Duration::from_millis(500)).await;
    }
}

/// Poll `/proc/net/arp` on `iface` until at least one peer IP appears.
async fn poll_arp_for_peer(
    ctrl: &WpaCliController,
    iface: &str,
    timeout: Duration,
) -> Option<Ipv4Addr> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Ok(peers) = ctrl.arp_peers_on_iface(iface).await {
            if let Some(ip) = peers.into_iter().next() {
                return Some(ip);
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        sleep(Duration::from_secs(1)).await;
    }
}

#[cfg(test)]
mod tests;
