use std::net::{IpAddr, SocketAddr, ToSocketAddrs};

use anyhow::{bail, Context, Result};
use pim_core::{
    Config, Ipv4Prefix, Ipv6Prefix, PeerEndpointConfig, DEFAULT_MESH_IPV4_PREFIX,
    DEFAULT_MESH_IPV6_PREFIX,
};
use pim_discovery::NodeCapabilities;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::reconnect::ConnectTarget;

/// If `packet` is an IPv4 ICMP Echo Request, return the corresponding Echo
/// Reply with src/dst swapped and checksums recalculated.  Returns `None` for
/// any other packet type.
pub(crate) fn icmp_echo_reply(packet: &[u8]) -> Option<Vec<u8>> {
    // Minimum: 20-byte IP header + 8-byte ICMP header
    if packet.len() < 28 {
        return None;
    }
    let ihl = ((packet[0] & 0x0f) as usize) * 4;
    if packet.len() < ihl + 8 {
        return None;
    }
    // Protocol must be ICMP (1)
    if packet[9] != 1 {
        return None;
    }
    // ICMP type must be Echo Request (8), code 0
    if packet[ihl] != 8 || packet[ihl + 1] != 0 {
        return None;
    }

    let mut reply = packet.to_vec();

    // Swap src ↔ dst IP addresses (offsets 12..16 and 16..20)
    for i in 0..4 {
        reply.swap(12 + i, 16 + i);
    }

    // Set ICMP type to Echo Reply (0)
    reply[ihl] = 0;
    // Zero ICMP checksum before recalculation
    reply[ihl + 2] = 0;
    reply[ihl + 3] = 0;

    // Recalculate ICMP checksum
    let icmp_data = &reply[ihl..];
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < icmp_data.len() {
        sum += u16::from_be_bytes([icmp_data[i], icmp_data[i + 1]]) as u32;
        i += 2;
    }
    if i < icmp_data.len() {
        sum += (icmp_data[i] as u32) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    let cksum = !(sum as u16);
    reply[ihl + 2] = (cksum >> 8) as u8;
    reply[ihl + 3] = (cksum & 0xff) as u8;

    // Recalculate IP header checksum (src/dst changed)
    reply[10] = 0;
    reply[11] = 0;
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < ihl {
        sum += u16::from_be_bytes([reply[i], reply[i + 1]]) as u32;
        i += 2;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    let cksum = !(sum as u16);
    reply[10] = (cksum >> 8) as u8;
    reply[11] = (cksum & 0xff) as u8;

    Some(reply)
}

/// Resolve `interface.mesh_ipv4_prefix` (with the `pim-core` default
/// when the field is unset) into a canonical [`Ipv4Prefix`].
pub(crate) fn parse_mesh_ipv4_prefix(value: &Option<String>) -> Result<Ipv4Prefix> {
    let raw = value.as_deref().unwrap_or(DEFAULT_MESH_IPV4_PREFIX);
    Ipv4Prefix::parse(raw).map_err(|e| anyhow::anyhow!("{e}"))
}

/// Resolve `interface.mesh_ipv6_prefix` (with the `pim-core` default
/// when the field is unset) into a canonical [`Ipv6Prefix`].
pub(crate) fn parse_mesh_ipv6_prefix(value: &Option<String>) -> Result<Ipv6Prefix> {
    let raw = value.as_deref().unwrap_or(DEFAULT_MESH_IPV6_PREFIX);
    Ipv6Prefix::parse(raw).map_err(|e| anyhow::anyhow!("{e}"))
}

pub(crate) async fn install_signal_handler(cancel: CancellationToken) {
    #[cfg(unix)]
    {
        let mut sigterm =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(sigterm) => sigterm,
                Err(e) => {
                    warn!("failed to install SIGTERM handler: {e}");
                    tokio::signal::ctrl_c().await.ok();
                    info!("shutdown signal received");
                    cancel.cancel();
                    return;
                }
            };

        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = sigterm.recv() => {}
        }
    }

    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await.ok();
    }

    info!("shutdown signal received");
    cancel.cancel();
}

#[derive(Debug, Default)]
pub(crate) struct ResolvedPeerTargets {
    pub(crate) startup_targets: Vec<ConnectTarget>,
    pub(crate) reconnect_targets: Vec<ConnectTarget>,
    pub(crate) bluetooth_static_targets: Vec<SocketAddr>,
}

pub(crate) fn resolve_configured_peer_targets(config: &Config) -> Result<ResolvedPeerTargets> {
    let mut resolved = ResolvedPeerTargets::default();

    for peer in &config.peers {
        match &peer.endpoint {
            PeerEndpointConfig::Tcp { address } => {
                let mut addrs = address
                    .to_socket_addrs()
                    .with_context(|| format!("failed to resolve TCP peer address {address}"))?;
                let addr = addrs.next().with_context(|| {
                    format!("no socket addresses resolved for TCP peer {address}")
                })?;
                let target = ConnectTarget::Tcp(addr);
                resolved.startup_targets.push(target);
                resolved.reconnect_targets.push(target);
            }
            PeerEndpointConfig::Bluetooth { ip } => {
                if !config.bluetooth.enabled {
                    bail!(
                        "bluetooth peer {ip} configured in [[peers]] but [bluetooth].enabled is false"
                    );
                }
                let ip = ip
                    .parse::<IpAddr>()
                    .with_context(|| format!("invalid Bluetooth peer IP {ip}"))?;
                let addr = SocketAddr::new(ip, config.transport.listen_port);
                let target = ConnectTarget::BluetoothPan(addr);
                resolved.reconnect_targets.push(target);
                resolved.bluetooth_static_targets.push(addr);
            }
        }
    }

    Ok(resolved)
}

/// Derive the [`NodeCapabilities`] bitfield from the loaded configuration.
///
/// * Gateway node  → `CLIENT | RELAY | GATEWAY` (bits `0x07`)
/// * Relay node    → `CLIENT | RELAY`            (bits `0x03`)
/// * Client node   → `CLIENT`                    (bits `0x01`)
pub(crate) fn node_capabilities(config: &Config) -> NodeCapabilities {
    if config.gateway.enabled {
        NodeCapabilities::gateway() // CLIENT | RELAY | GATEWAY (0x07)
    } else if config.relay.enabled {
        NodeCapabilities::relay() // CLIENT | RELAY (0x03)
    } else {
        NodeCapabilities::client() // CLIENT (0x01)
    }
}
