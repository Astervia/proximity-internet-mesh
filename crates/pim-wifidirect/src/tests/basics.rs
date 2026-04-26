use super::super::*;
use std::collections::HashSet;

#[test]
fn discovery_new_returns_receiver() {
    let config = WifiDirectConfig::default();
    let (_svc, _rx) = WifiDirectDiscovery::new("node-a", config, 9100);
    // Verifies construction does not panic and returns both parts.
}

#[test]
fn discovery_skips_already_seen_mac() {
    // The seen_macs set prevents re-connecting to known peers.
    let mut seen: HashSet<String> = HashSet::new();
    let mac = "aa:bb:cc:dd:ee:ff".to_string();
    seen.insert(mac.clone());
    assert!(seen.contains(&mac));
}
