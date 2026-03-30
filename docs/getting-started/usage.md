# Usage

This page documents the CLI that exists in the repository today.

## Commands

The `pim` binary exposes three commands:

```text
pim up
pim down
pim status
pim config generate
```

Run help at any level:

```bash
pim --help
pim up --help
pim down --help
pim status --help
pim config generate --help
```

## `pim config generate`

Generates a commented TOML config template for one or more roles.

Examples:

```bash
pim config generate client
pim config generate relay
pim config generate gateway
pim config generate relay gateway
```

Write to a file:

```bash
pim config generate client --output /etc/pim/pim.toml
```

Useful flags:

- `--name`: override the generated node name
- `--output`, `-o`: write to a file instead of stdout
- `--force`: overwrite an existing output file

The template is intentionally comment-heavy:

- enabled settings are active TOML
- disabled gateway settings stay commented out
- peer examples are commented so they can be selectively enabled

## `pim up`

Starts `pim-daemon`.

```bash
sudo pim up --config /etc/pim/pim.toml
```

Foreground mode blocks the terminal and is useful while developing because logs stay attached to the current process.

Start in the background:

```bash
sudo pim up --config /etc/pim/pim.toml --daemon
```

Flags:

- `--config`, `-c`: path to the TOML configuration file
- `--pid-file`: PID file location, default `/run/pim.pid`
- `--daemon`, `-d`: detach and run in the background

## `pim down`

Stops the daemon by reading the PID file and sending `SIGTERM`.

```bash
sudo pim down
```

Optional flag:

- `--pid-file`: PID file location, default `/run/pim.pid`

## `pim status`

Reports whether the daemon is running and, when possible, prints basic config-derived information.

```bash
sudo pim status
```

Verbose status also reads `/run/pim.stats` and shows live counters written by the daemon:

```bash
sudo pim status --verbose
```

Current metrics include:

- `peers`
- `routes`
- `packets_forwarded`
- `bytes_forwarded`
- `packets_dropped`
- `congestion_drops`
- `conntrack_size`
- `uptime_secs`

## Runtime Files

- config file: `/etc/pim/pim.toml`
- pid file: `/run/pim.pid`
- stats file: `/run/pim.stats`

## Typical Manual Workflow

1. Build and install the binaries.
2. Write `/etc/pim/pim.toml`.
3. Start the node with `sudo pim up --config /etc/pim/pim.toml`.
4. Check health with `sudo pim status --verbose`.
5. Stop the node with `sudo pim down`.

## Docker Workflow

Inside the repository, most end-to-end usage is exercised through Docker Compose:

```bash
make docker-build
make up-p1
make sh-p1-client
pim status --verbose
make down-p1
```
