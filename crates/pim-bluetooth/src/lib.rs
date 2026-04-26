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
}

mod platform_impl;
mod service;
mod support;

#[cfg(test)]
mod tests;

use support::*;
