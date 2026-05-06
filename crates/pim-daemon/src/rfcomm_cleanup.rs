//! Phase 2 of `plans/rfcomm-reconnect/plan.md` — opt-in periodic
//! cleanup of paired RFCOMM peers we have not been able to reach in a
//! long time.
//!
//! The task iterates the `rfcomm_peer_lifecycle` table on
//! `PeerDirectoryService` (peers.db) populated by Phase 1
//! (`observe_rfcomm_paired` / `record_rfcomm_connected`). For each row
//! whose freshness signal is older than `max_unreachable_lifetime_s`
//! AND whose peer is not currently connected (per BlueZ), the daemon
//! shells out to `bluetoothctl remove <bd_addr>` and drops the
//! lifecycle row.
//!
//! Default-off. Reading the schedule in seconds — not minutes — keeps
//! the comparison code symmetrical with `first_paired_at_s`.
//!
//! This module is currently Linux-only; the rest of the daemon
//! integrates via `app.rs` which already gates the RFCOMM service on
//! `target_os = "linux"`.

#![cfg(target_os = "linux")]

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::process::Command;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::peer_directory::PeerDirectoryService;

/// Bounds applied to user-supplied `max_unreachable_lifetime_s`.
/// 1 hour is the smallest setting that still represents an
/// "unreachable for a long time" intent — anything shorter is almost
/// certainly a typoed minutes-vs-seconds confusion that would
/// silently delete pairings.
const MIN_LIFETIME_S: u64 = 3600;
/// 60 s lower bound on the sweep cadence — labs run with values like
/// 2 seconds for fast verification, but the production minimum
/// protects against a config that would otherwise busy-loop
/// bluetoothctl.
const MIN_INTERVAL_S: u64 = 60;

/// Inputs the cleanup task needs. Built from `BluetoothRfcommConfig`
/// in `app.rs`; isolated as a struct so unit tests can construct one
/// without dragging in the full config tree.
#[derive(Debug, Clone)]
pub(crate) struct CleanupConfig {
    /// `bluetoothctl` (or override) — same path the rest of the
    /// RFCOMM stack uses. Resolved by `bluetoothctl_command()` in
    /// `bluetooth_env.rs`.
    pub bluetoothctl_command: PathBuf,
    /// Threshold past which an unreachable peer is unpaired.
    pub max_unreachable_lifetime: Duration,
    /// Sweep cadence.
    pub interval: Duration,
}

/// Compose a `CleanupConfig` from user-supplied seconds, applying
/// the safety floors. Returned `Duration` values are guaranteed to be
/// at least `MIN_LIFETIME_S` and `MIN_INTERVAL_S` respectively.
pub(crate) fn build_cleanup_config(
    bluetoothctl_command: PathBuf,
    max_unreachable_lifetime_s: u64,
    cleanup_interval_s: u64,
) -> CleanupConfig {
    let lifetime = max_unreachable_lifetime_s.max(MIN_LIFETIME_S);
    let interval = cleanup_interval_s.max(MIN_INTERVAL_S);
    CleanupConfig {
        bluetoothctl_command,
        max_unreachable_lifetime: Duration::from_secs(lifetime),
        interval: Duration::from_secs(interval),
    }
}

/// Spawn the periodic cleanup task. Cancellable via the supplied
/// token. The task logs every removal at INFO with the `last_seen`
/// timestamp and the reason ("unreachable for N days") so an operator
/// can audit destructive actions in the journal.
pub(crate) fn spawn(
    cfg: CleanupConfig,
    directory: Arc<PeerDirectoryService>,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        info!(
            interval_s = cfg.interval.as_secs(),
            lifetime_s = cfg.max_unreachable_lifetime.as_secs(),
            "rfcomm peer cleanup task started"
        );
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    debug!("rfcomm peer cleanup shutdown");
                    return;
                }
                _ = tokio::time::sleep(cfg.interval) => {}
            }
            if let Err(e) = run_once(&cfg, &directory).await {
                warn!("rfcomm peer cleanup tick failed: {e}");
            }
        }
    });
}

/// Single sweep of the lifecycle table. Pulled out so unit tests can
/// drive it without a tokio scheduler.
pub(crate) async fn run_once(
    cfg: &CleanupConfig,
    directory: &Arc<PeerDirectoryService>,
) -> anyhow::Result<()> {
    let directory_for_list = directory.clone();
    let rows =
        tokio::task::spawn_blocking(move || directory_for_list.list_rfcomm_lifecycle()).await??;
    if rows.is_empty() {
        debug!("rfcomm cleanup: lifecycle table empty");
        return Ok(());
    }

    let paired = scan_paired_addrs(&cfg.bluetoothctl_command).await?;
    let now_s = unix_seconds_now();
    let lifetime_s = cfg.max_unreachable_lifetime.as_secs() as i64;

    for row in rows {
        if !paired.contains(&row.bd_addr) {
            // User unpaired manually — drop the dangling lifecycle
            // row so the table reflects ground truth.
            forget_lifecycle(directory, &row.bd_addr).await;
            info!(
                bd_addr = %row.bd_addr,
                name = %row.name,
                "rfcomm cleanup: peer no longer paired in BlueZ; dropped lifecycle row"
            );
            continue;
        }

        let last_seen = std::cmp::max(
            row.first_paired_at_s,
            row.last_connected_at_s.unwrap_or(row.first_paired_at_s),
        );
        let age_s = now_s.saturating_sub(last_seen);
        if age_s <= lifetime_s {
            continue;
        }

        if is_connected(&cfg.bluetoothctl_command, &row.bd_addr).await {
            // Currently in a session. Don't touch — they're alive.
            continue;
        }

        match remove_paired_device(&cfg.bluetoothctl_command, &row.bd_addr).await {
            Ok(()) => {
                forget_lifecycle(directory, &row.bd_addr).await;
                info!(
                    bd_addr = %row.bd_addr,
                    name = %row.name,
                    last_seen_s = last_seen,
                    age_days = age_s / 86_400,
                    "rfcomm cleanup: unpaired unreachable peer"
                );
            }
            Err(e) => {
                warn!(
                    bd_addr = %row.bd_addr,
                    "rfcomm cleanup: bluetoothctl remove failed: {e}"
                );
            }
        }
    }

    Ok(())
}

async fn scan_paired_addrs(bluetoothctl: &std::path::Path) -> anyhow::Result<HashSet<String>> {
    let out = Command::new(bluetoothctl)
        .args(["devices", "Paired"])
        .output()
        .await?;
    if !out.status.success() {
        anyhow::bail!(
            "bluetoothctl devices Paired exit {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut addrs = HashSet::new();
    for line in stdout.lines() {
        // Format: "Device AA:BB:CC:DD:EE:FF Some Name"
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Device ") {
            if let Some((addr, _)) = rest.split_once(' ') {
                addrs.insert(addr.to_string());
            }
        }
    }
    Ok(addrs)
}

async fn is_connected(bluetoothctl: &std::path::Path, bd_addr: &str) -> bool {
    let out = match Command::new(bluetoothctl)
        .args(["info", bd_addr])
        .output()
        .await
    {
        Ok(o) => o,
        Err(_) => return false,
    };
    if !out.status.success() {
        return false;
    }
    parse_connected(&String::from_utf8_lossy(&out.stdout))
}

async fn remove_paired_device(bluetoothctl: &std::path::Path, bd_addr: &str) -> anyhow::Result<()> {
    let out = Command::new(bluetoothctl)
        .args(["remove", bd_addr])
        .output()
        .await?;
    if !out.status.success() {
        anyhow::bail!(
            "bluetoothctl remove {bd_addr} exit {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(())
}

async fn forget_lifecycle(directory: &Arc<PeerDirectoryService>, bd_addr: &str) {
    let directory = directory.clone();
    let addr = bd_addr.to_string();
    let _ = tokio::task::spawn_blocking(move || directory.forget_rfcomm_peer(&addr)).await;
}

fn parse_connected(info_output: &str) -> bool {
    for line in info_output.lines() {
        let line = line.trim();
        if let Some(value) = line.strip_prefix("Connected:") {
            return value.trim().eq_ignore_ascii_case("yes");
        }
    }
    false
}

fn unix_seconds_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_cleanup_config_clamps_too_short_lifetime() {
        let cfg = build_cleanup_config(PathBuf::from("/bin/true"), 60, 600);
        assert_eq!(cfg.max_unreachable_lifetime.as_secs(), MIN_LIFETIME_S);
        assert_eq!(cfg.interval.as_secs(), 600);
    }

    #[test]
    fn build_cleanup_config_clamps_too_short_interval() {
        let cfg = build_cleanup_config(PathBuf::from("/bin/true"), 7200, 5);
        assert_eq!(cfg.max_unreachable_lifetime.as_secs(), 7200);
        assert_eq!(cfg.interval.as_secs(), MIN_INTERVAL_S);
    }

    #[test]
    fn build_cleanup_config_passes_through_when_above_floors() {
        let cfg = build_cleanup_config(PathBuf::from("/bin/true"), 86_400, 600);
        assert_eq!(cfg.max_unreachable_lifetime.as_secs(), 86_400);
        assert_eq!(cfg.interval.as_secs(), 600);
    }

    #[test]
    fn parse_connected_yes() {
        let stdout = "\
Device 00:15:83:3D:0A:57 (public)
\tName: PIM-foo
\tConnected: yes
\tPaired: yes
";
        assert!(parse_connected(stdout));
    }

    #[test]
    fn parse_connected_no() {
        let stdout = "\
Device 00:15:83:3D:0A:57 (public)
\tName: PIM-foo
\tConnected: no
\tPaired: yes
";
        assert!(!parse_connected(stdout));
    }

    #[test]
    fn parse_connected_missing() {
        // `bluetoothctl info` for an unknown device returns just an
        // error line. Treat "no Connected: line" as not-connected so
        // a failed lookup never blocks a removal it should permit.
        let stdout = "Device 00:15:83:3D:0A:57 not available\n";
        assert!(!parse_connected(stdout));
    }

    #[tokio::test]
    async fn run_once_drops_rows_for_peers_no_longer_paired() {
        // /bin/true succeeds with empty stdout — every lifecycle row
        // is treated as "no longer paired" and dropped.
        let dir = open_temp_directory();
        dir.observe_rfcomm_paired("AA:BB:CC:DD:EE:FF", "PIM-x", 100)
            .unwrap();
        let cfg = CleanupConfig {
            bluetoothctl_command: PathBuf::from("/bin/true"),
            max_unreachable_lifetime: Duration::from_secs(MIN_LIFETIME_S),
            interval: Duration::from_secs(MIN_INTERVAL_S),
        };
        let directory = Arc::new(dir);
        run_once(&cfg, &directory).await.expect("run_once");
        assert!(directory.list_rfcomm_lifecycle().unwrap().is_empty());
    }

    #[tokio::test]
    async fn run_once_keeps_fresh_peers_when_scan_fails() {
        // /bin/false errors out → run_once should return Err and
        // leave the lifecycle table untouched. Never assume "absent
        // paired list" means "drop every row" without a successful
        // scan.
        let directory = Arc::new(open_temp_directory());
        directory
            .observe_rfcomm_paired("AA:BB:CC:DD:EE:FF", "PIM-x", unix_seconds_now())
            .unwrap();
        let cfg = CleanupConfig {
            bluetoothctl_command: PathBuf::from("/bin/false"),
            max_unreachable_lifetime: Duration::from_secs(MIN_LIFETIME_S),
            interval: Duration::from_secs(MIN_INTERVAL_S),
        };
        let result = run_once(&cfg, &directory).await;
        assert!(result.is_err(), "expected scan failure to surface");
        assert_eq!(directory.list_rfcomm_lifecycle().unwrap().len(), 1);
    }

    fn open_temp_directory() -> PeerDirectoryService {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("peers.db");
        // Leak the TempDir so the file outlives this helper. Tests
        // in this module don't need cleanup; the OS reclaims `/tmp`
        // on process exit.
        std::mem::forget(dir);
        PeerDirectoryService::open(path).expect("open peers directory")
    }
}
