//! pim — command-line interface for the proximity internet mesh daemon.
//!
//! Commands:
//!   pim up   [--config <path>] [--daemon]   Start the daemon
//!   pim down [--pid-file <path>]            Stop the running daemon
//!   pim status [--pid-file <path>]          Show daemon state
//!   pim route <on|off|status>               Manage split-default routing via pim0
//!   pim config generate <roles...>          Generate a commented config template

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::process;
use std::str::FromStr;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use pim_core::{DebugSnapshot, NodeId};

const DEFAULT_CONFIG: &str = "/etc/pim/pim.toml";
const DEFAULT_PID_FILE: &str = "/run/pim.pid";
const DEFAULT_LOG_FILE: &str = "/run/pim.log";
const DEFAULT_DEBUG_SNAPSHOT: &str = "/run/pim-debug.json";
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

        /// Run the daemon in the background; logs are written to --log-file
        #[arg(short = 'd', long = "detach", alias = "daemon")]
        detach: bool,

        /// Log file path used when running detached (follow with `pim logs`)
        #[arg(long, default_value = DEFAULT_LOG_FILE)]
        log_file: PathBuf,
    },

    /// Stream live daemon logs.
    ///
    /// RUST_LOG controls what the daemon records — set it before starting:
    ///
    ///   RUST_LOG=info,pim_bluetooth=debug  pim up --detach
    ///   RUST_LOG=debug                     pim up --detach
    ///
    /// Then stream with:
    ///
    ///   pim logs                        # live tail (Ctrl-C to stop)
    ///   pim logs -n 50                  # last 50 lines, then follow
    ///   pim logs --no-follow            # print existing lines and exit
    ///   pim logs --since 5m             # lines from the last 5 minutes
    ///   pim logs --since 1h --until 30m # window between 1 h ago and 30 min ago
    Logs {
        /// Path to the daemon log file
        #[arg(long, default_value = DEFAULT_LOG_FILE)]
        log_file: PathBuf,

        /// Print existing lines and exit; do not follow new output
        #[arg(long = "no-follow")]
        no_follow: bool,

        /// Follow by file name — reopen the log if it is rotated or recreated
        #[arg(short = 'F', long = "follow-name")]
        follow_name: bool,

        /// Wait for the log file to appear if it does not exist yet
        #[arg(long)]
        retry: bool,

        /// Show only the last N lines before following (0 = all)
        #[arg(short = 'n', long = "lines", default_value_t = 0)]
        lines: usize,

        /// Strip the timestamp prefix from each log line
        #[arg(long)]
        no_timestamp: bool,

        /// Only show lines at or after this time (RFC3339 or relative: 5m, 1h30m, 2d)
        #[arg(long)]
        since: Option<String>,

        /// Stop at this time and exit (RFC3339 or relative); implies --no-follow
        #[arg(long)]
        until: Option<String>,
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

    /// Inspect live mesh state for debugging
    Debug {
        #[command(subcommand)]
        command: DebugCommands,
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

#[derive(Subcommand)]
enum DebugCommands {
    /// Show connected peers and their connection mechanisms
    Peers {
        /// Path to the daemon debug snapshot
        #[arg(long, default_value = DEFAULT_DEBUG_SNAPSHOT)]
        snapshot: PathBuf,
    },

    /// Show installed routes
    Routes {
        /// Path to the daemon debug snapshot
        #[arg(long, default_value = DEFAULT_DEBUG_SNAPSHOT)]
        snapshot: PathBuf,
    },

    /// Show known gateways and the selected gateway
    Gateways {
        /// Path to the daemon debug snapshot
        #[arg(long, default_value = DEFAULT_DEBUG_SNAPSHOT)]
        snapshot: PathBuf,
    },

    /// Show peers seen by the discovery layer
    Discovery {
        /// Path to the daemon debug snapshot
        #[arg(long, default_value = DEFAULT_DEBUG_SNAPSHOT)]
        snapshot: PathBuf,
    },

    /// Explain the current route decision for a destination
    Route {
        #[command(subcommand)]
        command: DebugRouteCommands,
    },
}

#[derive(Subcommand)]
enum DebugRouteCommands {
    /// Show the route used for a peer, mesh IP, or `internet`
    Get {
        /// Destination: 32-char node id hex, mesh IPv4, or the literal `internet`
        target: String,

        /// Path to the daemon debug snapshot
        #[arg(long, default_value = DEFAULT_DEBUG_SNAPSHOT)]
        snapshot: PathBuf,
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
            detach,
            log_file,
        } => cmd_up(config, pid_file, detach, log_file),

        Commands::Logs {
            log_file,
            no_follow,
            follow_name,
            retry,
            lines,
            no_timestamp,
            since,
            until,
        } => cmd_logs(
            log_file,
            no_follow,
            follow_name,
            retry,
            lines,
            no_timestamp,
            since,
            until,
        ),

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

        Commands::Debug { command } => match command {
            DebugCommands::Peers { snapshot } => cmd_debug_peers(snapshot),
            DebugCommands::Routes { snapshot } => cmd_debug_routes(snapshot),
            DebugCommands::Gateways { snapshot } => cmd_debug_gateways(snapshot),
            DebugCommands::Discovery { snapshot } => cmd_debug_discovery(snapshot),
            DebugCommands::Route { command } => match command {
                DebugRouteCommands::Get { target, snapshot } => {
                    cmd_debug_route_get(snapshot, target)
                }
            },
        },
    }
}

// ── `pim up` ──────────────────────────────────────────────────────────────────

fn cmd_up(config: PathBuf, pid_file: PathBuf, detach: bool, log_file: PathBuf) -> Result<()> {
    // Validate config exists
    if !config.exists() {
        bail!("config file not found: {}", config.display());
    }

    // Check that the daemon binary is available
    let daemon_bin = find_daemon_binary()?;

    if detach {
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);

        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o640);
        }

        let log = options
            .open(&log_file)
            .with_context(|| format!("failed to open log file: {}", log_file.display()))?;

        let child = process::Command::new(&daemon_bin)
            .arg(config.to_str().unwrap_or(DEFAULT_CONFIG))
            .arg(pid_file.to_str().unwrap_or(DEFAULT_PID_FILE))
            .stdin(process::Stdio::null())
            .stdout(process::Stdio::null())
            .stderr(log)
            .spawn()
            .with_context(|| format!("failed to spawn {daemon_bin:?}"))?;

        println!(
            "pim daemon started (pid {}), logs → {}",
            child.id(),
            log_file.display()
        );
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

mod commands;

use commands::config::*;
use commands::logs::*;
use commands::route::*;

// ── `pim down` ────────────────────────────────────────────────────────────────

#[cfg(unix)]
fn cmd_down(pid_file: PathBuf) -> Result<()> {
    let pid = read_pid(&pid_file)?;

    // Send SIGTERM
    let ret = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    if ret != 0 {
        let e = std::io::Error::last_os_error();
        bail!("failed to send SIGTERM to pid {pid}: {e}");
    }
    println!("Sent SIGTERM to pim daemon (pid {pid})");

    Ok(())
}

#[cfg(not(unix))]
fn cmd_down(_pid_file: PathBuf) -> Result<()> {
    bail!("pim down is only supported on Unix systems");
}

// ── `pim status` ──────────────────────────────────────────────────────────────

fn cmd_debug_peers(snapshot_path: PathBuf) -> Result<()> {
    let snapshot = read_debug_snapshot(&snapshot_path)?;
    println!(
        "connected peers: {}  node={} ({})",
        snapshot.peers.len(),
        snapshot.node.name,
        snapshot.node.short_id
    );
    if snapshot.peers.is_empty() {
        println!("  none");
        return Ok(());
    }

    for peer in snapshot.peers {
        let mechanism = peer.mechanism.as_deref().unwrap_or("unknown");
        let addr = peer.addr.as_deref().unwrap_or("-");
        let hb = peer
            .last_heartbeat_age_ms
            .map(|ms| format!("{}ms", ms))
            .unwrap_or_else(|| "-".into());
        println!(
            "  {}  direct={}  mechanism={}  addr={}  configured={}  discovered={}  hb_age={}",
            peer.short_id, peer.direct, mechanism, addr, peer.configured, peer.discovered, hb
        );
    }
    Ok(())
}

fn cmd_debug_routes(snapshot_path: PathBuf) -> Result<()> {
    let snapshot = read_debug_snapshot(&snapshot_path)?;
    println!("installed routes: {}", snapshot.routes.len());
    if snapshot.routes.is_empty() {
        println!("  none");
        return Ok(());
    }

    for route in snapshot.routes {
        let mesh_ip = route.mesh_ip.as_deref().unwrap_or("-");
        let rtt = route
            .rtt_ms
            .map(|ms| format!("{ms}ms"))
            .unwrap_or_else(|| "-".into());
        println!(
            "  {}  via={}  hops={}  learned_from={}  gateway={}  load={}  rtt={}  mesh_ip={}  age={}ms  blacklisted={}",
            route.destination_short_id,
            route.next_hop_short_id,
            route.hops,
            route.learned_from_short_id,
            route.is_gateway,
            route.gateway_load,
            rtt,
            mesh_ip,
            route.age_ms,
            route.next_hop_blacklisted
        );
    }
    Ok(())
}

fn cmd_debug_gateways(snapshot_path: PathBuf) -> Result<()> {
    let snapshot = read_debug_snapshot(&snapshot_path)?;
    println!("known gateways: {}", snapshot.gateways.len());
    if snapshot.gateways.is_empty() {
        println!("  none");
        return Ok(());
    }

    for gateway in snapshot.gateways {
        let marker = if gateway.selected { "*" } else { " " };
        let rtt = gateway
            .rtt_ms
            .map(|ms| format!("{ms}ms"))
            .unwrap_or_else(|| "-".into());
        let mesh_ip = gateway.mesh_ip.as_deref().unwrap_or("-");
        println!(
            "{} {}  via={}  hops={}  score={}  load={}  rtt={}  mesh_ip={}",
            marker,
            gateway.short_id,
            gateway.next_hop_short_id,
            gateway.hops,
            gateway.score,
            gateway.gateway_load,
            rtt,
            mesh_ip
        );
    }
    Ok(())
}

fn cmd_debug_discovery(snapshot_path: PathBuf) -> Result<()> {
    let snapshot = read_debug_snapshot(&snapshot_path)?;
    println!("discovered peers: {}", snapshot.discovered_peers.len());
    if snapshot.discovered_peers.is_empty() {
        println!("  none");
        return Ok(());
    }

    for peer in snapshot.discovered_peers {
        println!(
            "  {}  addr={}  client={}  relay={}  gateway={}  age={}ms",
            peer.short_id,
            peer.addr,
            peer.is_client,
            peer.is_relay,
            peer.is_gateway,
            peer.last_seen_age_ms
        );
    }
    Ok(())
}

fn cmd_debug_route_get(snapshot_path: PathBuf, target: String) -> Result<()> {
    let snapshot = read_debug_snapshot(&snapshot_path)?;

    if target == "internet" {
        if snapshot.node.is_gateway {
            println!(
                "internet route: local node {} ({}) is the gateway",
                snapshot.node.name, snapshot.node.short_id
            );
            return Ok(());
        }

        let gateway = snapshot
            .gateways
            .iter()
            .find(|gateway| gateway.selected)
            .with_context(|| "no gateway route is currently selected")?;
        println!("internet route:");
        println!("  gateway:   {}", gateway.short_id);
        println!("  next_hop:  {}", gateway.next_hop_short_id);
        println!("  hops:      {}", gateway.hops);
        println!("  score:     {}", gateway.score);
        println!(
            "  mechanism: {}",
            peer_mechanism(&snapshot, &gateway.next_hop_id)
        );
        return Ok(());
    }

    if let Ok(mesh_ip) = target.parse::<Ipv4Addr>() {
        let route = snapshot
            .routes
            .iter()
            .find(|route| route.mesh_ip.as_deref() == Some(&mesh_ip.to_string()))
            .with_context(|| format!("no installed route for mesh IP {mesh_ip}"))?;
        print_route_explanation(&snapshot, route);
        return Ok(());
    }

    let route = find_route_by_node_target(&snapshot, &target)
        .with_context(|| format!("no installed route for destination {target}"))?;
    print_route_explanation(&snapshot, route);
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn read_debug_snapshot(path: &PathBuf) -> Result<DebugSnapshot> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read debug snapshot: {}", path.display()))?;
    serde_json::from_str(&content)
        .with_context(|| format!("invalid debug snapshot JSON in {}", path.display()))
}

fn find_route_by_node_target<'a>(
    snapshot: &'a DebugSnapshot,
    target: &str,
) -> Option<&'a pim_core::DebugRouteSnapshot> {
    if let Ok(node_id) = NodeId::from_str(target) {
        let hex = node_id.to_hex();
        return snapshot
            .routes
            .iter()
            .find(|route| route.destination_id == hex);
    }

    let mut matches = snapshot.routes.iter().filter(|route| {
        route.destination_id.starts_with(target) || route.destination_short_id == target
    });
    let first = matches.next()?;
    if matches.next().is_some() {
        None
    } else {
        Some(first)
    }
}

fn peer_mechanism(snapshot: &DebugSnapshot, next_hop_id: &str) -> String {
    snapshot
        .peers
        .iter()
        .find(|peer| peer.node_id == next_hop_id)
        .and_then(|peer| peer.mechanism.clone())
        .unwrap_or_else(|| "unknown".into())
}

fn print_route_explanation(snapshot: &DebugSnapshot, route: &pim_core::DebugRouteSnapshot) {
    let mesh_ip = route.mesh_ip.as_deref().unwrap_or("-");
    let rtt = route
        .rtt_ms
        .map(|ms| format!("{ms}ms"))
        .unwrap_or_else(|| "-".into());
    println!("route:");
    println!("  destination: {}", route.destination_short_id);
    println!("  next_hop:    {}", route.next_hop_short_id);
    println!(
        "  mechanism:   {}",
        peer_mechanism(snapshot, &route.next_hop_id)
    );
    println!("  hops:        {}", route.hops);
    println!("  learned_from: {}", route.learned_from_short_id);
    println!("  gateway:     {}", route.is_gateway);
    println!("  gateway_load: {}", route.gateway_load);
    println!("  rtt:         {}", rtt);
    println!("  mesh_ip:     {}", mesh_ip);
    println!("  age_ms:      {}", route.age_ms);
    println!("  blacklisted: {}", route.next_hop_blacklisted);
}

fn read_pid(pid_file: &PathBuf) -> Result<u32> {
    let content = std::fs::read_to_string(pid_file)
        .with_context(|| format!("cannot read PID file: {}", pid_file.display()))?;
    content
        .trim()
        .parse::<u32>()
        .with_context(|| format!("invalid PID in {}", pid_file.display()))
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    // kill(pid, 0) returns 0 if the process exists and we can signal it
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(not(unix))]
fn process_alive(_pid: u32) -> bool {
    // On non-Unix platforms, we can't reliably check without spawning a process
    false
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

#[cfg(test)]
mod tests;
