//! Thin async wrapper around `wpa_cli` for controlling wpa_supplicant's P2P subsystem.

use crate::WifiDirectError;
use std::net::Ipv4Addr;
use tokio::process::Command;

/// Information about a discovered P2P peer.
#[derive(Debug, Clone, PartialEq)]
pub struct P2pPeerInfo {
    /// P2P device MAC address.
    pub mac: String,
    /// Human-readable device name advertised by the peer.
    pub device_name: String,
    /// Primary device type string (e.g. `"1-0050F204-1"`).
    pub pri_dev_type: String,
    /// Supported WPS configuration methods bitmask.
    pub config_methods: u16,
}

/// Async controller that drives `wpa_cli` subcommands for P2P operations.
///
/// Each method spawns a short-lived `wpa_cli -i <iface> <cmd>` subprocess and
/// returns after it exits.  No persistent connection to wpa_supplicant is held.
pub struct WpaCliController {
    /// The Wi-Fi interface managed by wpa_supplicant (e.g. `wlan0`).
    pub interface: String,
    /// Path to the `wpa_cli` binary. Defaults to `"wpa_cli"` (searched via `$PATH`).
    pub wpa_cli_bin: String,
}

impl WpaCliController {
    /// Create a new controller for the given interface.
    pub fn new(interface: impl Into<String>) -> Self {
        Self {
            interface: interface.into(),
            wpa_cli_bin: "wpa_cli".into(),
        }
    }

    /// Override the path to the `wpa_cli` binary (useful in tests).
    pub fn with_bin(mut self, bin: impl Into<String>) -> Self {
        self.wpa_cli_bin = bin.into();
        self
    }

    async fn run_cmd(&self, args: &[&str]) -> Result<String, WifiDirectError> {
        let output = Command::new(&self.wpa_cli_bin)
            .arg("-i")
            .arg(&self.interface)
            .args(args)
            .output()
            .await
            .map_err(|e| WifiDirectError::WpaCli(format!("spawn failed: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            return Err(WifiDirectError::WpaCli(format!(
                "wpa_cli {:?} failed: {}",
                args, stderr
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Start P2P peer discovery.
    pub async fn p2p_find(&self) -> Result<(), WifiDirectError> {
        let out = self.run_cmd(&["p2p_find"]).await?;
        if out.trim() == "OK" || out.contains("OK") {
            Ok(())
        } else {
            Err(WifiDirectError::WpaCli(format!("p2p_find unexpected: {out}")))
        }
    }

    /// Stop P2P peer discovery.
    pub async fn p2p_stop_find(&self) -> Result<(), WifiDirectError> {
        let out = self.run_cmd(&["p2p_stop_find"]).await?;
        if out.trim() == "OK" || out.contains("OK") {
            Ok(())
        } else {
            Err(WifiDirectError::WpaCli(format!("p2p_stop_find unexpected: {out}")))
        }
    }

    /// List MAC addresses of currently discovered P2P peers.
    ///
    /// Returns an empty vec when no peers are known.  Output format: one MAC per line.
    pub async fn p2p_peers(&self) -> Result<Vec<String>, WifiDirectError> {
        let out = self.run_cmd(&["p2p_peers"]).await?;
        Ok(parse_p2p_peers(&out))
    }

    /// Fetch detailed information about a discovered peer.
    pub async fn p2p_peer_info(&self, mac: &str) -> Result<P2pPeerInfo, WifiDirectError> {
        let out = self.run_cmd(&["p2p_peer", mac]).await?;
        parse_p2p_peer_info(mac, &out)
    }

    /// Initiate a PBC (push-button) connection to the peer at `mac`.
    pub async fn p2p_connect_pbc(&self, mac: &str) -> Result<(), WifiDirectError> {
        let out = self.run_cmd(&["p2p_connect", mac, "pbc"]).await?;
        if out.trim() == "OK" || out.contains("OK") {
            Ok(())
        } else {
            Err(WifiDirectError::WpaCli(format!("p2p_connect pbc unexpected: {out}")))
        }
    }

    /// Initiate a PIN-based connection to the peer at `mac`.
    pub async fn p2p_connect_pin(&self, mac: &str, pin: &str) -> Result<(), WifiDirectError> {
        let out = self.run_cmd(&["p2p_connect", mac, pin]).await?;
        if out.trim() == "OK" || out.contains("OK") {
            Ok(())
        } else {
            Err(WifiDirectError::WpaCli(format!("p2p_connect pin unexpected: {out}")))
        }
    }

    /// Create an autonomous Group Owner group (this node becomes GO immediately).
    pub async fn p2p_group_add(&self) -> Result<(), WifiDirectError> {
        let out = self.run_cmd(&["p2p_group_add"]).await?;
        if out.trim() == "OK" || out.contains("OK") {
            Ok(())
        } else {
            Err(WifiDirectError::WpaCli(format!("p2p_group_add unexpected: {out}")))
        }
    }

    /// Remove the P2P group on `group_iface` (e.g. `p2p-wlan0-0`).
    pub async fn p2p_group_remove(&self, group_iface: &str) -> Result<(), WifiDirectError> {
        let out = self.run_cmd(&["p2p_group_remove", group_iface]).await?;
        if out.trim() == "OK" || out.contains("OK") {
            Ok(())
        } else {
            Err(WifiDirectError::WpaCli(format!(
                "p2p_group_remove unexpected: {out}"
            )))
        }
    }

    /// Return names of all interfaces currently managed by wpa_supplicant.
    ///
    /// The output is parsed from `interface` command output which lists one
    /// interface name per line.  Caller can filter for `p2p-*` names to detect
    /// a newly-formed P2P group interface.
    pub async fn list_interfaces(&self) -> Result<Vec<String>, WifiDirectError> {
        let out = self.run_cmd(&["interface"]).await?;
        Ok(parse_interface_list(&out))
    }

    /// Read the IPv4 address assigned to `iface` by parsing `ip addr show <iface>`.
    ///
    /// Returns the first `inet` address found, or `None` if the interface has no
    /// IPv4 address yet.
    pub async fn iface_ipv4(&self, iface: &str) -> Result<Option<Ipv4Addr>, WifiDirectError> {
        let output = Command::new("ip")
            .args(["addr", "show", iface])
            .output()
            .await
            .map_err(|e| WifiDirectError::WpaCli(format!("ip addr show failed: {e}")))?;
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        Ok(parse_inet_addr(&text))
    }

    /// Read the ARP table from `/proc/net/arp` and return IPv4 addresses visible
    /// on `iface`.  Used by the Group Owner to locate the GC after DHCP assignment.
    pub async fn arp_peers_on_iface(&self, iface: &str) -> Result<Vec<Ipv4Addr>, WifiDirectError> {
        let content = tokio::fs::read_to_string("/proc/net/arp")
            .await
            .map_err(|e| WifiDirectError::WpaCli(format!("read /proc/net/arp: {e}")))?;
        Ok(parse_arp_table(&content, iface))
    }
}

// ── Pure parsing helpers (testable without any subprocess) ───────────────────

/// Parse `wpa_cli p2p_peers` output — one MAC per line, empty lines ignored.
pub(crate) fn parse_p2p_peers(output: &str) -> Vec<String> {
    output
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("Selected") && !l.starts_with("p2p_peers"))
        .map(String::from)
        .collect()
}

/// Parse `wpa_cli p2p_peer <mac>` output into a `P2pPeerInfo`.
pub(crate) fn parse_p2p_peer_info(mac: &str, output: &str) -> Result<P2pPeerInfo, WifiDirectError> {
    let mut device_name = String::new();
    let mut pri_dev_type = String::new();
    let mut config_methods: u16 = 0;

    for line in output.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("device_name=") {
            device_name = v.to_string();
        } else if let Some(v) = line.strip_prefix("pri_dev_type=") {
            pri_dev_type = v.to_string();
        } else if let Some(v) = line.strip_prefix("config_methods=") {
            config_methods = u16::from_str_radix(v.trim_start_matches("0x"), 16).unwrap_or(0);
        }
    }

    if device_name.is_empty() {
        return Err(WifiDirectError::WpaCli(format!(
            "p2p_peer {mac}: device_name not found in output"
        )));
    }

    Ok(P2pPeerInfo {
        mac: mac.to_string(),
        device_name,
        pri_dev_type,
        config_methods,
    })
}

/// Parse `wpa_cli interface` output — one interface name per line.
pub(crate) fn parse_interface_list(output: &str) -> Vec<String> {
    output
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("Available") && !l.starts_with("interface"))
        .map(String::from)
        .collect()
}

/// Extract the first `inet <addr>/prefix` address from `ip addr show` output.
pub(crate) fn parse_inet_addr(output: &str) -> Option<Ipv4Addr> {
    for line in output.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("inet ") {
            let addr_part = rest.split_whitespace().next()?;
            let addr_str = addr_part.split('/').next()?;
            if let Ok(addr) = addr_str.parse::<Ipv4Addr>() {
                return Some(addr);
            }
        }
    }
    None
}

/// Extract IPv4 addresses from `/proc/net/arp` for the given interface.
pub(crate) fn parse_arp_table(content: &str, iface: &str) -> Vec<Ipv4Addr> {
    let mut addrs = Vec::new();
    for line in content.lines().skip(1) {
        // Format: IP address  HW type  Flags  HW address  Mask  Device
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() >= 6 && fields[5] == iface {
            if let Ok(addr) = fields[0].parse::<Ipv4Addr>() {
                addrs.push(addr);
            }
        }
    }
    addrs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wpa_cli_p2p_peers_parses_empty_output() {
        let peers = parse_p2p_peers("");
        assert!(peers.is_empty());
    }

    #[test]
    fn wpa_cli_p2p_peers_parses_single_mac() {
        let out = "aa:bb:cc:dd:ee:ff\n";
        let peers = parse_p2p_peers(out);
        assert_eq!(peers, vec!["aa:bb:cc:dd:ee:ff"]);
    }

    #[test]
    fn wpa_cli_p2p_peers_parses_multiple_macs() {
        let out = "aa:bb:cc:dd:ee:ff\n11:22:33:44:55:66\n";
        let peers = parse_p2p_peers(out);
        assert_eq!(peers.len(), 2);
        assert!(peers.contains(&"aa:bb:cc:dd:ee:ff".to_string()));
        assert!(peers.contains(&"11:22:33:44:55:66".to_string()));
    }

    #[test]
    fn wpa_cli_p2p_peers_ignores_header_lines() {
        let out = "Selected interface 'wlan0'\naa:bb:cc:dd:ee:ff\n";
        let peers = parse_p2p_peers(out);
        assert_eq!(peers, vec!["aa:bb:cc:dd:ee:ff"]);
    }

    #[test]
    fn p2p_peer_info_parses_device_name() {
        let out = "aa:bb:cc:dd:ee:ff\ndevice_name=MyPhone\npri_dev_type=10-0050F204-5\nconfig_methods=0x0188\n";
        let info = parse_p2p_peer_info("aa:bb:cc:dd:ee:ff", out).unwrap();
        assert_eq!(info.device_name, "MyPhone");
        assert_eq!(info.pri_dev_type, "10-0050F204-5");
        assert_eq!(info.config_methods, 0x0188);
    }

    #[test]
    fn p2p_peer_info_returns_error_when_device_name_missing() {
        let out = "aa:bb:cc:dd:ee:ff\npri_dev_type=10-0050F204-5\n";
        assert!(parse_p2p_peer_info("aa:bb:cc:dd:ee:ff", out).is_err());
    }

    #[test]
    fn parse_inet_addr_extracts_ip() {
        let out = "2: p2p-wlan0-0: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500\n    link/ether aa:bb:cc:dd:ee:ff brd ff:ff:ff:ff:ff:ff\n    inet 192.168.49.1/24 brd 192.168.49.255 scope global p2p-wlan0-0\n";
        let addr = parse_inet_addr(out).unwrap();
        assert_eq!(addr, "192.168.49.1".parse::<Ipv4Addr>().unwrap());
    }

    #[test]
    fn parse_inet_addr_returns_none_for_no_ip() {
        let out = "2: p2p-wlan0-0: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500\n";
        assert!(parse_inet_addr(out).is_none());
    }

    #[test]
    fn parse_arp_table_extracts_peer_on_iface() {
        let content = "IP address       HW type     Flags       HW address            Mask     Device\n192.168.49.100   0x1         0x2         aa:bb:cc:dd:ee:ff     *        p2p-wlan0-0\n10.0.0.1         0x1         0x2         11:22:33:44:55:66     *        eth0\n";
        let peers = parse_arp_table(content, "p2p-wlan0-0");
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0], "192.168.49.100".parse::<Ipv4Addr>().unwrap());
    }

    #[test]
    fn parse_arp_table_returns_empty_for_wrong_iface() {
        let content = "IP address       HW type     Flags       HW address            Mask     Device\n192.168.49.100   0x1         0x2         aa:bb:cc:dd:ee:ff     *        p2p-wlan0-0\n";
        let peers = parse_arp_table(content, "eth0");
        assert!(peers.is_empty());
    }

    #[test]
    fn parse_interface_list_extracts_interfaces() {
        let out = "Available interfaces:\nwlan0\np2p-wlan0-0\n";
        let ifaces = parse_interface_list(out);
        assert!(ifaces.contains(&"wlan0".to_string()));
        assert!(ifaces.contains(&"p2p-wlan0-0".to_string()));
    }
}
