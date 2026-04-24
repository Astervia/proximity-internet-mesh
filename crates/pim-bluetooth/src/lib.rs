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

#[cfg(target_os = "linux")]
use std::collections::HashMap;
use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr, SocketAddrV6};
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
/// Default `iptables` command used to install NAT rules for the Bluetooth subnet.
pub const DEFAULT_IPTABLES_COMMAND: &str = "iptables";
/// Default `dnsmasq` command used to run a DHCP server on the NAP bridge.
pub const DEFAULT_DNSMASQ_COMMAND: &str = "dnsmasq";
/// Default `dhclient` command used to acquire an IP on the PAN interface.
pub const DEFAULT_DHCLIENT_COMMAND: &str = "dhclient";
/// Default location of the host resolver file used to inherit upstream DNS.
pub const DEFAULT_RESOLV_CONF: &str = "/etc/resolv.conf";

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

impl BluetoothError {
    fn is_missing_device_error(&self) -> bool {
        match self {
            Self::CommandFailed { command, message } if *command == "ip" => {
                message.contains("Cannot find device") || message.contains("does not exist")
            }
            _ => false,
        }
    }
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
    #[cfg(target_os = "linux")]
    iptables_command: PathBuf,
    #[cfg(target_os = "linux")]
    dnsmasq_command: PathBuf,
    #[cfg(target_os = "linux")]
    dhclient_command: PathBuf,
    #[cfg(target_os = "linux")]
    resolv_conf_path: PathBuf,
    #[allow(dead_code)]
    nat_interface: Option<String>,
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
            #[cfg(target_os = "linux")]
            DEFAULT_IPTABLES_COMMAND,
            #[cfg(not(target_os = "linux"))]
            "",
            #[cfg(target_os = "linux")]
            DEFAULT_DNSMASQ_COMMAND,
            #[cfg(not(target_os = "linux"))]
            "",
            #[cfg(target_os = "linux")]
            DEFAULT_DHCLIENT_COMMAND,
            #[cfg(not(target_os = "linux"))]
            "",
            #[cfg(target_os = "linux")]
            DEFAULT_RESOLV_CONF,
            #[cfg(not(target_os = "linux"))]
            "",
            None::<String>,
        )
    }

    /// Build a watcher with explicit command and sysfs paths.
    #[allow(clippy::too_many_arguments)]
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
        #[cfg(target_os = "linux")] iptables_command: impl Into<PathBuf>,
        #[cfg(not(target_os = "linux"))] _iptables_command: impl Into<PathBuf>,
        #[cfg(target_os = "linux")] dnsmasq_command: impl Into<PathBuf>,
        #[cfg(not(target_os = "linux"))] _dnsmasq_command: impl Into<PathBuf>,
        #[cfg(target_os = "linux")] dhclient_command: impl Into<PathBuf>,
        #[cfg(not(target_os = "linux"))] _dhclient_command: impl Into<PathBuf>,
        #[cfg(target_os = "linux")] resolv_conf_path: impl Into<PathBuf>,
        #[cfg(not(target_os = "linux"))] _resolv_conf_path: impl Into<PathBuf>,
        nat_interface: Option<impl Into<String>>,
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
                #[cfg(target_os = "linux")]
                iptables_command: iptables_command.into(),
                #[cfg(target_os = "linux")]
                dnsmasq_command: dnsmasq_command.into(),
                #[cfg(target_os = "linux")]
                dhclient_command: dhclient_command.into(),
                #[cfg(target_os = "linux")]
                resolv_conf_path: resolv_conf_path.into(),
                nat_interface: nat_interface.map(Into::into),
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
            self.install_bluetooth_masquerade().await?;
            Some(self.start_nap_server().await?)
        } else {
            None
        };
        #[cfg(target_os = "linux")]
        let mut dnsmasq_child: Option<Child> = None;
        #[cfg(target_os = "linux")]
        let mut dhclient_children: HashMap<String, Child> = HashMap::new();
        #[cfg(target_os = "linux")]
        let mut pan_clients: HashMap<String, Child> = HashMap::new();

        let mut active_interfaces: Vec<ResolvedPanInterface> = Vec::new();
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
                    {
                        self.teardown_linux(
                            &mut nap_server,
                            &mut dnsmasq_child,
                            &mut dhclient_children,
                            &mut pan_clients,
                        )
                        .await;
                    }
                    debug!("Bluetooth service cancelled");
                    return Ok(());
                }
                _ = interface_interval.tick() => {
                    #[cfg(target_os = "linux")]
                    {
                        self.ensure_nap_server_running(&mut nap_server).await?;
                        if self.config.serve_nap {
                            let bridge = self.config.nap_bridge.trim().to_string();
                            if !bridge.is_empty() {
                                self.attach_bnep_to_bridge(&bridge).await?;
                                if self.config.dhcp_enabled {
                                    self.ensure_dnsmasq_running(&bridge, &mut dnsmasq_child).await?;
                                }
                            }
                        }
                    }

                    let resolved = self.resolve_pan_interfaces().await?;
                    if active_interfaces != resolved {
                        let previous: HashSet<&str> =
                            active_interfaces.iter().map(|iface| iface.name.as_str()).collect();
                        let current: HashSet<&str> =
                            resolved.iter().map(|iface| iface.name.as_str()).collect();

                        for interface in &resolved {
                            if !previous.contains(interface.name.as_str()) {
                                info!(
                                    configured_interface = %self.config.interface,
                                    resolved_interface = %interface.name,
                                    source = interface.source,
                                    "Bluetooth PAN interface selected"
                                );
                            }
                        }

                        for previous_interface in &active_interfaces {
                            if !current.contains(previous_interface.name.as_str()) {
                                warn!(
                                    previous_interface = %previous_interface.name,
                                    "Bluetooth PAN interface disappeared; waiting for it to return"
                                );
                            }
                        }

                        active_interfaces = resolved;
                    }

                    #[cfg(target_os = "linux")]
                    if self.config.request_dhcp
                        && self.config.connect_pan
                        && !self.config.serve_nap
                    {
                        self.prune_dhclient_children(&active_interfaces, &mut dhclient_children)
                            .await;
                        for interface in &active_interfaces {
                            if let Err(err) = self
                                .ensure_dhclient_running(&interface.name, &mut dhclient_children)
                                .await
                            {
                                warn!(
                                    interface = %interface.name,
                                    dhclient = %self.dhclient_command.display(),
                                    %err,
                                    "Bluetooth DHCP client unavailable; continuing without DHCP (peer discovery still works via IPv6 link-local)"
                                );
                            }
                        }
                    }

                    if active_interfaces.is_empty() && Instant::now() >= startup_deadline {
                        warn!(
                            interface = %self.config.interface,
                            timeout_ms = self.config.startup_timeout_ms,
                            "Bluetooth PAN interface did not become ready before timeout"
                        );
                        if !(self.config.radio_discovery_enabled && self.config.connect_pan) {
                            warn!(
                                interface = %self.config.interface,
                                "Bluetooth PAN discovery exiting: interface never ready and radio scan is disabled"
                            );
                            return Ok(());
                        }
                    }

                    if !active_interfaces.is_empty() && !emitted_static {
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
                    #[cfg(target_os = "linux")]
                    {
                        pan_clients.retain(|mac, child| match child.try_wait() {
                            Ok(Some(status)) => {
                                warn!(%mac, ?status, "Bluetooth PAN client (bt-network) exited; will retry on next scan");
                                seen_macs.remove(mac);
                                false
                            }
                            Ok(None) => true,
                            Err(err) => {
                                warn!(%mac, %err, "failed to poll bt-network child; dropping handle");
                                seen_macs.remove(mac);
                                false
                            }
                        });
                    }

                    info!(
                        prefix = %self.config.device_name_prefix,
                        active_connections = seen_macs.len(),
                        "Bluetooth radio scan started"
                    );
                    let devices = self.discover_devices().await?;
                    info!(
                        found = devices.len(),
                        new = devices.iter().filter(|d| !seen_macs.contains(&d.mac)).count(),
                        "Bluetooth radio scan complete"
                    );
                    for device in devices {
                        if seen_macs.contains(&device.mac) {
                            continue;
                        }
                        match self.pair_and_request_pan(&device).await {
                            Ok(()) => {
                                #[cfg(target_os = "linux")]
                                {
                                    match self.start_pan_client(&device.mac).await {
                                        Ok(child) => {
                                            info!(mac = %device.mac, name = %device.name, "Bluetooth radio-discovered peer prepared");
                                            pan_clients.insert(device.mac.clone(), child);
                                            seen_macs.insert(device.mac);
                                        }
                                        Err(err) => {
                                            warn!(mac = %device.mac, name = %device.name, "Bluetooth bt-network spawn failed: {err}");
                                        }
                                    }
                                }
                                #[cfg(not(target_os = "linux"))]
                                {
                                    info!(mac = %device.mac, name = %device.name, "Bluetooth radio-discovered peer prepared");
                                    seen_macs.insert(device.mac);
                                }
                            }
                            Err(err) => {
                                warn!(mac = %device.mac, name = %device.name, "Bluetooth radio discovery failed: {err}");
                            }
                        }
                    }
                }
                _ = peer_interval.tick(), if !active_interfaces.is_empty() && self.config.auto_discover_peers => {
                    let mut discovered_all: HashSet<SocketAddr> = HashSet::new();
                    let mut missing_interfaces: HashSet<String> = HashSet::new();

                    for interface in &active_interfaces {
                        let discovered = match self.discover_neighbor_targets(&interface.name).await {
                            Ok(discovered) => discovered,
                            Err(err) if err.is_missing_device_error() => {
                                warn!(
                                    interface = %interface.name,
                                    "Bluetooth PAN interface disappeared during neighbor scan; waiting for it to return"
                                );
                                missing_interfaces.insert(interface.name.clone());
                                continue;
                            }
                            Err(err) => return Err(err),
                        };

                        for addr in discovered {
                            discovered_all.insert(addr);
                            if seen_addrs.insert(addr) {
                                info!(%addr, interface = %interface.name, "Bluetooth PAN discovered peer addr");
                                if self.peer_tx.send(addr).await.is_err() {
                                    return Ok(());
                                }
                            }
                        }
                    }

                    seen_addrs.retain(|addr| {
                        self.static_targets.contains(addr) || discovered_all.contains(addr)
                    });

                    if !missing_interfaces.is_empty() {
                        active_interfaces
                            .retain(|interface| !missing_interfaces.contains(&interface.name));
                    }
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

    async fn resolve_pan_interfaces(&self) -> Result<Vec<ResolvedPanInterface>, BluetoothError> {
        self.resolve_pan_interfaces_impl().await
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
    async fn start_pan_client(&self, mac: &str) -> Result<Child, BluetoothError> {
        let child = Command::new(&self.bt_network_command)
            .args(["-c", mac, "nap"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        Ok(child)
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
        Ok(())
    }

    #[cfg(target_os = "linux")]
    async fn start_nap_server(&self) -> Result<Child, BluetoothError> {
        let bridge = self.config.nap_bridge.trim();
        if bridge.is_empty() {
            return Err(BluetoothError::CommandFailed {
                command: "bt-network",
                message: "serve_nap requires a non-empty nap_bridge".to_string(),
            });
        }
        self.ensure_bridge_ready(bridge).await?;

        let child = Command::new(&self.bt_network_command)
            .args(["-s", "nap", bridge])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        info!(bridge, "Bluetooth NAP server started");
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

    #[cfg(target_os = "linux")]
    async fn ensure_bridge_ready(&self, bridge: &str) -> Result<(), BluetoothError> {
        let bridge_path = self.sysfs_root.join(bridge);
        if !bridge_path.exists() {
            let output = Command::new(&self.ip_command)
                .args(["link", "add", "name", bridge, "type", "bridge"])
                .output()
                .await?;
            if !output.status.success() {
                let msg = String::from_utf8_lossy(&output.stderr).trim().to_string();
                if !msg.contains("File exists") && !msg.contains("already exists") {
                    return Err(BluetoothError::CommandFailed {
                        command: "ip",
                        message: format!("failed to create bridge {bridge}: {msg}"),
                    });
                }
            } else {
                info!(bridge, "Bluetooth NAP bridge created");
            }
        }

        let up_output = Command::new(&self.ip_command)
            .args(["link", "set", bridge, "up"])
            .output()
            .await?;
        if !up_output.status.success() {
            let msg = String::from_utf8_lossy(&up_output.stderr)
                .trim()
                .to_string();
            return Err(BluetoothError::CommandFailed {
                command: "ip",
                message: format!("failed to bring bridge {bridge} up: {msg}"),
            });
        }

        let addr = self.config.nap_bridge_addr.trim();
        if !addr.is_empty() {
            let output = Command::new(&self.ip_command)
                .args(["addr", "add", addr, "dev", bridge])
                .output()
                .await?;
            if output.status.success() {
                info!(bridge, %addr, "Bluetooth NAP bridge address assigned");
            } else {
                let msg = String::from_utf8_lossy(&output.stderr).trim().to_string();
                if !msg.contains("File exists") {
                    return Err(BluetoothError::CommandFailed {
                        command: "ip",
                        message: format!("failed to assign address {addr} to {bridge}: {msg}"),
                    });
                }
            }
        }

        Ok(())
    }

    /// Walk `/sys/class/net` and add any BNEP (DEVTYPE=bluetooth) interface
    /// that is not already enslaved to a bridge to the NAP bridge, then bring
    /// it up. Works around BlueZ's `bt-network -s nap` failing to auto-bridge
    /// incoming PAN connections on some distros.
    #[cfg(target_os = "linux")]
    async fn attach_bnep_to_bridge(&self, bridge: &str) -> Result<(), BluetoothError> {
        if bridge.is_empty() {
            return Ok(());
        }
        let mut entries = match tokio::fs::read_dir(&self.sysfs_root).await {
            Ok(entries) => entries,
            Err(err) => {
                debug!(sysfs = %self.sysfs_root.display(), %err, "cannot enumerate sysfs net dir");
                return Ok(());
            }
        };
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name().to_string_lossy().to_string();
            if name == bridge {
                continue;
            }
            let uevent = match tokio::fs::read_to_string(entry.path().join("uevent")).await {
                Ok(contents) => contents,
                Err(_) => continue,
            };
            if !uevent.lines().any(|l| l.trim() == "DEVTYPE=bluetooth") {
                continue;
            }
            if entry.path().join("master").exists() {
                continue;
            }
            info!(iface = %name, bridge, "Bluetooth PAN client connecting; attaching BNEP interface to NAP bridge");
            let set_master = Command::new(&self.ip_command)
                .args(["link", "set", &name, "master", bridge])
                .output()
                .await?;
            if !set_master.status.success() {
                let msg = String::from_utf8_lossy(&set_master.stderr)
                    .trim()
                    .to_string();
                warn!(iface = %name, bridge, "failed to attach BNEP to bridge: {msg}");
                continue;
            }
            let set_up = Command::new(&self.ip_command)
                .args(["link", "set", &name, "up"])
                .output()
                .await?;
            if !set_up.status.success() {
                let msg = String::from_utf8_lossy(&set_up.stderr).trim().to_string();
                warn!(iface = %name, "failed to bring BNEP interface up: {msg}");
                continue;
            }
            info!(iface = %name, bridge, "Bluetooth BNEP interface attached to NAP bridge");
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    async fn install_bluetooth_masquerade(&self) -> Result<(), BluetoothError> {
        let Some(nat_iface) = self.nat_interface.as_deref() else {
            debug!("Bluetooth MASQUERADE not installed; no nat_interface configured");
            return Ok(());
        };
        let (gateway, prefix) = match parse_ipv4_cidr(&self.config.nap_bridge_addr) {
            Ok(v) => v,
            Err(msg) => {
                warn!(
                    addr = %self.config.nap_bridge_addr,
                    error = %msg,
                    "Bluetooth MASQUERADE: invalid nap_bridge_addr; skipping"
                );
                return Ok(());
            }
        };
        let (network, _) = subnet_network(gateway, prefix);
        let subnet = format!("{network}/{prefix}");

        let _ = Command::new("sysctl")
            .args(["-w", "net.ipv4.ip_forward=1"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;

        let post_args = [
            "-t",
            "nat",
            "-A",
            "POSTROUTING",
            "-s",
            subnet.as_str(),
            "-o",
            nat_iface,
            "-j",
            "MASQUERADE",
        ];
        self.iptables_ensure(&post_args).await?;

        let fwd_args = ["-A", "FORWARD", "-s", subnet.as_str(), "-j", "ACCEPT"];
        self.iptables_ensure(&fwd_args).await?;

        info!(%subnet, nat_iface, "Bluetooth MASQUERADE installed");
        Ok(())
    }

    /// Reverse of `install_bluetooth_masquerade`: removes the POSTROUTING
    /// MASQUERADE and FORWARD ACCEPT rules we installed. Idempotent and
    /// best-effort; errors are logged, not propagated.
    #[cfg(target_os = "linux")]
    async fn uninstall_bluetooth_masquerade(&self) {
        let Some(nat_iface) = self.nat_interface.as_deref() else {
            return;
        };
        let (gateway, prefix) = match parse_ipv4_cidr(&self.config.nap_bridge_addr) {
            Ok(v) => v,
            Err(_) => return,
        };
        let (network, _) = subnet_network(gateway, prefix);
        let subnet = format!("{network}/{prefix}");

        let post_args = [
            "-t",
            "nat",
            "-D",
            "POSTROUTING",
            "-s",
            subnet.as_str(),
            "-o",
            nat_iface,
            "-j",
            "MASQUERADE",
        ];
        self.iptables_delete_while_present(&post_args).await;

        let fwd_args = ["-D", "FORWARD", "-s", subnet.as_str(), "-j", "ACCEPT"];
        self.iptables_delete_while_present(&fwd_args).await;

        info!(%subnet, nat_iface, "Bluetooth MASQUERADE removed");
    }

    /// Repeatedly run `iptables -D ...` until it reports the rule is gone.
    /// `iptables_ensure` uses `-C` before `-A`, so at most one copy should
    /// exist, but loop defensively up to a small bound in case the daemon
    /// was restarted without cleanup in the past.
    #[cfg(target_os = "linux")]
    async fn iptables_delete_while_present(&self, delete_args: &[&str]) {
        for _ in 0..8 {
            let mut check: Vec<&str> = delete_args.to_vec();
            if let Some(pos) = check.iter().position(|a| *a == "-D") {
                check[pos] = "-C";
            }
            let present = Command::new(&self.iptables_command)
                .args(&check)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await
                .map(|s| s.success())
                .unwrap_or(false);
            if !present {
                return;
            }
            let _ = Command::new(&self.iptables_command)
                .args(delete_args)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;
        }
    }

    /// Bring the NAP bridge down and delete it, if it exists.
    /// Best-effort: errors are logged, not propagated.
    #[cfg(target_os = "linux")]
    async fn delete_bridge_if_present(&self, bridge: &str) {
        if bridge.is_empty() {
            return;
        }
        // Best-effort: unconditionally attempt the delete. Bridge existence
        // cannot be inferred from `sysfs_root` since tests override that to
        // a fake tree while the real bridge is created via the `ip` command.
        let down = Command::new(&self.ip_command)
            .args(["link", "set", bridge, "down"])
            .output()
            .await;
        if let Ok(out) = &down {
            if !out.status.success() {
                let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
                if !msg.is_empty() && !msg.contains("Cannot find device") {
                    debug!(bridge, "ip link set down: {msg}");
                }
            }
        }
        let del = Command::new(&self.ip_command)
            .args(["link", "delete", bridge])
            .output()
            .await;
        match del {
            Ok(out) if out.status.success() => {
                info!(bridge, "Bluetooth NAP bridge deleted");
            }
            Ok(out) => {
                let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
                if !msg.contains("Cannot find device") && !msg.contains("does not exist") {
                    warn!(bridge, "ip link delete failed: {msg}");
                }
            }
            Err(err) => {
                warn!(bridge, "ip link delete errored: {err}");
            }
        }
    }

    /// Shut down Linux-specific resources in order: child processes first,
    /// then iptables rules, then the bridge. Safe to call even when some
    /// resources were never created (e.g. NAP disabled).
    #[cfg(target_os = "linux")]
    async fn teardown_linux(
        &self,
        nap_server: &mut Option<Child>,
        dnsmasq_child: &mut Option<Child>,
        dhclient_children: &mut HashMap<String, Child>,
        pan_clients: &mut HashMap<String, Child>,
    ) {
        if let Some(mut child) = nap_server.take() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        if let Some(mut child) = dnsmasq_child.take() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        for (_, mut child) in dhclient_children.drain() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        for (_, mut child) in pan_clients.drain() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        if self.config.serve_nap {
            self.uninstall_bluetooth_masquerade().await;
            let bridge = self.config.nap_bridge.trim().to_string();
            self.delete_bridge_if_present(&bridge).await;
        }
    }

    #[cfg(target_os = "linux")]
    async fn iptables_ensure(&self, args: &[&str]) -> Result<(), BluetoothError> {
        let mut check_args: Vec<&str> = args.to_vec();
        if let Some(pos) = check_args.iter().position(|a| *a == "-A") {
            check_args[pos] = "-C";
        }
        let check = Command::new(&self.iptables_command)
            .args(&check_args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await?;
        if check.success() {
            return Ok(());
        }
        let output = Command::new(&self.iptables_command)
            .args(args)
            .output()
            .await?;
        if !output.status.success() {
            return Err(BluetoothError::CommandFailed {
                command: "iptables",
                message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    async fn resolve_dhcp_dns(&self) -> String {
        if let Some(custom) = self.config.dhcp_dns.as_deref() {
            let trimmed = custom.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
        match tokio::fs::read_to_string(&self.resolv_conf_path).await {
            Ok(content) => {
                let servers: Vec<String> = content
                    .lines()
                    .filter_map(|line| {
                        let line = line.trim();
                        if line.starts_with('#') {
                            return None;
                        }
                        let stripped = line.strip_prefix("nameserver")?.trim();
                        if stripped.is_empty() {
                            return None;
                        }
                        Some(stripped.split_whitespace().next()?.to_string())
                    })
                    .collect();
                if servers.is_empty() {
                    "1.1.1.1,8.8.8.8".to_string()
                } else {
                    servers.join(",")
                }
            }
            Err(err) => {
                debug!(
                    path = %self.resolv_conf_path.display(),
                    %err,
                    "unable to read resolv.conf; falling back to public DNS"
                );
                "1.1.1.1,8.8.8.8".to_string()
            }
        }
    }

    #[cfg(target_os = "linux")]
    async fn start_dnsmasq(&self, bridge: &str) -> Result<Child, BluetoothError> {
        let (gateway, prefix) = parse_ipv4_cidr(&self.config.nap_bridge_addr).map_err(|msg| {
            BluetoothError::CommandFailed {
                command: "dnsmasq",
                message: msg,
            }
        })?;
        let range = match self.config.dhcp_range.as_deref().map(str::trim) {
            Some(explicit) if !explicit.is_empty() => explicit.to_string(),
            _ => default_dhcp_range(gateway, prefix).ok_or_else(|| {
                BluetoothError::CommandFailed {
                    command: "dnsmasq",
                    message: format!(
                        "unable to derive DHCP range from {}",
                        self.config.nap_bridge_addr
                    ),
                }
            })?,
        };
        let lease = self.config.dhcp_lease_time.trim();
        let dns = self.resolve_dhcp_dns().await;
        let dhcp_range_arg = if lease.is_empty() {
            format!("--dhcp-range={range}")
        } else {
            format!("--dhcp-range={range},{lease}")
        };
        let router_arg = format!("--dhcp-option=3,{gateway}");
        let dns_arg = format!("--dhcp-option=6,{dns}");
        let iface_arg = format!("--interface={bridge}");
        let child = Command::new(&self.dnsmasq_command)
            .args([
                "--keep-in-foreground",
                "--log-facility=-",
                "--port=0",
                "--bind-interfaces",
                "--except-interface=lo",
                iface_arg.as_str(),
                dhcp_range_arg.as_str(),
                router_arg.as_str(),
                dns_arg.as_str(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        info!(bridge, %range, %gateway, %dns, "Bluetooth DHCP server started");
        Ok(child)
    }

    #[cfg(target_os = "linux")]
    async fn ensure_dnsmasq_running(
        &self,
        bridge: &str,
        child: &mut Option<Child>,
    ) -> Result<(), BluetoothError> {
        if let Some(existing) = child.as_mut() {
            if let Some(status) = existing.try_wait()? {
                warn!(?status, bridge, "Bluetooth DHCP server exited; restarting");
                *child = Some(self.start_dnsmasq(bridge).await?);
            }
        } else {
            *child = Some(self.start_dnsmasq(bridge).await?);
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    async fn start_dhclient(&self, interface: &str) -> Result<Child, BluetoothError> {
        let child = Command::new(&self.dhclient_command)
            .args(["-d", "-v", interface])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        info!(interface, "Bluetooth DHCP client started");
        Ok(child)
    }

    #[cfg(target_os = "linux")]
    async fn ensure_dhclient_running(
        &self,
        interface: &str,
        children: &mut HashMap<String, Child>,
    ) -> Result<(), BluetoothError> {
        let mut should_start = false;
        if let Some(existing) = children.get_mut(interface) {
            if let Some(status) = existing.try_wait()? {
                warn!(
                    ?status,
                    interface, "Bluetooth DHCP client exited; restarting"
                );
                should_start = true;
            }
        } else {
            should_start = true;
        }

        if should_start {
            if let Some(mut existing) = children.remove(interface) {
                let _ = existing.kill().await;
                let _ = existing.wait().await;
            }
            children.insert(interface.to_string(), self.start_dhclient(interface).await?);
        }

        Ok(())
    }

    #[cfg(target_os = "linux")]
    async fn prune_dhclient_children(
        &self,
        active_interfaces: &[ResolvedPanInterface],
        children: &mut HashMap<String, Child>,
    ) {
        let active: HashSet<&str> = active_interfaces
            .iter()
            .map(|interface| interface.name.as_str())
            .collect();
        let stale: Vec<String> = children
            .keys()
            .filter(|name| !active.contains(name.as_str()))
            .cloned()
            .collect();
        for interface in stale {
            if let Some(mut child) = children.remove(&interface) {
                warn!(interface = %interface, "Bluetooth DHCP client interface disappeared; stopping");
                let _ = child.kill().await;
                let _ = child.wait().await;
            }
        }
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
    async fn resolve_pan_interfaces_impl(
        &self,
    ) -> Result<Vec<ResolvedPanInterface>, BluetoothError> {
        let candidates = list_pan_candidates(&self.sysfs_root).await?;

        let interfaces = select_pan_interfaces(
            &candidates,
            preferred_interface_hint(&self.config.interface),
            self.config
                .serve_nap
                .then_some(self.config.nap_bridge.as_str()),
        );
        if !interfaces.is_empty() {
            return Ok(interfaces);
        }

        if !candidates.is_empty() {
            debug!(
                configured_interface = %self.config.interface,
                candidates = %format_candidate_summary(&candidates),
                "Bluetooth PAN interface not ready yet"
            );
        }
        Ok(Vec::new())
    }

    #[cfg(target_os = "macos")]
    async fn resolve_pan_interfaces_impl(
        &self,
    ) -> Result<Vec<ResolvedPanInterface>, BluetoothError> {
        let interface = resolve_macos_pan_interface_hint(&self.config.interface);
        let output = Command::new("ifconfig").arg(interface).output().await?;
        if !output.status.success() {
            return Ok(Vec::new());
        }
        Ok(
            is_ready_ifconfig_output(&String::from_utf8_lossy(&output.stdout))
                .then(|| ResolvedPanInterface {
                    name: interface.to_string(),
                    source: if self.config.interface.trim() == "auto" {
                        "auto-default"
                    } else {
                        "configured"
                    },
                })
                .into_iter()
                .collect(),
        )
    }

    #[cfg(target_os = "linux")]
    async fn discover_neighbor_targets_impl(
        &self,
        interface: &str,
    ) -> Result<Vec<SocketAddr>, BluetoothError> {
        let scope_id = interface_index(interface);
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
            scope_id,
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
fn parse_ipv4_cidr(cidr: &str) -> Result<(std::net::Ipv4Addr, u8), String> {
    let trimmed = cidr.trim();
    let (addr_str, prefix_str) = trimmed
        .split_once('/')
        .ok_or_else(|| format!("invalid CIDR {trimmed:?} (expected IPV4/PREFIX)"))?;
    let addr: std::net::Ipv4Addr = addr_str
        .parse()
        .map_err(|err| format!("invalid IPv4 in {trimmed:?}: {err}"))?;
    let prefix: u8 = prefix_str
        .parse()
        .map_err(|err| format!("invalid prefix in {trimmed:?}: {err}"))?;
    if prefix > 32 {
        return Err(format!("prefix out of range in {trimmed:?}"));
    }
    Ok((addr, prefix))
}

#[cfg(any(test, target_os = "linux"))]
fn subnet_network(addr: std::net::Ipv4Addr, prefix: u8) -> (std::net::Ipv4Addr, u32) {
    let octets = u32::from(addr);
    let mask = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    };
    (std::net::Ipv4Addr::from(octets & mask), mask)
}

#[cfg(any(test, target_os = "linux"))]
fn default_dhcp_range(gateway: std::net::Ipv4Addr, prefix: u8) -> Option<String> {
    if prefix >= 31 {
        return None;
    }
    let (network, mask) = subnet_network(gateway, prefix);
    let network_int = u32::from(network);
    let broadcast_int = network_int | !mask;
    let mut start = network_int.saturating_add(10);
    let mut end = broadcast_int.saturating_sub(10);
    if start >= end {
        start = network_int.saturating_add(2);
        end = broadcast_int.saturating_sub(1);
    }
    if start >= end {
        return None;
    }
    let gw_int = u32::from(gateway);
    if start == gw_int {
        start = start.saturating_add(1);
    }
    if end == gw_int {
        end = end.saturating_sub(1);
    }
    if start >= end {
        return None;
    }
    Some(format!(
        "{},{}",
        std::net::Ipv4Addr::from(start),
        std::net::Ipv4Addr::from(end)
    ))
}

#[cfg(any(test, target_os = "linux"))]
fn preferred_interface_hint(interface: &str) -> Option<&str> {
    let interface = interface.trim();
    (!interface.is_empty() && interface != "auto").then_some(interface)
}

#[cfg(any(test, target_os = "linux"))]
fn select_pan_interfaces(
    candidates: &[PanInterfaceCandidate],
    preferred: Option<&str>,
    nap_bridge: Option<&str>,
) -> Vec<ResolvedPanInterface> {
    let nap_bridge = nap_bridge.and_then(preferred_interface_hint);

    if let Some(preferred) = preferred {
        for candidate in candidates {
            if preferred == candidate.name.as_str() && candidate_is_ready(candidate) {
                return vec![ResolvedPanInterface {
                    name: candidate.name.clone(),
                    source: "configured",
                }];
            }
        }
    }

    let mut selected = Vec::new();

    for candidate in candidates {
        if nap_bridge == Some(candidate.name.as_str()) {
            selected.push(ResolvedPanInterface {
                name: candidate.name.clone(),
                source: "nap_bridge",
            });
        }
    }

    for candidate in candidates {
        if candidate.name.starts_with("bnep") && candidate_is_ready(candidate) {
            selected.push(ResolvedPanInterface {
                name: candidate.name.clone(),
                source: "dynamic-bnep",
            });
        } else if candidate.name.starts_with("enx") && candidate_is_ready(candidate) {
            selected.push(ResolvedPanInterface {
                name: candidate.name.clone(),
                source: "dynamic-enx",
            });
        }
    }

    selected
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
fn parse_neighbor_output(
    output: &str,
    listen_port: u16,
    ipv6_scope_id: Option<u32>,
) -> Vec<SocketAddr> {
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
        let addr =
            match ip {
                IpAddr::V6(ipv6) if ipv6.is_unicast_link_local() => SocketAddr::V6(
                    SocketAddrV6::new(ipv6, listen_port, 0, ipv6_scope_id.unwrap_or(0)),
                ),
                _ => SocketAddr::new(ip, listen_port),
            };
        if seen.insert(addr) {
            addrs.push(addr);
        }
    }

    addrs
}

#[cfg(target_os = "linux")]
fn interface_index(interface: &str) -> Option<u32> {
    let c_interface = std::ffi::CString::new(interface).ok()?;
    let index = unsafe { libc::if_nametoindex(c_interface.as_ptr()) };
    (index != 0).then_some(index)
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
        let parsed = parse_neighbor_output(output, 9100, Some(7));
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0], "192.168.44.2:9100".parse().unwrap());
        assert_eq!(
            parsed[1],
            SocketAddr::V6(SocketAddrV6::new("fe80::1234".parse().unwrap(), 9100, 0, 7))
        );
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
    fn select_pan_interfaces_prefers_configured_ready_interface() {
        let selected = select_pan_interfaces(
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
        );
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].name, "enx1234");
        assert_eq!(selected[0].source, "configured");
    }

    #[test]
    fn select_pan_interfaces_fall_back_to_dynamic_linux_names() {
        let selected = select_pan_interfaces(
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
        );
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].name, "enx6432a8144f4b");
        assert_eq!(selected[0].source, "dynamic-enx");
    }

    #[test]
    fn select_pan_interfaces_use_nap_bridge_when_serving() {
        let selected = select_pan_interfaces(
            &[PanInterfaceCandidate {
                name: "br-bt".into(),
                operstate: Some("down".into()),
            }],
            None,
            Some("br-bt"),
        );
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].name, "br-bt");
        assert_eq!(selected[0].source, "nap_bridge");
    }

    #[test]
    fn select_pan_interfaces_include_all_ready_dynamic_pan_links() {
        let selected = select_pan_interfaces(
            &[
                PanInterfaceCandidate {
                    name: "bnep0".into(),
                    operstate: Some("up".into()),
                },
                PanInterfaceCandidate {
                    name: "enx6432a8144f4b".into(),
                    operstate: Some("up".into()),
                },
                PanInterfaceCandidate {
                    name: "eth0".into(),
                    operstate: Some("up".into()),
                },
            ],
            None,
            None,
        );
        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].name, "bnep0");
        assert_eq!(selected[1].name, "enx6432a8144f4b");
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
            request_dhcp: false,
            ..Default::default()
        };

        let fake_iptables = fake_root.join("iptables");
        fs::write(&fake_iptables, "#!/bin/sh\nexit 0\n").unwrap();
        make_executable(&fake_iptables);
        let fake_dnsmasq = fake_root.join("dnsmasq");
        fs::write(&fake_dnsmasq, "#!/bin/sh\nexit 0\n").unwrap();
        make_executable(&fake_dnsmasq);
        let fake_dhclient = fake_root.join("dhclient");
        fs::write(&fake_dhclient, "#!/bin/sh\nexit 0\n").unwrap();
        make_executable(&fake_dhclient);
        let fake_resolv = fake_root.join("resolv.conf");
        fs::write(&fake_resolv, "nameserver 1.1.1.1\n").unwrap();

        let (svc, mut rx) = BluetoothDiscovery::new_with_system_paths(
            config,
            9100,
            Vec::new(),
            fake_root.join("sysfs"),
            fake_ip,
            fake_bluetoothctl,
            fake_bt_network,
            fake_iptables,
            fake_dnsmasq,
            fake_dhclient,
            fake_resolv,
            None::<String>,
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
        let _ = fs::remove_dir_all(fake_root);
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
            request_dhcp: false,
            ..Default::default()
        };

        let fake_iptables = fake_root.join("iptables");
        fs::write(&fake_iptables, "#!/bin/sh\nexit 0\n").unwrap();
        make_executable(&fake_iptables);
        let fake_dnsmasq = fake_root.join("dnsmasq");
        fs::write(&fake_dnsmasq, "#!/bin/sh\nexit 0\n").unwrap();
        make_executable(&fake_dnsmasq);
        let fake_dhclient = fake_root.join("dhclient");
        fs::write(&fake_dhclient, "#!/bin/sh\nexit 0\n").unwrap();
        make_executable(&fake_dhclient);
        let fake_resolv = fake_root.join("resolv.conf");
        fs::write(&fake_resolv, "nameserver 1.1.1.1\n").unwrap();

        let (svc, mut rx) = BluetoothDiscovery::new_with_system_paths(
            config,
            9100,
            Vec::new(),
            fake_root.join("sysfs"),
            fake_ip,
            fake_bluetoothctl,
            fake_bt_network,
            fake_iptables,
            fake_dnsmasq,
            fake_dhclient,
            fake_resolv,
            None::<String>,
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
        let _ = fs::remove_dir_all(fake_root);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn run_discovers_neighbors_across_multiple_pan_interfaces() {
        let fake_root = unique_test_dir("pim-bt-fake-root-multi-pan");
        fs::create_dir_all(fake_root.join("sysfs/bnep0")).unwrap();
        fs::create_dir_all(fake_root.join("sysfs/enx6432a8144f4b")).unwrap();
        fs::write(fake_root.join("sysfs/bnep0/operstate"), "up\n").unwrap();
        fs::write(fake_root.join("sysfs/enx6432a8144f4b/operstate"), "up\n").unwrap();

        let fake_bluetoothctl = fake_root.join("bluetoothctl");
        fs::write(&fake_bluetoothctl, "#!/bin/sh\nexit 0\n").unwrap();

        let fake_bt_network = fake_root.join("bt-network");
        fs::write(&fake_bt_network, "#!/bin/sh\nexit 0\n").unwrap();

        let fake_ip = fake_root.join("ip");
        fs::write(
            &fake_ip,
            "#!/bin/sh\nif [ \"$1\" = \"neigh\" ] && [ \"$2\" = \"show\" ] && [ \"$3\" = \"dev\" ] && [ \"$4\" = \"bnep0\" ]; then\n  printf '192.168.44.2 dev bnep0 lladdr 02:00:00:00:00:02 REACHABLE\\n'\n  exit 0\nfi\nif [ \"$1\" = \"neigh\" ] && [ \"$2\" = \"show\" ] && [ \"$3\" = \"dev\" ] && [ \"$4\" = \"enx6432a8144f4b\" ]; then\n  printf '192.168.44.9 dev enx6432a8144f4b lladdr 02:00:00:00:00:09 REACHABLE\\n'\n  exit 0\nfi\nexit 0\n",
        )
        .unwrap();
        make_executable(&fake_bluetoothctl);
        make_executable(&fake_bt_network);
        make_executable(&fake_ip);

        let config = BluetoothConfig {
            interface: "auto".into(),
            radio_discovery_enabled: false,
            local_alias: "PIM-self".into(),
            poll_interval_ms: 10,
            peer_discovery_interval_ms: 10,
            startup_timeout_ms: 500,
            request_dhcp: false,
            ..Default::default()
        };

        let fake_iptables = fake_root.join("iptables");
        fs::write(&fake_iptables, "#!/bin/sh\nexit 0\n").unwrap();
        make_executable(&fake_iptables);
        let fake_dnsmasq = fake_root.join("dnsmasq");
        fs::write(&fake_dnsmasq, "#!/bin/sh\nexit 0\n").unwrap();
        make_executable(&fake_dnsmasq);
        let fake_dhclient = fake_root.join("dhclient");
        fs::write(&fake_dhclient, "#!/bin/sh\nexit 0\n").unwrap();
        make_executable(&fake_dhclient);
        let fake_resolv = fake_root.join("resolv.conf");
        fs::write(&fake_resolv, "nameserver 1.1.1.1\n").unwrap();

        let (svc, mut rx) = BluetoothDiscovery::new_with_system_paths(
            config,
            9100,
            Vec::new(),
            fake_root.join("sysfs"),
            fake_ip,
            fake_bluetoothctl,
            fake_bt_network,
            fake_iptables,
            fake_dnsmasq,
            fake_dhclient,
            fake_resolv,
            None::<String>,
        )
        .unwrap();
        let cancel = CancellationToken::new();

        let runner = tokio::spawn({
            let cancel = cancel.clone();
            async move { svc.run(cancel).await }
        });

        let addr_a = tokio::time::timeout(Duration::from_millis(300), rx.recv())
            .await
            .expect("timed out waiting for first multi-interface Bluetooth address")
            .expect("channel closed before first address emitted");
        let addr_b = tokio::time::timeout(Duration::from_millis(300), rx.recv())
            .await
            .expect("timed out waiting for second multi-interface Bluetooth address")
            .expect("channel closed before second address emitted");

        let received: HashSet<SocketAddr> = [addr_a, addr_b].into_iter().collect();
        assert!(received.contains(&"192.168.44.2:9100".parse().unwrap()));
        assert!(received.contains(&"192.168.44.9:9100".parse().unwrap()));

        cancel.cancel();
        runner.await.unwrap().unwrap();
        fs::remove_dir_all(fake_root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn start_nap_server_auto_creates_bridge_and_invokes_bt_network_with_bridge() {
        let fake_root = unique_test_dir("pim-bt-fake-root-nap-auto");
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
        let fake_ip_log = fake_root.join("ip-invocations");
        fs::write(
            &fake_ip,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {log}\nexit 0\n",
                log = fake_ip_log.display(),
            ),
        )
        .unwrap();
        let fake_iptables = fake_root.join("iptables");
        fs::write(&fake_iptables, "#!/bin/sh\nexit 0\n").unwrap();
        let fake_dnsmasq = fake_root.join("dnsmasq");
        fs::write(&fake_dnsmasq, "#!/bin/sh\nexit 0\n").unwrap();
        let fake_dhclient = fake_root.join("dhclient");
        fs::write(&fake_dhclient, "#!/bin/sh\nexit 0\n").unwrap();
        let fake_resolv = fake_root.join("resolv.conf");
        fs::write(&fake_resolv, "nameserver 1.1.1.1\n").unwrap();
        make_executable(&fake_bt_network);
        make_executable(&fake_bluetoothctl);
        make_executable(&fake_ip);
        make_executable(&fake_iptables);
        make_executable(&fake_dnsmasq);
        make_executable(&fake_dhclient);

        let config = BluetoothConfig {
            serve_nap: true,
            connect_pan: false,
            nap_bridge: "br-bt".into(),
            nap_bridge_addr: "192.168.44.1/24".into(),
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
            fake_iptables,
            fake_dnsmasq,
            fake_dhclient,
            fake_resolv,
            None::<String>,
        )
        .unwrap();

        let mut child = svc.start_nap_server().await.unwrap();
        child.wait().await.unwrap();

        let args = fs::read_to_string(fake_args).unwrap();
        assert_eq!(args, "-s\nnap\nbr-bt\n");

        let ip_log = fs::read_to_string(&fake_ip_log).unwrap();
        assert!(
            ip_log.contains("link add name br-bt type bridge"),
            "expected bridge creation in ip log: {ip_log}"
        );
        assert!(
            ip_log.contains("link set br-bt up"),
            "expected bridge up in ip log: {ip_log}"
        );
        assert!(
            ip_log.contains("addr add 192.168.44.1/24 dev br-bt"),
            "expected address assignment in ip log: {ip_log}"
        );

        let _ = fs::remove_dir_all(fake_root);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn start_nap_server_rejects_empty_bridge() {
        let fake_root = unique_test_dir("pim-bt-fake-root-nap-empty");
        fs::create_dir_all(fake_root.join("sysfs")).unwrap();

        let fake_bt_network = fake_root.join("bt-network");
        fs::write(&fake_bt_network, "#!/bin/sh\nexit 0\n").unwrap();
        let fake_bluetoothctl = fake_root.join("bluetoothctl");
        fs::write(&fake_bluetoothctl, "#!/bin/sh\nexit 0\n").unwrap();
        let fake_ip = fake_root.join("ip");
        fs::write(&fake_ip, "#!/bin/sh\nexit 0\n").unwrap();
        let fake_iptables = fake_root.join("iptables");
        fs::write(&fake_iptables, "#!/bin/sh\nexit 0\n").unwrap();
        let fake_dnsmasq = fake_root.join("dnsmasq");
        fs::write(&fake_dnsmasq, "#!/bin/sh\nexit 0\n").unwrap();
        let fake_dhclient = fake_root.join("dhclient");
        fs::write(&fake_dhclient, "#!/bin/sh\nexit 0\n").unwrap();
        let fake_resolv = fake_root.join("resolv.conf");
        fs::write(&fake_resolv, "nameserver 1.1.1.1\n").unwrap();
        make_executable(&fake_bt_network);
        make_executable(&fake_bluetoothctl);
        make_executable(&fake_ip);
        make_executable(&fake_iptables);
        make_executable(&fake_dnsmasq);
        make_executable(&fake_dhclient);

        let config = BluetoothConfig {
            serve_nap: true,
            connect_pan: false,
            nap_bridge: "".into(),
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
            fake_iptables,
            fake_dnsmasq,
            fake_dhclient,
            fake_resolv,
            None::<String>,
        )
        .unwrap();

        let err = svc.start_nap_server().await.unwrap_err();
        match err {
            BluetoothError::CommandFailed { command, message } => {
                assert_eq!(command, "bt-network");
                assert!(message.contains("non-empty nap_bridge"), "got: {message}");
            }
            other => panic!("expected CommandFailed, got {other:?}"),
        }

        let _ = fs::remove_dir_all(fake_root);
    }

    #[test]
    fn parse_ipv4_cidr_accepts_common_cases() {
        let (ip, prefix) = parse_ipv4_cidr("192.168.44.1/24").unwrap();
        assert_eq!(ip, std::net::Ipv4Addr::new(192, 168, 44, 1));
        assert_eq!(prefix, 24);
        assert!(parse_ipv4_cidr("not a cidr").is_err());
        assert!(parse_ipv4_cidr("192.168.44.1/33").is_err());
    }

    #[test]
    fn default_dhcp_range_keeps_gateway_out_of_pool() {
        let range = default_dhcp_range(std::net::Ipv4Addr::new(192, 168, 44, 1), 24).unwrap();
        assert_eq!(range, "192.168.44.10,192.168.44.245");
    }

    #[test]
    fn missing_device_ip_error_is_classified_as_transient() {
        let err = BluetoothError::CommandFailed {
            command: "ip",
            message: "Cannot find device \"enx6432a8144f4b\"".into(),
        };
        assert!(err.is_missing_device_error());

        let other = BluetoothError::CommandFailed {
            command: "ip",
            message: "RTNETLINK answers: File exists".into(),
        };
        assert!(!other.is_missing_device_error());
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
