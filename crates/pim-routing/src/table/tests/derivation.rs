//! Tests for the derivation-driven mesh-IP reverse index.
//!
//! These guard the contract that mesh IPs are computed from a peer's
//! `NodeId` (not trusted from advertisements), and that collisions
//! degrade gracefully via "first writer wins".

use std::net::Ipv4Addr;

use super::*;

#[test]
fn add_route_populates_derived_mesh_ip() {
    let prefix = Ipv4Prefix::parse("10.77.0.0/16").unwrap();
    let mut t = RoutingTable::new(id(1), false, prefix);
    let peer = id(2);
    t.add_peer(peer);

    let derived = derive_mesh_ipv4(&peer, prefix);
    let entry = t
        .routes_snapshot()
        .into_iter()
        .find(|(d, _)| *d == peer)
        .map(|(_, e)| e)
        .expect("route exists");
    assert_eq!(entry.mesh_ip, Some(derived));

    let (resolved_dst, _next_hop) = t.lookup_mesh_ip(derived).expect("reverse lookup hits");
    assert_eq!(resolved_dst, peer);
}

#[test]
fn apply_update_ignores_advertised_mesh_ip_value() {
    // Peer advertises `mesh_ip = 10.99.0.0` (outside our prefix); the
    // routing table must store the **derived** value instead.
    let prefix = Ipv4Prefix::parse("10.77.0.0/16").unwrap();
    let mut t = super::new_table(id(1), false);
    t.set_ipv4_prefix(prefix);
    let from = id(7);
    let dst = id(8);
    let frame = RouteUpdateFrame {
        origin_id: from,
        sequence: 1,
        entries: vec![
            RouteEntry {
                destination: from,
                hops: 0,
                flags: 0,
                mesh_ip: [10, 77, 0, 7],
            },
            RouteEntry {
                destination: dst,
                hops: 1,
                flags: 0,
                mesh_ip: [10, 99, 0, 0], // intentionally bogus
            },
        ],
        signature: [0u8; 64],
    };
    assert_eq!(t.apply_update(&frame, from), UpdateResult::Changed);

    let entry = t
        .routes_snapshot()
        .into_iter()
        .find(|(d, _)| *d == dst)
        .map(|(_, e)| e)
        .expect("route exists");
    assert_eq!(entry.mesh_ip, Some(derive_mesh_ipv4(&dst, prefix)));
    assert_eq!(t.lookup_mesh_ip(Ipv4Addr::new(10, 99, 0, 0)), None);
}

#[test]
fn collision_is_recorded_first_writer_wins() {
    // Use a tiny `/30` prefix so two synthetic NodeIds derive into
    // the same host slot with high probability. We brute-force the
    // first IDs that collide.
    let prefix = Ipv4Prefix::parse("192.0.2.0/30").unwrap();
    let mut a = 0u8;
    let mut b = 0u8;
    'outer: for x in 0u8..255 {
        for y in (x + 1)..=255 {
            if derive_mesh_ipv4(&id(x), prefix) == derive_mesh_ipv4(&id(y), prefix) {
                a = x;
                b = y;
                break 'outer;
            }
        }
    }
    assert!(
        a != b,
        "expected to find a derivation collision inside /30; check the test prefix"
    );

    let mut t = RoutingTable::new(id(0), false, prefix);
    t.add_peer(id(a));
    t.add_peer(id(b));

    let collision_ip = derive_mesh_ipv4(&id(a), prefix);
    let (resolved, _) = t.lookup_mesh_ip(collision_ip).expect("first-writer entry");
    assert_eq!(resolved, id(a), "first writer should keep the index");
    assert_eq!(t.mesh_ip_collisions_total(), 1);
}
