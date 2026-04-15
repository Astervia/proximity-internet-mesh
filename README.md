# Proximity Internet Mesh

Proximity Internet Mesh (PIM) is a Rust workspace for running a local mesh adapter that forwards IP traffic across nearby peers until it reaches a node with internet access. The daemon now supports local TUN operation on Linux and macOS, accepts packets from the host network stack, and forwards them through the mesh. Gateway NAT remains Linux-only.

The codebase is currently centered on a Linux daemon, a small CLI, and Docker-based multi-node test environments. The long-term architecture targets proximity transports such as Wi-Fi Direct, but the transport implemented in the repository today is TCP. The documentation below distinguishes current behavior from broader design material.

## Current Scope

- Linux and macOS client/relay runtime with a TUN interface
- Rust workspace with separate crates for protocol, crypto, routing, transport, gateway, discovery, daemon, and CLI
- `pim` CLI with `up`, `down`, `status`, and `route on|off|status`
- `pim-daemon` runtime that handles peer sessions, routing, fragmentation, metrics, and gateway NAT
- Docker Compose labs for single-hop, relay, discovery, resilience, and multi-gateway scenarios

## Repository Layout

```text
.
├── crates/                # Rust workspace members
├── docker/                # Compose files, node configs, test scripts, entrypoint
├── docs/                  # Detailed project documentation
├── Cargo.toml             # Workspace manifest
├── Dockerfile             # Multi-stage build for the PIM image
└── Makefile               # Docker build, test, and stack-management targets
```

## Prerequisites

For local development and manual runs:

- Linux or macOS
- Rust toolchain new enough to build the workspace when installing from source
- privileges to create a TUN interface and update routes, typically `root` or `CAP_NET_ADMIN`
- Linux gateways additionally require `/dev/net/tun`, `iproute2`, and `iptables`

For Docker-based integration testing:

- Docker Engine
- Docker Compose v2
- outbound internet access from the host so gateway containers can NAT traffic

## Build

Build the workspace in development mode:

```bash
cargo build --workspace
```

Build optimized binaries:

```bash
cargo build --workspace --release
```

The main binaries are:

- `target/release/pim`
- `target/release/pim-daemon`

## Generate A Starter Config

The CLI can generate a commented config template for one or more roles:

```bash
pim config generate client
pim config generate relay
pim config generate gateway
pim config generate relay gateway
```

Write directly to a file:

```bash
pim config generate client --output /tmp/pim.toml
pim config generate gateway --name edge-a --output /tmp/pim-gateway.toml
```

The generated TOML is meant to be edited by hand:

- active settings are emitted as real TOML
- inactive gateway settings are left commented out
- peer examples are commented so they can be enabled selectively

## Install

Published releases include prebuilt archives for:

- Linux x86_64 via `x86_64-unknown-linux-musl`, which is portable across mainstream distros
- macOS Intel via `x86_64-apple-darwin`
- macOS Apple Silicon via `aarch64-apple-darwin`

The daemon can now run as a client or relay on macOS using `utunN` interfaces and macOS route management. Gateway mode, NAT setup, and the Docker integration labs still require Linux.

Install the latest released binaries for your host:

```bash
VERSION="$(curl -fsSLI -o /dev/null -w '%{url_effective}' \
  https://github.com/Astervia/proximity-internet-mesh/releases/latest \
  | sed 's:.*/::')"

if [ -z "${VERSION}" ]; then
  echo "Failed to determine the latest GitHub release version" >&2
  exit 1
fi

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) ASSET="pim-${VERSION}-x86_64-unknown-linux-musl" ;;
  Darwin-x86_64) ASSET="pim-${VERSION}-x86_64-apple-darwin" ;;
  Darwin-arm64) ASSET="pim-${VERSION}-aarch64-apple-darwin" ;;
  *)
    echo "No published release artifact for $(uname -s)-$(uname -m)" >&2
    exit 1
    ;;
esac

curl -LO "https://github.com/Astervia/proximity-internet-mesh/releases/download/${VERSION}/${ASSET}.tar.gz"
curl -LO "https://github.com/Astervia/proximity-internet-mesh/releases/download/${VERSION}/${ASSET}.sha256"

if command -v sha256sum >/dev/null 2>&1; then
  sha256sum -c "${ASSET}.sha256"
else
  shasum -a 256 -c "${ASSET}.sha256"
fi

tar -xzf "${ASSET}.tar.gz"
sudo mkdir -p /usr/local/bin
sudo install -m 755 "${ASSET}/pim" /usr/local/bin/pim
sudo install -m 755 "${ASSET}/pim-daemon" /usr/local/bin/pim-daemon

if [ "$(uname -s)" = "Linux" ]; then
  sudo mkdir -p /etc/pim /var/lib/pim /run
fi
```

If you need to build from source instead:

```bash
cargo build --workspace --release
sudo install -Dm755 target/release/pim /usr/local/bin/pim
sudo install -Dm755 target/release/pim-daemon /usr/local/bin/pim-daemon
if [ "$(uname -s)" = "Linux" ]; then
  sudo install -d /etc/pim /var/lib/pim /run
fi
```

Then create `/etc/pim/pim.toml`. The easiest starting point is:

```bash
sudo pim config generate client --output /etc/pim/pim.toml
```

You can also start from a minimal static client example:

```toml
[node]
name = "client-a"
data_dir = "/var/lib/pim"

[interface]
name = "pim0" # use "utun0" on macOS
mesh_ip = "10.77.0.100/24"
mtu = 1400

[transport]
type = "tcp"
listen_port = 9100

[routing]
max_hops = 10
algorithm = "distance-vector"
route_expiry_s = 300

[gateway]
enabled = false
nat_interface = "eth0"
max_connections = 200

[security]
key_file = "/var/lib/pim/node.key"
require_encryption = true

[[peers]]
mechanism = "tcp"
address = "gateway.example.internal:9100"
label = "gateway"
```

For a gateway node, set `gateway.enabled = true`, choose the gateway mesh IP, and point `nat_interface` at the internet-facing interface.

## Uninstall

Stop the daemon first if it is running:

```bash
sudo pim down || true
```

Remove the installed binaries:

```bash
sudo rm -f /usr/local/bin/pim /usr/local/bin/pim-daemon
```

Remove system-wide config and runtime directories if you no longer need them:

```bash
sudo rm -rf /etc/pim /var/lib/pim
sudo rm -f /run/pim.pid /run/pim.stats
```

If you only want to remove the binaries but keep keys and configuration for a future reinstall, skip deleting `/etc/pim` and `/var/lib/pim`.

## Use

Start in the foreground:

```bash
sudo pim up --config /etc/pim/pim.toml
```

Start in the background:

```bash
sudo pim up --config /etc/pim/pim.toml --daemon
```

Inspect status:

```bash
sudo pim status
sudo pim status --verbose
```

Enable split-default routing through `pim0`:

```bash
sudo pim route on
sudo pim route status
```

Disable split-default routing and return to the normal underlay route:

```bash
sudo pim route off
sudo pim route status
```

Stop the daemon:

```bash
sudo pim down
```

Default runtime paths:

- config: `/etc/pim/pim.toml`
- pid file: `/run/pim.pid`
- stats file: `/run/pim.stats`

What `pim status --verbose` reads today:

- peer count
- route count
- forwarded packets and bytes
- dropped packets
- congestion drops
- conntrack size
- uptime

## Use PIM As The Active Route

PIM does not create a Wi-Fi network entry or a separate OS-visible "network" to join. The normal LAN or Wi-Fi connection remains the underlay, while `pim-daemon` creates a TUN interface such as `pim0` for overlay traffic.

To route internet-bound traffic through the mesh from a client node:

1. Start `pim` so `pim0` exists and the client has connected to a relay or gateway.
2. Enable the split-default routes with `sudo pim route on`.
3. Verify with `sudo pim route status`.

The route command installs:

- `0.0.0.0/1` via the mesh gateway on `pim0`
- `128.0.0.0/1` via the mesh gateway on `pim0`

This approach prefers the PIM tunnel for general internet traffic without replacing the host's underlying LAN/Wi-Fi connection.

Notes:

- On clients using `mesh_ip = "auto"`, `pim0` must already be up before `pim route on` can determine the active mesh gateway.
- `sudo pim route off` removes those split-default routes and returns traffic selection to the normal underlay path.

## Docker Test Workflows

Build the container image:

```bash
make docker-build
```

Run unit tests:

```bash
make test-unit
```

Run Docker phases:

```bash
make test-p1
make test-p2
make test-p3
make test-p4
make test-p5
make test-all
```

Bring up a lab manually:

```bash
make up-p1
make logs-p1
make sh-p1-client
make down-p1
```

## Documentation

Start with [docs/README.md](/home/rfluid/development/proximity-internet-mesh/docs/README.md). The docs are grouped by topic:

- Getting started: install, configure, and operate a node
- Architecture: system model, packet flow, routing, protocol, and security
- Operations: test strategy and Docker-based validation
- Project: workspace internals, roadmap, and implementation checklist

## Notes On Current Behavior

- The transport backend in this repository is TCP, not Wi-Fi Direct.
- Linux is the practical target for running the daemon because TUN and route setup are Linux-specific here.
- The CLI currently exposes only `up`, `down`, and `status`.
- The daemon supports static peer configuration and includes discovery and dynamic-IP building blocks; the Docker scenarios in the repository still rely on explicit node config files.
