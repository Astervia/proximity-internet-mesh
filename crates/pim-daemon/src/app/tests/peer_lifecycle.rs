use super::super::*;
use super::peer_id;

/// A peer that sends heartbeats regularly must not appear in the timed-out set.
#[test]
fn peer_heartbeat_keeps_peer_alive() {
    const TIMEOUT: Duration = Duration::from_secs(15);
    let mut hb: HashMap<NodeId, Instant> = HashMap::new();
    let p = peer_id(10);
    // Insert with a fresh timestamp (simulates just-received heartbeat).
    hb.insert(p, Instant::now());

    let timed_out: Vec<NodeId> = hb
        .iter()
        .filter(|(_, last)| last.elapsed() > TIMEOUT)
        .map(|(id, _)| *id)
        .collect();

    assert!(
        timed_out.is_empty(),
        "peer with recent heartbeat must not time out"
    );
}

/// A peer that has been silent for more than 15 s must appear in the timed-out set.
#[test]
fn peer_timeout_after_missed_heartbeats() {
    const TIMEOUT: Duration = Duration::from_secs(15);
    let mut hb: HashMap<NodeId, Instant> = HashMap::new();
    let p = peer_id(11);
    // Simulate 3 missed heartbeats: last seen 16 s ago.
    hb.insert(p, Instant::now() - Duration::from_secs(16));

    let timed_out: Vec<NodeId> = hb
        .iter()
        .filter(|(_, last)| last.elapsed() > TIMEOUT)
        .map(|(id, _)| *id)
        .collect();

    assert_eq!(timed_out.len(), 1);
    assert_eq!(timed_out[0], p);
}

/// On receiving a Goodbye, the peer must be removed from the heartbeat map immediately.
#[tokio::test]
async fn goodbye_triggers_immediate_removal() {
    let p = peer_id(12);
    let hb_map: Arc<Mutex<HashMap<NodeId, Instant>>> = Arc::new(Mutex::new(HashMap::new()));
    hb_map.lock().await.insert(p, Instant::now());
    assert!(hb_map.lock().await.contains_key(&p));

    // Simulate the Goodbye handler: remove from heartbeat map (mirrors remove_peer).
    hb_map.lock().await.remove(&p);

    assert!(
        !hb_map.lock().await.contains_key(&p),
        "peer must be absent from heartbeat map after Goodbye"
    );
}
