use super::super::*;

#[test]
fn load_normalized_zero_when_no_packets() {
    // 0 packets in interval → load = 0
    let delta: u64 = 0;
    let load = (delta.min(2000) * 255 / 2000) as u8;
    assert_eq!(load, 0);
}

#[test]
fn load_normalized_255_at_saturation() {
    // ≥2000 packets in interval → load = 255
    let delta: u64 = 2000;
    let load = (delta.min(2000) * 255 / 2000) as u8;
    assert_eq!(load, 255);
}

#[test]
fn load_normalized_midpoint() {
    // 1000 packets → load ≈ 127
    let delta: u64 = 1000;
    let load = (delta.min(2000) * 255 / 2000) as u8;
    assert_eq!(load, 127);
}

#[tokio::test]
async fn pending_pings_gc_removes_stale_entries() {
    let pings: Arc<Mutex<HashMap<u64, (NodeId, Instant)>>> = Arc::new(Mutex::new(HashMap::new()));
    let stale_time = Instant::now() - Duration::from_secs(60);
    pings
        .lock()
        .await
        .insert(1u64, (NodeId::from_bytes([1; 16]), stale_time));
    pings
        .lock()
        .await
        .insert(2u64, (NodeId::from_bytes([2; 16]), Instant::now()));

    // Simulate the GC step from run_gateway_probes
    pings
        .lock()
        .await
        .retain(|_, (_, sent_at)| sent_at.elapsed() < PENDING_PING_TTL);

    let locked = pings.lock().await;
    assert!(!locked.contains_key(&1u64), "stale ping should be removed");
    assert!(locked.contains_key(&2u64), "fresh ping should remain");
}
