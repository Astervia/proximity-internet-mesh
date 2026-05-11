#[allow(unused_imports)]
use super::super::*;
use super::{advertisement, id};

#[test]
fn direct_peer_route_installed_at_1_hop() {
    let mut rt = super::new_table(id(1), false);
    rt.add_peer(id(2));

    assert_eq!(rt.lookup(id(2)), Some(id(2)));
    assert_eq!(rt.routes[&id(2)].hops, 1);
}

#[test]
fn three_node_chain_converges() {
    // A (self) — B (direct) — C (via B)
    let a = id(1);
    let b = id(2);
    let c = id(3);

    let mut rt = super::new_table(a, false);
    rt.add_peer(b);

    // B advertises C at 1 hop
    let upd = advertisement(b, 1, vec![(c, 1, false)]);
    rt.apply_update(&upd, b);

    assert_eq!(rt.lookup(c), Some(b));
    assert_eq!(rt.routes[&c].hops, 2); // A→B (1) + B→C (1) = 2
}

#[test]
fn better_route_replaces_existing() {
    let a = id(1);
    let b = id(2);
    let c = id(3);
    let d = id(4);

    let mut rt = super::new_table(a, false);
    rt.add_peer(b);
    rt.add_peer(c);

    // B advertises D at 3 hops
    rt.apply_update(&advertisement(b, 1, vec![(d, 3, false)]), b);
    assert_eq!(rt.routes[&d].hops, 4);

    // C advertises D at 1 hop → better
    rt.apply_update(&advertisement(c, 1, vec![(d, 1, false)]), c);
    assert_eq!(rt.routes[&d].hops, 2);
    assert_eq!(rt.lookup(d), Some(c));
}
