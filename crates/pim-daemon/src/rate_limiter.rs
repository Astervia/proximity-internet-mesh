//! Per-peer token-bucket rate limiting.
//!
//! Each peer gets an independent token bucket.  On every incoming frame the
//! daemon calls `RateLimiter::allow`, consuming one token.  When the bucket
//! is empty the frame is dropped and the packet-drop counter is incremented.
//!
//! # Defaults
//!
//! - Burst capacity: 500 frames  (absorbs short bursts without false drops)
//! - Refill rate:    200 frames/sec  (~5 ms per frame, sustainable throughput)

use std::collections::HashMap;
use std::time::Instant;

use pim_core::NodeId;

/// Default burst capacity per peer (frames).
pub const DEFAULT_BURST: u32 = 500;
/// Default sustained rate per peer (frames per second).
pub const DEFAULT_RATE: f64 = 200.0;

/// Token bucket for a single peer.
pub struct TokenBucket {
    capacity: u32,
    tokens: f64,
    refill_rate: f64, // tokens / second
    last_refill: Instant,
}

impl TokenBucket {
    fn new(capacity: u32, refill_rate: f64) -> Self {
        Self {
            capacity,
            tokens: capacity as f64,
            refill_rate,
            last_refill: Instant::now(),
        }
    }

    /// Attempt to consume one token.  Returns `true` if the frame is allowed.
    pub fn try_consume(&mut self) -> bool {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.capacity as f64);
        self.last_refill = now;

        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// Global rate limiter: one `TokenBucket` per peer.
pub struct RateLimiter {
    buckets: HashMap<NodeId, TokenBucket>,
    capacity: u32,
    refill_rate: f64,
}

impl RateLimiter {
    pub fn new(capacity: u32, refill_rate: f64) -> Self {
        Self {
            buckets: HashMap::new(),
            capacity,
            refill_rate,
        }
    }

    /// Returns `true` if the next frame from `peer_id` is within the rate limit.
    pub fn allow(&mut self, peer_id: &NodeId) -> bool {
        let cap = self.capacity;
        let rate = self.refill_rate;
        self.buckets
            .entry(*peer_id)
            .or_insert_with(|| TokenBucket::new(cap, rate))
            .try_consume()
    }

    /// Remove the bucket for a peer that has disconnected.
    pub fn remove_peer(&mut self, peer_id: &NodeId) {
        self.buckets.remove(peer_id);
    }
}

#[cfg(test)]
#[path = "rate_limiter/tests.rs"]
mod tests;
