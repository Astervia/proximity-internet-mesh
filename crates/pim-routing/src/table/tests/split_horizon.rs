use super::super::*;
use super::{advertisement, id};

#[test]
fn split_horizon_does_not_advertise_back() {
    let a = id(1);
    let b = id(2);
    let c = id(3);

    let mut rt = RoutingTable::new(a, false);
    rt.add_peer(b);

    // A learned C via B
    rt.apply_update(&advertisement(b, 1, vec![(c, 1, false)]), b);

    // Advertisement to B: C should have hops=INFINITY (poison reverse)
    let adv = rt.generate_advertisement(b);
    let c_entry = adv.entries.iter().find(|e| e.destination == c).unwrap();
    assert_eq!(
        c_entry.hops, INFINITY,
        "poison reverse should set hops=INFINITY"
    );
}

#[test]
fn non_poisoned_routes_advertised_normally() {
    let a = id(1);
    let b = id(2);
    let c = id(3);
    let d = id(4);

    let mut rt = RoutingTable::new(a, false);
    rt.add_peer(b);
    rt.add_peer(c);

    // A learned D via C
    rt.apply_update(&advertisement(c, 1, vec![(d, 1, false)]), c);

    // Advertisement to B: D was NOT learned from B → advertise normally
    let adv = rt.generate_advertisement(b);
    let d_entry = adv.entries.iter().find(|e| e.destination == d).unwrap();
    assert!(
        d_entry.hops < INFINITY,
        "D should be advertised normally to B"
    );
}
