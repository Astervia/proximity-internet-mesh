use super::super::*;
use super::{advertisement, id, mesh_ip};

#[test]
fn replay_rejected_same_sequence() {
    let a = id(1);
    let b = id(2);
    let c = id(3);

    let mut rt = super::new_table(a, false);
    rt.add_peer(b);

    let upd = advertisement(b, 5, vec![(c, 1, false)]);
    assert_eq!(rt.apply_update(&upd, b), UpdateResult::Changed);

    // Same seq again — replay
    let upd2 = advertisement(b, 5, vec![(c, 1, false)]);
    assert_eq!(rt.apply_update(&upd2, b), UpdateResult::Unchanged);
}

#[test]
fn replay_rejected_lower_sequence() {
    let a = id(1);
    let b = id(2);
    let c = id(3);

    let mut rt = super::new_table(a, false);
    rt.add_peer(b);

    rt.apply_update(&advertisement(b, 10, vec![(c, 1, false)]), b);
    // Seq 5 < 10 — replay
    let result = rt.apply_update(&advertisement(b, 5, vec![(c, 1, false)]), b);
    assert_eq!(result, UpdateResult::Unchanged);
}

#[test]
fn sequence_window_resets_when_peer_rejoins() {
    let a = id(1);
    let b = id(2);
    let c = id(3);

    let mut rt = super::new_table(a, false);
    rt.add_peer(b);
    assert_eq!(
        rt.apply_update(&advertisement(b, 10, vec![(c, 1, false)]), b),
        UpdateResult::Changed
    );

    rt.remove_peer(b);
    rt.add_peer(b);

    assert_eq!(
        rt.apply_update(&advertisement(b, 1, vec![(c, 1, false)]), b),
        UpdateResult::Changed,
        "rejoined peer should be allowed to restart its route sequence"
    );
}

#[test]
fn higher_sequence_accepted() {
    let a = id(1);
    let b = id(2);
    let c = id(3);

    let mut rt = super::new_table(a, false);
    rt.add_peer(b);

    rt.apply_update(&advertisement(b, 1, vec![(c, 2, false)]), b);
    let result = rt.apply_update(&advertisement(b, 2, vec![(c, 1, false)]), b);
    assert_eq!(result, UpdateResult::Changed);
    assert_eq!(rt.routes[&c].hops, 2); // 1 advertised + 1 for the hop to b
}

#[test]
fn origin_mismatch_rejected() {
    let a = id(1);
    let b = id(2);
    let c = id(3);
    let impostor = id(99);

    let mut rt = super::new_table(a, false);
    rt.add_peer(b);

    // Frame claims origin=impostor but is sent by b
    let upd = RouteUpdateFrame {
        origin_id: impostor,
        sequence: 1,
        entries: vec![RouteEntry {
            destination: c,
            hops: 1,
            flags: 0,
            mesh_ip: mesh_ip(3),
        }],
        signature: [0u8; 64],
    };
    let result = rt.apply_update(&upd, b);
    assert_eq!(
        result,
        UpdateResult::Unchanged,
        "mismatched origin must be rejected"
    );
    assert!(rt.lookup(c).is_none());
}

#[test]
fn zero_hop_to_non_self_rejected() {
    let a = id(1);
    let b = id(2);
    let victim = id(42);

    let mut rt = super::new_table(a, false);
    rt.add_peer(b);

    // b claims to be 0 hops from victim (impossible unless b == victim)
    let upd = RouteUpdateFrame {
        origin_id: b,
        sequence: 1,
        entries: vec![RouteEntry {
            destination: victim,
            hops: 0,
            flags: 0,
            mesh_ip: mesh_ip(42),
        }],
        signature: [0u8; 64],
    };
    let result = rt.apply_update(&upd, b);
    assert_eq!(
        result,
        UpdateResult::Unchanged,
        "0-hop claim for non-self must be rejected"
    );
}

#[test]
fn zero_hop_self_entry_allowed() {
    let a = id(1);
    let b = id(2);
    let c = id(3);

    let mut rt = super::new_table(a, false);
    rt.add_peer(b);

    // b claims 0 hops to itself (normal — every advertisement includes this)
    let upd = RouteUpdateFrame {
        origin_id: b,
        sequence: 1,
        entries: vec![
            RouteEntry {
                destination: b,
                hops: 0,
                flags: 0,
                mesh_ip: mesh_ip(2),
            }, // self-entry — OK
            RouteEntry {
                destination: c,
                hops: 1,
                flags: 0,
                mesh_ip: mesh_ip(3),
            },
        ],
        signature: [0u8; 64],
    };
    let result = rt.apply_update(&upd, b);
    assert_eq!(
        result,
        UpdateResult::Changed,
        "self zero-hop entry should be accepted"
    );
    assert!(rt.lookup(c).is_some());
}

#[test]
fn blacklisted_peer_route_invisible() {
    let a = id(1);
    let b = id(2);
    let c = id(3);

    let mut rt = super::new_table(a, false);
    rt.add_peer(b);
    rt.apply_update(&advertisement(b, 1, vec![(c, 1, false)]), b);

    assert!(rt.lookup(c).is_some());

    rt.blacklist_peer(b);
    assert!(rt.lookup(b).is_none(), "blacklisted peer itself invisible");
    assert!(
        rt.lookup(c).is_none(),
        "routes via blacklisted peer invisible"
    );
}

#[test]
fn blacklisted_gateway_excluded_from_selection() {
    let a = id(1);
    let gw1 = id(10);
    let gw2 = id(11);

    let mut rt = super::new_table(a, false);
    rt.add_peer(gw1);
    rt.add_peer(gw2);
    rt.apply_update(&advertisement(gw1, 1, vec![(gw1, 0, true)]), gw1);
    rt.apply_update(&advertisement(gw2, 1, vec![(gw2, 0, true)]), gw2);

    // Normally gw1 is picked (same hops, comes first)
    assert!(rt.nearest_gateway_route().is_some());

    rt.blacklist_peer(gw1);
    let (dst, _) = rt.nearest_gateway_route().unwrap();
    assert_eq!(dst, gw2, "blacklisted gateway must be skipped");
}

#[test]
fn unblacklist_restores_visibility() {
    let a = id(1);
    let b = id(2);

    let mut rt = super::new_table(a, false);
    rt.add_peer(b);
    rt.blacklist_peer(b);
    assert!(rt.lookup(b).is_none());

    rt.unblacklist_peer(&b);
    // Route was removed by blacklist_peer; re-add it
    rt.add_peer(b);
    assert!(rt.lookup(b).is_some());
}
