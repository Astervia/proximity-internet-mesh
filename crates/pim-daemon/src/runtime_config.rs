use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};

use anyhow::{bail, Context, Result};
use pim_core::{Config, PeerEndpointConfig};
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

pub(crate) fn parse_cidr(s: &str) -> Result<(Ipv4Addr, u8)> {
    let parts: Vec<&str> = s.split('/').collect();
    if parts.len() != 2 {
        bail!("invalid CIDR: {s}");
    }
    let ip: Ipv4Addr = parts[0].parse().context("invalid IP in CIDR")?;
    let prefix: u8 = parts[1].parse().context("invalid prefix in CIDR")?;
    Ok((ip, prefix))
}

pub(crate) fn parse_ipv6_cidr(s: &str) -> Result<(Ipv6Addr, u8)> {
    let parts: Vec<&str> = s.split('/').collect();
    if parts.len() != 2 {
        bail!("invalid CIDR: {s}");
    }
    let ip: Ipv6Addr = parts[0].parse().context("invalid IPv6 in CIDR")?;
    let prefix: u8 = parts[1].parse().context("invalid IPv6 prefix in CIDR")?;
    if prefix > 128 {
        bail!("invalid IPv6 prefix in CIDR");
    }
    Ok((ip, prefix))
}

pub(crate) fn first_host_in_subnet(network: Ipv4Addr, prefix_len: u8) -> Ipv4Addr {
    let n = u32::from(network);
    let mask: u32 = if prefix_len >= 32 {
        0xffff_ffff
    } else {
        !((1u32 << (32 - prefix_len)) - 1)
    };
    Ipv4Addr::from((n & mask) | 1)
}

pub(crate) fn first_host_in_subnet_v6(network: Ipv6Addr, prefix_len: u8) -> Ipv6Addr {
    let n = u128::from(network);
    let mask: u128 = if prefix_len == 0 {
        0
    } else if prefix_len >= 128 {
        u128::MAX
    } else {
        !((1u128 << (128 - prefix_len)) - 1)
    };
    Ipv6Addr::from((n & mask) | 1)
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
