# CLI Reference

This document is the canonical reference for the `pim` and `pim-daemon`
binaries shipped by the workspace. For task-oriented walkthroughs see the
[getting-started/usage.md](../getting-started/usage.md) doc.

## `pim`

The user-facing CLI. Invoke with `pim <subcommand> [flags]`.

### `pim up`

Start the PIM daemon. By default the daemon runs in the foreground; pass
`--detach` to run it as a background process.

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `-c`, `--config <path>` | path | `/etc/pim/pim.toml` | Path to the TOML configuration file |
| `--pid-file <path>` | path | `/run/pim.pid` | Path to the PID file written by the daemon |
| `-d`, `--detach` (alias `--daemon`) | flag | false | Run the daemon in the background; logs are written to `--log-file` |
| `--log-file <path>` | path | `/run/pim.log` | Log file path used when running detached (follow with `pim logs`) |

### `pim down`

Stop the running daemon (Unix only). Sends `SIGTERM` to the PID recorded in
the PID file.

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--pid-file <path>` | path | `/run/pim.pid` | Path to the PID file |

### `pim status`

Show the current daemon state. Reads the PID file and, when `--verbose` is
given, prints live metrics from `/run/pim.stats`.

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--pid-file <path>` | path | `/run/pim.pid` | Path to the PID file |
| `-v`, `--verbose` | flag | false | Show detailed live metrics (peer count, routes, forwarded packets, etc.) |

### `pim logs`

Stream live daemon logs written to the log file. The daemon must have been
started with `--detach` (or any method that directs stderr to a file).

`RUST_LOG` controls what the daemon records — set it before starting:

```
RUST_LOG=info,pim_bluetooth=debug  pim up --detach
RUST_LOG=debug                     pim up --detach
```

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--log-file <path>` | path | `/run/pim.log` | Path to the daemon log file |
| `--no-follow` | flag | false | Print existing lines and exit; do not follow new output |
| `-F`, `--follow-name` | flag | false | Follow by file name — reopen the log if it is rotated or recreated |
| `--retry` | flag | false | Wait for the log file to appear if it does not exist yet |
| `-n`, `--lines <N>` | integer | `0` (all) | Show only the last N lines before following (0 = all) |
| `--no-timestamp` | flag | false | Strip the timestamp prefix from each log line |
| `--since <time>` | string | — | Only show lines at or after this time (RFC3339 or relative: `5m`, `1h30m`, `2d`) |
| `--until <time>` | string | — | Stop at this time and exit (RFC3339 or relative); implies `--no-follow` |

### `pim config generate`

Generate a readable, commented configuration template for one or more node
roles. Roles are positional arguments; at least one is required.

```
pim config generate <role>...
```

Valid roles: `client`, `relay`, `gateway`. Multiple roles may be combined
(e.g. `pim config generate relay gateway`).

| Argument / Flag | Type | Default | Description |
|-----------------|------|---------|-------------|
| `<roles>...` | `client` \| `relay` \| `gateway` | — | One or more roles to enable in the generated config (required) |
| `--name <name>` | string | — | Override the generated node name |
| `-o`, `--output <path>` | path | — | Write the template to a file instead of stdout |
| `--force` | flag | false | Overwrite the output file if it already exists |

### `pim route on`

Route internet-bound traffic through the `pim0` tunnel interface by
installing split-default routes (`0.0.0.0/1` and `128.0.0.0/1`). Requires
`pim up` to be running first. Supported on Linux and macOS only.

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `-c`, `--config <path>` | path | `/etc/pim/pim.toml` | Path to the TOML configuration file |

### `pim route off`

Remove split-default routes through `pim0`, returning traffic to the normal
underlay path.

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `-c`, `--config <path>` | path | `/etc/pim/pim.toml` | Path to the TOML configuration file |

### `pim route status`

Show whether split-default PIM routes are currently active.

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `-c`, `--config <path>` | path | `/etc/pim/pim.toml` | Path to the TOML configuration file |

### `pim debug peers`

Show connected peers and their connection mechanisms. Reads the daemon debug
snapshot written to `/run/pim-debug.json` by default.

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--snapshot <path>` | path | `/run/pim-debug.json` | Path to the daemon debug snapshot |

### `pim debug routes`

Show installed routes in the routing table.

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--snapshot <path>` | path | `/run/pim-debug.json` | Path to the daemon debug snapshot |

### `pim debug gateways`

Show known gateways and which one is currently selected for internet egress.

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--snapshot <path>` | path | `/run/pim-debug.json` | Path to the daemon debug snapshot |

### `pim debug discovery`

Show peers seen by the discovery layer (broadcast discovery, Wi-Fi Direct,
Bluetooth).

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--snapshot <path>` | path | `/run/pim-debug.json` | Path to the daemon debug snapshot |

### `pim debug route get`

Explain the current route decision for a specific destination.

```
pim debug route get <target>
```

| Argument / Flag | Type | Default | Description |
|-----------------|------|---------|-------------|
| `<target>` | string | — | Destination: 32-char node ID hex, mesh IPv4 address, or the literal `internet` |
| `--snapshot <path>` | path | `/run/pim-debug.json` | Path to the daemon debug snapshot |

---

## `pim-daemon`

The runtime daemon binary. Under normal operation it is started by
`pim up` rather than directly. It accepts two positional arguments parsed
from `std::env::args` (not clap); there are no named flags.

| Position | Default | Description |
|----------|---------|-------------|
| `argv[1]` — config path | `/etc/pim/pim.toml` | Path to the TOML configuration file |
| `argv[2]` — PID file path | `/run/pim.pid` | Path where the daemon writes its PID on startup |

Logging is controlled entirely by the `RUST_LOG` environment variable
(parsed by `tracing-subscriber`'s `EnvFilter`). Example values:

```
RUST_LOG=info                           # default verbosity
RUST_LOG=debug                          # verbose
RUST_LOG=info,pim_bluetooth=debug       # per-module override
```

---

## Exit Codes

Both binaries use `anyhow::Result<()>` propagated through `main`. The Rust
runtime converts an `Err` result to exit code `1` and prints the error chain
to stderr. A successful run exits with code `0`.

No additional numeric exit codes are used; the codebase contains no calls to
`process::exit` or explicit `ExitCode` constants.

| Code | Meaning |
|------|---------|
| `0` | Success |
| `1` | Any runtime error (config not found, daemon not running, I/O failure, etc.) |
