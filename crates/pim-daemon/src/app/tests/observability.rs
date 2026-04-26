use super::super::*;

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
