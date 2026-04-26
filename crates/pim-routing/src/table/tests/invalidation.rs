use super::super::*;
use super::{advertisement, id};

#[test]
fn remove_routes_via_dead_peer() {
    let a = id(1);
    let b = id(2);
    let c = id(3);

    let mut rt = RoutingTable::new(a, false);
    rt.add_peer(b);
    rt.apply_update(&advertisement(b, 1, vec![(c, 1, false)]), b);

    assert!(rt.lookup(c).is_some());

    let result = rt.remove_peer(b);
    assert_eq!(result, UpdateResult::Changed);
    assert!(rt.lookup(b).is_none());
    assert!(rt.lookup(c).is_none());
}

#[test]
fn poison_reverse_invalidates_route() {
    let a = id(1);
    let b = id(2);
    let c = id(3);

    let mut rt = RoutingTable::new(a, false);
    rt.add_peer(b);
    rt.apply_update(&advertisement(b, 1, vec![(c, 1, false)]), b);
    assert!(rt.lookup(c).is_some());

    // B now sends hops=INFINITY for C (poison)
    let poison = advertisement(b, 2, vec![(c, INFINITY, false)]);
    let result = rt.apply_update(&poison, b);
    assert_eq!(result, UpdateResult::Changed);
    assert!(rt.lookup(c).is_none(), "poisoned route should be removed");
}

#[test]
fn missing_route_in_full_update_withdraws_old_path() {
    let a = id(1);
    let b = id(2);
    let c = id(3);

    let mut rt = RoutingTable::new(a, false);
    rt.add_peer(b);
    rt.apply_update(&advertisement(b, 1, vec![(c, 1, false)]), b);
    assert!(rt.lookup(c).is_some());

    let result = rt.apply_update(&advertisement(b, 2, vec![]), b);
    assert_eq!(result, UpdateResult::Changed);
    assert!(
        rt.lookup(c).is_none(),
        "route omitted from full update should be withdrawn"
    );
}
