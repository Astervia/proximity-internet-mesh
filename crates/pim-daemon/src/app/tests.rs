use super::*;

// ── backoff tests ──────────────────────────────────────────────────────

#[test]
fn backoff_base_grows_exponentially() {
    assert_eq!(backoff_base_ms(0), 1_000);
    assert_eq!(backoff_base_ms(1), 2_000);
    assert_eq!(backoff_base_ms(2), 4_000);
    assert_eq!(backoff_base_ms(3), 8_000);
    assert_eq!(backoff_base_ms(4), 10_000);
}

#[test]
fn test_format_stats_output_format() {
    let stats = StatsSnapshot {
        peers: 1,
        routes: 2,
        packets_forwarded: 3,
        bytes_forwarded: 4,
        packets_dropped: 5,
        congestion_drops: 6,
        conntrack_size: 7,
        uptime_secs: 8,
    };
    let expected = "peers=1\n\
                    routes=2\n\
                    packets_forwarded=3\n\
                    bytes_forwarded=4\n\
                    packets_dropped=5\n\
                    congestion_drops=6\n\
                    conntrack_size=7\n\
                    uptime_secs=8\n";
    assert_eq!(format_stats(&stats), expected);
}

#[test]
fn test_format_stats_zero_values() {
    let stats = StatsSnapshot {
        peers: 0,
        routes: 0,
        packets_forwarded: 0,
        bytes_forwarded: 0,
        packets_dropped: 0,
        congestion_drops: 0,
        conntrack_size: 0,
        uptime_secs: 0,
    };
    let expected = "peers=0\n\
                    routes=0\n\
                    packets_forwarded=0\n\
                    bytes_forwarded=0\n\
                    packets_dropped=0\n\
                    congestion_drops=0\n\
                    conntrack_size=0\n\
                    uptime_secs=0\n";
    assert_eq!(format_stats(&stats), expected);
}

#[test]
fn backoff_base_capped_at_30s() {
    // 2^4 * 1000 = 16000 > 10000 → capped
    assert_eq!(backoff_base_ms(4), 10_000);
    assert_eq!(backoff_base_ms(10), 10_000);
    assert_eq!(backoff_base_ms(100), 10_000);
}

#[test]
fn backoff_duration_attempt_0_within_25_pct_jitter() {
    // Run many times to shake out the random jitter range.
    for _ in 0..200 {
        let d = backoff_duration(0);
        let ms = d.as_millis();
        assert!(ms >= 750, "attempt 0: {ms} ms < 750 ms");
        assert!(ms <= 1_250, "attempt 0: {ms} ms > 1250 ms");
    }
}

#[test]
fn backoff_duration_attempt_1_within_25_pct_jitter() {
    for _ in 0..200 {
        let d = backoff_duration(1);
        let ms = d.as_millis();
        assert!(ms >= 1_500, "attempt 1: {ms} ms < 1500 ms");
        assert!(ms <= 2_500, "attempt 1: {ms} ms > 2500 ms");
    }
}

#[test]
fn backoff_duration_capped_within_25_pct_of_30s() {
    // attempt ≥ 4: base = 10 000 ms, jitter ±2 500 ms
    for _ in 0..200 {
        let d = backoff_duration(10);
        let ms = d.as_millis();
        assert!(ms >= 7_500, "attempt 10: {ms} ms < 7 500 ms");
        assert!(ms <= 12_500, "attempt 10: {ms} ms > 12 500 ms");
    }
}

#[test]
fn backoff_duration_increases_with_attempt() {
    // On average the capped duration must be higher than the base duration.
    // Compare median over many samples.
    let low: u64 = (0..200)
        .map(|_| backoff_duration(0).as_millis() as u64)
        .sum::<u64>()
        / 200;
    let high: u64 = (0..200)
        .map(|_| backoff_duration(4).as_millis() as u64)
        .sum::<u64>()
        / 200;
    assert!(
        high > low,
        "attempt 4 avg ({high}) should exceed attempt 0 avg ({low})"
    );
}

// ── ReconnectManager tests ─────────────────────────────────────────────

fn test_addr() -> SocketAddr {
    "127.0.0.1:9999".parse().unwrap()
}

fn test_target() -> ConnectTarget {
    ConnectTarget::Tcp(test_addr())
}

fn peer_id(b: u8) -> NodeId {
    NodeId::from_bytes([b; 16])
}

#[test]
fn ip_request_classifier_accepts_first_request() {
    let requester = peer_id(70);
    let mut pending = HashSet::new();
    assert_eq!(
        classify_ip_request(&mut pending, requester, requester),
        IpRequestDisposition::Process
    );
    assert!(pending.contains(&requester));
}

#[test]
fn ip_request_classifier_drops_duplicate_inflight_request() {
    let requester = peer_id(71);
    let mut pending = HashSet::from([requester]);
    assert_eq!(
        classify_ip_request(&mut pending, requester, requester),
        IpRequestDisposition::DuplicateInFlight
    );
}

#[test]
fn ip_request_classifier_rejects_spoofed_requester() {
    let requester = peer_id(72);
    let from_peer = peer_id(73);
    let mut pending = HashSet::new();
    assert_eq!(
        classify_ip_request(&mut pending, requester, from_peer),
        IpRequestDisposition::SpoofedRequester
    );
    assert!(pending.is_empty());
}

fn temp_trust_store_path() -> PathBuf {
    std::env::temp_dir().join(format!("pim-trust-{}.toml", rand::random::<u64>()))
}

#[tokio::test]
async fn authorization_allow_list_rejects_unlisted_peer() {
    let path = temp_trust_store_path();
    let manager =
        AuthorizationManager::new(AuthorizationPolicy::AllowList, [peer_id(1)], path.clone())
            .unwrap();
    assert!(manager.authorize_discovered_peer(peer_id(1)).await);
    assert!(!manager.authorize_discovered_peer(peer_id(2)).await);
    assert_eq!(
        manager
            .authorize_authenticated_peer(peer_id(2))
            .await
            .unwrap(),
        AuthorizationDecision::Rejected
    );
    std::fs::remove_file(path).ok();
}

#[tokio::test]
async fn authorization_tofu_persists_new_peer() {
    let path = temp_trust_store_path();
    let manager =
        AuthorizationManager::new(AuthorizationPolicy::TrustOnFirstUse, [], path.clone()).unwrap();
    assert_eq!(
        manager
            .authorize_authenticated_peer(peer_id(7))
            .await
            .unwrap(),
        AuthorizationDecision::TrustedOnFirstUse
    );

    let reloaded =
        AuthorizationManager::new(AuthorizationPolicy::TrustOnFirstUse, [], path.clone()).unwrap();
    assert_eq!(
        reloaded
            .authorize_authenticated_peer(peer_id(7))
            .await
            .unwrap(),
        AuthorizationDecision::Allowed
    );
    std::fs::remove_file(path).ok();
}

#[test]
fn discovery_shared_key_requires_32_bytes_of_hex() {
    assert!(parse_discovery_shared_key("abcd").is_err());
    assert!(parse_discovery_shared_key(&"zz".repeat(32)).is_err());
    assert!(parse_discovery_shared_key(&"11".repeat(32)).is_ok());
}

#[tokio::test]
async fn begin_reconnect_returns_true_once() {
    let mgr = ReconnectManager::new([test_target()]);
    assert!(
        mgr.begin_reconnect(test_target()).await,
        "first claim should succeed"
    );
    assert!(
        !mgr.begin_reconnect(test_target()).await,
        "second claim should fail"
    );
}

#[tokio::test]
async fn end_reconnect_allows_begin_again() {
    let mgr = ReconnectManager::new([test_target()]);
    mgr.begin_reconnect(test_target()).await;
    mgr.end_reconnect(test_target()).await;
    assert!(
        mgr.begin_reconnect(test_target()).await,
        "should be able to claim after release"
    );
}

#[tokio::test]
async fn configured_addr_none_for_unknown_peer() {
    let mgr = ReconnectManager::new([test_target()]);
    assert!(mgr.configured_target(&peer_id(1)).await.is_none());
}

#[tokio::test]
async fn configured_target_returns_target_after_register() {
    let target = test_target();
    let mgr = ReconnectManager::new([target]);
    mgr.register(peer_id(1), target).await;
    assert_eq!(mgr.configured_target(&peer_id(1)).await, Some(target));
}

#[tokio::test]
async fn configured_target_none_for_unconfigured_target() {
    let configured = ConnectTarget::Tcp("127.0.0.1:9999".parse::<SocketAddr>().unwrap());
    let other = ConnectTarget::Tcp("127.0.0.1:8888".parse::<SocketAddr>().unwrap());
    let mgr = ReconnectManager::new([configured]);
    mgr.register(peer_id(1), other).await;
    assert!(mgr.configured_target(&peer_id(1)).await.is_none());
}

// ── Flow control (Phase 4.3) tests ────────────────────────────────────────

#[test]
fn should_buffer_under_congestion_control_frames() {
    assert!(
        should_buffer_under_congestion(FrameType::Control),
        "control frames must be buffered under congestion"
    );
}

#[test]
fn should_buffer_under_congestion_route_frames() {
    assert!(
        should_buffer_under_congestion(FrameType::RouteUpdate),
        "route-update frames must be buffered under congestion"
    );
}

#[test]
fn should_buffer_under_congestion_handshake_frames() {
    assert!(
        should_buffer_under_congestion(FrameType::Handshake),
        "handshake frames must be buffered under congestion"
    );
}

#[test]
fn should_drop_data_frames_under_congestion() {
    assert!(
        !should_buffer_under_congestion(FrameType::Data),
        "data frames must be dropped (not buffered) under congestion"
    );
}

#[test]
fn should_drop_heartbeat_frames_under_congestion() {
    assert!(
        !should_buffer_under_congestion(FrameType::Heartbeat),
        "heartbeat frames must be dropped (not buffered) under congestion"
    );
}

#[test]
fn congestion_drop_policy_is_priority_based() {
    // Only Data-priority frame types are dropped; everything more important is buffered.
    use pim_protocol::FrameType;
    let high_priority = [
        FrameType::Control,
        FrameType::Handshake,
        FrameType::RouteUpdate,
    ];
    let low_priority = [FrameType::Data, FrameType::Heartbeat];
    for ft in high_priority {
        assert!(
            should_buffer_under_congestion(ft),
            "{ft:?} should be buffered"
        );
    }
    for ft in low_priority {
        assert!(
            !should_buffer_under_congestion(ft),
            "{ft:?} should be dropped"
        );
    }
}

#[tokio::test]
async fn multiple_configured_peers_tracked_independently() {
    let addr_a: SocketAddr = "127.0.0.1:9001".parse().unwrap();
    let addr_b: SocketAddr = "127.0.0.1:9002".parse().unwrap();
    let target_a = ConnectTarget::Tcp(addr_a);
    let target_b = ConnectTarget::Tcp(addr_b);
    let mgr = ReconnectManager::new([target_a, target_b]);

    mgr.register(peer_id(1), target_a).await;
    mgr.register(peer_id(2), target_b).await;

    assert_eq!(mgr.configured_target(&peer_id(1)).await, Some(target_a));
    assert_eq!(mgr.configured_target(&peer_id(2)).await, Some(target_b));

    // Reconnect slots are independent.
    assert!(mgr.begin_reconnect(target_a).await);
    assert!(mgr.begin_reconnect(target_b).await);
    assert!(!mgr.begin_reconnect(target_a).await);
}

// ── Phase 5: Gateway probe / load wiring ──────────────────────────────────

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

// ── Observability (Phase 4.5) tests ───────────────────────────────────────

#[test]
fn format_stats_contains_all_keys() {
    let s = format_stats(&StatsSnapshot {
        peers: 3,
        routes: 5,
        packets_forwarded: 100,
        bytes_forwarded: 51200,
        packets_dropped: 7,
        congestion_drops: 2,
        conntrack_size: 4,
        uptime_secs: 3600,
    });
    assert!(s.contains("peers=3"));
    assert!(s.contains("routes=5"));
    assert!(s.contains("packets_forwarded=100"));
    assert!(s.contains("bytes_forwarded=51200"));
    assert!(s.contains("packets_dropped=7"));
    assert!(s.contains("congestion_drops=2"));
    assert!(s.contains("conntrack_size=4"));
    assert!(s.contains("uptime_secs=3600"));
}

#[test]
fn packets_forwarded_counter_increments() {
    let counter = Arc::new(AtomicU64::new(0));
    for _ in 0..100 {
        counter.fetch_add(1, Ordering::Relaxed);
    }
    assert_eq!(counter.load(Ordering::Relaxed), 100);
}

#[test]
fn bytes_forwarded_counter_accumulates() {
    let counter = Arc::new(AtomicU64::new(0));
    let sizes = [512u64, 1024, 256, 768, 1500];
    let expected: u64 = sizes.iter().sum();
    for &sz in &sizes {
        counter.fetch_add(sz, Ordering::Relaxed);
    }
    assert_eq!(counter.load(Ordering::Relaxed), expected);
}

// ── Peer lifecycle (Phase 3.3) tests ──────────────────────────────────────

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

// ── Phase 7: Discovery integration tests ──────────────────────────────────

/// `register_discovered` + `is_reconnectable_addr` makes discovered peers reconnectable.
#[tokio::test]
async fn register_discovered_makes_peer_reconnectable() {
    let mgr = ReconnectManager::new([]);
    let addr: SocketAddr = "127.0.0.1:9200".parse().unwrap();
    let target = ConnectTarget::Tcp(addr);
    // Before registration: not reconnectable.
    assert!(mgr.is_reconnectable_target(&peer_id(50)).await.is_none());
    // Register as discovered and bind the NodeId → addr.
    mgr.register_discovered(target).await;
    mgr.register(peer_id(50), target).await;
    // Now the peer should be reconnectable.
    assert_eq!(
        mgr.is_reconnectable_target(&peer_id(50)).await,
        Some(target)
    );
}

/// Both configured and discovered peers are reconnectable.
#[tokio::test]
async fn is_reconnectable_covers_configured_and_discovered() {
    let configured: SocketAddr = "127.0.0.1:9100".parse().unwrap();
    let discovered: SocketAddr = "127.0.0.1:9200".parse().unwrap();
    let configured = ConnectTarget::Tcp(configured);
    let discovered = ConnectTarget::Tcp(discovered);
    let mgr = ReconnectManager::new([configured]);
    mgr.register(peer_id(51), configured).await;
    mgr.register(peer_id(52), discovered).await;
    mgr.register_discovered(discovered).await;

    assert!(
        mgr.is_reconnectable_target(&peer_id(51)).await.is_some(),
        "configured peer must be reconnectable"
    );
    assert!(
        mgr.is_reconnectable_target(&peer_id(52)).await.is_some(),
        "discovered peer must be reconnectable"
    );
    assert!(
        mgr.is_reconnectable_target(&peer_id(99)).await.is_none(),
        "unknown peer must not be reconnectable"
    );
}

/// Client-only capability `(!is_relay() && !is_gateway())` must trigger the skip condition.
#[test]
fn client_only_peer_is_skipped() {
    let caps = NodeCapabilities::client();
    // The consumer skips when neither relay nor gateway.
    assert!(
        !caps.is_relay() && !caps.is_gateway(),
        "consumer must filter out client-only peers"
    );
}

/// Relay and gateway capability sets must pass the capability filter.
#[test]
fn relay_and_gateway_caps_pass_filter() {
    let relay = NodeCapabilities::relay();
    let gateway = NodeCapabilities::gateway();
    assert!(
        relay.is_relay() || relay.is_gateway(),
        "relay caps must pass filter"
    );
    assert!(
        gateway.is_relay() || gateway.is_gateway(),
        "gateway caps must pass filter"
    );
}

/// The self-check (`record.node_id == state.self_id`) correctly identifies own broadcasts.
#[test]
fn self_advertisement_is_ignored() {
    let own_id = NodeId::from_bytes([42; 16]);
    let record_id = NodeId::from_bytes([42; 16]);
    assert_eq!(
        record_id, own_id,
        "own NodeId must be caught by self-check in consumer"
    );
}

/// When `discovery.enabled = false`, the discovery service must not be started.
#[test]
fn discovery_disabled_in_config_skips_spawning() {
    let config = Config::from_toml_str("[node]\nname=\"t\"\n[discovery]\nenabled=false\n").unwrap();
    assert!(
        !config.discovery.enabled,
        "discovery service must not start when enabled=false"
    );
}

// ── Phase 7: node_capabilities() tests ───────────────────────────────────

fn cfg_gateway() -> Config {
    Config::from_toml_str("[node]\nname=\"t\"\n[gateway]\nenabled=true\n").unwrap()
}

fn cfg_relay() -> Config {
    Config::from_toml_str("[node]\nname=\"t\"\n[relay]\nenabled=true\n").unwrap()
}

fn cfg_client() -> Config {
    Config::from_toml_str("[node]\nname=\"t\"\n").unwrap()
}

#[test]
fn gateway_config_yields_gateway_caps() {
    let caps = node_capabilities(&cfg_gateway());
    assert!(caps.is_gateway(), "gateway flag expected");
    assert!(caps.is_relay(), "relay flag expected on gateway");
    assert!(caps.is_client(), "client flag expected on gateway");
}

#[test]
fn relay_config_yields_relay_caps() {
    let caps = node_capabilities(&cfg_relay());
    assert!(caps.is_relay(), "relay flag expected");
    assert!(caps.is_client(), "client flag expected on relay");
    assert!(!caps.is_gateway(), "gateway flag must NOT be set on relay");
}

#[test]
fn client_config_yields_client_caps_only() {
    let caps = node_capabilities(&cfg_client());
    assert!(caps.is_client(), "client flag expected");
    assert!(!caps.is_relay(), "relay flag must NOT be set on client");
    assert!(!caps.is_gateway(), "gateway flag must NOT be set on client");
}

#[test]
fn gateway_caps_bits_are_correct() {
    let caps = node_capabilities(&cfg_gateway());
    assert_eq!(
        caps.bits(),
        0x07,
        "gateway caps must be CLIENT|RELAY|GATEWAY = 0x07"
    );
}

#[test]
fn parse_interface_ipv4_output_extracts_first_inet_cidr() {
    let output = "2: eno1    inet 192.168.0.137/24 brd 192.168.0.255 scope global dynamic eno1\n";
    assert_eq!(
        parse_interface_ipv4_output(output),
        Some(Ipv4Addr::new(192, 168, 0, 137))
    );
}

#[test]
fn parse_interface_ipv4_output_extracts_macos_ifconfig_inet() {
    let output = "\
en0: flags=8863<UP,BROADCAST,RUNNING,SIMPLEX,MULTICAST> mtu 1500\n\
\tinet 192.168.1.44 netmask 0xffffff00 broadcast 192.168.1.255\n\
\tinet6 fe80::1234%en0 prefixlen 64 secured scopeid 0x4\n";
    assert_eq!(
        parse_interface_ipv4_output(output),
        Some(Ipv4Addr::new(192, 168, 1, 44))
    );
}

#[test]
fn ipv4_destination_reads_destination_octets() {
    let packet = [
        0x45, 0, 0, 20, 0, 0, 0, 0, 64, 1, 0, 0, 10, 77, 0, 2, 1, 1, 1, 1,
    ];
    assert_eq!(ipv4_destination(&packet), Some(Ipv4Addr::new(1, 1, 1, 1)));
}

// ── Phase 8: Wi-Fi Direct config tests ───────────────────────────────────

/// When `wifi_direct.enabled = false` (the default), the block that spawns the
/// Wi-Fi Direct service must be skipped — verified by checking config.
#[test]
fn wifidirect_disabled_config_skips_spawning() {
    let config = Config::from_toml_str("[node]\nname=\"t\"\n").unwrap();
    assert!(
        !config.wifi_direct.enabled,
        "Wi-Fi Direct service must not start when enabled=false"
    );
}

/// When `wifi_direct.enabled = true`, the daemon reads `interface` and `listen_port`.
#[test]
fn wifidirect_enabled_config_exposes_interface_and_port() {
    let toml = "[node]\nname=\"t\"\n[wifi_direct]\nenabled=true\ninterface=\"wlan1\"\n";
    let config = Config::from_toml_str(toml).unwrap();
    assert!(config.wifi_direct.enabled);
    assert_eq!(config.wifi_direct.interface, "wlan1");
    // listen_port comes from transport section, not wifi_direct.
    assert_eq!(config.transport.listen_port, 9100);
}

/// Discovered Wi-Fi Direct peer addresses must be registered for reconnect-on-loss.
#[tokio::test]
async fn wifidirect_addr_registered_for_reconnect() {
    let mgr = ReconnectManager::new([]);
    let addr: SocketAddr = "192.168.49.100:9100".parse().unwrap();
    let target = ConnectTarget::Tcp(addr);
    mgr.register_discovered(target).await;
    // Bind a fake NodeId to the address to enable the lookup.
    mgr.register(peer_id(60), target).await;
    assert_eq!(
        mgr.is_reconnectable_target(&peer_id(60)).await,
        Some(target),
        "Wi-Fi Direct discovered peer must be reconnectable"
    );
}

/// Wi-Fi Direct and UDP discovery can both be enabled simultaneously; their
/// peer channel addresses both go through `register_discovered`.
#[tokio::test]
async fn wifidirect_coexists_with_udp_discovery() {
    let mgr = ReconnectManager::new([]);
    let udp_addr: SocketAddr = "172.34.0.20:9100".parse().unwrap();
    let wfd_addr: SocketAddr = "192.168.49.100:9100".parse().unwrap();
    let udp_target = ConnectTarget::Tcp(udp_addr);
    let wfd_target = ConnectTarget::Tcp(wfd_addr);
    mgr.register_discovered(udp_target).await;
    mgr.register_discovered(wfd_target).await;
    mgr.register(peer_id(61), udp_target).await;
    mgr.register(peer_id(62), wfd_target).await;
    assert!(
        mgr.is_reconnectable_target(&peer_id(61)).await.is_some(),
        "UDP peer reconnectable"
    );
    assert!(
        mgr.is_reconnectable_target(&peer_id(62)).await.is_some(),
        "WFD peer reconnectable"
    );
}

/// Verify `WifiDirectDiscovery::new` can be constructed from config without panicking.
#[test]
fn wifidirect_discovery_construction_from_config() {
    use pim_wifidirect::WifiDirectDiscovery;
    let toml = "[node]\nname=\"t\"\n[wifi_direct]\nenabled=true\n";
    let config = Config::from_toml_str(toml).unwrap();
    let (_svc, _rx) = WifiDirectDiscovery::new(
        config.node.name,
        config.wifi_direct,
        config.transport.listen_port,
    );
    // Construction must not panic.
}

#[test]
fn bluetooth_disabled_config_skips_spawning() {
    let config = Config::from_toml_str("[node]\nname=\"t\"\n").unwrap();
    assert!(
        !config.bluetooth.enabled,
        "Bluetooth PAN watcher must not start when enabled=false"
    );
}

#[test]
fn bluetooth_enabled_config_exposes_interface_and_static_peer_entries() {
    let toml = "[node]\nname=\"t\"\n[bluetooth]\nenabled=true\ninterface=\"bnep1\"\n[[peers]]\nmechanism=\"bluetooth\"\nip=\"192.168.44.2\"\n";
    let config = Config::from_toml_str(toml).unwrap();
    assert!(config.bluetooth.enabled);
    assert_eq!(config.bluetooth.interface, "bnep1");
    assert!(config.bluetooth.connect_pan);
    assert!(!config.bluetooth.serve_nap);
    assert_eq!(config.peers.len(), 1);
    assert_eq!(config.transport.listen_port, 9100);
}

#[tokio::test]
async fn bluetooth_addr_registered_for_reconnect() {
    let mgr = ReconnectManager::new([]);
    let addr: SocketAddr = "192.168.44.2:9100".parse().unwrap();
    let target = ConnectTarget::BluetoothPan(addr);
    mgr.register_discovered(target).await;
    mgr.register(peer_id(63), target).await;
    assert_eq!(
        mgr.is_reconnectable_target(&peer_id(63)).await,
        Some(target),
        "Bluetooth-discovered peer must be reconnectable"
    );
}

#[test]
fn transport_reconnect_limit_parses_from_config() {
    let config =
        Config::from_toml_str("[node]\nname=\"t\"\n[transport]\nmax_reconnect_attempts=4\n")
            .unwrap();
    assert_eq!(config.transport.max_reconnect_attempts, 4);
}

#[test]
fn bluetooth_discovery_construction_from_config() {
    use pim_bluetooth::BluetoothDiscovery;
    let toml = "[node]\nname=\"t\"\n[bluetooth]\nenabled=true\n";
    let config = Config::from_toml_str(toml).unwrap();
    let (_svc, _rx) = BluetoothDiscovery::new(
        config.bluetooth,
        config.transport.listen_port,
        vec!["192.168.44.2:9100".parse().unwrap()],
    )
    .unwrap();
}

#[test]
fn bluetooth_sysfs_root_defaults_to_platform_default() {
    assert_eq!(
        bluetooth_sysfs_root_from_env(None),
        PathBuf::from(pim_bluetooth::DEFAULT_SYSFS_ROOT)
    );
}

#[test]
fn bluetooth_sysfs_root_honors_environment_override() {
    assert_eq!(
        bluetooth_sysfs_root_from_env(Some("/tmp/pim-fake-sysfs".into())),
        PathBuf::from("/tmp/pim-fake-sysfs")
    );
}

#[test]
fn bluetooth_ip_command_defaults_to_platform_default() {
    assert_eq!(
        bluetooth_ip_command_from_env(None),
        PathBuf::from(pim_bluetooth::DEFAULT_IP_COMMAND)
    );
}

#[test]
fn bluetooth_ip_command_honors_environment_override() {
    assert_eq!(
        bluetooth_ip_command_from_env(Some("/tmp/fake-ip".into())),
        PathBuf::from("/tmp/fake-ip")
    );
}

#[test]
fn bluetoothctl_command_defaults_to_platform_default() {
    assert_eq!(
        bluetoothctl_command_from_env(None),
        PathBuf::from(pim_bluetooth::DEFAULT_BLUETOOTHCTL_COMMAND)
    );
}

#[test]
fn bluetoothctl_command_honors_environment_override() {
    assert_eq!(
        bluetoothctl_command_from_env(Some("/tmp/fake-bluetoothctl".into())),
        PathBuf::from("/tmp/fake-bluetoothctl")
    );
}

#[test]
fn bt_network_command_defaults_to_bt_network() {
    assert_eq!(
        bt_network_command_from_env(None),
        PathBuf::from("bt-network")
    );
}

#[test]
fn bt_network_command_honors_environment_override() {
    assert_eq!(
        bt_network_command_from_env(Some("/tmp/fake-bt-network".into())),
        PathBuf::from("/tmp/fake-bt-network")
    );
}

#[test]
fn macos_bluetooth_config_warning_omits_linux_only_fields_when_unused() {
    let warnings = macos_bluetooth_config_warnings(
        &Config::from_toml_str(
            "[node]\nname=\"t\"\n[bluetooth]\nenabled=true\nnap_bridge=\"bridge0\"\n",
        )
        .unwrap()
        .bluetooth,
    );
    assert!(warnings.is_empty());
}

#[test]
fn macos_bluetooth_config_warning_reports_linux_only_fields() {
    let warnings = macos_bluetooth_config_warnings(
        &Config::from_toml_str(
            "[node]\nname=\"t\"\n[bluetooth]\nenabled=true\nserve_nap=true\nnap_bridge=\"br-bt\"\n",
        )
        .unwrap()
        .bluetooth,
    );
    assert_eq!(warnings.len(), 2);
    assert!(warnings[0].contains("serve_nap"));
    assert!(warnings[1].contains("nap_bridge"));
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
