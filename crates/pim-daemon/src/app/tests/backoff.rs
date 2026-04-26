use super::super::*;

#[test]
fn backoff_base_grows_exponentially() {
    assert_eq!(backoff_base_ms(0), 1_000);
    assert_eq!(backoff_base_ms(1), 2_000);
    assert_eq!(backoff_base_ms(2), 4_000);
    assert_eq!(backoff_base_ms(3), 8_000);
    assert_eq!(backoff_base_ms(4), 10_000);
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
