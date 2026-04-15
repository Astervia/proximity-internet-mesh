//! Bluetooth discovery and PAN link monitoring for PIM.
//!
//! This crate keeps Bluetooth scoped to peer finding and link setup. It can:
//!
//! 1. Discover nearby Bluetooth devices whose names match the configured PIM
//!    prefix.
//! 2. Pair/connect to matching devices.
//! 3. Wait for a PAN interface to become active.
//! 4. Learn peer IPs from the PAN neighbor table and emit `SocketAddr`s so the
//!    daemon can reuse the normal TCP transport and handshake path unchanged.

#![warn(missing_docs)]

use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use pim_core::BluetoothConfig;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

/// Default Linux sysfs root used to inspect network interface state.
#[cfg(target_os = "linux")]
pub const DEFAULT_SYSFS_ROOT: &str = "/sys/class/net";
/// Default placeholder sysfs root for platforms that do not use sysfs.
#[cfg(not(target_os = "linux"))]
pub const DEFAULT_SYSFS_ROOT: &str = "";
/// Default command used to inspect Bluetooth PAN neighbors.
#[cfg(target_os = "linux")]
pub const DEFAULT_IP_COMMAND: &str = "ip";
/// Default command used to inspect Bluetooth PAN neighbors.
#[cfg(target_os = "macos")]
pub const DEFAULT_IP_COMMAND: &str = "arp";
/// Default command used for radio discovery and pairing.
#[cfg(target_os = "linux")]
pub const DEFAULT_BLUETOOTHCTL_COMMAND: &str = "bluetoothctl";
/// Default command used for radio discovery and pairing.
#[cfg(target_os = "macos")]
pub const DEFAULT_BLUETOOTHCTL_COMMAND: &str = "blueutil";
/// Default `bt-network` command used to request a PAN/NAP connection.
pub const DEFAULT_BT_NETWORK_COMMAND: &str = "bt-network";

/// Errors produced by the Bluetooth subsystem.
#[derive(Debug, thiserror::Error)]
pub enum BluetoothError {
    /// An I/O error occurred while reading interface state from sysfs or running commands.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// Running an external helper command failed.
    #[error("{command} failed: {message}")]
    CommandFailed {
        /// Helper command name.
        command: &'static str,
        /// Human-readable error text from stderr.
        message: String,
    },
}

/// A discovered Bluetooth device candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredDevice {
    /// Device MAC address.
    pub mac: String,
    /// Human-readable Bluetooth device name.
    pub name: String,
}

/// Watches Bluetooth radio state and PAN neighbors, emitting peer socket addresses.
#[derive(Debug)]
pub struct BluetoothDiscovery {
    config: BluetoothConfig,
    listen_port: u16,
    static_targets: Vec<SocketAddr>,
    sysfs_root: PathBuf,
    ip_command: PathBuf,
    bluetoothctl_command: PathBuf,
    bt_network_command: PathBuf,
    peer_tx: mpsc::Sender<SocketAddr>,
}

impl BluetoothDiscovery {
    /// Build a new Bluetooth watcher with system-default command paths.
    pub fn new(
        config: BluetoothConfig,
        listen_port: u16,
        static_targets: Vec<SocketAddr>,
    ) -> Result<(Self, mpsc::Receiver<SocketAddr>), BluetoothError> {
        Self::new_with_system_paths(
            config,
            listen_port,
            static_targets,
            DEFAULT_SYSFS_ROOT,
            DEFAULT_IP_COMMAND,
            DEFAULT_BLUETOOTHCTL_COMMAND,
            DEFAULT_BT_NETWORK_COMMAND,
        )
    }

    /// Build a watcher with explicit command and sysfs paths.
    pub fn new_with_system_paths(
        config: BluetoothConfig,
        listen_port: u16,
        static_targets: Vec<SocketAddr>,
        sysfs_root: impl Into<PathBuf>,
        ip_command: impl Into<PathBuf>,
        bluetoothctl_command: impl Into<PathBuf>,
        bt_network_command: impl Into<PathBuf>,
    ) -> Result<(Self, mpsc::Receiver<SocketAddr>), BluetoothError> {
        let (peer_tx, peer_rx) = mpsc::channel(16);

        Ok((
            Self {
                config,
                listen_port,
                static_targets,
                sysfs_root: sysfs_root.into(),
                ip_command: ip_command.into(),
                bluetoothctl_command: bluetoothctl_command.into(),
                bt_network_command: bt_network_command.into(),
                peer_tx,
            },
            peer_rx,
        ))
    }

    /// Returns the statically configured peer socket addresses.
    pub fn target_socket_addrs(&self) -> &[SocketAddr] {
        &self.static_targets
    }

    /// Run the Bluetooth service until cancellation.
    pub async fn run(self, cancel: CancellationToken) -> Result<(), BluetoothError> {
        if self.static_targets.is_empty()
            && !self.config.auto_discover_peers
            && !self.config.radio_discovery_enabled
        {
            warn!(
                interface = %self.config.interface,
                "Bluetooth enabled with neither static peers, PAN neighbor discovery, nor radio discovery; skipping"
            );
            return Ok(());
        }

        info!(
            interface = %self.config.interface,
            static_peers = self.static_targets.len(),
            radio_discovery = self.config.radio_discovery_enabled,
            auto_discover_pan_peers = self.config.auto_discover_peers,
            "Bluetooth service starting"
        );

        if self.config.radio_discovery_enabled {
            self.prepare_controller().await?;
            self.run_bluetoothctl(["scan", "on"]).await?;
        }

        let mut interface_ready = false;
        let startup_deadline =
            Instant::now() + Duration::from_millis(self.config.startup_timeout_ms);
        let mut emitted_static = false;
        let mut seen_addrs: HashSet<SocketAddr> = HashSet::new();
        let mut seen_macs: HashSet<String> = HashSet::new();

        let mut interface_interval =
            tokio::time::interval(Duration::from_millis(self.config.poll_interval_ms.max(1)));
        let mut scan_interval =
            tokio::time::interval(Duration::from_millis(self.config.scan_interval_ms.max(1)));
        let mut peer_interval = tokio::time::interval(Duration::from_millis(
            self.config.peer_discovery_interval_ms.max(1),
        ));

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    debug!("Bluetooth service cancelled");
                    return Ok(());
                }
                _ = interface_interval.tick() => {
                    if !interface_ready {
                        interface_ready = self.interface_is_ready().await?;
                        if interface_ready {
                            info!(interface = %self.config.interface, "Bluetooth PAN interface ready");
                        } else if Instant::now() >= startup_deadline {
                            warn!(
                                interface = %self.config.interface,
                                timeout_ms = self.config.startup_timeout_ms,
                                "Bluetooth PAN interface did not become ready before timeout"
                            );
                            return Ok(());
                        }
                    }

                    if interface_ready && !emitted_static {
                        for addr in &self.static_targets {
                            info!(%addr, "Bluetooth PAN static peer ready");
                            if self.peer_tx.send(*addr).await.is_err() {
                                return Ok(());
                            }
                            seen_addrs.insert(*addr);
                        }
                        emitted_static = true;
                    }
                }
                _ = scan_interval.tick(), if self.config.radio_discovery_enabled => {
                    let devices = self.discover_devices().await?;
                    for device in devices {
                        if seen_macs.contains(&device.mac) {
                            continue;
                        }
                        match self.pair_and_request_pan(&device).await {
                            Ok(()) => {
                                info!(mac = %device.mac, name = %device.name, "Bluetooth radio-discovered peer prepared");
                                seen_macs.insert(device.mac);
                            }
                            Err(err) => {
                                warn!(mac = %device.mac, name = %device.name, "Bluetooth radio discovery failed: {err}");
                            }
                        }
                    }
                }
                _ = peer_interval.tick(), if interface_ready && self.config.auto_discover_peers => {
                    let discovered = self.discover_neighbor_targets().await?;
                    let discovered_set: HashSet<SocketAddr> = discovered.iter().copied().collect();

                    for addr in discovered {
                        if seen_addrs.insert(addr) {
                            info!(%addr, "Bluetooth PAN discovered peer addr");
                            if self.peer_tx.send(addr).await.is_err() {
                                return Ok(());
                            }
                        }
                    }

                    seen_addrs.retain(|addr| {
                        self.static_targets.contains(addr) || discovered_set.contains(addr)
                    });
                }
            }
        }
    }

    async fn prepare_controller(&self) -> Result<(), BluetoothError> {
        #[cfg(target_os = "macos")]
        {
            self.run_bluetoothctl(["--power", "1"]).await?;
            self.run_bluetoothctl(["--discoverable", "1"]).await?;
            if !self.config.local_alias.is_empty() {
                warn!(
                    local_alias = %self.config.local_alias,
                    "macOS Bluetooth backend does not set the host controller alias automatically; set the Mac Bluetooth name manually if discovery by prefix is required"
                );
            }
            return Ok(());
        }

        self.run_bluetoothctl(["power", "on"]).await?;
        self.run_bluetoothctl(["pairable", "on"]).await?;
        self.run_bluetoothctl([
            "discoverable-timeout",
            &self.config.discoverable_timeout_s.to_string(),
        ])
        .await?;
        self.run_bluetoothctl(["discoverable", "on"]).await?;
        self.run_bluetoothctl(["agent", "NoInputNoOutput"]).await?;
        self.run_bluetoothctl(["default-agent"]).await?;
        if !self.config.local_alias.is_empty() {
            self.run_bluetoothctl(["system-alias", &self.config.local_alias])
                .await?;
        }
        Ok(())
    }

    async fn discover_devices(&self) -> Result<Vec<DiscoveredDevice>, BluetoothError> {
        #[cfg(target_os = "macos")]
        {
            let output = self
                .run_bluetoothctl_capture([
                    "--inquiry",
                    &self.config.bluetoothctl_timeout_s.to_string(),
                ])
                .await?;
            return Ok(parse_blueutil_inquiry_output(
                &output,
                &self.config.device_name_prefix,
                &self.config.local_alias,
            ));
        }

        let output = self.run_bluetoothctl_capture(["devices"]).await?;
        Ok(parse_devices_output(
            &output,
            &self.config.device_name_prefix,
            &self.config.local_alias,
        ))
    }

    async fn pair_and_request_pan(&self, device: &DiscoveredDevice) -> Result<(), BluetoothError> {
        #[cfg(target_os = "macos")]
        {
            self.run_bluetoothctl(["--pair", &device.mac]).await?;
            self.run_bluetoothctl(["--connect", &device.mac]).await?;
            return Ok(());
        }

        self.run_bluetoothctl(["pair", &device.mac]).await?;
        self.run_bluetoothctl(["trust", &device.mac]).await?;
        self.run_bluetoothctl(["connect", &device.mac]).await?;
        self.run_bt_network(&device.mac).await?;
        Ok(())
    }

    async fn interface_is_ready(&self) -> Result<bool, BluetoothError> {
        #[cfg(target_os = "macos")]
        {
            let output = Command::new("ifconfig")
                .arg(&self.config.interface)
                .output()
                .await?;
            if !output.status.success() {
                return Ok(false);
            }
            return Ok(is_ready_ifconfig_output(&String::from_utf8_lossy(
                &output.stdout,
            )));
        }

        let operstate = interface_operstate_path(&self.sysfs_root, &self.config.interface);
        Ok(read_operstate_if_present(&operstate)
            .await?
            .is_some_and(|state| is_ready_operstate(&state)))
    }

    async fn discover_neighbor_targets(&self) -> Result<Vec<SocketAddr>, BluetoothError> {
        #[cfg(target_os = "macos")]
        {
            let output = Command::new(&self.ip_command)
                .args(["-an", "-i", &self.config.interface])
                .output()
                .await?;

            if !output.status.success() {
                return Err(BluetoothError::CommandFailed {
                    command: "arp",
                    message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
                });
            }

            return Ok(parse_arp_output(
                &String::from_utf8_lossy(&output.stdout),
                &self.config.interface,
                self.listen_port,
            ));
        }

        let output = Command::new(&self.ip_command)
            .args(["neigh", "show", "dev", &self.config.interface])
            .output()
            .await?;

        if !output.status.success() {
            return Err(BluetoothError::CommandFailed {
                command: "ip",
                message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }

        Ok(parse_neighbor_output(
            &String::from_utf8_lossy(&output.stdout),
            self.listen_port,
        ))
    }

    async fn run_bluetoothctl<const N: usize>(
        &self,
        args: [&str; N],
    ) -> Result<(), BluetoothError> {
        self.run_bluetoothctl_capture(args).await.map(|_| ())
    }

    async fn run_bluetoothctl_capture<const N: usize>(
        &self,
        args: [&str; N],
    ) -> Result<String, BluetoothError> {
        let mut cmd = Command::new(&self.bluetoothctl_command);
        #[cfg(target_os = "linux")]
        {
            let timeout = self.config.bluetoothctl_timeout_s.to_string();
            cmd.arg("--timeout").arg(&timeout);
        }
        #[cfg(target_os = "macos")]
        {
            // The daemon typically runs with elevated privileges; blueutil refuses
            // to operate as root unless this override is present.
            cmd.env("BLUEUTIL_ALLOW_ROOT", "1");
        }
        for arg in args {
            cmd.arg(arg);
        }
        let output = cmd.output().await?;
        if !output.status.success() {
            return Err(BluetoothError::CommandFailed {
                command: "bluetoothctl",
                message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    async fn run_bt_network(&self, mac: &str) -> Result<(), BluetoothError> {
        let output = Command::new(&self.bt_network_command)
            .args(["-c", mac, "nap"])
            .output()
            .await?;
        if !output.status.success() {
            return Err(BluetoothError::CommandFailed {
                command: "bt-network",
                message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        Ok(())
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

#[cfg(any(test, target_os = "macos"))]
fn is_ready_ifconfig_output(output: &str) -> bool {
    let output = output.trim();
    !output.is_empty() && !output.contains("status: inactive")
}

fn parse_neighbor_output(output: &str, listen_port: u16) -> Vec<SocketAddr> {
    let mut addrs = Vec::new();
    let mut seen = HashSet::new();

    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() || line.contains("FAILED") || line.contains("INCOMPLETE") {
            continue;
        }

        let Some(first) = line.split_whitespace().next() else {
            continue;
        };
        let Ok(ip) = first.parse::<IpAddr>() else {
            continue;
        };
        let addr = SocketAddr::new(ip, listen_port);
        if seen.insert(addr) {
            addrs.push(addr);
        }
    }

    addrs
}

fn parse_devices_output(output: &str, prefix: &str, local_alias: &str) -> Vec<DiscoveredDevice> {
    let mut devices = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if !line.starts_with("Device ") {
            continue;
        }
        let mut parts = line.splitn(3, ' ');
        let _ = parts.next();
        let Some(mac) = parts.next() else { continue };
        let Some(name) = parts.next() else { continue };
        if !name.starts_with(prefix) || name == local_alias {
            continue;
        }
        devices.push(DiscoveredDevice {
            mac: mac.to_string(),
            name: name.to_string(),
        });
    }
    devices
}

#[cfg(any(test, target_os = "macos"))]
fn parse_blueutil_inquiry_output(
    output: &str,
    prefix: &str,
    local_alias: &str,
) -> Vec<DiscoveredDevice> {
    let mut devices = Vec::new();

    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let Some(address_part) = line.strip_prefix("address: ") else {
            continue;
        };
        let Some((mac, rest)) = address_part.split_once(',') else {
            continue;
        };
        let Some(name_start) = rest.find("name: \"") else {
            continue;
        };
        let name_value = &rest[name_start + 7..];
        let Some(name_end) = name_value.find('"') else {
            continue;
        };
        let name = &name_value[..name_end];
        if !name.starts_with(prefix) || name == local_alias {
            continue;
        }

        devices.push(DiscoveredDevice {
            mac: mac.trim().replace('-', ":").to_uppercase(),
            name: name.to_string(),
        });
    }

    devices
}

#[cfg(any(test, target_os = "macos"))]
fn parse_arp_output(output: &str, interface: &str, listen_port: u16) -> Vec<SocketAddr> {
    let mut addrs = Vec::new();
    let mut seen = HashSet::new();

    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() || !line.contains(&format!(" on {interface}")) {
            continue;
        }
        let Some(start) = line.find('(') else {
            continue;
        };
        let Some(end) = line[start + 1..].find(')') else {
            continue;
        };
        let ip_str = &line[start + 1..start + 1 + end];
        let Ok(ip) = ip_str.parse::<IpAddr>() else {
            continue;
        };
        let addr = SocketAddr::new(ip, listen_port);
        if seen.insert(addr) {
            addrs.push(addr);
        }
    }

    addrs
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn discovery_new_returns_receiver_and_targets() {
        let config = BluetoothConfig {
            radio_discovery_enabled: false,
            auto_discover_peers: false,
            ..Default::default()
        };

        let (svc, _rx) = BluetoothDiscovery::new(
            config,
            9100,
            vec![
                "192.168.44.2:9100".parse().unwrap(),
                "[fd00::2]:9100".parse().unwrap(),
            ],
        )
        .unwrap();
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
    fn operstate_helper_accepts_up_and_unknown() {
        assert!(is_ready_operstate("up\n"));
        assert!(is_ready_operstate("unknown"));
        assert!(!is_ready_operstate("down"));
    }

    #[test]
    fn ifconfig_output_treats_non_inactive_interface_as_ready() {
        let active = "\
bridge0: flags=41<UP,RUNNING> mtu 1500\n\
\tstatus: active\n";
        let no_status = "bridge0: flags=41<UP,RUNNING> mtu 1500\n";
        let inactive = "\
bridge0: flags=41<UP,RUNNING> mtu 1500\n\
\tstatus: inactive\n";
        assert!(is_ready_ifconfig_output(active));
        assert!(is_ready_ifconfig_output(no_status));
        assert!(!is_ready_ifconfig_output(inactive));
    }

    #[test]
    fn neighbor_output_parses_ipv4_and_ipv6_and_skips_failed_entries() {
        let output = "\
192.168.44.2 dev bnep0 lladdr 02:00:00:00:00:02 REACHABLE
fe80::1234 dev bnep0 lladdr 02:00:00:00:00:03 router STALE
192.168.44.3 dev bnep0 FAILED
";
        let parsed = parse_neighbor_output(output, 9100);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0], "192.168.44.2:9100".parse().unwrap());
        assert_eq!(parsed[1], "[fe80::1234]:9100".parse().unwrap());
    }

    #[test]
    fn devices_output_filters_to_matching_prefix() {
        let output = "\
Device AA:BB:CC:DD:EE:01 PIM-gateway
Device AA:BB:CC:DD:EE:02 Phone
Device AA:BB:CC:DD:EE:03 PIM-client
";
        let parsed = parse_devices_output(output, "PIM-", "PIM-self");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].mac, "AA:BB:CC:DD:EE:01");
        assert_eq!(parsed[1].name, "PIM-client");
    }

    #[test]
    fn blueutil_inquiry_output_filters_to_matching_prefix() {
        let output = "\
address: aa-bb-cc-dd-ee-01, not connected, not favourite, not paired, name: \"PIM-gateway\", recent access date: -\n\
address: aa-bb-cc-dd-ee-02, not connected, not favourite, not paired, name: \"Phone\", recent access date: -\n\
address: aa-bb-cc-dd-ee-03, not connected, not favourite, not paired, name: \"PIM-self\", recent access date: -\n";
        let parsed = parse_blueutil_inquiry_output(output, "PIM-", "PIM-self");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].mac, "AA:BB:CC:DD:EE:01");
        assert_eq!(parsed[0].name, "PIM-gateway");
    }

    #[test]
    fn arp_output_parses_interface_scoped_neighbors() {
        let output = "\
? (192.168.44.2) at aa:bb:cc:dd:ee:01 on bridge0 ifscope [ethernet]\n\
? (192.168.44.3) at aa:bb:cc:dd:ee:02 on en0 ifscope [ethernet]\n\
? (fe80::1) at aa:bb:cc:dd:ee:03 on bridge0 ifscope permanent [ethernet]\n";
        let parsed = parse_arp_output(output, "bridge0", 9100);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0], "192.168.44.2:9100".parse().unwrap());
        assert_eq!(parsed[1], "[fe80::1]:9100".parse().unwrap());
    }

    #[test]
    fn interface_operstate_path_uses_supplied_root() {
        let path = interface_operstate_path(Path::new("/tmp/fake-sysfs"), "bnep0");
        assert_eq!(path, PathBuf::from("/tmp/fake-sysfs/bnep0/operstate"));
    }

    #[tokio::test]
    async fn run_discovers_radio_peer_and_emits_neighbor_target() {
        let fake_root = unique_test_dir("pim-bt-fake-root");
        fs::create_dir_all(fake_root.join("sysfs/bnep0")).unwrap();
        fs::write(fake_root.join("sysfs/bnep0/operstate"), "down\n").unwrap();

        let fake_bluetoothctl = fake_root.join("bluetoothctl");
        fs::write(
            &fake_bluetoothctl,
            "#!/bin/sh\nif [ \"$3\" = \"devices\" ]; then\n  printf 'Device AA:BB:CC:DD:EE:FF PIM-peer\\n'\n  exit 0\nfi\nexit 0\n",
        )
        .unwrap();
        let fake_bt_network = fake_root.join("bt-network");
        fs::write(
            &fake_bt_network,
            format!(
                "#!/bin/sh\ntouch {ready}\nprintf 'up\\n' > {operstate}\nexit 0\n",
                ready = fake_root.join("fake-neigh-ready").display(),
                operstate = fake_root.join("sysfs/bnep0/operstate").display(),
            ),
        )
        .unwrap();
        let fake_ip = fake_root.join("ip");
        fs::write(
            &fake_ip,
            format!(
                "#!/bin/sh\nif [ \"$1\" = \"neigh\" ] && [ \"$2\" = \"show\" ] && [ \"$3\" = \"dev\" ] && [ \"$4\" = \"bnep0\" ]; then\n  if [ -f {ready} ]; then\n    printf '192.168.44.2 dev bnep0 lladdr 02:00:00:00:00:02 REACHABLE\\n'\n  fi\n  exit 0\nfi\nexit 0\n",
                ready = fake_root.join("fake-neigh-ready").display(),
            ),
        )
        .unwrap();
        make_executable(&fake_bluetoothctl);
        make_executable(&fake_bt_network);
        make_executable(&fake_ip);

        let config = BluetoothConfig {
            interface: "bnep0".into(),
            local_alias: "PIM-self".into(),
            poll_interval_ms: 10,
            scan_interval_ms: 10,
            peer_discovery_interval_ms: 10,
            startup_timeout_ms: 500,
            bluetoothctl_timeout_s: 1,
            ..Default::default()
        };

        let (svc, mut rx) = BluetoothDiscovery::new_with_system_paths(
            config,
            9100,
            Vec::new(),
            fake_root.join("sysfs"),
            fake_ip,
            fake_bluetoothctl,
            fake_bt_network,
        )
        .unwrap();
        let cancel = CancellationToken::new();

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
        runner.await.unwrap().unwrap();
        fs::remove_dir_all(fake_root).unwrap();
    }

    fn unique_test_dir(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()))
    }

    fn make_executable(path: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(path).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(path, perms).unwrap();
        }
    }
}
