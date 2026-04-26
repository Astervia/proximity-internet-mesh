use super::*;

fn id(n: u8) -> NodeId {
    NodeId::from_bytes([n; 16])
}

fn mesh_ip(n: u8) -> [u8; 4] {
    [10, 77, 0, n]
}

/// Build a RouteUpdateFrame advertising `entries` from `origin`.
fn advertisement(origin: NodeId, seq: u64, entries: Vec<(NodeId, u8, bool)>) -> RouteUpdateFrame {
    RouteUpdateFrame {
        origin_id: origin,
        sequence: seq,
        entries: entries
            .into_iter()
            .map(|(dst, hops, is_gw)| RouteEntry {
                destination: dst,
                hops,
                flags: if is_gw { 0x01 } else { 0x00 },
                mesh_ip: mesh_ip(dst.as_bytes()[0]),
            })
            .collect(),
        signature: [0u8; 64],
    }
}

// ── Basic convergence ─────────────────────────────────────────────────────

#[test]
fn direct_peer_route_installed_at_1_hop() {
    let mut rt = RoutingTable::new(id(1), false);
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

    let mut rt = RoutingTable::new(a, false);
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

    let mut rt = RoutingTable::new(a, false);
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

// ── Split horizon / poison reverse ────────────────────────────────────────

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

// ── Invalidation ──────────────────────────────────────────────────────────

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

// ── Stale expiry ──────────────────────────────────────────────────────────

#[test]
fn stale_routes_expired() {
    let a = id(1);
    let b = id(2);
    let c = id(3);

    let mut rt = RoutingTable::new(a, false);
    rt.add_peer(b);
    rt.apply_update(&advertisement(b, 1, vec![(c, 1, false)]), b);

    // Age the entries manually
    for entry in rt.routes.values_mut() {
        entry.last_seen = Instant::now() - Duration::from_secs(400);
    }

    let result = rt.expire_stale(Duration::from_secs(300));
    assert_eq!(result, UpdateResult::Changed);
    assert!(rt.lookup(c).is_none());
}

#[test]
fn fresh_routes_not_expired() {
    let a = id(1);
    let b = id(2);

    let mut rt = RoutingTable::new(a, false);
    rt.add_peer(b);

    let result = rt.expire_stale(Duration::from_secs(300));
    assert_eq!(result, UpdateResult::Unchanged);
    assert!(rt.lookup(b).is_some());
}

// ── Gateway selection ─────────────────────────────────────────────────────

#[test]
fn nearest_gateway_selected() {
    let a = id(1);
    let b = id(2); // relay, 1 hop
    let gw1 = id(10); // gateway via b at 2 hops
    let c = id(3); // relay, 1 hop
    let gw2 = id(11); // gateway via c at 3 hops

    let mut rt = RoutingTable::new(a, false);
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
    let rt = RoutingTable::new(id(1), true);
    assert!(rt.nearest_gateway().is_none());
}

// ── Advertisement sequence ────────────────────────────────────────────────

#[test]
fn own_node_included_in_advertisement() {
    let a = id(1);
    let b = id(2);

    let mut rt = RoutingTable::new(a, false);
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

    let mut rt = RoutingTable::new(a, false);
    rt.add_peer(b);

    let s1 = rt.generate_advertisement(b).sequence;
    let s2 = rt.generate_advertisement(b).sequence;
    assert!(s2 > s1);
}

#[test]
fn no_loop_route_self() {
    let a = id(1);
    let b = id(2);

    let mut rt = RoutingTable::new(a, false);
    rt.add_peer(b);

    // B tries to advertise A back to A (shouldn't be installed)
    let upd = advertisement(b, 1, vec![(a, 1, false)]);
    rt.apply_update(&upd, b);

    // There's no route to self in the table
    assert!(!rt.routes.contains_key(&a));
    assert!(rt.lookup(a).is_none());
}

// ── Phase 6: Routing security ─────────────────────────────────────────────

#[test]
fn replay_rejected_same_sequence() {
    let a = id(1);
    let b = id(2);
    let c = id(3);

    let mut rt = RoutingTable::new(a, false);
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

    let mut rt = RoutingTable::new(a, false);
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

    let mut rt = RoutingTable::new(a, false);
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

    let mut rt = RoutingTable::new(a, false);
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

    let mut rt = RoutingTable::new(a, false);
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

    let mut rt = RoutingTable::new(a, false);
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

    let mut rt = RoutingTable::new(a, false);
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

    let mut rt = RoutingTable::new(a, false);
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

    let mut rt = RoutingTable::new(a, false);
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

    let mut rt = RoutingTable::new(a, false);
    rt.add_peer(b);
    rt.blacklist_peer(b);
    assert!(rt.lookup(b).is_none());

    rt.unblacklist_peer(&b);
    // Route was removed by blacklist_peer; re-add it
    rt.add_peer(b);
    assert!(rt.lookup(b).is_some());
}

// ── Phase 5: Multi-gateway scoring ───────────────────────────────────────

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

    let mut rt = RoutingTable::new(self_id, false);
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

    let mut rt = RoutingTable::new(self_id, false);
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

    let mut rt = RoutingTable::new(self_id, false);
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

    let mut rt = RoutingTable::new(self_id, false);
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

    let mut rt = RoutingTable::new(self_id, false);
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
    let mut rt = RoutingTable::new(id(1), false);
    rt.add_peer(id(2));
    // id(2) is not a gateway — update_gateway_load should be a no-op
    rt.update_gateway_load(id(2), 200);
    assert_eq!(rt.routes[&id(2)].gateway_load, 0);
}

#[test]
fn update_gateway_rtt_only_applies_to_gateway_entries() {
    let mut rt = RoutingTable::new(id(1), false);
    rt.add_peer(id(2));
    rt.update_gateway_rtt(id(2), 50);
    assert_eq!(rt.routes[&id(2)].rtt_ms, None);
}

#[test]
fn update_returns_changed_only_on_change() {
    let a = id(1);
    let b = id(2);
    let c = id(3);

    let mut rt = RoutingTable::new(a, false);
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

    let mut rt = RoutingTable::new(a, false);
    rt.add_peer(b);
    assert!(!rt.routes[&b].is_gateway);

    let upd = advertisement(b, 1, vec![(b, 0, true)]);
    assert_eq!(rt.apply_update(&upd, b), UpdateResult::Changed);
    assert!(rt.routes[&b].is_gateway);
}
