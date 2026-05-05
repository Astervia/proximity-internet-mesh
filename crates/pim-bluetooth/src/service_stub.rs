//! Bluetooth PAN watcher stub for platforms without a host backend.
//!
//! Phase A leaves Android (and any other non-Linux/non-macOS target)
//! without a real Bluetooth integration. The kernel's `pim-daemon`
//! always calls [`BluetoothDiscovery::run`] when `[bluetooth].enabled`
//! is set in the config, so this module exists only to satisfy that
//! call site: `run` returns immediately with a single warn log, and
//! the channel handed to the daemon never produces a peer address.
//!
//! Phase B replaces this with a real backend that bridges to Java
//! `BluetoothAdapter` via JNI. Until then, mesh peering on Android
//! relies on the LAN/UDP discovery path and Wi-Fi Direct only.

use crate::{BluetoothDiscovery, BluetoothError};
use tokio_util::sync::CancellationToken;

impl BluetoothDiscovery {
    /// Stub that immediately returns Ok. The daemon spawns this future
    /// when `[bluetooth].enabled = true` in config; on platforms
    /// without a backend the future logs a single warning and exits so
    /// the rest of the daemon (LAN/UDP discovery, Wi-Fi Direct, TCP
    /// transport) keeps running.
    pub async fn run(self, _cancel: CancellationToken) -> Result<(), BluetoothError> {
        tracing::warn!(
            "Bluetooth PAN watcher: no host backend on this target; \
             skipping. Mesh peering falls back to LAN/UDP and Wi-Fi Direct."
        );
        Ok(())
    }
}
