use super::super::*;
use pim_core::NodeId;

fn peer(b: u8) -> NodeId {
    NodeId::from_bytes([b; 16])
}

#[test]
fn burst_is_allowed_up_to_capacity() {
    let mut rl = RateLimiter::new(5, 0.0); // zero refill so burst only
    let p = peer(1);
    for _ in 0..5 {
        assert!(rl.allow(&p));
    }
    assert!(!rl.allow(&p)); // 6th is rejected
}

#[test]
fn throttled_after_burst_exhausted() {
    let mut rl = RateLimiter::new(3, 0.0);
    let p = peer(2);
    assert!(rl.allow(&p));
    assert!(rl.allow(&p));
    assert!(rl.allow(&p));
    assert!(!rl.allow(&p));
    assert!(!rl.allow(&p));
}

#[test]
fn different_peers_tracked_independently() {
    let mut rl = RateLimiter::new(2, 0.0);
    let a = peer(3);
    let b = peer(4);
    assert!(rl.allow(&a));
    assert!(rl.allow(&a));
    assert!(!rl.allow(&a)); // a exhausted
    assert!(rl.allow(&b)); // b still has tokens
    assert!(rl.allow(&b));
    assert!(!rl.allow(&b)); // b now exhausted
}

#[test]
fn remove_peer_resets_bucket() {
    let mut rl = RateLimiter::new(1, 0.0);
    let p = peer(5);
    assert!(rl.allow(&p));
    assert!(!rl.allow(&p)); // exhausted
    rl.remove_peer(&p);
    assert!(rl.allow(&p)); // bucket recreated with full capacity
}
