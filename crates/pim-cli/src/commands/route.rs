use anyhow::{bail, Context, Result};
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::PathBuf;
use std::process;

use crate::{process_alive, read_pid, DEFAULT_CONFIG};

pub(crate) fn cmd_status(pid_file: PathBuf, verbose: bool) -> Result<()> {
    match read_pid(&pid_file) {
        Err(_) => {
            println!("pim: stopped (no PID file at {})", pid_file.display());
            Ok(())
        }
        Ok(pid) => {
            // Check if process is alive
            if process_alive(pid) {
                println!("pim: running (pid {pid})");

                // Try to read the config and show basic info
                if let Some(config_info) = read_config_info() {
                    println!("  node:      {}", config_info.name);
                    println!("  mesh:      {}", config_info.mesh_ipv4_prefix);
                    println!(
                        "  role:      {}",
                        if config_info.is_gateway {
                            "gateway"
                        } else {
                            "client"
                        }
                    );
                    println!("  transport: :{}", config_info.listen_port);
                }

                if verbose {
                    println!();
                    match read_stats() {
                        Some(stats) => {
                            println!("Live metrics:");
                            for (k, v) in stats {
                                println!("  {:<22} {}", format!("{k}:"), v);
                            }
                        }
                        None => println!("  (live metrics not available yet)"),
                    }
                }
            } else {
                println!("pim: stopped (stale PID file — pid {pid} not found)");
                // Remove stale PID file
                std::fs::remove_file(&pid_file).ok();
            }
            Ok(())
        }
    }
}

// ── `pim config generate` ────────────────────────────────────────────────────

pub(crate) fn cmd_route_on(config_path: PathBuf) -> Result<()> {
    let route_info = load_route_info(&config_path)?;
    ensure_pim_interface_present(&route_info.iface)?;

    for cidr in split_default_cidrs() {
        replace_split_default_route(cidr, &route_info)?;
    }
    if route_info.gateway_ipv6.is_some() {
        for cidr in split_default_ipv6_cidrs() {
            replace_split_default_route_v6(cidr, &route_info)?;
        }
    }

    println!(
        "pim routes enabled via {} dev {}",
        route_info.gateway_ip, route_info.iface
    );
    Ok(())
}

pub(crate) fn cmd_route_off(config_path: PathBuf) -> Result<()> {
    let route_info = load_route_info(&config_path)?;
    let mut removed = 0usize;

    for cidr in split_default_cidrs() {
        if remove_split_default_route(cidr, &route_info)? {
            removed += 1;
        }
    }
    if route_info.gateway_ipv6.is_some() {
        for cidr in split_default_ipv6_cidrs() {
            if remove_split_default_route_v6(cidr, &route_info)? {
                removed += 1;
            }
        }
    }

    if removed == 0 {
        println!("pim routes already disabled");
    } else {
        println!("pim routes disabled");
    }
    Ok(())
}

pub(crate) fn cmd_route_status(config_path: PathBuf) -> Result<()> {
    let route_info = load_route_info(&config_path)?;
    let active_v4 = split_default_cidrs()
        .iter()
        .all(|cidr| split_default_route_present(cidr, &route_info).unwrap_or(false));
    let active_v6 = route_info
        .gateway_ipv6
        .as_ref()
        .map(|_| {
            split_default_ipv6_cidrs()
                .iter()
                .all(|cidr| split_default_route_present_v6(cidr, &route_info).unwrap_or(false))
        })
        .unwrap_or(true);

    if active_v4 && active_v6 {
        println!(
            "pim routes: enabled via {} dev {}",
            route_info.gateway_ip, route_info.iface
        );
    } else {
        println!(
            "pim routes: disabled (expected via {} dev {})",
            route_info.gateway_ip, route_info.iface
        );
    }

    Ok(())
}

// ── `pim debug` ──────────────────────────────────────────────────────────────

pub(crate) struct ConfigInfo {
    name: String,
    mesh_ipv4_prefix: String,
    is_gateway: bool,
    listen_port: u16,
}

pub(crate) struct RouteInfo {
    iface: String,
    gateway_ip: Ipv4Addr,
    gateway_ipv6: Option<Ipv6Addr>,
}

const STATS_PATH: &str = "/run/pim.stats";

/// Parse a key=value stats string into a list of (key, value) pairs.
pub(crate) fn parse_stats_str(s: &str) -> Vec<(String, String)> {
    s.lines()
        .filter_map(|line| {
            let (k, v) = line.split_once('=')?;
            Some((k.to_string(), v.to_string()))
        })
        .collect()
}

pub(crate) fn read_stats() -> Option<Vec<(String, String)>> {
    let content = std::fs::read_to_string(STATS_PATH).ok()?;
    let pairs = parse_stats_str(&content);
    if pairs.is_empty() {
        None
    } else {
        Some(pairs)
    }
}

pub(crate) fn read_config_info() -> Option<ConfigInfo> {
    let path = DEFAULT_CONFIG;
    let content = std::fs::read_to_string(path).ok()?;
    let config: pim_core::Config = toml::from_str(&content).ok()?;
    let mesh_ipv4_prefix = config
        .interface
        .mesh_ipv4_prefix
        .clone()
        .unwrap_or_else(|| pim_core::DEFAULT_MESH_IPV4_PREFIX.to_string());
    Some(ConfigInfo {
        name: config.node.name,
        mesh_ipv4_prefix,
        is_gateway: config.gateway.enabled,
        listen_port: config.transport.listen_port,
    })
}

pub(crate) fn load_route_info(config_path: &PathBuf) -> Result<RouteInfo> {
    let content = std::fs::read_to_string(config_path)
        .with_context(|| format!("cannot read config file: {}", config_path.display()))?;
    let config: pim_core::Config = toml::from_str(&content)
        .with_context(|| format!("invalid TOML in {}", config_path.display()))?;
    let iface = config.interface.name;
    // Mesh addresses are derived from each NodeId now, so the
    // gateway IP isn't a static `.1` of the prefix anymore. The CLI
    // can only honestly install a route via the live pim0 interface
    // — when the daemon is running, its route_installer (which
    // queries `RoutingTable::nearest_gateway_mesh_ip`) is the
    // authoritative path.
    let gateway_ip = active_gateway_ip(&iface).with_context(|| {
        format!(
            "cannot determine gateway IP for {iface}; start pim first so the \
                 daemon's route installer can elect a gateway"
        )
    })?;

    Ok(RouteInfo {
        iface,
        gateway_ip,
        gateway_ipv6: None,
    })
}

pub(crate) fn ensure_pim_interface_present(iface: &str) -> Result<()> {
    let status = interface_present_command(iface)
        .status()
        .with_context(|| format!("failed to inspect interface {iface}"))?;
    if !status.success() {
        bail!("interface {iface} is not present; start pim first");
    }
    Ok(())
}

pub(crate) fn active_gateway_ip(iface: &str) -> Option<Ipv4Addr> {
    let output = interface_ipv4_command(iface).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    parse_first_ipv4_cidr(&stdout).and_then(gateway_ip_from_cidr)
}

pub(crate) fn parse_first_ipv4_cidr(s: &str) -> Option<&str> {
    s.split_whitespace()
        .find(|token| token.contains('.'))
        .map(|token| token.trim_matches(|c: char| c == '"' || c == ','))
}

pub(crate) fn gateway_ip_from_cidr(cidr: &str) -> Option<Ipv4Addr> {
    let (ip, prefix_len) = cidr.split_once('/')?;
    let ip: Ipv4Addr = ip.parse().ok()?;
    let prefix_len: u8 = prefix_len.parse().ok()?;
    first_host_in_subnet(ip, prefix_len)
}

pub(crate) fn first_host_in_subnet(ip: Ipv4Addr, prefix_len: u8) -> Option<Ipv4Addr> {
    if prefix_len > 32 {
        return None;
    }
    let ip_u32 = u32::from(ip);
    let mask = if prefix_len == 0 {
        0
    } else if prefix_len >= 32 {
        u32::MAX
    } else {
        !((1u32 << (32 - prefix_len)) - 1)
    };
    let network = ip_u32 & mask;
    Some(Ipv4Addr::from(network.saturating_add(1)))
}

pub(crate) fn split_default_cidrs() -> [&'static str; 2] {
    ["0.0.0.0/1", "128.0.0.0/1"]
}

pub(crate) fn split_default_ipv6_cidrs() -> [&'static str; 2] {
    ["::/1", "8000::/1"]
}

#[cfg(target_os = "linux")]
pub(crate) fn replace_split_default_route(cidr: &str, route_info: &RouteInfo) -> Result<()> {
    let status = process::Command::new("ip")
        .args([
            "route",
            "replace",
            cidr,
            "via",
            &route_info.gateway_ip.to_string(),
            "dev",
            &route_info.iface,
            "onlink",
        ])
        .status()
        .with_context(|| format!("failed to run ip route replace for {cidr}"))?;
    if !status.success() {
        bail!(
            "ip route replace {cidr} via {} dev {} onlink failed",
            route_info.gateway_ip,
            route_info.iface
        );
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub(crate) fn replace_split_default_route(cidr: &str, route_info: &RouteInfo) -> Result<()> {
    let _ = process::Command::new("route")
        .args([
            "-n",
            "delete",
            "-net",
            cidr,
            "-interface",
            &route_info.iface,
        ])
        .status();
    let status = process::Command::new("route")
        .args(["-n", "add", "-net", cidr, "-interface", &route_info.iface])
        .status()
        .with_context(|| format!("failed to run route add for {cidr}"))?;
    if !status.success() {
        bail!(
            "route add -net {cidr} -interface {} failed",
            route_info.iface
        );
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn replace_split_default_route(_cidr: &str, _route_info: &RouteInfo) -> Result<()> {
    bail!("split-default route management is not supported on this platform")
}

#[cfg(target_os = "linux")]
pub(crate) fn replace_split_default_route_v6(cidr: &str, route_info: &RouteInfo) -> Result<()> {
    let _gateway_ipv6 = route_info
        .gateway_ipv6
        .with_context(|| format!("cannot determine IPv6 gateway for {}", route_info.iface))?;
    let status = process::Command::new("ip")
        .args(["-6", "route", "replace", cidr, "dev", &route_info.iface])
        .status()
        .with_context(|| format!("failed to run ip -6 route replace for {cidr}"))?;
    if !status.success() {
        bail!("ip -6 route replace {cidr} dev {} failed", route_info.iface);
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub(crate) fn replace_split_default_route_v6(cidr: &str, route_info: &RouteInfo) -> Result<()> {
    let _gateway_ipv6 = route_info
        .gateway_ipv6
        .with_context(|| format!("cannot determine IPv6 gateway for {}", route_info.iface))?;
    let _ = process::Command::new("route")
        .args([
            "-n",
            "delete",
            "-inet6",
            cidr,
            "-interface",
            &route_info.iface,
        ])
        .status();
    let status = process::Command::new("route")
        .args(["-n", "add", "-inet6", cidr, "-interface", &route_info.iface])
        .status()
        .with_context(|| format!("failed to run route add -inet6 for {cidr}"))?;
    if !status.success() {
        bail!("route add -inet6 {cidr} failed");
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn replace_split_default_route_v6(_cidr: &str, _route_info: &RouteInfo) -> Result<()> {
    bail!("split-default IPv6 route management is not supported on this platform")
}

#[cfg(target_os = "linux")]
pub(crate) fn remove_split_default_route(cidr: &str, route_info: &RouteInfo) -> Result<bool> {
    let status = process::Command::new("ip")
        .args([
            "route",
            "del",
            cidr,
            "via",
            &route_info.gateway_ip.to_string(),
            "dev",
            &route_info.iface,
        ])
        .status()
        .with_context(|| format!("failed to run ip route del for {cidr}"))?;
    Ok(status.success())
}

#[cfg(target_os = "macos")]
pub(crate) fn remove_split_default_route(cidr: &str, route_info: &RouteInfo) -> Result<bool> {
    let status = process::Command::new("route")
        .args([
            "-n",
            "delete",
            "-net",
            cidr,
            "-interface",
            &route_info.iface,
        ])
        .status()
        .with_context(|| format!("failed to run route delete for {cidr}"))?;
    Ok(status.success())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn remove_split_default_route(_cidr: &str, _route_info: &RouteInfo) -> Result<bool> {
    bail!("split-default route management is not supported on this platform")
}

#[cfg(target_os = "linux")]
pub(crate) fn remove_split_default_route_v6(cidr: &str, route_info: &RouteInfo) -> Result<bool> {
    let Some(_gateway_ipv6) = route_info.gateway_ipv6 else {
        return Ok(false);
    };
    let status = process::Command::new("ip")
        .args(["-6", "route", "del", cidr, "dev", &route_info.iface])
        .status()
        .with_context(|| format!("failed to run ip -6 route del for {cidr}"))?;
    Ok(status.success())
}

#[cfg(target_os = "macos")]
pub(crate) fn remove_split_default_route_v6(cidr: &str, route_info: &RouteInfo) -> Result<bool> {
    let status = process::Command::new("route")
        .args([
            "-n",
            "delete",
            "-inet6",
            cidr,
            "-interface",
            &route_info.iface,
        ])
        .status()
        .with_context(|| format!("failed to run route delete -inet6 for {cidr}"))?;
    Ok(status.success())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn remove_split_default_route_v6(_cidr: &str, _route_info: &RouteInfo) -> Result<bool> {
    bail!("split-default IPv6 route management is not supported on this platform")
}

#[cfg(target_os = "linux")]
pub(crate) fn split_default_route_present(cidr: &str, route_info: &RouteInfo) -> Result<bool> {
    let routes = read_ip_route_table()?;
    Ok(route_present_linux(
        &routes,
        cidr,
        route_info.gateway_ip,
        &route_info.iface,
    ))
}

#[cfg(target_os = "macos")]
pub(crate) fn split_default_route_present(cidr: &str, route_info: &RouteInfo) -> Result<bool> {
    let probe = if cidr == "0.0.0.0/1" {
        "1.1.1.1"
    } else {
        "129.0.0.1"
    };
    let output = process::Command::new("route")
        .args(["-n", "get", probe])
        .output()
        .with_context(|| format!("failed to run route get for {probe}"))?;
    if !output.status.success() {
        return Ok(false);
    }
    let stdout = String::from_utf8(output.stdout).context("invalid UTF-8 from route get output")?;
    Ok(stdout
        .lines()
        .filter_map(|line| line.split_once(':'))
        .any(|(key, value)| key.trim() == "interface" && value.trim() == route_info.iface))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn split_default_route_present(_cidr: &str, _route_info: &RouteInfo) -> Result<bool> {
    bail!("split-default route management is not supported on this platform")
}

#[cfg(target_os = "linux")]
pub(crate) fn split_default_route_present_v6(cidr: &str, route_info: &RouteInfo) -> Result<bool> {
    let Some(_gateway_ipv6) = route_info.gateway_ipv6 else {
        return Ok(false);
    };
    let routes = read_ip_route_table_v6()?;
    let expected = format!("{cidr} dev {}", route_info.iface);
    Ok(routes.lines().any(|line| line.contains(&expected)))
}

#[cfg(target_os = "macos")]
pub(crate) fn split_default_route_present_v6(cidr: &str, route_info: &RouteInfo) -> Result<bool> {
    let probe = if cidr == "::/1" {
        "2001:4860:4860::8888"
    } else {
        "8000::1"
    };
    let output = process::Command::new("route")
        .args(["-n", "get", "-inet6", probe])
        .output()
        .with_context(|| format!("failed to run route get -inet6 for {probe}"))?;
    if !output.status.success() {
        return Ok(false);
    }
    let stdout = String::from_utf8(output.stdout).context("invalid UTF-8 from route get output")?;
    Ok(stdout
        .lines()
        .filter_map(|line| line.split_once(':'))
        .any(|(key, value)| key.trim() == "interface" && value.trim() == route_info.iface))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn split_default_route_present_v6(_cidr: &str, _route_info: &RouteInfo) -> Result<bool> {
    bail!("split-default IPv6 route management is not supported on this platform")
}

#[cfg(target_os = "linux")]
pub(crate) fn read_ip_route_table() -> Result<String> {
    let output = process::Command::new("ip")
        .args(["route", "show"])
        .output()
        .context("failed to run ip route show")?;
    if !output.status.success() {
        bail!(
            "ip route show exited with status {:?}",
            output.status.code()
        );
    }
    String::from_utf8(output.stdout).context("invalid UTF-8 from ip route show")
}

#[cfg(target_os = "linux")]
pub(crate) fn read_ip_route_table_v6() -> Result<String> {
    let output = process::Command::new("ip")
        .args(["-6", "route", "show"])
        .output()
        .context("failed to run ip -6 route show")?;
    if !output.status.success() {
        bail!(
            "ip -6 route show exited with status {:?}",
            output.status.code()
        );
    }
    String::from_utf8(output.stdout).context("invalid UTF-8 from ip -6 route show")
}

#[cfg(target_os = "linux")]
pub(crate) fn route_present_linux(
    routes: &str,
    cidr: &str,
    gateway_ip: Ipv4Addr,
    iface: &str,
) -> bool {
    let expected = format!("{cidr} via {gateway_ip} dev {iface}");
    routes.lines().any(|line| line.contains(&expected))
}

#[cfg(target_os = "linux")]
pub(crate) fn interface_present_command(iface: &str) -> process::Command {
    let mut cmd = process::Command::new("ip");
    cmd.args(["link", "show", "dev", iface]);
    cmd
}

#[cfg(target_os = "macos")]
pub(crate) fn interface_present_command(iface: &str) -> process::Command {
    let mut cmd = process::Command::new("ifconfig");
    cmd.arg(iface);
    cmd
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn interface_present_command(_iface: &str) -> process::Command {
    process::Command::new("false")
}

#[cfg(target_os = "linux")]
pub(crate) fn interface_ipv4_command(iface: &str) -> process::Command {
    let mut cmd = process::Command::new("ip");
    cmd.args(["-4", "-o", "addr", "show", "dev", iface]);
    cmd
}

#[cfg(target_os = "macos")]
pub(crate) fn interface_ipv4_command(iface: &str) -> process::Command {
    let mut cmd = process::Command::new("ifconfig");
    cmd.arg(iface);
    cmd
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn interface_ipv4_command(_iface: &str) -> process::Command {
    process::Command::new("false")
}
