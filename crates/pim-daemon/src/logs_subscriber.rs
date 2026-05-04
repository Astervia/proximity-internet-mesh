//! In-memory log fan-out for the JSON-RPC `logs.event` notification stream.
//!
//! `pim-daemon` uses `tracing` everywhere. To expose the same stream over
//! RPC without re-instrumenting every callsite, we install a custom
//! `tracing_subscriber::Layer` (`LogsLayer`) that converts each event
//! into a JSON payload matching docs/RPC.md §5.6 (`level`, `source`,
//! `message`, `at`, optional `fields`) and broadcasts it to any RPC
//! client that has called `logs.subscribe`.
//!
//! Architecture:
//!
//!   * Single `tokio::sync::broadcast::Sender<Value>` lives behind a
//!     `OnceLock` for the lifetime of the process. New connections call
//!     `subscribe()` to get their own `Receiver`.
//!   * Capacity is bounded — a slow subscriber falls behind by at most
//!     `CAPACITY` events; older events are dropped (the `RecvError::Lagged`
//!     branch is logged once and the receiver re-syncs from the head).
//!   * `LogsLayer::on_event` is hot-path code on every log call. We
//!     intentionally avoid heap allocation for the visitor message
//!     buffer and skip the broadcast entirely if no subscriber exists
//!     (zero-cost when no UI is attached).
//!
//! Filtering: clients can pass `min_level` to `logs.subscribe`; the
//! filter is applied in the RPC `logs.subscribe` handler when forwarding,
//! NOT here. This layer always broadcasts every event the surrounding
//! `EnvFilter` admits.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};
use tokio::sync::broadcast;
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

/// Per-subscriber backlog ceiling on the broadcast channel. 1024 events
/// covers ~5 min of typical daemon chatter without unbounded memory
/// growth even if a client disappears mid-stream.
const CAPACITY: usize = 1024;

/// History ring buffer size. Each `logs.subscribe` call replays the most
/// recent `HISTORY_CAPACITY` events before joining the live stream. This
/// is what gives the UI the daemon's startup logs ("daemon starting",
/// "TUN up", "rpc listening", etc.) even though the UI subscribes only
/// AFTER it connects, which is well after those events fired.
const HISTORY_CAPACITY: usize = 2048;

static SENDER: OnceLock<broadcast::Sender<Value>> = OnceLock::new();
static HISTORY: OnceLock<Mutex<VecDeque<Value>>> = OnceLock::new();

/// Install the global broadcast sender + history ring buffer, then
/// return the `Layer` to wire into `tracing_subscriber::registry()`.
/// Safe to call once per process; debug builds assert otherwise.
pub(crate) fn init() -> LogsLayer {
    let (tx, _rx) = broadcast::channel(CAPACITY);
    if SENDER.set(tx).is_err() {
        debug_assert!(false, "logs_subscriber::init called twice (SENDER)");
    }
    if HISTORY
        .set(Mutex::new(VecDeque::with_capacity(HISTORY_CAPACITY)))
        .is_err()
    {
        debug_assert!(false, "logs_subscriber::init called twice (HISTORY)");
    }
    LogsLayer
}

/// Subscribe to the live broadcast WITHOUT taking a history snapshot.
/// Used by the per-connection logs forwarder, which is spawned at
/// connect time and forwards every event for the lifetime of the
/// connection. History replay is owned by `logs.subscribe` (gated by a
/// per-connection `history_replayed` flag so it fires exactly once per
/// connection regardless of how many subscribers the UI registers).
///
/// Splitting `subscribe_with_history` into "live only" + "history only"
/// is what lets a single `logs.subscribe` call be a no-op-with-history
/// instead of spawning a fresh forwarder task — every extra forwarder
/// per connection used to write a duplicate copy of every log line, so
/// StrictMode double-mount, HMR module replacement, or any second
/// `logs.subscribe` call on the same connection multiplied every event
/// the UI rendered.
pub(crate) fn live_subscribe() -> Option<broadcast::Receiver<Value>> {
    SENDER.get().map(|s| s.subscribe())
}

/// Take a snapshot of the history ring buffer without subscribing to
/// the live broadcast. Caller is responsible for sequencing this
/// against the live forwarder if it wants no overlap (the live
/// forwarder subscribes once at connect time, before the first
/// `logs.subscribe`, so a tiny overlap window with replay is possible
/// but the entries are identical and the UI dedupes consecutive
/// duplicates anyway).
pub(crate) fn history_snapshot() -> Option<Vec<Value>> {
    let history = HISTORY.get()?;
    let buf = history.lock().ok()?;
    Some(buf.iter().cloned().collect())
}

pub(crate) struct LogsLayer;

impl<S> Layer<S> for LogsLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let (Some(tx), Some(history)) = (SENDER.get(), HISTORY.get()) else {
            return;
        };

        // Note: we do NOT skip when receiver_count == 0 anymore. The
        // history buffer must keep the daemon's startup logs even when
        // nobody is listening yet, so the FIRST `logs.subscribe` (which
        // happens after the UI connects, ie minutes after `daemon
        // starting`) can replay them. The cost is one allocation per
        // log event; bounded by EnvFilter.

        let metadata = event.metadata();
        let level = metadata.level().as_str().to_lowercase();
        let target = metadata.target().to_string();

        let mut visitor = LogVisitor::default();
        event.record(&mut visitor);

        let ts = system_time_to_iso8601(SystemTime::now());

        let mut payload = Map::with_capacity(5);
        // The wire field name is `ts` per pim-ui's LogEvent type
        // (rpc-types.ts §5.6) — NOT `at`. The two field names get
        // confused easily because docs/RPC.md §2.4 generic notification
        // examples use `at`; the typed LogEvent and PeerEvent both use
        // `ts`/`at` differently. The UI is the source of truth here:
        // LogEvent.ts is the field consumers check.
        payload.insert("ts".to_string(), Value::String(ts));
        payload.insert("level".to_string(), Value::String(level));
        payload.insert("source".to_string(), Value::String(target));
        payload.insert(
            "message".to_string(),
            Value::String(std::mem::take(&mut visitor.message)),
        );
        if !visitor.fields.is_empty() {
            payload.insert("fields".to_string(), Value::Object(visitor.fields));
        }
        let value = Value::Object(payload);

        // Critical section: hold the history lock across BOTH the
        // ring-buffer push AND the broadcast send. This blocks
        // `subscribe_with_history` from observing inconsistent state
        // (event in history but not yet broadcast, or vice-versa).
        // Lock contention is negligible — events arrive at most a
        // few hundred per second under stress, and the work inside
        // the lock is microseconds.
        if let Ok(mut buf) = history.lock() {
            if buf.len() >= HISTORY_CAPACITY {
                buf.pop_front();
            }
            buf.push_back(value.clone());
            let _ = tx.send(value);
        }
    }
}

#[derive(Default)]
struct LogVisitor {
    message: String,
    fields: Map<String, Value>,
}

impl tracing::field::Visit for LogVisitor {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            self.fields
                .insert(field.name().to_string(), Value::String(value.to_string()));
        }
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.fields.insert(field.name().to_string(), json!(value));
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.fields.insert(field.name().to_string(), json!(value));
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.fields.insert(field.name().to_string(), json!(value));
    }

    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        self.fields.insert(field.name().to_string(), json!(value));
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let formatted = format!("{value:?}");
        if field.name() == "message" {
            self.message = formatted;
        } else {
            self.fields
                .insert(field.name().to_string(), Value::String(formatted));
        }
    }
}

/// RFC-3339 / ISO-8601 formatter using only std primitives — avoids
/// pulling chrono just for this. Same algorithm used in `rpc.rs`.
fn system_time_to_iso8601(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let days_since_epoch = secs / 86_400;
    let secs_today = secs % 86_400;
    let hh = secs_today / 3600;
    let mm = (secs_today % 3600) / 60;
    let ss = secs_today % 60;
    let (y, mo, d) = days_to_ymd(days_since_epoch as i64);
    format!("{y:04}-{mo:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

fn days_to_ymd(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i32 + era as i32 * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_round_trips_known_epoch() {
        let t = UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        assert_eq!(system_time_to_iso8601(t), "2023-11-14T22:13:20Z");
    }

    #[test]
    fn live_subscribe_and_history_snapshot_return_none_before_init() {
        // SENDER + HISTORY are process-global; this only holds in a
        // fresh test process. `cargo test` provides one per integration
        // test. We skip if SENDER happens to already be set.
        if SENDER.get().is_none() {
            assert!(live_subscribe().is_none());
            assert!(history_snapshot().is_none());
        }
    }
}
