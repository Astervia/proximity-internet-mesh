# CLI Design

PIM's CLI is built with [clap](https://docs.rs/clap) using the **derive API**. It follows the subcommand pattern (like `git`, `cargo`, `docker`).

## Binary Structure

The CLI is a thin client. It does not run mesh logic itself — it communicates with the running `pim-daemon` over a Unix domain socket (`/var/run/pim/pim.sock`).

```
pim-cli (binary: `pim`)
  │
  ├── sends command to pim-daemon via Unix socket
  ├── receives structured response (JSON internally)
  └── formats and prints human-readable output
```

The only exception is `pim up`, which spawns the daemon process itself (or runs it in the foreground with `--foreground`).

## Top-Level Structure

```rust
use clap::{Parser, Subcommand};

/// Proximity Internet Mesh — decentralized network adapter
#[derive(Parser)]
#[command(name = "pim", version, about, propagate_version = true)]
pub struct Cli {
    /// Path to configuration file
    #[arg(short, long, global = true, default_value = "/etc/pim/config.toml")]
    pub config: PathBuf,

    /// Enable verbose logging output
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Start the PIM daemon and connect to the mesh
    Up(UpArgs),
    /// Disconnect from the mesh and stop the daemon
    Down,
    /// Show connection status and mesh information
    Status(StatusArgs),
    /// List discovered peers
    Peers(PeersArgs),
    /// Show the routing table
    Routes(RoutesArgs),
    /// Show or update configuration
    Config(ConfigArgs),
    /// Generate a new node identity keypair
    Keygen(KeygenArgs),
    /// Diagnostic tools
    Diag(DiagArgs),
}
```

## Subcommand Definitions

### `pim up`

```rust
/// Start the PIM daemon and connect to the mesh
#[derive(clap::Args)]
pub struct UpArgs {
    /// Run the daemon in the foreground (don't daemonize)
    #[arg(short, long)]
    pub foreground: bool,

    /// Override the TUN interface name
    #[arg(long)]
    pub interface: Option<String>,

    /// Override the node role (auto, client, relay, gateway)
    #[arg(long, value_enum)]
    pub role: Option<NodeRole>,
}

#[derive(clap::ValueEnum, Clone)]
pub enum NodeRole {
    Auto,
    Client,
    Relay,
    Gateway,
}
```

```
$ pim up
Starting PIM daemon...
Discovering peers...
Connected to 2 peers
Mesh IP: 10.77.0.5/16
Gateway: 10.77.0.1 (via relay-b, 2 hops)
PIM is active on pim0

$ pim up --foreground --role gateway
[2026-03-26T12:00:00Z INFO  pim_daemon] Starting in gateway mode
[2026-03-26T12:00:00Z INFO  pim_daemon::tun] Created interface pim0
[2026-03-26T12:00:01Z INFO  pim_daemon::discovery] Found 3 peers
...
```

### `pim down`

```rust
/// Disconnect from the mesh and stop the daemon
#[derive(clap::Args)]
pub struct Down;
// No additional args — sends SIGTERM to daemon via PID file or socket command.
```

```
$ pim down
Sending goodbye to peers...
Removing interface pim0...
PIM stopped.
```

### `pim status`

```rust
/// Show connection status and mesh information
#[derive(clap::Args)]
pub struct StatusArgs {
    /// Show extended statistics
    #[arg(long)]
    pub verbose: bool,

    /// Output as JSON for scripting
    #[arg(long)]
    pub json: bool,
}
```

```
$ pim status
Status:       connected
Interface:    pim0
Mesh IP:      10.77.0.5/16
Gateway:      10.77.0.1 (node-d, 2 hops)
Peers:        3 connected
Uptime:       1h 23m

$ pim status --verbose
Status:       connected
Interface:    pim0
Mesh IP:      10.77.0.5/16
Gateway:      10.77.0.1 (node-d, 2 hops)
Peers:        3 connected
Uptime:       1h 23m
Packets TX:   12,482
Packets RX:   11,917
Bytes TX:     8.2 MB
Bytes RX:     14.1 MB
Forwarded:    0
Dropped:      23
Routes:       5
Session key:  rotated 12m ago

$ pim status --json
{"status":"connected","interface":"pim0","mesh_ip":"10.77.0.5","peers":3,...}
```

### `pim peers`

```rust
/// List discovered peers
#[derive(clap::Args)]
pub struct PeersArgs {
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}
```

```
$ pim peers
NODE ID              ROLE       HOPS   LAST SEEN   ADDR
a3f1..b2c4           relay      1      2s ago      172.20.0.12
d9e2..4a71           gateway    2      1s ago      (via a3f1..b2c4)
f0c8..9d32           client     1      4s ago      172.20.0.14
```

### `pim routes`

```rust
/// Show the routing table
#[derive(clap::Args)]
pub struct RoutesArgs {
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}
```

```
$ pim routes
DESTINATION          NEXT HOP             HOPS   GATEWAY   EXPIRES
a3f1..b2c4           a3f1..b2c4 (direct)  1      no        4m 52s
d9e2..4a71           a3f1..b2c4           2      yes       4m 48s
f0c8..9d32           f0c8..9d32 (direct)  1      no        4m 55s
```

### `pim config`

```rust
/// Show or update configuration
#[derive(clap::Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub action: ConfigAction,
}

#[derive(Subcommand)]
pub enum ConfigAction {
    /// Print the active configuration
    Show {
        /// Output as JSON instead of TOML
        #[arg(long)]
        json: bool,
    },
    /// Get a specific config value
    Get {
        /// Config key in dot notation (e.g., "routing.max_hops")
        key: String,
    },
    /// Set a config value (writes to config file)
    Set {
        /// Config key in dot notation
        key: String,
        /// New value
        value: String,
    },
}
```

```
$ pim config show
[node]
name = "my-device"
data_dir = "/home/user/.pim"
...

$ pim config get routing.max_hops
10

$ pim config set routing.max_hops 15
Updated routing.max_hops = 15
Restart PIM for changes to take effect.
```

### `pim keygen`

```rust
/// Generate a new node identity keypair
#[derive(clap::Args)]
pub struct KeygenArgs {
    /// Output file for the private key
    #[arg(short, long, default_value = "~/.pim/node.key")]
    pub output: PathBuf,

    /// Overwrite existing key without prompting
    #[arg(long)]
    pub force: bool,
}
```

```
$ pim keygen
Generated new identity:
  Node ID:     a3f1..b2c4
  Public key:  MCowBQYDK2Vw...
  Private key: ~/.pim/node.key

$ pim keygen
Key already exists at ~/.pim/node.key
Use --force to overwrite.
```

### `pim diag`

```rust
/// Diagnostic tools
#[derive(clap::Args)]
pub struct DiagArgs {
    #[command(subcommand)]
    pub tool: DiagTool,
}

#[derive(Subcommand)]
pub enum DiagTool {
    /// Ping a peer through the mesh (mesh-level, not ICMP)
    Ping {
        /// Target node ID or mesh IP
        target: String,

        /// Number of pings to send
        #[arg(short, long, default_value = "4")]
        count: u32,
    },
    /// Trace the route to a peer or gateway
    Traceroute {
        /// Target node ID or mesh IP
        target: String,
    },
    /// Dump raw mesh frames (for debugging)
    Dump {
        /// Number of frames to capture
        #[arg(short, long, default_value = "20")]
        count: u32,

        /// Filter by frame type
        #[arg(long, value_enum)]
        frame_type: Option<FrameTypeFilter>,
    },
}

#[derive(clap::ValueEnum, Clone)]
pub enum FrameTypeFilter {
    Data,
    Handshake,
    Route,
    Heartbeat,
    Control,
}
```

```
$ pim diag ping d9e2..4a71
PING d9e2..4a71 via mesh:
  reply from d9e2..4a71: hops=2 time=12.3ms
  reply from d9e2..4a71: hops=2 time=11.8ms
  reply from d9e2..4a71: hops=2 time=13.1ms
  reply from d9e2..4a71: hops=2 time=12.0ms
--- 4 sent, 4 received, 0% loss, avg=12.3ms ---

$ pim diag traceroute d9e2..4a71
TRACEROUTE to d9e2..4a71:
  1  a3f1..b2c4  (relay)    5.2ms
  2  d9e2..4a71  (gateway)  12.1ms

$ pim diag dump --count 5 --frame-type data
[12:00:01.234] DATA src=f0c8..9d32 dst=d9e2..4a71 ttl=9 len=1420 flags=INTERNET
[12:00:01.248] DATA src=d9e2..4a71 dst=f0c8..9d32 ttl=8 len=892  flags=
...
```

## Daemon Communication Protocol

The CLI talks to the daemon over a Unix socket at `/var/run/pim/pim.sock`. Messages are length-prefixed JSON:

```rust
/// Request from CLI to daemon
#[derive(Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DaemonRequest {
    Status,
    Peers,
    Routes,
    Ping { target: String, count: u32 },
    Traceroute { target: String },
    Dump { count: u32, frame_type: Option<String> },
    Shutdown,
}

/// Response from daemon to CLI
#[derive(Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DaemonResponse {
    Status { info: StatusInfo },
    Peers { list: Vec<PeerInfo> },
    Routes { table: Vec<RouteInfo> },
    PingResult { results: Vec<PingReply> },
    TracerouteResult { hops: Vec<TracerouteHop> },
    DumpFrame { frame: FrameSummary },
    Ok,
    Error { message: String },
}
```

## Error Handling

The CLI uses `anyhow` for error propagation and prints user-friendly messages:

```rust
fn main() {
    let cli = Cli::parse();

    if let Err(e) = run(cli) {
        eprintln!("error: {e}");

        // Print context chain for --verbose
        if cli.verbose > 0 {
            for cause in e.chain().skip(1) {
                eprintln!("  caused by: {cause}");
            }
        }

        std::process::exit(1);
    }
}
```

Common error messages:

```
$ pim status
error: daemon is not running (could not connect to /var/run/pim/pim.sock)

$ pim up
error: permission denied creating TUN device (try running with sudo or CAP_NET_ADMIN)

$ pim down
error: daemon is not running
```

## Exit Codes

| Code | Meaning                                      |
| ---- | -------------------------------------------- |
| 0    | Success                                      |
| 1    | General error                                |
| 2    | CLI usage error (bad args) — handled by clap |
| 3    | Daemon not running                           |
| 4    | Permission denied                            |
| 5    | Connection to daemon lost                    |

```rust
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum ExitCode {
    Success = 0,
    Error = 1,
    Usage = 2,
    DaemonNotRunning = 3,
    PermissionDenied = 4,
    ConnectionLost = 5,
}
```

## Clap Best Practices Applied

- **Derive API** over builder: type-safe, compile-time checked, less boilerplate
- **`#[command(propagate_version = true)]`**: all subcommands inherit `--version`
- **Global args** (`--config`, `--verbose`): defined once at root, available to all subcommands
- **`ValueEnum`** for closed sets (roles, frame types): clap validates input, generates completions
- **Nested subcommands** (`pim config show`, `pim diag ping`): natural grouping without overloading the top level
- **`ArgAction::Count`** for verbosity: `-v`, `-vv`, `-vvv` for increasing detail
- **Default values** specified via `#[arg(default_value = "...")]`: self-documenting, shown in `--help`
- **`--json` flag** on output commands: machine-readable output for scripting and testing
- **No positional args on destructive commands**: `pim down` has no args that could be confused
- **Shell completions**: generated at build time via `clap_complete`

### Shell Completions

```rust
// build.rs or a `pim completions` subcommand
use clap_complete::{generate_to, shells::*};

fn generate_completions() {
    let mut cmd = Cli::command();
    for shell in [Bash, Zsh, Fish] {
        generate_to(shell, &mut cmd, "pim", "./completions/").unwrap();
    }
}
```

```
$ pim completions --shell zsh > _pim
$ pim completions --shell bash > pim.bash
```
