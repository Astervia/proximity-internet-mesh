use super::super::*;
use std::net::{IpAddr, Ipv4Addr};

fn make_record(n: u8) -> PeerRecord {
    PeerRecord {
        node_id: NodeId::from_bytes([n; 16]),
        public_key: [n; 32],
        capabilities: NodeCapabilities::client(),
        listen_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, n)), 9100),
        last_seen: Instant::now(),
    }
}

#[test]
fn upsert_returns_true_for_new_peer() {
    let mut table = PeerTable::new();
    assert!(table.upsert(make_record(1)));
}

#[test]
fn upsert_returns_false_for_refresh() {
    let mut table = PeerTable::new();
    table.upsert(make_record(1));
    assert!(!table.upsert(make_record(1)));
}

#[test]
fn remove_extracts_peer() {
    let mut table = PeerTable::new();
    table.upsert(make_record(2));
    let id = NodeId::from_bytes([2; 16]);
    assert!(table.remove(&id).is_some());
    assert!(table.get(&id).is_none());
}

#[test]
fn len_tracks_correctly() {
    let mut table = PeerTable::new();
    assert_eq!(table.len(), 0);
    table.upsert(make_record(1));
    table.upsert(make_record(2));
    assert_eq!(table.len(), 2);
    table.upsert(make_record(1)); // refresh
    assert_eq!(table.len(), 2);
}

#[test]
fn expire_stale_removes_old_peers() {
    let mut table = PeerTable::new();
    // Insert a peer with an artificially old last_seen
    let mut old = make_record(5);
    old.last_seen = Instant::now() - Duration::from_secs(60);
    table.upsert(old);
    table.upsert(make_record(6)); // fresh

    let removed = table.expire_stale(Duration::from_secs(30));
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0], NodeId::from_bytes([5; 16]));
    assert_eq!(table.len(), 1);
}

#[test]
fn expire_stale_keeps_fresh_peers() {
    let mut table = PeerTable::new();
    table.upsert(make_record(1));
    table.upsert(make_record(2));
    let removed = table.expire_stale(Duration::from_secs(60));
    assert!(removed.is_empty());
    assert_eq!(table.len(), 2);
}
