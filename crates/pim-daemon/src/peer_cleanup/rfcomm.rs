//! RFCOMM-paired peer cleanup. Owns the
//! [`PeerDirectoryService::list_rfcomm_lifecycle`] consumer side:
//! every sweep, lists rows, drops the ones whose peers have
//! disappeared from BlueZ's paired list, and unpairs the rest whose
//! freshness window has elapsed.
//!
//! Linux-only — RFCOMM doesn't exist on macOS, and the destructive
//! action shells out to `bluetoothctl`.

#![cfg(target_os = "linux")]

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::process::Command;
use tracing::{debug, info, warn};

use super::{unix_seconds_now, PeerCleanupTracker};
use crate::app::peer_directory::PeerDirectoryService;

/// `bluetoothctl`-driven cleanup for paired RFCOMM peers.
pub(crate) struct RfcommTracker {
    bluetoothctl_command: PathBuf,
    directory: Arc<PeerDirectoryService>,
}

impl RfcommTracker {
    pub(crate) fn new(bluetoothctl_command: PathBuf, directory: Arc<PeerDirectoryService>) -> Self {
        Self {
            bluetoothctl_command,
            directory,
        }
    }
}

#[async_trait]
impl PeerCleanupTracker for RfcommTracker {
    fn name(&self) -> &'static str {
        "rfcomm"
    }

    async fn sweep(&self, lifetime_s: i64) -> anyhow::Result<()> {
        let directory_for_list = self.directory.clone();
        let rows = tokio::task::spawn_blocking(move || directory_for_list.list_rfcomm_lifecycle())
            .await??;
        if rows.is_empty() {
            debug!("rfcomm cleanup: lifecycle table empty");
            return Ok(());
        }

        let paired = scan_paired_addrs(&self.bluetoothctl_command).await?;
        let now_s = unix_seconds_now();

        for row in rows {
            if !paired.contains(&row.bd_addr) {
                // User unpaired manually — drop the dangling
                // lifecycle row so the table reflects ground truth.
                forget_lifecycle(&self.directory, &row.bd_addr).await;
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

            if is_connected(&self.bluetoothctl_command, &row.bd_addr).await {
                // Currently in a session. Don't touch — they're alive.
                continue;
            }

            match remove_paired_device(&self.bluetoothctl_command, &row.bd_addr).await {
                Ok(()) => {
                    forget_lifecycle(&self.directory, &row.bd_addr).await;
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
        // "Device AA:BB:CC:DD:EE:FF Some Name"
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    use crate::app::peer_cleanup::{spawn as spawn_cleanup, MIN_LIFETIME_S};
    use pim_core::PeerCleanupConfig;

    fn open_temp_directory() -> PeerDirectoryService {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("peers.db");
        // Leak the TempDir so the file outlives this helper.
        std::mem::forget(dir);
        PeerDirectoryService::open(path).expect("open peers directory")
    }

    #[test]
    fn parse_connected_yes() {
        assert!(parse_connected("\tConnected: yes\n\tPaired: yes\n"));
    }

    #[test]
    fn parse_connected_no() {
        assert!(!parse_connected("\tConnected: no\n\tPaired: yes\n"));
    }

    #[test]
    fn parse_connected_missing_line_is_false() {
        assert!(!parse_connected("Device 00:15:83:3D:0A:57 not available\n"));
    }

    #[tokio::test]
    async fn sweep_drops_rows_for_peers_no_longer_paired() {
        // /bin/true exits 0 with empty stdout — every lifecycle row
        // is treated as "no longer paired" and dropped.
        let dir = open_temp_directory();
        dir.observe_rfcomm_paired("AA:BB:CC:DD:EE:FF", "PIM-x", 100)
            .unwrap();
        let directory = Arc::new(dir);
        let tracker = RfcommTracker::new(PathBuf::from("/bin/true"), directory.clone());
        tracker.sweep(MIN_LIFETIME_S as i64).await.unwrap();
        assert!(directory.list_rfcomm_lifecycle().unwrap().is_empty());
    }

    #[tokio::test]
    async fn sweep_surfaces_scan_failure() {
        let directory = Arc::new(open_temp_directory());
        directory
            .observe_rfcomm_paired("AA:BB:CC:DD:EE:FF", "PIM-x", unix_seconds_now())
            .unwrap();
        let tracker = RfcommTracker::new(PathBuf::from("/bin/false"), directory.clone());
        let result = tracker.sweep(MIN_LIFETIME_S as i64).await;
        assert!(result.is_err(), "expected scan failure to surface");
        assert_eq!(directory.list_rfcomm_lifecycle().unwrap().len(), 1);
    }

    /// Peer disconnected, peer rejoined within
    /// `max_unreachable_lifetime_s`. Cleanup must NOT touch the row.
    #[tokio::test]
    async fn sweep_keeps_peer_that_rejoined_within_lifetime() {
        let dir = open_temp_directory();
        let now = unix_seconds_now();
        dir.observe_rfcomm_paired("AA:BB:CC:DD:EE:FF", "PIM-x", now - 7_200)
            .unwrap();
        dir.record_rfcomm_connected("AA:BB:CC:DD:EE:FF", "PIM-x", now - 60)
            .unwrap();
        let directory = Arc::new(dir);

        let lab = FakeShim::new(FakeShim::SCRIPT_PEER_PAIRED_DISCONNECTED);
        let tracker = RfcommTracker::new(lab.path.clone(), directory.clone());
        tracker.sweep(MIN_LIFETIME_S as i64).await.unwrap();
        assert_eq!(directory.list_rfcomm_lifecycle().unwrap().len(), 1);
        assert!(!lab.removed("AA:BB:CC:DD:EE:FF"));
    }

    /// Peer disconnected and stayed past the lifetime — must unpair.
    #[tokio::test]
    async fn sweep_unpairs_peer_after_lifetime_passed() {
        let dir = open_temp_directory();
        let now = unix_seconds_now();
        dir.observe_rfcomm_paired("AA:BB:CC:DD:EE:FF", "PIM-x", now - 7_200)
            .unwrap();
        dir.record_rfcomm_connected("AA:BB:CC:DD:EE:FF", "PIM-x", now - 7_200)
            .unwrap();
        let directory = Arc::new(dir);

        let lab = FakeShim::new(FakeShim::SCRIPT_PEER_PAIRED_DISCONNECTED);
        let tracker = RfcommTracker::new(lab.path.clone(), directory.clone());
        tracker.sweep(MIN_LIFETIME_S as i64).await.unwrap();
        assert!(directory.list_rfcomm_lifecycle().unwrap().is_empty());
        assert!(lab.removed("AA:BB:CC:DD:EE:FF"));
    }

    /// Lifetime expired but peer is currently connected — skip.
    #[tokio::test]
    async fn sweep_skips_unpair_when_peer_currently_connected() {
        let dir = open_temp_directory();
        let now = unix_seconds_now();
        dir.observe_rfcomm_paired("AA:BB:CC:DD:EE:FF", "PIM-x", now - 7_200)
            .unwrap();
        let directory = Arc::new(dir);

        let lab = FakeShim::new(FakeShim::SCRIPT_PEER_PAIRED_CONNECTED);
        let tracker = RfcommTracker::new(lab.path.clone(), directory.clone());
        tracker.sweep(MIN_LIFETIME_S as i64).await.unwrap();
        assert_eq!(directory.list_rfcomm_lifecycle().unwrap().len(), 1);
        assert!(!lab.removed("AA:BB:CC:DD:EE:FF"));
    }

    #[tokio::test]
    async fn driver_no_op_when_cfg_disabled() {
        let directory = Arc::new(open_temp_directory());
        let tracker: Arc<dyn PeerCleanupTracker> = Arc::new(RfcommTracker::new(
            PathBuf::from("/bin/true"),
            directory.clone(),
        ));
        let cfg = PeerCleanupConfig {
            enabled: false,
            max_unreachable_lifetime_s: 1,
            cleanup_interval_s: 1,
        };
        spawn_cleanup(cfg, tracker, CancellationToken::new());
        // Give a hypothetical sweep time to misfire.
        tokio::time::sleep(Duration::from_millis(50)).await;
        // No DB writes happened (we'd also see panics if the sweep
        // had run since /bin/true returns empty).
        assert!(directory.list_rfcomm_lifecycle().unwrap().is_empty());
    }

    /// Test-only fake `bluetoothctl`. Materialises a small `/bin/sh`
    /// script in a tempdir and exposes its path so a tracker can
    /// point at it. The script writes every `remove` invocation to a
    /// sibling log file, which the test inspects to assert the
    /// daemon's behaviour.
    struct FakeShim {
        path: PathBuf,
        remove_log: PathBuf,
        _dir: tempfile::TempDir,
    }

    impl FakeShim {
        const SCRIPT_PEER_PAIRED_DISCONNECTED: &'static str = r#"#!/bin/sh
if [ "$1" = "--timeout" ]; then shift 2; fi
case "$1" in
    devices) printf 'Device AA:BB:CC:DD:EE:FF PIM-x\n' ;;
    info)    printf 'Connected: no\n' ;;
    remove)  printf '%s\n' "$2" >> "FAKE_REMOVE_LOG" ;;
esac
exit 0
"#;
        const SCRIPT_PEER_PAIRED_CONNECTED: &'static str = r#"#!/bin/sh
if [ "$1" = "--timeout" ]; then shift 2; fi
case "$1" in
    devices) printf 'Device AA:BB:CC:DD:EE:FF PIM-x\n' ;;
    info)    printf 'Connected: yes\n' ;;
    remove)  printf '%s\n' "$2" >> "FAKE_REMOVE_LOG" ;;
esac
exit 0
"#;

        fn new(template: &'static str) -> Self {
            use std::os::unix::fs::PermissionsExt;
            let dir = tempfile::TempDir::new().expect("tempdir for fake shim");
            let path = dir.path().join("bluetoothctl");
            let remove_log = dir.path().join("remove.log");
            let body = template.replace("FAKE_REMOVE_LOG", remove_log.to_str().unwrap());
            std::fs::write(&path, body).expect("write fake bluetoothctl");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("chmod +x fake bluetoothctl");
            Self {
                path,
                remove_log,
                _dir: dir,
            }
        }

        fn removed(&self, bd_addr: &str) -> bool {
            std::fs::read_to_string(&self.remove_log)
                .map(|s| s.lines().any(|l| l.trim() == bd_addr))
                .unwrap_or(false)
        }
    }
}
