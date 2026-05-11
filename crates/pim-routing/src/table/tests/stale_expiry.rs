use super::super::*;
use super::{advertisement, id};

#[test]
fn stale_routes_expired() {
    let a = id(1);
    let b = id(2);
    let c = id(3);

    let mut rt = super::new_table(a, false);
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

    let mut rt = super::new_table(a, false);
    rt.add_peer(b);

    let result = rt.expire_stale(Duration::from_secs(300));
    assert_eq!(result, UpdateResult::Unchanged);
    assert!(rt.lookup(b).is_some());
}
