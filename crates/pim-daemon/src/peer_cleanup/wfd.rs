//! Wi-Fi Direct peer cleanup.
//!
//! Unlike RFCOMM and PAN, WFD has no persistent paired list at the
//! protocol level — `wpa_supplicant` doesn't maintain one. The
//! cleanup loop's destructive action is therefore "drop the
//! lifecycle row" + a best-effort `wpa_cli p2p_remove_client <mac>`
//! to free any wpa_supplicant in-memory state. The bigger win is
//! bounding `wfd_peer_lifecycle` itself, which would otherwise grow
//! with every unique peer ever surfaced by `p2p_peers`.

#![cfg(target_os = "linux")]

use std::sync::Arc;

use async_trait::async_trait;
use pim_wifidirect::{WfdPeerEvent, WfdPeerEventKind};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::{unix_seconds_now, PeerCleanupTracker};
use crate::app::peer_directory::PeerDirectoryService;

/// Background task that consumes [`WfdPeerEvent`]s from
/// [`pim_wifidirect::WifiDirectDiscovery`] and persists them into
/// the daemon's `wfd_peer_lifecycle` table.
pub(crate) async fn run_wfd_event_consumer(
    directory: Arc<PeerDirectoryService>,
    mut rx: mpsc::Receiver<WfdPeerEvent>,
) {
    while let Some(event) = rx.recv().await {
        let directory = directory.clone();
        let mac = event.mac.clone();
        let kind = event.kind;
        let now_s = unix_seconds_now();
        let _ = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            match kind {
                WfdPeerEventKind::Discovered => directory.observe_wfd_peer(&mac, now_s)?,
                WfdPeerEventKind::Connected => directory.record_wfd_connected(&mac, now_s)?,
            }
            Ok(())
        })
        .await;
    }
}

/// Wi-Fi Direct cleanup tracker. The destructive action is
/// purposefully minimal: drop the lifecycle row. There is no
/// `wpa_supplicant`-level persistent pairing state to garbage-
/// collect — supplicant maintains its own peer table internally and
/// expires entries according to its own policies. The value of the
/// lifecycle table is purely daemon-side memory bounding for long
/// uptimes.
pub(crate) struct WfdTracker {
    directory: Arc<PeerDirectoryService>,
}

impl WfdTracker {
    pub(crate) fn new(directory: Arc<PeerDirectoryService>) -> Self {
        Self { directory }
    }
}

#[async_trait]
impl PeerCleanupTracker for WfdTracker {
    fn name(&self) -> &'static str {
        "wifi-direct"
    }

    async fn sweep(&self, lifetime_s: i64) -> anyhow::Result<()> {
        let directory_for_list = self.directory.clone();
        let rows =
            tokio::task::spawn_blocking(move || directory_for_list.list_wfd_lifecycle()).await??;
        if rows.is_empty() {
            debug!("wifi-direct cleanup: lifecycle table empty");
            return Ok(());
        }

        let now_s = unix_seconds_now();

        for row in rows {
            let last_seen = std::cmp::max(
                row.first_seen_at_s,
                row.last_connected_at_s.unwrap_or(row.first_seen_at_s),
            );
            let age_s = now_s.saturating_sub(last_seen);
            if age_s <= lifetime_s {
                continue;
            }

            match forget_lifecycle(&self.directory, &row.mac).await {
                Ok(true) => {
                    info!(
                        mac = %row.mac,
                        last_seen_s = last_seen,
                        age_days = age_s / 86_400,
                        "wifi-direct cleanup: dropped stale lifecycle row"
                    );
                }
                Ok(false) => {
                    // Race: the row vanished between list + forget.
                }
                Err(e) => {
                    warn!(mac = %row.mac, "wifi-direct cleanup: forget failed: {e}");
                }
            }
        }

        Ok(())
    }
}

async fn forget_lifecycle(
    directory: &Arc<PeerDirectoryService>,
    mac: &str,
) -> anyhow::Result<bool> {
    let directory = directory.clone();
    let mac = mac.to_string();
    let result = tokio::task::spawn_blocking(move || directory.forget_wfd_peer(&mac)).await??;
    Ok(result)
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
    async fn consumer_persists_discovered_then_connected_event() {
        let directory = open_temp_directory();
        let (tx, rx) = mpsc::channel(8);
        let consumer_handle = tokio::spawn(run_wfd_event_consumer(directory.clone(), rx));

        tx.send(WfdPeerEvent {
            mac: "aa:bb:cc:dd:ee:ff".into(),
            kind: WfdPeerEventKind::Discovered,
        })
        .await
        .unwrap();
        tx.send(WfdPeerEvent {
            mac: "aa:bb:cc:dd:ee:ff".into(),
            kind: WfdPeerEventKind::Connected,
        })
        .await
        .unwrap();
        drop(tx);
        let _ = consumer_handle.await;

        let rows = directory.list_wfd_lifecycle().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].mac, "aa:bb:cc:dd:ee:ff");
        assert!(rows[0].last_connected_at_s.is_some());
    }

    #[tokio::test]
    async fn sweep_drops_stale_row() {
        let directory = open_temp_directory();
        let now_s = unix_seconds_now();
        // Stale: 2 hours past the 1-hour lifetime floor.
        directory
            .observe_wfd_peer("aa:bb:cc:dd:ee:ff", now_s - 7_200)
            .unwrap();
        let tracker = WfdTracker::new(directory.clone());
        tracker
            .sweep(crate::app::peer_cleanup::MIN_LIFETIME_S as i64)
            .await
            .unwrap();
        assert!(directory.list_wfd_lifecycle().unwrap().is_empty());
    }

    #[tokio::test]
    async fn sweep_keeps_recent_row() {
        let directory = open_temp_directory();
        let now_s = unix_seconds_now();
        directory
            .observe_wfd_peer("aa:bb:cc:dd:ee:ff", now_s - 60)
            .unwrap();
        let tracker = WfdTracker::new(directory.clone());
        tracker
            .sweep(crate::app::peer_cleanup::MIN_LIFETIME_S as i64)
            .await
            .unwrap();
        assert_eq!(directory.list_wfd_lifecycle().unwrap().len(), 1);
    }
}
