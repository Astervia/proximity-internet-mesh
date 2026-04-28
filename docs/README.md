# Documentation

This folder is organized by purpose so operators, contributors, and reviewers
can find the right level of detail quickly.

## Getting Started

- [installation.md](getting-started/installation.md) — host requirements, build flow, prebuilt-archive install.
- [configuration.md](getting-started/configuration.md) — config file reference and worked examples.
- [usage.md](getting-started/usage.md) — CLI commands and runtime files.
- [example-topology.md](getting-started/example-topology.md) — three-node walkthrough and packet flow.
- [client-guide.md](getting-started/client-guide.md) — running a client node.
- [gateway-guide.md](getting-started/gateway-guide.md) — running a gateway node.

## Architecture

- [overview.md](architecture/overview.md) — high-level runtime model and major components.
- [packet-flow.md](architecture/packet-flow.md) — how packets move through the daemon.
- [routing.md](architecture/routing.md) — topology and route propagation model.
- [protocol.md](architecture/protocol.md) — frame layout and wire-level behavior.
- [security.md](architecture/security.md) — identities, handshake, transport encryption, end-to-end protection.
- [discovery.md](architecture/discovery.md) — peer discovery (UDP broadcast, Bonjour, Wi-Fi Direct scan).
- [transports/](architecture/transports/README.md) — Bluetooth PAN and Wi-Fi Direct backends.

## Operations

- [testing.md](operations/testing.md) — unit, component, and integration test strategy.
- [test-writing.md](operations/test-writing.md) — conventions for placing and organizing unit tests.
- [docker-labs.md](operations/docker-labs.md) — Compose labs, scripts, and lab troubleshooting.
- [bluetooth-gateway-shutdown.md](operations/bluetooth-gateway-shutdown.md) — controlled-shutdown procedure for Bluetooth gateways.

## Troubleshooting

- [README.md](troubleshooting/README.md) — index, scope, first-step daemon-stop guidance.
- [bluetooth-nap-bridge.md](troubleshooting/bluetooth-nap-bridge.md) — recover when `br-bt` already has the configured NAP bridge address.
- [bluetooth-dhcp-client.md](troubleshooting/bluetooth-dhcp-client.md) — `Bluetooth DHCP client unavailable` warning.

## Reference

- [README.md](reference/README.md) — reference index.
- [cli.md](reference/cli.md) — `pim` and `pim-daemon` CLI reference.
- [config-schema.md](reference/config-schema.md) — TOML schema, field-by-field.
- [platform-support.md](reference/platform-support.md) — Linux vs. macOS matrix.
- [config-examples/](reference/config-examples/) — sample TOML configs.

## Project Internals

- [workspace.md](project/workspace.md) — crate-by-crate inventory and dependency layers.
- [roadmap.md](project/roadmap.md) — phased delivery view (forward-looking).
- [history.md](project/history.md) — historical implementation log.

## Reading Paths

### Operator (running a node)

1. [../README.md](../README.md)
2. [getting-started/installation.md](getting-started/installation.md)
3. [getting-started/configuration.md](getting-started/configuration.md)
4. [getting-started/usage.md](getting-started/usage.md)
5. [reference/cli.md](reference/cli.md)
6. [troubleshooting/README.md](troubleshooting/README.md)

### Contributor (changing code)

1. [../CONTRIBUTING.md](../CONTRIBUTING.md)
2. [architecture/overview.md](architecture/overview.md)
3. [project/workspace.md](project/workspace.md)
4. [operations/testing.md](operations/testing.md)
5. [project/roadmap.md](project/roadmap.md)

### Reviewer (auditing security or protocol)

1. [architecture/overview.md](architecture/overview.md)
2. [architecture/protocol.md](architecture/protocol.md)
3. [architecture/security.md](architecture/security.md)
4. [reference/config-schema.md](reference/config-schema.md)
