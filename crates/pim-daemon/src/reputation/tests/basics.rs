use super::super::*;
use pim_core::NodeId;

fn peer(b: u8) -> NodeId {
    NodeId::from_bytes([b; 16])
}

#[test]
fn failure_accumulation_reaches_threshold() {
    let mut rt = ReputationTracker::new();
    let p = peer(1);
    for i in 1..BLACKLIST_THRESHOLD {
        assert!(
            !rt.record_failure(p),
            "failure {i} should not newly blacklist"
        );
    }
    assert!(rt.record_failure(p), "final failure should newly blacklist");
}

#[test]
fn blacklist_threshold_detected() {
    let mut rt = ReputationTracker::new();
    let p = peer(2);
    for _ in 0..BLACKLIST_THRESHOLD {
        rt.record_failure(p);
    }
    assert!(rt.is_blacklisted(&p));
}

#[test]
fn success_decrements_score() {
    let mut rt = ReputationTracker::new();
    let p = peer(3);
    for _ in 0..5 {
        rt.record_failure(p);
    }
    rt.record_success(p);
    assert!(!rt.is_blacklisted(&p));
    // Score should be 4 — not yet at threshold
    for _ in 0..(BLACKLIST_THRESHOLD - 4 - 1) {
        rt.record_failure(p);
    }
    assert!(!rt.is_blacklisted(&p));
}

#[test]
fn success_floor_is_zero() {
    let mut rt = ReputationTracker::new();
    let p = peer(4);
    // record_success on unknown peer should not panic or underflow
    rt.record_success(p);
    rt.record_success(p);
    assert!(!rt.is_blacklisted(&p));
}

#[test]
fn pardon_resets_score() {
    let mut rt = ReputationTracker::new();
    let p = peer(5);
    for _ in 0..BLACKLIST_THRESHOLD {
        rt.record_failure(p);
    }
    assert!(rt.is_blacklisted(&p));
    rt.pardon(&p);
    assert!(!rt.is_blacklisted(&p));
}
