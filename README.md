# Proximity Internet Mesh

Proximity Internet Mesh (PIM) is a Rust workspace for running a local mesh adapter that forwards IP traffic across nearby peers until it reaches a node with internet access. The daemon supports local TUN operation on Linux and macOS, accepts packets from the host network stack, and forwards them through the mesh, including gateway NAT on both platforms.

## Current Scope

- Linux and macOS client/relay/gateway runtime with a TUN interface
- Rust workspace with separate crates for protocol, crypto, routing, transport, gateway, discovery, daemon, and CLI
- `pim` CLI with `up`, `down`, `status`, `route on|off|status`, and `config generate`
- `pim-daemon` runtime that handles peer sessions, routing, fragmentation, metrics, and gateway NAT
- Docker Compose labs for single-hop, relay, discovery, resilience, and multi-gateway scenarios

## Platform Support

| Capability        | Linux     | macOS         |
| ----------------- | --------- | ------------- |
| Client / relay    | Supported | Supported     |
| Gateway + NAT     | Supported | Supported     |
| Wi-Fi Direct      | Supported | Supported     |
| Bluetooth PAN     | Supported | Supported     |
| Docker labs       | Supported | Not supported |

For per-feature host requirements and OS-specific guidance see
[docs/reference/platform-support.md](docs/reference/platform-support.md).

## Status

The workspace is under active development. The runtime supports client,
relay, and gateway roles on Linux and macOS, with TCP, Bluetooth PAN, and
Wi-Fi Direct transports. See [docs/project/roadmap.md](docs/project/roadmap.md)
for the phased delivery view and
[docs/project/history.md](docs/project/history.md) for the historical
implementation log.

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

## Quick Start

```bash
cargo build --workspace --release
sudo install -Dm755 target/release/pim /usr/local/bin/pim
sudo install -Dm755 target/release/pim-daemon /usr/local/bin/pim-daemon
sudo pim config generate client --output /etc/pim/pim.toml  # macOS: drop sudo
sudo pim up --config /etc/pim/pim.toml --daemon
sudo pim status --verbose
```

Full instructions:

- [docs/getting-started/installation.md](docs/getting-started/installation.md) — host requirements, prebuilt-archive install, build from source.
- [docs/getting-started/configuration.md](docs/getting-started/configuration.md) — config reference and worked examples.
- [docs/getting-started/usage.md](docs/getting-started/usage.md) — CLI walkthrough and runtime files.
- [docs/getting-started/example-topology.md](docs/getting-started/example-topology.md) — three-node walkthrough with packet flow.

## Docker Test Workflows

```bash
make docker-build      # build container image
make test-unit         # run unit tests
make test-all          # run all Docker integration phases (p1–p5)
make up-p1             # bring up phase-1 lab manually
make logs-p1           # tail logs
make sh-p1-client      # shell into client container
make down-p1           # tear down
```

For full details see [docs/operations/docker-labs.md](docs/operations/docker-labs.md).

## Documentation

Start with [docs/README.md](docs/README.md). The docs are grouped by purpose:

- **Getting started** — install, configure, and operate a node.
- **Architecture** — runtime model, packet flow, routing, protocol, security, transports.
- **Operations** — testing strategy and Docker-based validation.
- **Troubleshooting** — operator recovery commands and known cleanup procedures (replaces the previous root TROUBLESHOOTING.md).
- **Reference** — CLI, config schema, platform support, sample TOMLs.
- **Project internals** — workspace, roadmap, and delivery history.

For runtime debugging and operator recovery steps, see
[docs/troubleshooting/](docs/troubleshooting/README.md).
