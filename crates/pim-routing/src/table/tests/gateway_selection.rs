#[allow(unused_imports)]
use super::super::*;
use super::{advertisement, id};

#[test]
fn nearest_gateway_selected() {
    let a = id(1);
    let b = id(2); // relay, 1 hop
    let gw1 = id(10); // gateway via b at 2 hops
    let c = id(3); // relay, 1 hop
    let gw2 = id(11); // gateway via c at 3 hops

    let mut rt = super::new_table(a, false);
    rt.add_peer(b);
    rt.add_peer(c);

    rt.apply_update(&advertisement(b, 1, vec![(gw1, 1, true)]), b);
    rt.apply_update(&advertisement(c, 1, vec![(gw2, 2, true)]), c);

    let (next_hop, hops) = rt.nearest_gateway().unwrap();
    assert_eq!(next_hop, b);
    assert_eq!(hops, 2);
}

#[test]
fn gateway_node_has_no_nearest_gateway() {
    let rt = super::new_table(id(1), true);
    assert!(rt.nearest_gateway().is_none());
}
