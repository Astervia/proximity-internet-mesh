use super::super::*;
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
