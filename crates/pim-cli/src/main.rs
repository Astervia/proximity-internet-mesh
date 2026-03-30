//! pim — command-line interface for the proximity internet mesh daemon.
//!
//! Commands:
//!   pim up   [--config <path>] [--daemon]   Start the daemon
//!   pim down [--pid-file <path>]            Stop the running daemon
//!   pim status [--pid-file <path>]          Show daemon state
//!   pim route <on|off|status>               Manage split-default routing via pim0
//!   pim config generate <roles...>          Generate a commented config template

use std::collections::BTreeSet;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::process;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};

const DEFAULT_CONFIG: &str = "/etc/pim/pim.toml";
const DEFAULT_PID_FILE: &str = "/run/pim.pid";
const DAEMON_BIN: &str = "pim-daemon";

// ── CLI model ─────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "pim", about = "Proximity Internet Mesh")]
#[command(version, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the PIM daemon
    Up {
        /// Path to the TOML configuration file
        #[arg(short, long, default_value = DEFAULT_CONFIG)]
        config: PathBuf,

        /// Path to the PID file written by the daemon
        #[arg(long, default_value = DEFAULT_PID_FILE)]
        pid_file: PathBuf,

        /// Run the daemon in the background (detach from terminal)
        #[arg(short, long)]
        daemon: bool,
    },

    /// Stop the running daemon
    Down {
        /// Path to the PID file
        #[arg(long, default_value = DEFAULT_PID_FILE)]
        pid_file: PathBuf,
    },

    /// Show the current daemon state
    Status {
        /// Path to the PID file
        #[arg(long, default_value = DEFAULT_PID_FILE)]
        pid_file: PathBuf,

        /// Show detailed live metrics (peer count, routes, forwarded packets, etc.)
        #[arg(short, long)]
        verbose: bool,
    },

    /// Generate commented configuration templates
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },

    /// Manage split-default routing through the PIM tunnel
    Route {
        #[command(subcommand)]
        command: RouteCommands,
    },
}

#[derive(Subcommand)]
enum ConfigCommands {
    /// Generate a readable default config for one or more roles
    Generate {
        /// Roles to enable in the generated config
        #[arg(value_enum, num_args = 1..)]
        roles: Vec<NodeRole>,

        /// Override the generated node name
        #[arg(long)]
        name: Option<String>,

        /// Write the template to a file instead of stdout
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Overwrite the output file if it already exists
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum RouteCommands {
    /// Route internet-bound traffic through `pim0`
    On {
        /// Path to the TOML configuration file
        #[arg(short, long, default_value = DEFAULT_CONFIG)]
        config: PathBuf,
    },

    /// Remove split-default routes through `pim0`
    Off {
        /// Path to the TOML configuration file
        #[arg(short, long, default_value = DEFAULT_CONFIG)]
        config: PathBuf,
    },

    /// Show whether split-default PIM routes are active
    Status {
        /// Path to the TOML configuration file
        #[arg(short, long, default_value = DEFAULT_CONFIG)]
        config: PathBuf,
    },
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, ValueEnum)]
enum NodeRole {
    Client,
    Relay,
    Gateway,
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Up {
            config,
            pid_file,
            daemon,
        } => cmd_up(config, pid_file, daemon),

        Commands::Down { pid_file } => cmd_down(pid_file),

        Commands::Status { pid_file, verbose } => cmd_status(pid_file, verbose),

        Commands::Config { command } => match command {
            ConfigCommands::Generate {
                roles,
                name,
                output,
                force,
            } => cmd_config_generate(roles, name, output, force),
        },

        Commands::Route { command } => match command {
            RouteCommands::On { config } => cmd_route_on(config),
            RouteCommands::Off { config } => cmd_route_off(config),
            RouteCommands::Status { config } => cmd_route_status(config),
        },
    }
}

// ── `pim up` ──────────────────────────────────────────────────────────────────

fn cmd_up(config: PathBuf, pid_file: PathBuf, daemonize: bool) -> Result<()> {
    // Validate config exists
    if !config.exists() {
        bail!("config file not found: {}", config.display());
    }

    // Check that the daemon binary is available
    let daemon_bin = find_daemon_binary()?;

    if daemonize {
        // Spawn pim-daemon in background, redirecting stdio
        let child = process::Command::new(&daemon_bin)
            .arg(config.to_str().unwrap_or(DEFAULT_CONFIG))
            .arg(pid_file.to_str().unwrap_or(DEFAULT_PID_FILE))
            .stdin(process::Stdio::null())
            .stdout(process::Stdio::null())
            .stderr(process::Stdio::null())
            .spawn()
            .with_context(|| format!("failed to spawn {daemon_bin:?}"))?;

        println!("pim daemon started (pid {})", child.id());
    } else {
        // Run in foreground — replace this process with pim-daemon
        // On Unix we could use exec(), but for portability we use spawn + wait
        let status = process::Command::new(&daemon_bin)
            .arg(config.to_str().unwrap_or(DEFAULT_CONFIG))
            .arg(pid_file.to_str().unwrap_or(DEFAULT_PID_FILE))
            .status()
            .with_context(|| format!("failed to run {daemon_bin:?}"))?;

        if !status.success() {
            bail!("pim-daemon exited with status {:?}", status.code());
        }
    }

    Ok(())
}

// ── `pim down` ────────────────────────────────────────────────────────────────

fn cmd_down(pid_file: PathBuf) -> Result<()> {
    let pid = read_pid(&pid_file)?;

    // Send SIGTERM
    #[cfg(unix)]
    {
        let ret = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        if ret != 0 {
            let e = std::io::Error::last_os_error();
            bail!("failed to send SIGTERM to pid {pid}: {e}");
        }
        println!("Sent SIGTERM to pim daemon (pid {pid})");
    }

    #[cfg(not(unix))]
    {
        bail!("pim down is only supported on Unix systems");
    }

    Ok(())
}

// ── `pim status` ──────────────────────────────────────────────────────────────

fn cmd_status(pid_file: PathBuf, verbose: bool) -> Result<()> {
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
                    println!("  mesh_ip:   {}", config_info.mesh_ip);
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

fn cmd_config_generate(
    roles: Vec<NodeRole>,
    name: Option<String>,
    output: Option<PathBuf>,
    force: bool,
) -> Result<()> {
    let rendered = render_config_template(&roles, name.as_deref());

    if let Some(path) = output {
        if path.exists() && !force {
            bail!(
                "refusing to overwrite existing file: {} (use --force to overwrite)",
                path.display()
            );
        }
        std::fs::write(&path, rendered)
            .with_context(|| format!("failed to write config template to {}", path.display()))?;
        println!("Wrote config template to {}", path.display());
    } else {
        print!("{rendered}");
    }

    Ok(())
}

// ── `pim route` ──────────────────────────────────────────────────────────────

fn cmd_route_on(config_path: PathBuf) -> Result<()> {
    let route_info = load_route_info(&config_path)?;
    ensure_pim_interface_present(&route_info.iface)?;

    for cidr in split_default_cidrs() {
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
    }

    println!(
        "pim routes enabled via {} dev {}",
        route_info.gateway_ip, route_info.iface
    );
    Ok(())
}

fn cmd_route_off(config_path: PathBuf) -> Result<()> {
    let route_info = load_route_info(&config_path)?;
    let mut removed = 0usize;

    for cidr in split_default_cidrs() {
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
        if status.success() {
            removed += 1;
        }
    }

    if removed == 0 {
        println!("pim routes already disabled");
    } else {
        println!("pim routes disabled");
    }
    Ok(())
}

fn cmd_route_status(config_path: PathBuf) -> Result<()> {
    let route_info = load_route_info(&config_path)?;
    let routes = read_ip_route_table()?;
    let active = split_default_cidrs()
        .iter()
        .all(|cidr| route_present(&routes, cidr, route_info.gateway_ip, &route_info.iface));

    if active {
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

// ── Helpers ───────────────────────────────────────────────────────────────────

fn read_pid(pid_file: &PathBuf) -> Result<u32> {
    let content = std::fs::read_to_string(pid_file)
        .with_context(|| format!("cannot read PID file: {}", pid_file.display()))?;
    content
        .trim()
        .parse::<u32>()
        .with_context(|| format!("invalid PID in {}", pid_file.display()))
}

fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // kill(pid, 0) returns 0 if the process exists and we can signal it
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        // On non-Unix platforms, we can't reliably check without spawning a process
        false
    }
}

/// Find the `pim-daemon` binary relative to the current executable or PATH.
fn find_daemon_binary() -> Result<PathBuf> {
    // 1. Same directory as the running `pim` binary
    if let Ok(exe) = std::env::current_exe() {
        let sibling = exe.with_file_name(DAEMON_BIN);
        if sibling.exists() {
            return Ok(sibling);
        }
    }

    // 2. Check PATH via `which`-style lookup
    for dir in std::env::var("PATH").unwrap_or_default().split(':') {
        let candidate = PathBuf::from(dir).join(DAEMON_BIN);
        if candidate.exists() {
            return Ok(candidate);
        }
    }

    bail!(
        "cannot find {DAEMON_BIN} binary — ensure it is in the same directory as `pim` or on PATH"
    )
}

/// Minimal info extracted from the config file for `pim status`.
struct ConfigInfo {
    name: String,
    mesh_ip: String,
    is_gateway: bool,
    listen_port: u16,
}

struct RouteInfo {
    iface: String,
    gateway_ip: Ipv4Addr,
}

const STATS_PATH: &str = "/run/pim.stats";

/// Parse a key=value stats string into a list of (key, value) pairs.
pub fn parse_stats_str(s: &str) -> Vec<(String, String)> {
    s.lines()
        .filter_map(|line| {
            let (k, v) = line.split_once('=')?;
            Some((k.to_string(), v.to_string()))
        })
        .collect()
}

fn read_stats() -> Option<Vec<(String, String)>> {
    let content = std::fs::read_to_string(STATS_PATH).ok()?;
    let pairs = parse_stats_str(&content);
    if pairs.is_empty() {
        None
    } else {
        Some(pairs)
    }
}

fn read_config_info() -> Option<ConfigInfo> {
    let path = DEFAULT_CONFIG;
    let content = std::fs::read_to_string(path).ok()?;
    let config: pim_core::Config = toml::from_str(&content).ok()?;
    Some(ConfigInfo {
        name: config.node.name,
        mesh_ip: config.interface.mesh_ip,
        is_gateway: config.gateway.enabled,
        listen_port: config.transport.listen_port,
    })
}

fn load_route_info(config_path: &PathBuf) -> Result<RouteInfo> {
    let content = std::fs::read_to_string(config_path)
        .with_context(|| format!("cannot read config file: {}", config_path.display()))?;
    let config: pim_core::Config = toml::from_str(&content)
        .with_context(|| format!("invalid TOML in {}", config_path.display()))?;
    let iface = config.interface.name;
    let gateway_ip = active_gateway_ip(&iface)
        .or_else(|| gateway_ip_from_config_mesh_ip(&config.interface.mesh_ip))
        .with_context(|| {
            format!(
                "cannot determine gateway IP for {}; start pim first or use a static mesh_ip",
                iface
            )
        })?;

    Ok(RouteInfo { iface, gateway_ip })
}

fn ensure_pim_interface_present(iface: &str) -> Result<()> {
    let status = process::Command::new("ip")
        .args(["link", "show", "dev", iface])
        .status()
        .with_context(|| format!("failed to inspect interface {iface}"))?;
    if !status.success() {
        bail!("interface {iface} is not present; start pim first");
    }
    Ok(())
}

fn active_gateway_ip(iface: &str) -> Option<Ipv4Addr> {
    let output = process::Command::new("ip")
        .args(["-4", "-o", "addr", "show", "dev", iface])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    parse_first_ipv4_cidr(&stdout).and_then(gateway_ip_from_cidr)
}

fn gateway_ip_from_config_mesh_ip(mesh_ip: &str) -> Option<Ipv4Addr> {
    parse_first_ipv4_cidr(mesh_ip).and_then(gateway_ip_from_cidr)
}

fn parse_first_ipv4_cidr(s: &str) -> Option<&str> {
    s.split_whitespace()
        .find(|token| token.contains('.'))
        .map(|token| token.trim_matches(|c: char| c == '"' || c == ','))
}

fn gateway_ip_from_cidr(cidr: &str) -> Option<Ipv4Addr> {
    let (ip, prefix_len) = cidr.split_once('/')?;
    let ip: Ipv4Addr = ip.parse().ok()?;
    let prefix_len: u8 = prefix_len.parse().ok()?;
    first_host_in_subnet(ip, prefix_len)
}

fn first_host_in_subnet(ip: Ipv4Addr, prefix_len: u8) -> Option<Ipv4Addr> {
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

fn split_default_cidrs() -> [&'static str; 2] {
    ["0.0.0.0/1", "128.0.0.0/1"]
}

fn read_ip_route_table() -> Result<String> {
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

fn route_present(routes: &str, cidr: &str, gateway_ip: Ipv4Addr, iface: &str) -> bool {
    let expected = format!("{cidr} via {gateway_ip} dev {iface}");
    routes.lines().any(|line| line.contains(&expected))
}

fn render_config_template(roles: &[NodeRole], override_name: Option<&str>) -> String {
    let roles = unique_roles(roles);
    let is_gateway = roles.contains(&NodeRole::Gateway);
    let is_relay = roles.contains(&NodeRole::Relay);
    let is_client = roles.contains(&NodeRole::Client);
    let node_name = override_name
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| default_node_name(&roles));
    let mesh_ip = default_mesh_ip(&roles);
    let peer_example = default_peer_example(&roles);
    let roles_label = roles
        .iter()
        .map(|role| role.as_str())
        .collect::<Vec<_>>()
        .join(", ");

    let mut out = String::new();
    push_line(&mut out, "# Proximity Internet Mesh configuration template");
    push_line(&mut out, &format!("# Roles enabled: {roles_label}"));
    push_line(
        &mut out,
        "# Edit the values below, save the file, then start the daemon with:",
    );
    push_line(&mut out, "#   sudo pim up --config /etc/pim/pim.toml");
    push_blank(&mut out);

    push_line(&mut out, "[node]");
    push_line(
        &mut out,
        "# Human-readable node name used in logs and status output.",
    );
    push_line(&mut out, &format!("name = {:?}", node_name));
    push_line(
        &mut out,
        "# Writable state directory for generated keys and runtime data.",
    );
    push_line(&mut out, "data_dir = \"/var/lib/pim\"");
    push_blank(&mut out);

    push_line(&mut out, "[interface]");
    push_line(&mut out, "# Linux TUN interface exposed by the daemon.");
    push_line(&mut out, "name = \"pim0\"");
    push_line(
        &mut out,
        "# Use a static CIDR for predictable labs or \"auto\" to request an address from a gateway.",
    );
    push_line(&mut out, &format!("mesh_ip = {:?}", mesh_ip));
    push_line(
        &mut out,
        "# Keep this aligned with the mesh MTU expected by other peers.",
    );
    push_line(&mut out, "mtu = 1400");
    push_blank(&mut out);

    push_line(&mut out, "[discovery]");
    push_line(
        &mut out,
        "# Broadcast-based peer discovery. Static peers below are still the simplest way to start.",
    );
    push_line(&mut out, "broadcast_interval_ms = 5000");
    push_line(&mut out, "peer_timeout_ms = 30000");
    push_blank(&mut out);

    push_line(&mut out, "[transport]");
    push_line(
        &mut out,
        "# Transport backend implemented in this repository today.",
    );
    push_line(&mut out, "type = \"tcp\"");
    push_line(
        &mut out,
        "# TCP port this node listens on for direct peer sessions.",
    );
    push_line(&mut out, "listen_port = 9100");
    push_blank(&mut out);

    push_line(&mut out, "[routing]");
    push_line(
        &mut out,
        "# Distance-vector settings used for route propagation and expiry.",
    );
    push_line(&mut out, "max_hops = 10");
    push_line(&mut out, "algorithm = \"distance-vector\"");
    push_line(&mut out, "route_expiry_s = 300");
    push_blank(&mut out);

    if is_gateway {
        push_line(&mut out, "[gateway]");
        push_line(
            &mut out,
            "# Enable NAT and internet egress on a node with upstream connectivity.",
        );
        push_line(&mut out, "enabled = true");
        push_line(
            &mut out,
            "# Replace this with the real internet-facing interface on the host.",
        );
        push_line(&mut out, "nat_interface = \"eth0\"");
        push_line(
            &mut out,
            "# Maximum concurrent gateway connection-tracking entries.",
        );
        push_line(&mut out, "max_connections = 200");
    } else {
        push_line(
            &mut out,
            "# [gateway]  # Uncomment this section only on a node that should provide internet access.",
        );
        push_line(&mut out, "# enabled = true");
        push_line(&mut out, "# nat_interface = \"eth0\"");
        push_line(&mut out, "# max_connections = 200");
    }
    push_blank(&mut out);

    push_line(&mut out, "[security]");
    push_line(
        &mut out,
        "# The daemon creates this Ed25519 private key on first startup if it does not exist.",
    );
    push_line(&mut out, "key_file = \"/var/lib/pim/node.key\"");
    push_line(
        &mut out,
        "# Reject direct peer sessions that do not complete the authenticated handshake.",
    );
    push_line(&mut out, "require_encryption = true");
    push_blank(&mut out);

    push_line(
        &mut out,
        "# Static peers are the easiest way to bootstrap a mesh in development and Docker labs.",
    );
    push_line(
        &mut out,
        "# Uncomment one or more entries and replace the example address with a real peer.",
    );
    if is_gateway && !is_relay && !is_client {
        push_line(
            &mut out,
            "# A standalone gateway can usually start without static peers and wait for inbound connections.",
        );
    }
    push_line(&mut out, "# [[peers]]");
    push_line(&mut out, &format!("# address = {:?}", peer_example));
    push_line(&mut out, "# label = \"replace-with-hostname-or-purpose\"");

    if is_relay {
        push_line(
            &mut out,
            "# [[peers]]  # Relays commonly connect to a second upstream or downstream neighbor.",
        );
        push_line(&mut out, "# address = \"another-peer:9100\"");
        push_line(&mut out, "# label = \"secondary-link\"");
    }

    if is_client {
        push_line(
            &mut out,
            "# Clients usually point at a nearby relay or directly at a gateway.",
        );
    }

    if is_relay {
        push_line(
            &mut out,
            "# Relays forward traffic for other nodes and typically keep at least one static upstream.",
        );
    }

    if is_gateway {
        push_line(
            &mut out,
            "# Gateways may also keep static peers if they should proactively connect into the mesh.",
        );
    }

    out
}

fn unique_roles(roles: &[NodeRole]) -> BTreeSet<NodeRole> {
    roles.iter().copied().collect()
}

impl NodeRole {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Client => "client",
            Self::Relay => "relay",
            Self::Gateway => "gateway",
        }
    }
}

fn default_node_name(roles: &BTreeSet<NodeRole>) -> String {
    let joined = roles
        .iter()
        .map(|role| role.as_str())
        .collect::<Vec<_>>()
        .join("-");
    format!("{joined}-node")
}

fn default_mesh_ip(roles: &BTreeSet<NodeRole>) -> &'static str {
    if roles.contains(&NodeRole::Gateway) {
        "10.77.0.1/24"
    } else if roles.contains(&NodeRole::Relay) {
        "10.77.0.10/24"
    } else {
        "auto"
    }
}

fn default_peer_example(roles: &BTreeSet<NodeRole>) -> &'static str {
    if roles.contains(&NodeRole::Gateway) && !roles.contains(&NodeRole::Relay) {
        "relay:9100"
    } else if roles.contains(&NodeRole::Relay) {
        "gateway:9100"
    } else {
        "relay-or-gateway:9100"
    }
}

fn push_line(out: &mut String, line: &str) {
    out.push_str(line);
    out.push('\n');
}

fn push_blank(out: &mut String) {
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_stats_str_extracts_key_value_pairs() {
        let input = "peers=3\nroutes=5\npackets_forwarded=100\nbytes_forwarded=51200\n";
        let pairs = parse_stats_str(input);
        assert_eq!(pairs.len(), 4);
        assert_eq!(pairs[0], ("peers".to_string(), "3".to_string()));
        assert_eq!(pairs[1], ("routes".to_string(), "5".to_string()));
        assert_eq!(
            pairs[2],
            ("packets_forwarded".to_string(), "100".to_string())
        );
        assert_eq!(
            pairs[3],
            ("bytes_forwarded".to_string(), "51200".to_string())
        );
    }

    #[test]
    fn parse_stats_str_skips_malformed_lines() {
        let input = "peers=3\nnot-a-pair\nbytes_forwarded=512\n";
        let pairs = parse_stats_str(input);
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].0, "peers");
        assert_eq!(pairs[1].0, "bytes_forwarded");
    }

    #[test]
    fn parse_stats_str_empty_input() {
        let pairs = parse_stats_str("");
        assert!(pairs.is_empty());
    }

    #[test]
    fn client_template_has_commented_gateway_block_and_parses() {
        let rendered = render_config_template(&[NodeRole::Client], None);
        assert!(rendered.contains("# [gateway]"));
        assert!(rendered.contains("mesh_ip = \"auto\""));

        let config = pim_core::Config::from_str(&rendered).unwrap();
        assert_eq!(config.node.name, "client-node");
        assert_eq!(config.interface.mesh_ip, "auto");
        assert!(!config.gateway.enabled);
    }

    #[test]
    fn gateway_template_enables_gateway_and_parses() {
        let rendered = render_config_template(&[NodeRole::Gateway], Some("edge-a"));
        assert!(rendered.contains("[gateway]"));
        assert!(rendered.contains("enabled = true"));

        let config = pim_core::Config::from_str(&rendered).unwrap();
        assert_eq!(config.node.name, "edge-a");
        assert!(config.gateway.enabled);
        assert_eq!(config.interface.mesh_ip, "10.77.0.1/24");
    }

    #[test]
    fn multi_role_template_deduplicates_roles() {
        let rendered =
            render_config_template(&[NodeRole::Relay, NodeRole::Gateway, NodeRole::Relay], None);

        assert!(rendered.contains("# Roles enabled: relay, gateway"));
        let config = pim_core::Config::from_str(&rendered).unwrap();
        assert_eq!(config.node.name, "relay-gateway-node");
        assert!(config.gateway.enabled);
    }

    #[test]
    fn gateway_ip_from_static_mesh_cidr_uses_first_host() {
        assert_eq!(
            gateway_ip_from_config_mesh_ip("10.77.0.42/24"),
            Some(Ipv4Addr::new(10, 77, 0, 1))
        );
    }

    #[test]
    fn gateway_ip_from_auto_mesh_ip_is_unknown() {
        assert_eq!(gateway_ip_from_config_mesh_ip("auto"), None);
    }

    #[test]
    fn route_present_matches_split_default_route() {
        let routes = "0.0.0.0/1 via 10.77.0.1 dev pim0 onlink\n";
        assert!(route_present(
            routes,
            "0.0.0.0/1",
            Ipv4Addr::new(10, 77, 0, 1),
            "pim0"
        ));
    }
}
