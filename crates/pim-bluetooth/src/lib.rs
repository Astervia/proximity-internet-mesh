//! Bluetooth PAN link monitoring for PIM.
//!
//! This crate intentionally keeps Bluetooth scoped to link setup. It assumes
//! the host OS or the operator has already formed a Bluetooth PAN link that
//! yields an IP-capable interface such as `bnep0`. Once that interface is up,
//! the crate emits peer `SocketAddr`s so the daemon can reuse the normal TCP
//! transport and handshake path unchanged.

#![warn(missing_docs)]

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use pim_core::BluetoothConfig;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

/// Errors produced by the Bluetooth PAN subsystem.
#[derive(Debug, thiserror::Error)]
pub enum BluetoothError {
    /// A configured peer address could not be parsed as an IP address.
    #[error("invalid bluetooth peer address: {0}")]
    InvalidPeerAddress(String),
    /// An I/O error occurred while reading interface state from sysfs.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Watches a Bluetooth PAN interface and emits peer socket addresses.
#[derive(Debug)]
pub struct BluetoothDiscovery {
    config: BluetoothConfig,
    targets: Vec<SocketAddr>,
    sysfs_root: PathBuf,
    peer_tx: mpsc::Sender<SocketAddr>,
}

/// Default Linux sysfs root used to inspect network interface state.
pub const DEFAULT_SYSFS_ROOT: &str = "/sys/class/net";

impl BluetoothDiscovery {
    /// Build a new Bluetooth PAN watcher and a receiver for discovered targets.
    pub fn new(
        config: BluetoothConfig,
        listen_port: u16,
    ) -> Result<(Self, mpsc::Receiver<SocketAddr>), BluetoothError> {
        Self::new_with_sysfs_root(config, listen_port, DEFAULT_SYSFS_ROOT)
    }

    /// Build a watcher with an explicit sysfs root.
    ///
    /// This is primarily useful for tests and Docker-based simulations where a
    /// fake interface tree is mounted somewhere other than `/sys/class/net`.
    pub fn new_with_sysfs_root(
        config: BluetoothConfig,
        listen_port: u16,
        sysfs_root: impl Into<PathBuf>,
    ) -> Result<(Self, mpsc::Receiver<SocketAddr>), BluetoothError> {
        let (peer_tx, peer_rx) = mpsc::channel(16);
        let mut targets = Vec::with_capacity(config.peer_addresses.len());
        for addr in &config.peer_addresses {
            let ip = addr
                .parse::<IpAddr>()
                .map_err(|_| BluetoothError::InvalidPeerAddress(addr.clone()))?;
            targets.push(SocketAddr::new(ip, listen_port));
        }
        Ok((
            Self {
                config,
                targets,
                sysfs_root: sysfs_root.into(),
                peer_tx,
            },
            peer_rx,
        ))
    }

    /// Returns the peer socket addresses that will be emitted when the PAN link is ready.
    pub fn target_socket_addrs(&self) -> &[SocketAddr] {
        &self.targets
    }

    /// Run the Bluetooth watcher until cancellation.
    ///
    /// The watcher waits for `config.interface` to appear in sysfs and report
    /// an operstate of `up` or `unknown`, then emits the configured peer
    /// socket addresses once.
    pub async fn run(self, cancel: CancellationToken) -> Result<(), BluetoothError> {
        if self.targets.is_empty() {
            warn!(
                interface = %self.config.interface,
                "Bluetooth PAN enabled with no peer_addresses configured; skipping"
            );
            return Ok(());
        }

        info!(
            interface = %self.config.interface,
            peers = self.targets.len(),
            "Bluetooth PAN watcher starting"
        );

        let ready = self.wait_for_interface(cancel.clone()).await?;
        if !ready {
            warn!(
                interface = %self.config.interface,
                timeout_ms = self.config.startup_timeout_ms,
                "Bluetooth PAN interface did not become ready before timeout"
            );
            return Ok(());
        }

        for addr in self.targets {
            info!(%addr, "Bluetooth PAN peer ready");
            if self.peer_tx.send(addr).await.is_err() {
                break;
            }
        }

        cancel.cancelled().await;
        debug!("Bluetooth PAN watcher cancelled");
        Ok(())
    }

    async fn wait_for_interface(&self, cancel: CancellationToken) -> Result<bool, BluetoothError> {
        let deadline = Instant::now() + Duration::from_millis(self.config.startup_timeout_ms);
        let poll = Duration::from_millis(self.config.poll_interval_ms.max(1));
        let operstate = interface_operstate_path(&self.sysfs_root, &self.config.interface);

        loop {
            tokio::select! {
                _ = cancel.cancelled() => return Ok(false),
                _ = tokio::time::sleep(poll) => {}
            }

            if let Some(state) = read_operstate_if_present(&operstate).await? {
                if is_ready_operstate(&state) {
                    return Ok(true);
                }
                debug!(
                    interface = %self.config.interface,
                    operstate = state.trim(),
                    "Bluetooth PAN interface present but not ready"
                );
            }

            if Instant::now() >= deadline {
                return Ok(false);
            }
        }
    }
}

fn interface_operstate_path(sysfs_root: &Path, interface: &str) -> PathBuf {
    sysfs_root.join(interface).join("operstate")
}

async fn read_operstate_if_present(path: &Path) -> Result<Option<String>, std::io::Error> {
    match tokio::fs::read_to_string(path).await {
        Ok(state) => Ok(Some(state)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

fn is_ready_operstate(state: &str) -> bool {
    matches!(state.trim(), "up" | "unknown")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn discovery_new_returns_receiver_and_targets() {
        let mut config = BluetoothConfig::default();
        config.peer_addresses = vec!["192.168.44.2".into(), "fd00::2".into()];

        let (svc, _rx) = BluetoothDiscovery::new(config, 9100).unwrap();
        assert_eq!(svc.target_socket_addrs().len(), 2);
        assert_eq!(
            svc.target_socket_addrs()[0],
            "192.168.44.2:9100".parse().unwrap()
        );
        assert_eq!(
            svc.target_socket_addrs()[1],
            "[fd00::2]:9100".parse().unwrap()
        );
    }

    #[test]
    fn invalid_peer_address_is_rejected() {
        let mut config = BluetoothConfig::default();
        config.peer_addresses = vec!["not-an-ip".into()];

        let err = BluetoothDiscovery::new(config, 9100).unwrap_err();
        assert!(matches!(err, BluetoothError::InvalidPeerAddress(addr) if addr == "not-an-ip"));
    }

    #[test]
    fn operstate_helper_accepts_up_and_unknown() {
        assert!(is_ready_operstate("up\n"));
        assert!(is_ready_operstate("unknown"));
        assert!(!is_ready_operstate("down"));
    }

    #[test]
    fn interface_operstate_path_uses_supplied_root() {
        let path = interface_operstate_path(Path::new("/tmp/fake-sysfs"), "bnep0");
        assert_eq!(path, PathBuf::from("/tmp/fake-sysfs/bnep0/operstate"));
    }

    #[tokio::test]
    async fn run_emits_targets_from_fake_sysfs_root() {
        let fake_sysfs = unique_test_dir("pim-bt-fake-sysfs");
        fs::create_dir_all(fake_sysfs.join("bnep0")).unwrap();
        fs::write(fake_sysfs.join("bnep0/operstate"), "down\n").unwrap();

        let mut config = BluetoothConfig::default();
        config.interface = "bnep0".into();
        config.peer_addresses = vec!["192.168.44.2".into()];
        config.poll_interval_ms = 10;
        config.startup_timeout_ms = 500;

        let (svc, mut rx) =
            BluetoothDiscovery::new_with_sysfs_root(config, 9100, fake_sysfs.clone()).unwrap();
        let cancel = CancellationToken::new();
        let operstate = fake_sysfs.join("bnep0/operstate");

        let writer = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            tokio::fs::write(operstate, "up\n").await.unwrap();
        });

        let runner = tokio::spawn({
            let cancel = cancel.clone();
            async move { svc.run(cancel).await }
        });

        let addr = tokio::time::timeout(Duration::from_millis(300), rx.recv())
            .await
            .expect("timed out waiting for Bluetooth address")
            .expect("channel closed before address emitted");
        assert_eq!(addr, "192.168.44.2:9100".parse().unwrap());

        cancel.cancel();
        writer.await.unwrap();
        runner.await.unwrap().unwrap();
        fs::remove_dir_all(fake_sysfs).unwrap();
    }

    fn unique_test_dir(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()))
    }
}
