#[allow(unused_imports)]
use super::super::*;
use super::{advertisement, id};

#[test]
fn own_node_included_in_advertisement() {
    let a = id(1);
    let b = id(2);

    let mut rt = super::new_table(a, false);
    rt.add_peer(b);

    let adv = rt.generate_advertisement(b);
    let self_entry = adv.entries.iter().find(|e| e.destination == a);
    assert!(self_entry.is_some(), "advertisement should include self");
    assert_eq!(self_entry.unwrap().hops, 0);
}

#[test]
fn sequence_increments_per_advertisement() {
    let a = id(1);
    let b = id(2);

    let mut rt = super::new_table(a, false);
    rt.add_peer(b);

    let s1 = rt.generate_advertisement(b).sequence;
    let s2 = rt.generate_advertisement(b).sequence;
    assert!(s2 > s1);
}

#[test]
fn no_loop_route_self() {
    let a = id(1);
    let b = id(2);

    let mut rt = super::new_table(a, false);
    rt.add_peer(b);

    // B tries to advertise A back to A (shouldn't be installed)
    let upd = advertisement(b, 1, vec![(a, 1, false)]);
    rt.apply_update(&upd, b);

    // There's no route to self in the table
    assert!(!rt.routes.contains_key(&a));
    assert!(rt.lookup(a).is_none());
}
