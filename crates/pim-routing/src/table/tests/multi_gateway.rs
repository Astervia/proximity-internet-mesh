use super::super::*;
use super::{advertisement, id};

#[test]
fn gateway_score_zero_for_1hop_idle() {
    // 1 hop, 0 load, no RTT → score = 100
    assert_eq!(gateway_score(1, 0, None), 100);
}

#[test]
fn gateway_score_load_adds_penalty() {
    // 1 hop + full load (255) → 100 + 127 = 227
    assert_eq!(gateway_score(1, 255, None), 227);
}

#[test]
fn gateway_score_rtt_adds_penalty() {
    // 1 hop + 1000ms RTT → 100 + 0 + 100 = 200
    assert_eq!(gateway_score(1, 0, Some(1000)), 200);
}

#[test]
fn gateway_score_combined() {
    // 2 hops + load 100 + rtt 200ms → 200 + 50 + 20 = 270
    assert_eq!(gateway_score(2, 100, Some(200)), 270);
}

#[test]
fn load_aware_selection_prefers_less_loaded_gateway() {
    // relay_a → gw1: 1 advertised hop → 2 total hops, heavy load (220) → score=200+110=310
    // relay_b → gw2: 2 advertised hops → 3 total hops, no load         → score=300
    // gw2 should win despite being farther
    let self_id = id(1);
    let relay_a = id(2);
    let relay_b = id(3);
    let gw1 = id(10);
    let gw2 = id(11);

    let mut rt = super::new_table(self_id, false);
    rt.add_peer(relay_a);
    rt.add_peer(relay_b);

    rt.apply_update(&advertisement(relay_a, 1, vec![(gw1, 1, true)]), relay_a);
    rt.apply_update(&advertisement(relay_b, 1, vec![(gw2, 2, true)]), relay_b);

    rt.update_gateway_load(gw1, 220);

    // gw1: hops=2, load=220 → score=200+110=310
    // gw2: hops=3, load=0   → score=300
    let (dst, _next_hop) = rt.nearest_gateway_route().unwrap();
    assert_eq!(
        dst, gw2,
        "less-loaded 3-hop gateway should beat saturated 2-hop gateway"
    );
}

#[test]
fn rtt_aware_selection_prefers_lower_latency_gateway() {
    // gw1: 2 hops, no load, rtt=800ms → score=200+0+80=280
    // gw2: 3 hops, no load, no rtt    → score=300
    // gw1 wins (280 < 300)
    let self_id = id(1);
    let relay_a = id(2);
    let relay_b = id(3);
    let gw1 = id(10);
    let gw2 = id(11);

    let mut rt = super::new_table(self_id, false);
    rt.add_peer(relay_a);
    rt.add_peer(relay_b);

    rt.apply_update(&advertisement(relay_a, 1, vec![(gw1, 1, true)]), relay_a);
    rt.apply_update(&advertisement(relay_b, 1, vec![(gw2, 2, true)]), relay_b);

    rt.update_gateway_rtt(gw1, 800);
    // gw1 score = 200+0+80 = 280, gw2 score = 300
    let (dst, _) = rt.nearest_gateway_route().unwrap();
    assert_eq!(
        dst, gw1,
        "lower-RTT 2-hop gateway should beat clean 3-hop gateway at 800ms"
    );
}

#[test]
fn rtt_aware_selection_switches_at_high_latency() {
    // gw1: 2 hops, no load, rtt=3000ms → score=200+0+300=500
    // gw2: 3 hops, no load, no rtt     → score=300
    // gw2 wins
    let self_id = id(1);
    let relay_a = id(2);
    let relay_b = id(3);
    let gw1 = id(10);
    let gw2 = id(11);

    let mut rt = super::new_table(self_id, false);
    rt.add_peer(relay_a);
    rt.add_peer(relay_b);

    rt.apply_update(&advertisement(relay_a, 1, vec![(gw1, 1, true)]), relay_a);
    rt.apply_update(&advertisement(relay_b, 1, vec![(gw2, 2, true)]), relay_b);

    rt.update_gateway_rtt(gw1, 3000);
    // gw1 score = 500, gw2 score = 300
    let (dst, _) = rt.nearest_gateway_route().unwrap();
    assert_eq!(dst, gw2, "3-hop gateway should beat 2-hop with 3s RTT");
}

#[test]
fn failover_to_second_gateway_when_first_removed() {
    // gw1: 2 hops (clearly better); gw2: 3 hops
    let self_id = id(1);
    let relay_a = id(2);
    let relay_b = id(3);
    let gw1 = id(10);
    let gw2 = id(11);

    let mut rt = super::new_table(self_id, false);
    rt.add_peer(relay_a);
    rt.add_peer(relay_b);

    rt.apply_update(&advertisement(relay_a, 1, vec![(gw1, 1, true)]), relay_a);
    rt.apply_update(&advertisement(relay_b, 1, vec![(gw2, 2, true)]), relay_b);

    // Initially gw1 is preferred (2 hops < 3 hops)
    let (dst, _) = rt.nearest_gateway_route().unwrap();
    assert_eq!(dst, gw1);

    // gw1's relay goes down
    rt.remove_peer(relay_a);

    // Failover to gw2
    let (dst, _) = rt.nearest_gateway_route().unwrap();
    assert_eq!(dst, gw2, "should fail over to second gateway");
}

#[test]
fn all_gateways_sorted_by_score() {
    let self_id = id(1);
    let relay_a = id(2);
    let relay_b = id(3);
    let gw1 = id(10); // 2 hops, load=200 → score=200+100=300
    let gw2 = id(11); // 3 hops, load=0   → score=300

    let mut rt = super::new_table(self_id, false);
    rt.add_peer(relay_a);
    rt.add_peer(relay_b);

    rt.apply_update(&advertisement(relay_a, 1, vec![(gw1, 1, true)]), relay_a);
    rt.apply_update(&advertisement(relay_b, 1, vec![(gw2, 2, true)]), relay_b);

    rt.update_gateway_load(gw1, 200);
    // gw1: score=200+100=300; gw2: score=300+0=300 → tie broken by map iteration (any order)
    // Just verify both appear
    let gws = rt.all_gateways();
    assert_eq!(gws.len(), 2);
    let ids: Vec<NodeId> = gws.iter().map(|(id, _)| *id).collect();
    assert!(ids.contains(&gw1));
    assert!(ids.contains(&gw2));
}

#[test]
fn update_gateway_load_only_applies_to_gateway_entries() {
    let mut rt = super::new_table(id(1), false);
    rt.add_peer(id(2));
    // id(2) is not a gateway — update_gateway_load should be a no-op
    rt.update_gateway_load(id(2), 200);
    assert_eq!(rt.routes[&id(2)].gateway_load, 0);
}

#[test]
fn update_gateway_rtt_only_applies_to_gateway_entries() {
    let mut rt = super::new_table(id(1), false);
    rt.add_peer(id(2));
    rt.update_gateway_rtt(id(2), 50);
    assert_eq!(rt.routes[&id(2)].rtt_ms, None);
}

#[test]
fn update_returns_changed_only_on_change() {
    let a = id(1);
    let b = id(2);
    let c = id(3);

    let mut rt = super::new_table(a, false);
    rt.add_peer(b);

    let upd = advertisement(b, 1, vec![(c, 1, false)]);
    assert_eq!(rt.apply_update(&upd, b), UpdateResult::Changed);

    // Same advertisement again — not changed (same seq, same hops)
    // In our impl: same seq + same peer → no update
    let upd2 = advertisement(b, 1, vec![(c, 1, false)]);
    assert_eq!(rt.apply_update(&upd2, b), UpdateResult::Unchanged);
}

#[test]
fn gateway_flag_flip_marks_update_changed() {
    // Regression: a direct peer flipping from non-gateway to gateway
    // (same hop count, newer sequence) must report Changed so the
    // daemon can react (e.g. request a dynamic IP).
    let a = id(1);
    let b = id(2);

    let mut rt = super::new_table(a, false);
    rt.add_peer(b);
    assert!(!rt.routes[&b].is_gateway);

    let upd = advertisement(b, 1, vec![(b, 0, true)]);
    assert_eq!(rt.apply_update(&upd, b), UpdateResult::Changed);
    assert!(rt.routes[&b].is_gateway);
}
