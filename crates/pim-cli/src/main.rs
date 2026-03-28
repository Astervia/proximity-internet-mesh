//! pim — command-line interface for the proximity internet mesh daemon.
//!
//! Commands:
//!   pim up   [--config <path>] [--daemon]   Start the daemon
//!   pim down [--pid-file <path>]            Stop the running daemon
//!   pim status [--pid-file <path>]          Show daemon state

use std::path::PathBuf;
use std::process;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

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
    if pairs.is_empty() { None } else { Some(pairs) }
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
        assert_eq!(pairs[2], ("packets_forwarded".to_string(), "100".to_string()));
        assert_eq!(pairs[3], ("bytes_forwarded".to_string(), "51200".to_string()));
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
}
