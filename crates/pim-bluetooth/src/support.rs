#![allow(dead_code)]

use super::*;

#[cfg(any(test, target_os = "linux"))]
use std::path::{Path, PathBuf};

#[cfg(any(test, target_os = "linux"))]
pub(super) fn interface_operstate_path(sysfs_root: &Path, interface: &str) -> PathBuf {
    sysfs_root.join(interface).join("operstate")
}

#[cfg(target_os = "linux")]
pub(super) async fn read_operstate_if_present(
    path: &Path,
) -> Result<Option<String>, std::io::Error> {
    match tokio::fs::read_to_string(path).await {
        Ok(state) => Ok(Some(state)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

#[cfg(any(test, target_os = "linux"))]
pub(super) fn is_ready_operstate(state: &str) -> bool {
    matches!(state.trim(), "up" | "unknown")
}

#[cfg(any(test, target_os = "linux"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PanInterfaceCandidate {
    pub(super) name: String,
    pub(super) operstate: Option<String>,
}

#[cfg(target_os = "linux")]
pub(super) async fn list_pan_candidates(
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
pub(super) fn parse_ipv4_cidr(cidr: &str) -> Result<(std::net::Ipv4Addr, u8), String> {
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
pub(super) fn subnet_network(addr: std::net::Ipv4Addr, prefix: u8) -> (std::net::Ipv4Addr, u32) {
    let octets = u32::from(addr);
    let mask = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    };
    (std::net::Ipv4Addr::from(octets & mask), mask)
}

#[cfg(any(test, target_os = "linux"))]
pub(super) fn default_dhcp_range(gateway: std::net::Ipv4Addr, prefix: u8) -> Option<String> {
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
pub(super) fn preferred_interface_hint(interface: &str) -> Option<&str> {
    let interface = interface.trim();
    (!interface.is_empty() && interface != "auto").then_some(interface)
}

#[cfg(any(test, target_os = "linux"))]
pub(super) fn select_pan_interfaces(
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
pub(super) fn candidate_is_ready(candidate: &PanInterfaceCandidate) -> bool {
    candidate
        .operstate
        .as_deref()
        .is_some_and(is_ready_operstate)
}

#[cfg(any(test, target_os = "linux"))]
pub(super) fn format_candidate_summary(candidates: &[PanInterfaceCandidate]) -> String {
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
pub(super) fn is_ready_ifconfig_output(output: &str) -> bool {
    let output = output.trim();
    !output.is_empty() && !output.contains("status: inactive")
}

#[cfg(any(test, target_os = "macos"))]
pub(super) fn resolve_macos_pan_interface_hint(interface: &str) -> &str {
    match interface.trim() {
        "" | "auto" => "bridge0",
        explicit => explicit,
    }
}

#[cfg(any(test, target_os = "linux"))]
pub(super) fn parse_neighbor_output(
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
        let addr = match ip {
            IpAddr::V6(ipv6) if ipv6.is_unicast_link_local() => SocketAddr::V6(
                std::net::SocketAddrV6::new(ipv6, listen_port, 0, ipv6_scope_id.unwrap_or(0)),
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
pub(super) fn interface_index(interface: &str) -> Option<u32> {
    let c_interface = std::ffi::CString::new(interface).ok()?;
    let index = unsafe { libc::if_nametoindex(c_interface.as_ptr()) };
    (index != 0).then_some(index)
}

#[cfg(any(test, target_os = "linux"))]
pub(super) fn parse_devices_output(
    output: &str,
    prefix: &str,
    local_alias: &str,
    local_mac: Option<&str>,
) -> Vec<DiscoveredDevice> {
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
        if !name.starts_with(prefix) {
            continue;
        }
        // MAC-based self-filter is preferred — BD addresses are unique per
        // controller, while names are stickily cached by BlueZ and can
        // collide with our local alias when a remote controller previously
        // broadcast that alias. Alias check stays as a fallback when MAC
        // discovery hasn't completed yet.
        if let Some(self_mac) = local_mac {
            if mac.eq_ignore_ascii_case(self_mac) {
                continue;
            }
        } else if name == local_alias {
            continue;
        }
        devices.push(DiscoveredDevice {
            mac: mac.to_string(),
            name: name.to_string(),
        });
    }
    devices
}

/// Extract a Bluetooth controller BD address from the first line of
/// `bluetoothctl show`, which looks like
/// `Controller 00:15:83:3D:0A:57 (public)`. Returns the MAC verbatim
/// (uppercase, colon-separated) or `None` when the output doesn't match.
#[cfg(any(test, target_os = "linux", target_os = "macos"))]
pub(super) fn parse_controller_mac(output: &str) -> Option<String> {
    for line in output.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("Controller ") else {
            continue;
        };
        let mac = rest.split_whitespace().next()?;
        if mac.split(':').count() == 6 && mac.len() == 17 {
            return Some(mac.to_string());
        }
    }
    None
}

#[cfg(any(test, target_os = "macos"))]
pub(super) fn parse_blueutil_inquiry_output(
    output: &str,
    prefix: &str,
    local_alias: &str,
    local_mac: Option<&str>,
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
        if !name.starts_with(prefix) {
            continue;
        }
        let normalised_mac = mac.trim().replace('-', ":").to_uppercase();
        if let Some(self_mac) = local_mac {
            if normalised_mac.eq_ignore_ascii_case(self_mac) {
                continue;
            }
        } else if name == local_alias {
            continue;
        }

        devices.push(DiscoveredDevice {
            mac: normalised_mac,
            name: name.to_string(),
        });
    }

    devices
}

#[cfg(any(test, target_os = "macos"))]
pub(super) fn parse_arp_output(output: &str, interface: &str, listen_port: u16) -> Vec<SocketAddr> {
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

pub(super) fn is_safe_interface_name(name: &str) -> bool {
    let name = name.trim();
    if name.is_empty() || name.starts_with('-') || name.len() > 15 {
        return false;
    }
    name.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}
