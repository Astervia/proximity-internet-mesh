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
#[cfg(any(test, target_os = "linux"))]
use std::path::Path;
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::process::Stdio;
use std::time::{Duration, Instant};

use pim_core::BluetoothConfig;
#[cfg(target_os = "linux")]
use tokio::process::Child;
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedPanInterface {
    name: String,
    source: &'static str,
}

/// Watches Bluetooth radio state and PAN neighbors, emitting peer socket addresses.
#[derive(Debug)]
pub struct BluetoothDiscovery {
    config: BluetoothConfig,
    listen_port: u16,
    static_targets: Vec<SocketAddr>,
    #[cfg(target_os = "linux")]
    sysfs_root: PathBuf,
    ip_command: PathBuf,
    bluetoothctl_command: PathBuf,
    #[cfg(target_os = "linux")]
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
        #[cfg(target_os = "linux")] sysfs_root: impl Into<PathBuf>,
        #[cfg(not(target_os = "linux"))] _sysfs_root: impl Into<PathBuf>,
        ip_command: impl Into<PathBuf>,
        bluetoothctl_command: impl Into<PathBuf>,
        #[cfg(target_os = "linux")] bt_network_command: impl Into<PathBuf>,
        #[cfg(not(target_os = "linux"))] _bt_network_command: impl Into<PathBuf>,
    ) -> Result<(Self, mpsc::Receiver<SocketAddr>), BluetoothError> {
        let (peer_tx, peer_rx) = mpsc::channel(16);

        Ok((
            Self {
                config,
                listen_port,
                static_targets,
                #[cfg(target_os = "linux")]
                sysfs_root: sysfs_root.into(),
                ip_command: ip_command.into(),
                bluetoothctl_command: bluetoothctl_command.into(),
                #[cfg(target_os = "linux")]
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
            && !self.config.serve_nap
        {
            warn!(
                interface = %self.config.interface,
                "Bluetooth enabled with neither static peers, PAN neighbor discovery, nor radio discovery; skipping"
            );
            return Ok(());
        }

        info!(
            interface = %self.config.interface,
            connect_pan = self.config.connect_pan,
            serve_nap = self.config.serve_nap,
            nap_bridge = %self.config.nap_bridge,
            static_peers = self.static_targets.len(),
            radio_discovery = self.config.radio_discovery_enabled,
            auto_discover_pan_peers = self.config.auto_discover_peers,
            "Bluetooth service starting"
        );

        if self.config.radio_discovery_enabled {
            self.prepare_controller().await?;
            self.run_bluetoothctl(["scan", "on"]).await?;
        }

        #[cfg(target_os = "linux")]
        let mut nap_server = if self.config.serve_nap {
            Some(self.start_nap_server().await?)
        } else {
            None
        };

        let mut active_interface: Option<ResolvedPanInterface> = None;
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
                    #[cfg(target_os = "linux")]
                    if let Some(mut child) = nap_server.take() {
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                    }
                    debug!("Bluetooth service cancelled");
                    return Ok(());
                }
                _ = interface_interval.tick() => {
                    #[cfg(target_os = "linux")]
                    self.ensure_nap_server_running(&mut nap_server).await?;

                    let resolved = self.resolve_pan_interface().await?;
                    if active_interface != resolved {
                        match (&active_interface, &resolved) {
                            (_, Some(interface)) => info!(
                                configured_interface = %self.config.interface,
                                resolved_interface = %interface.name,
                                source = interface.source,
                                "Bluetooth PAN interface selected"
                            ),
                            (Some(previous), None) => warn!(
                                previous_interface = %previous.name,
                                "Bluetooth PAN interface disappeared; waiting for a new PAN interface"
                            ),
                            (None, None) => {}
                        }
                        active_interface = resolved;
                    }

                    if active_interface.is_none() && Instant::now() >= startup_deadline {
                        warn!(
                            interface = %self.config.interface,
                            timeout_ms = self.config.startup_timeout_ms,
                            "Bluetooth PAN interface did not become ready before timeout"
                        );
                        return Ok(());
                    }

                    if active_interface.is_some() && !emitted_static {
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
                _ = scan_interval.tick(), if self.config.radio_discovery_enabled && self.config.connect_pan => {
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
                _ = peer_interval.tick(), if active_interface.is_some() && self.config.auto_discover_peers => {
                    let interface = active_interface
                        .as_ref()
                        .expect("peer discovery tick requires active interface");
                    let discovered = self.discover_neighbor_targets(&interface.name).await?;
                    let discovered_set: HashSet<SocketAddr> = discovered.iter().copied().collect();

                    for addr in discovered {
                        if seen_addrs.insert(addr) {
                            info!(%addr, interface = %interface.name, "Bluetooth PAN discovered peer addr");
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
        self.prepare_controller_impl().await
    }

    async fn discover_devices(&self) -> Result<Vec<DiscoveredDevice>, BluetoothError> {
        self.discover_devices_impl().await
    }

    async fn pair_and_request_pan(&self, device: &DiscoveredDevice) -> Result<(), BluetoothError> {
        self.pair_and_request_pan_impl(device).await
    }

    async fn resolve_pan_interface(&self) -> Result<Option<ResolvedPanInterface>, BluetoothError> {
        self.resolve_pan_interface_impl().await
    }

    async fn discover_neighbor_targets(
        &self,
        interface: &str,
    ) -> Result<Vec<SocketAddr>, BluetoothError> {
        self.discover_neighbor_targets_impl(interface).await
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

    #[cfg(target_os = "linux")]
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

    #[cfg(target_os = "linux")]
    async fn prepare_controller_impl(&self) -> Result<(), BluetoothError> {
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

    #[cfg(target_os = "macos")]
    async fn prepare_controller_impl(&self) -> Result<(), BluetoothError> {
        self.run_bluetoothctl(["--power", "1"]).await?;
        self.run_bluetoothctl(["--discoverable", "1"]).await?;
        if !self.config.local_alias.is_empty() {
            warn!(
                local_alias = %self.config.local_alias,
                "macOS Bluetooth backend does not set the host controller alias automatically; set the Mac Bluetooth name manually if discovery by prefix is required"
            );
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    async fn discover_devices_impl(&self) -> Result<Vec<DiscoveredDevice>, BluetoothError> {
        let output = self.run_bluetoothctl_capture(["devices"]).await?;
        Ok(parse_devices_output(
            &output,
            &self.config.device_name_prefix,
            &self.config.local_alias,
        ))
    }

    #[cfg(target_os = "macos")]
    async fn discover_devices_impl(&self) -> Result<Vec<DiscoveredDevice>, BluetoothError> {
        let output = self
            .run_bluetoothctl_capture([
                "--inquiry",
                &self.config.bluetoothctl_timeout_s.to_string(),
            ])
            .await?;
        Ok(parse_blueutil_inquiry_output(
            &output,
            &self.config.device_name_prefix,
            &self.config.local_alias,
        ))
    }

    #[cfg(target_os = "linux")]
    async fn pair_and_request_pan_impl(
        &self,
        device: &DiscoveredDevice,
    ) -> Result<(), BluetoothError> {
        self.run_bluetoothctl(["pair", &device.mac]).await?;
        self.run_bluetoothctl(["trust", &device.mac]).await?;
        self.run_bluetoothctl(["connect", &device.mac]).await?;
        self.run_bt_network(&device.mac).await?;
        Ok(())
    }

    #[cfg(target_os = "linux")]
    async fn start_nap_server(&self) -> Result<Child, BluetoothError> {
        let bridge = self.config.nap_bridge.trim();
        let mut args = vec!["-s".to_string(), "nap".to_string()];
        let mut bridge_in_use = None;
        if bridge.is_empty() {
            warn!("serve_nap enabled with an empty nap_bridge; falling back to bt-network -s nap");
        } else if self.sysfs_root.join(bridge).exists() {
            args.push(bridge.to_string());
            bridge_in_use = Some(bridge);
        } else {
            warn!(
                bridge,
                sysfs_root = %self.sysfs_root.display(),
                "configured nap_bridge was not found; falling back to bt-network -s nap without a bridge"
            );
        }

        let child = Command::new(&self.bt_network_command)
            .args(&args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        if let Some(bridge) = bridge_in_use {
            info!(bridge, "Bluetooth NAP server started");
        } else {
            info!("Bluetooth NAP server started without an explicit bridge");
        }
        Ok(child)
    }

    #[cfg(target_os = "linux")]
    async fn ensure_nap_server_running(
        &self,
        child: &mut Option<Child>,
    ) -> Result<(), BluetoothError> {
        if !self.config.serve_nap {
            return Ok(());
        }

        if let Some(existing) = child.as_mut() {
            if let Some(status) = existing.try_wait()? {
                warn!(?status, bridge = %self.config.nap_bridge, "Bluetooth NAP server exited; restarting");
                *child = Some(self.start_nap_server().await?);
            }
        } else {
            *child = Some(self.start_nap_server().await?);
        }

        Ok(())
    }

    #[cfg(target_os = "macos")]
    async fn pair_and_request_pan_impl(
        &self,
        device: &DiscoveredDevice,
    ) -> Result<(), BluetoothError> {
        self.run_bluetoothctl(["--pair", &device.mac]).await?;
        self.run_bluetoothctl(["--connect", &device.mac]).await?;
        Ok(())
    }

    #[cfg(target_os = "linux")]
    async fn resolve_pan_interface_impl(
        &self,
    ) -> Result<Option<ResolvedPanInterface>, BluetoothError> {
        let candidates = list_pan_candidates(&self.sysfs_root).await?;

        if let Some(interface) = select_pan_interface(
            &candidates,
            preferred_interface_hint(&self.config.interface),
            self.config
                .serve_nap
                .then_some(self.config.nap_bridge.as_str()),
        ) {
            return Ok(Some(interface));
        }

        if !candidates.is_empty() {
            debug!(
                configured_interface = %self.config.interface,
                candidates = %format_candidate_summary(&candidates),
                "Bluetooth PAN interface not ready yet"
            );
        }
        Ok(None)
    }

    #[cfg(target_os = "macos")]
    async fn resolve_pan_interface_impl(
        &self,
    ) -> Result<Option<ResolvedPanInterface>, BluetoothError> {
        let interface = resolve_macos_pan_interface_hint(&self.config.interface);
        let output = Command::new("ifconfig").arg(interface).output().await?;
        if !output.status.success() {
            return Ok(None);
        }
        Ok(
            is_ready_ifconfig_output(&String::from_utf8_lossy(&output.stdout)).then(|| {
                ResolvedPanInterface {
                    name: interface.to_string(),
                    source: if self.config.interface.trim() == "auto" {
                        "auto-default"
                    } else {
                        "configured"
                    },
                }
            }),
        )
    }

    #[cfg(target_os = "linux")]
    async fn discover_neighbor_targets_impl(
        &self,
        interface: &str,
    ) -> Result<Vec<SocketAddr>, BluetoothError> {
        let output = Command::new(&self.ip_command)
            .args(["neigh", "show", "dev", interface])
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

    #[cfg(target_os = "macos")]
    async fn discover_neighbor_targets_impl(
        &self,
        interface: &str,
    ) -> Result<Vec<SocketAddr>, BluetoothError> {
        let output = Command::new(&self.ip_command)
            .args(["-an", "-i", interface])
            .output()
            .await?;

        if !output.status.success() {
            return Err(BluetoothError::CommandFailed {
                command: "arp",
                message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }

        Ok(parse_arp_output(
            &String::from_utf8_lossy(&output.stdout),
            interface,
            self.listen_port,
        ))
    }
}

#[cfg(test)]
fn interface_operstate_path(sysfs_root: &Path, interface: &str) -> PathBuf {
    sysfs_root.join(interface).join("operstate")
}

#[cfg(target_os = "linux")]
async fn read_operstate_if_present(path: &Path) -> Result<Option<String>, std::io::Error> {
    match tokio::fs::read_to_string(path).await {
        Ok(state) => Ok(Some(state)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

#[cfg(any(test, target_os = "linux"))]
fn is_ready_operstate(state: &str) -> bool {
    matches!(state.trim(), "up" | "unknown")
}

#[cfg(any(test, target_os = "linux"))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct PanInterfaceCandidate {
    name: String,
    operstate: Option<String>,
}

#[cfg(target_os = "linux")]
async fn list_pan_candidates(
    sysfs_root: &Path,
) -> Result<Vec<PanInterfaceCandidate>, std::io::Error> {
    let mut entries = tokio::fs::read_dir(sysfs_root).await?;
    let mut candidates = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name().to_string_lossy().to_string();
        let operstate = read_operstate_if_present(&entry.path().join("operstate")).await?;
        candidates.push(PanInterfaceCandidate { name, operstate });
    }
    candidates.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(candidates)
}

#[cfg(any(test, target_os = "linux"))]
fn preferred_interface_hint(interface: &str) -> Option<&str> {
    let interface = interface.trim();
    (!interface.is_empty() && interface != "auto").then_some(interface)
}

#[cfg(any(test, target_os = "linux"))]
fn select_pan_interface(
    candidates: &[PanInterfaceCandidate],
    preferred: Option<&str>,
    nap_bridge: Option<&str>,
) -> Option<ResolvedPanInterface> {
    let nap_bridge = nap_bridge.and_then(preferred_interface_hint);

    for candidate in candidates {
        if preferred == Some(candidate.name.as_str()) && candidate_is_ready(candidate) {
            return Some(ResolvedPanInterface {
                name: candidate.name.clone(),
                source: "configured",
            });
        }
    }

    for candidate in candidates {
        if nap_bridge == Some(candidate.name.as_str()) && candidate_is_ready(candidate) {
            return Some(ResolvedPanInterface {
                name: candidate.name.clone(),
                source: "nap_bridge",
            });
        }
    }

    for candidate in candidates {
        if candidate.name.starts_with("bnep") && candidate_is_ready(candidate) {
            return Some(ResolvedPanInterface {
                name: candidate.name.clone(),
                source: "dynamic-bnep",
            });
        }
    }

    for candidate in candidates {
        if candidate.name.starts_with("enx") && candidate_is_ready(candidate) {
            return Some(ResolvedPanInterface {
                name: candidate.name.clone(),
                source: "dynamic-enx",
            });
        }
    }

    None
}

#[cfg(any(test, target_os = "linux"))]
fn candidate_is_ready(candidate: &PanInterfaceCandidate) -> bool {
    candidate
        .operstate
        .as_deref()
        .is_some_and(is_ready_operstate)
}

#[cfg(any(test, target_os = "linux"))]
fn format_candidate_summary(candidates: &[PanInterfaceCandidate]) -> String {
    candidates
        .iter()
        .filter(|candidate| {
            candidate.name.starts_with("bnep")
                || candidate.name.starts_with("enx")
                || candidate.name.starts_with("br-")
        })
        .map(|candidate| {
            format!(
                "{}:{}",
                candidate.name,
                candidate.operstate.as_deref().unwrap_or("missing")
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(any(test, target_os = "macos"))]
fn is_ready_ifconfig_output(output: &str) -> bool {
    let output = output.trim();
    !output.is_empty() && !output.contains("status: inactive")
}

#[cfg(any(test, target_os = "macos"))]
fn resolve_macos_pan_interface_hint(interface: &str) -> &str {
    match interface.trim() {
        "" | "auto" => "bridge0",
        explicit => explicit,
    }
}

#[cfg(any(test, target_os = "linux"))]
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

#[cfg(any(test, target_os = "linux"))]
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
    fn macos_auto_interface_hint_defaults_to_bridge0() {
        assert_eq!(resolve_macos_pan_interface_hint("auto"), "bridge0");
        assert_eq!(resolve_macos_pan_interface_hint(""), "bridge0");
        assert_eq!(resolve_macos_pan_interface_hint("bridge1"), "bridge1");
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

    #[test]
    fn preferred_interface_hint_treats_auto_as_unset() {
        assert_eq!(preferred_interface_hint("auto"), None);
        assert_eq!(preferred_interface_hint(""), None);
        assert_eq!(preferred_interface_hint("bnep0"), Some("bnep0"));
    }

    #[test]
    fn select_pan_interface_prefers_configured_ready_interface() {
        let selected = select_pan_interface(
            &[
                PanInterfaceCandidate {
                    name: "enx1234".into(),
                    operstate: Some("up".into()),
                },
                PanInterfaceCandidate {
                    name: "bnep7".into(),
                    operstate: Some("up".into()),
                },
            ],
            Some("enx1234"),
            None,
        )
        .unwrap();
        assert_eq!(selected.name, "enx1234");
        assert_eq!(selected.source, "configured");
    }

    #[test]
    fn select_pan_interface_falls_back_to_dynamic_linux_names() {
        let selected = select_pan_interface(
            &[
                PanInterfaceCandidate {
                    name: "eth0".into(),
                    operstate: Some("up".into()),
                },
                PanInterfaceCandidate {
                    name: "enx6432a8144f4b".into(),
                    operstate: Some("up".into()),
                },
            ],
            Some("bnep0"),
            None,
        )
        .unwrap();
        assert_eq!(selected.name, "enx6432a8144f4b");
        assert_eq!(selected.source, "dynamic-enx");
    }

    #[test]
    fn select_pan_interface_uses_nap_bridge_when_serving() {
        let selected = select_pan_interface(
            &[PanInterfaceCandidate {
                name: "br-bt".into(),
                operstate: Some("unknown".into()),
            }],
            None,
            Some("br-bt"),
        )
        .unwrap();
        assert_eq!(selected.name, "br-bt");
        assert_eq!(selected.source, "nap_bridge");
    }

    #[cfg(target_os = "linux")]
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

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn run_resolves_dynamic_enx_interface() {
        let fake_root = unique_test_dir("pim-bt-fake-root-enx");
        fs::create_dir_all(fake_root.join("sysfs/enx6432a8144f4b")).unwrap();
        fs::write(fake_root.join("sysfs/enx6432a8144f4b/operstate"), "up\n").unwrap();

        let fake_bluetoothctl = fake_root.join("bluetoothctl");
        fs::write(&fake_bluetoothctl, "#!/bin/sh\nexit 0\n").unwrap();

        let fake_bt_network = fake_root.join("bt-network");
        fs::write(&fake_bt_network, "#!/bin/sh\nexit 0\n").unwrap();

        let fake_ip = fake_root.join("ip");
        fs::write(
            &fake_ip,
            "#!/bin/sh\nif [ \"$1\" = \"neigh\" ] && [ \"$2\" = \"show\" ] && [ \"$3\" = \"dev\" ] && [ \"$4\" = \"enx6432a8144f4b\" ]; then\n  printf '192.168.44.9 dev enx6432a8144f4b lladdr 02:00:00:00:00:09 REACHABLE\\n'\n  exit 0\nfi\nexit 0\n",
        )
        .unwrap();
        make_executable(&fake_bluetoothctl);
        make_executable(&fake_bt_network);
        make_executable(&fake_ip);

        let config = BluetoothConfig {
            interface: "bnep0".into(),
            radio_discovery_enabled: false,
            local_alias: "PIM-self".into(),
            poll_interval_ms: 10,
            peer_discovery_interval_ms: 10,
            startup_timeout_ms: 500,
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
            .expect("timed out waiting for dynamic enx Bluetooth address")
            .expect("channel closed before address emitted");
        assert_eq!(addr, "192.168.44.9:9100".parse().unwrap());

        cancel.cancel();
        runner.await.unwrap().unwrap();
        fs::remove_dir_all(fake_root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn start_nap_server_falls_back_when_bridge_is_missing() {
        let fake_root = unique_test_dir("pim-bt-fake-root-nap-fallback");
        fs::create_dir_all(fake_root.join("sysfs")).unwrap();

        let fake_bt_network = fake_root.join("bt-network");
        let fake_args = fake_root.join("bt-network-args");
        fs::write(
            &fake_bt_network,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > {args}\nexit 0\n",
                args = fake_args.display(),
            ),
        )
        .unwrap();
        let fake_bluetoothctl = fake_root.join("bluetoothctl");
        fs::write(&fake_bluetoothctl, "#!/bin/sh\nexit 0\n").unwrap();
        let fake_ip = fake_root.join("ip");
        fs::write(&fake_ip, "#!/bin/sh\nexit 0\n").unwrap();
        make_executable(&fake_bt_network);
        make_executable(&fake_bluetoothctl);
        make_executable(&fake_ip);

        let config = BluetoothConfig {
            serve_nap: true,
            connect_pan: false,
            nap_bridge: "br-bt".into(),
            ..Default::default()
        };

        let (svc, _rx) = BluetoothDiscovery::new_with_system_paths(
            config,
            9100,
            Vec::new(),
            fake_root.join("sysfs"),
            fake_ip,
            fake_bluetoothctl,
            fake_bt_network,
        )
        .unwrap();

        let mut child = svc.start_nap_server().await.unwrap();
        child.wait().await.unwrap();

        let args = fs::read_to_string(fake_args).unwrap();
        assert_eq!(args, "-s\nnap\n");

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
