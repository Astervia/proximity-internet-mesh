//! macOS Wi-Fi Direct backend built on Bonjour peer-to-peer browsing.
//!
//! Apple does not expose Linux-style P2P group control on macOS. Instead, the
//! supported path for peer-to-peer Wi-Fi is DNS-SD/Bonjour on the dedicated
//! peer-to-peer interface. This module advertises the daemon's TCP listen port,
//! browses for matching peers, resolves their socket addresses, and hands those
//! addresses back to the daemon.

use crate::{WifiDirectError, WifiDirectServiceType};
use async_dnssd::{
    browse_extended, register_extended, BrowseData, BrowseResult, BrowsedFlags, Interface,
    RegisterData, ResolveHostResult,
};
use futures_util::StreamExt;
use std::{collections::HashSet, net::SocketAddr, time::Duration};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

const RESOLVE_TIMEOUT: Duration = Duration::from_secs(10);
const ADDRESS_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) async fn run(
    node_name: String,
    listen_port: u16,
    peer_tx: mpsc::Sender<SocketAddr>,
    cancel: CancellationToken,
) {
    info!("Wi-Fi Direct on macOS using Bonjour peer-to-peer discovery");

    let registration = match register_extended(
        WifiDirectServiceType::REG_TYPE,
        listen_port,
        RegisterData {
            interface: Interface::PeerToPeer,
            name: Some(&node_name),
            ..Default::default()
        },
    ) {
        Ok(register) => register,
        Err(err) => {
            warn!("Wi-Fi Direct: failed to start macOS service registration: {err}");
            return;
        }
    };

    let (_registration, registered) = match registration.await {
        Ok(result) => result,
        Err(err) => {
            warn!("Wi-Fi Direct: macOS service registration failed: {err}");
            return;
        }
    };
    info!(
        service_name = %registered.name,
        reg_type = %registered.reg_type,
        "Wi-Fi Direct macOS service registered"
    );

    let mut seen_services = HashSet::new();
    let mut browse = Box::pin(browse_extended(
        WifiDirectServiceType::REG_TYPE,
        BrowseData {
            interface: Interface::PeerToPeer,
            ..Default::default()
        },
    ));

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                debug!("Wi-Fi Direct macOS discovery cancelled");
                return;
            }
            next = browse.next() => {
                let Some(event) = next else {
                    warn!("Wi-Fi Direct: macOS browse stream ended unexpectedly");
                    return;
                };
                match event {
                    Ok(service) => {
                        handle_service_event(
                            &registered.name,
                            service,
                            &mut seen_services,
                            &peer_tx,
                        ).await;
                    }
                    Err(err) => {
                        warn!("Wi-Fi Direct: macOS browse error: {err}");
                    }
                }
            }
        }
    }
}

async fn handle_service_event(
    own_service_name: &str,
    service: BrowseResult,
    seen_services: &mut HashSet<String>,
    peer_tx: &mpsc::Sender<SocketAddr>,
) {
    let key = service_key(&service.service_name, &service.domain);

    if !service.flags.contains(BrowsedFlags::ADD) {
        seen_services.remove(&key);
        debug!(service = %key, "Wi-Fi Direct: macOS peer removed");
        return;
    }

    if service.service_name == own_service_name {
        debug!(service = %key, "Wi-Fi Direct: ignoring local macOS service advertisement");
        return;
    }

    if !seen_services.insert(key.clone()) {
        return;
    }

    info!(service = %key, "Wi-Fi Direct: macOS peer discovered");
    match resolve_peer_addr(service).await {
        Ok(addr) => {
            info!(addr = %addr, "Wi-Fi Direct: macOS peer resolved");
            let _ = peer_tx.send(addr).await;
        }
        Err(err) => {
            warn!("Wi-Fi Direct: failed to resolve macOS peer {key}: {err}");
            seen_services.remove(&key);
        }
    }
}

async fn resolve_peer_addr(service: BrowseResult) -> Result<SocketAddr, WifiDirectError> {
    let mut resolve = Box::pin(service.resolve());

    loop {
        let next = tokio::time::timeout(RESOLVE_TIMEOUT, resolve.next())
            .await
            .map_err(|_| {
                WifiDirectError::Bonjour(format!(
                    "timed out resolving service {}",
                    service.service_name
                ))
            })?;

        let Some(result) = next else {
            return Err(WifiDirectError::Bonjour(format!(
                "service {} resolved without addresses",
                service.service_name
            )));
        };

        let resolved = result.map_err(|err| {
            WifiDirectError::Bonjour(format!(
                "resolve failed for {}: {err}",
                service.service_name
            ))
        })?;
        if let Some(addr) = resolve_socket_addr(resolved).await? {
            return Ok(addr);
        }
    }
}

async fn resolve_socket_addr(
    resolved: async_dnssd::ResolveResult,
) -> Result<Option<SocketAddr>, WifiDirectError> {
    let mut addresses = Box::pin(resolved.resolve_socket_address());
    loop {
        let next = tokio::time::timeout(ADDRESS_TIMEOUT, addresses.next())
            .await
            .map_err(|_| {
                WifiDirectError::Bonjour(format!(
                    "timed out resolving host {}",
                    resolved.host_target
                ))
            })?;

        let Some(result) = next else {
            return Ok(None);
        };

        let resolved_host = result.map_err(|err| {
            WifiDirectError::Bonjour(format!(
                "host resolution failed for {}: {err}",
                resolved.host_target
            ))
        })?;

        if let Some(addr) = socket_addr_from_host(resolved_host) {
            return Ok(Some(addr));
        }
    }
}

fn socket_addr_from_host(result: ResolveHostResult) -> Option<SocketAddr> {
    if result.flags.contains(async_dnssd::ResolvedHostFlags::ADD) {
        Some(result.address.into())
    } else {
        None
    }
}

fn service_key(service_name: &str, domain: &str) -> String {
    format!("{service_name}@{domain}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_dnssd::{ResolvedHostFlags, ScopedSocketAddr};
    use std::net::{IpAddr, Ipv4Addr};

    #[test]
    fn service_key_is_stable() {
        assert_eq!(service_key("node-a", "local."), "node-a@local.");
    }

    #[test]
    fn service_key_distinguishes_domains() {
        assert_ne!(
            service_key("node-a", "local."),
            service_key("node-a", "example.com.")
        );
    }

    #[test]
    fn socket_addr_from_host_ignores_removed_records() {
        let result = ResolveHostResult {
            flags: ResolvedHostFlags::empty(),
            address: ScopedSocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 9100, 0),
        };
        assert!(socket_addr_from_host(result).is_none());
    }

    #[test]
    fn socket_addr_from_host_returns_added_records() {
        let result = ResolveHostResult {
            flags: ResolvedHostFlags::ADD,
            address: ScopedSocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 9100, 0),
        };
        assert_eq!(
            socket_addr_from_host(result),
            Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 9100))
        );
    }
}
