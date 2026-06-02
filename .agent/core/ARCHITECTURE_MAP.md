# Architecture Map

This map is the default orientation for agent work in this repository.

## Workspace Entry Points

- `README.md`
    - current scope
    - local build and run workflow
    - operator-facing commands
- `docs/README.md`
    - top-level documentation index
- `Cargo.toml`
    - workspace membership
    - shared dependency versions
- `docs/project/workspace.md`
    - crate-by-crate responsibilities
- `docs/project/workspace.md`
    - high-level repository structure

## Runtime Spine

- `crates/pim-cli/src/main.rs`
    - user-facing commands
    - daemon lifecycle entry points
- `crates/pim-daemon/src/jni.rs`
    - Android integration, providing JNI bridges and VPN service configurations
- `crates/pim-daemon/src/main.rs`
    - main runtime orchestration
    - service startup and shutdown
    - connection, routing, forwarding, and gateway coordination
- `crates/pim-core/src/config.rs`
    - shared configuration model
    - feature toggles and defaults

## Major Subsystems

- `crates/pim-tun/src/lib.rs`
    - Linux TUN device lifecycle
    - packet ingress and egress with the host OS
- `crates/pim-transport/src/lib.rs`
    - transport trait and peer addressing
- `crates/pim-transport/src/tcp.rs`
    - current implemented transport backend
- `crates/pim-discovery/src/lib.rs`
    - LAN peer discovery
- `crates/pim-bluetooth/src/`
    - Bluetooth and RFCOMM link establishment, wire protocol, and platform backends
- `crates/pim-wifidirect/src/lib.rs`
    - alternative peer-finding or link-establishment path
- `crates/pim-crypto/src/`
    - identity, handshake, session crypto, and gateway encryption
- `crates/pim-protocol/src/`
    - frame formats, fragmentation, and stream framing
- `crates/pim-routing/src/lib.rs`
    - route computation and advertisement handling
- `crates/pim-gateway/src/lib.rs`
    - gateway NAT and internet-edge behavior

## Common Task Routing

- Config or defaults change:
  start in `crates/pim-core/src/config.rs`, then inspect the consuming crate.
- Daemon behavior change:
  start in `crates/pim-daemon/src/main.rs`, then follow the called subsystem.
- CLI behavior change:
  start in `crates/pim-cli/src/main.rs`.
- Wire-format or compatibility change:
  start in `crates/pim-protocol/src/` and inspect handshake or transport consumers.
- Crypto or session behavior change:
  start in `crates/pim-crypto/src/` and inspect daemon integration.
- Routing behavior change:
  start in `crates/pim-routing/src/lib.rs` and inspect route handling in the daemon.
- Host networking behavior change:
  start in `crates/pim-tun/src/lib.rs` and CLI or daemon callers.
- Multi-node runtime regression:
  inspect `docker/compose/`, `docker/configs/`, and `docker/tests/`.

## Validation Map

- Crate logic:
  `cargo test --workspace`
- Full-image build:
  `make docker-build`
- End-to-end phases:
  `make test-single-hop` through `make test-multi-gateway`

For broader repository context, use `core/DOCS_MAP.md` to navigate `./docs`
before adding new summary material under `.agent/`.

Use the domain-specific skills or prompts only after locating the owning layer.
