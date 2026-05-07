//! Bluetooth PAN peer cleanup. Mirrors the RFCOMM tracker but
//! consumes [`pim_bluetooth::PanPeerEvent`]s into the
//! `bluetooth_pan_lifecycle` table; the cleanup sweep itself is
//! identical in shape to RFCOMM (BlueZ paired-list intersection,
//! `Connected: no` confirmation, `bluetoothctl remove`).
//!
//! Linux-only — PAN doesn't exist on macOS in this codebase, and
//! `bluetoothctl` isn't available there either.

#![cfg(target_os = "linux")]

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use pim_bluetooth::{PanPeerEvent, PanPeerEventKind};
use tokio::process::Command;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::{unix_seconds_now, PeerCleanupTracker};
use crate::app::peer_directory::PeerDirectoryService;

/// Background task that consumes [`PanPeerEvent`]s from
/// [`pim_bluetooth::BluetoothDiscovery`] and persists them into the
/// daemon's `bluetooth_pan_lifecycle` table.
pub(crate) async fn run_pan_event_consumer(
    directory: Arc<PeerDirectoryService>,
    mut rx: mpsc::Receiver<PanPeerEvent>,
) {
    while let Some(event) = rx.recv().await {
        let directory = directory.clone();
        let mac = event.mac.clone();
        let name = event.name.clone();
        let kind = event.kind;
        let now_s = unix_seconds_now();
        // Persist via spawn_blocking — the SQL writes are tiny but
        // they're sync and we must not block the consumer task on
        // them.
        let _ = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            match kind {
                PanPeerEventKind::Paired => directory.observe_pan_paired(&mac, &name, now_s)?,
                PanPeerEventKind::Connected => {
                    directory.record_pan_connected(&mac, &name, now_s)?
                }
            }
            Ok(())
        })
        .await;
    }
}

/// `bluetoothctl`-driven cleanup for PAN-paired peers.
pub(crate) struct PanTracker {
    bluetoothctl_command: PathBuf,
    directory: Arc<PeerDirectoryService>,
}

impl PanTracker {
    pub(crate) fn new(bluetoothctl_command: PathBuf, directory: Arc<PeerDirectoryService>) -> Self {
        Self {
            bluetoothctl_command,
            directory,
        }
    }
}

#[async_trait]
impl PeerCleanupTracker for PanTracker {
    fn name(&self) -> &'static str {
        "bluetooth-pan"
    }

    async fn sweep(&self, lifetime_s: i64) -> anyhow::Result<()> {
        let directory_for_list = self.directory.clone();
        let rows =
            tokio::task::spawn_blocking(move || directory_for_list.list_pan_lifecycle()).await??;
        if rows.is_empty() {
            debug!("bluetooth-pan cleanup: lifecycle table empty");
            return Ok(());
        }

        let paired = scan_paired_addrs(&self.bluetoothctl_command).await?;
        let now_s = unix_seconds_now();

        for row in rows {
            if !paired.contains(&row.bd_addr) {
                forget_lifecycle(&self.directory, &row.bd_addr).await;
                info!(
                    bd_addr = %row.bd_addr,
                    name = %row.name,
                    "bluetooth-pan cleanup: peer no longer paired in BlueZ; dropped lifecycle row"
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
                        "bluetooth-pan cleanup: unpaired unreachable peer"
                    );
                }
                Err(e) => {
                    warn!(
                        bd_addr = %row.bd_addr,
                        "bluetooth-pan cleanup: bluetoothctl remove failed: {e}"
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
    let _ = tokio::task::spawn_blocking(move || directory.forget_pan_peer(&addr)).await;
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

    fn open_temp_directory() -> Arc<PeerDirectoryService> {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("peers.db");
        std::mem::forget(dir);
        Arc::new(PeerDirectoryService::open(path).expect("open peers directory"))
    }

    #[tokio::test]
    async fn sweep_drops_rows_for_peers_no_longer_paired() {
        let directory = open_temp_directory();
        directory
            .observe_pan_paired("AA:BB:CC:DD:EE:FF", "PIM-x", 100)
            .unwrap();
        let tracker = PanTracker::new(PathBuf::from("/bin/true"), directory.clone());
        tracker
            .sweep(crate::app::peer_cleanup::MIN_LIFETIME_S as i64)
            .await
            .unwrap();
        assert!(directory.list_pan_lifecycle().unwrap().is_empty());
    }

    #[tokio::test]
    async fn consumer_persists_paired_then_connected_event() {
        let directory = open_temp_directory();
        let (tx, rx) = mpsc::channel(8);
        let consumer_handle = tokio::spawn(run_pan_event_consumer(directory.clone(), rx));

        tx.send(PanPeerEvent {
            mac: "AA:BB:CC:DD:EE:FF".into(),
            name: "PIM-x".into(),
            kind: PanPeerEventKind::Paired,
        })
        .await
        .unwrap();
        tx.send(PanPeerEvent {
            mac: "AA:BB:CC:DD:EE:FF".into(),
            name: "PIM-x".into(),
            kind: PanPeerEventKind::Connected,
        })
        .await
        .unwrap();
        // Drop sender to terminate the consumer.
        drop(tx);
        let _ = consumer_handle.await;

        let rows = directory.list_pan_lifecycle().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].bd_addr, "AA:BB:CC:DD:EE:FF");
        assert!(rows[0].last_connected_at_s.is_some());
    }
}
