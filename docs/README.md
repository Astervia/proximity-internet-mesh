# Documentation

This folder is organized by purpose so operators, contributors, and reviewers can find the right level of detail quickly.

## Getting Started

- [installation.md](getting-started/installation.md): host requirements, build flow, and local installation
- [configuration.md](getting-started/configuration.md): config file reference and example node configurations
- [usage.md](getting-started/usage.md): actual CLI commands and runtime files
- [example-topology.md](getting-started/example-topology.md): three-node walkthrough and packet flow example

## Architecture

- [system-overview.md](architecture/system-overview.md): high-level runtime model and major components
- [packet-flow.md](architecture/packet-flow.md): how packets move through the daemon today
- [networking-and-routing.md](architecture/networking-and-routing.md): topology and route propagation model
- [protocol.md](architecture/protocol.md): frame layout and wire-level behavior
- [security.md](architecture/security.md): identities, handshake, transport encryption, and end-to-end protection

## Operations

- [testing.md](operations/testing.md): unit, component, and integration test strategy
- [docker-testing.md](operations/docker-testing.md): Docker Compose labs, scripts, and troubleshooting

## Project Internals

- [workspace.md](project/workspace.md): crate-by-crate responsibilities
- [workspace-layout.md](project/workspace-layout.md): workspace structure and source ownership
- [roadmap.md](project/roadmap.md): phased delivery view
- [implementation-plan.md](project/implementation-plan.md): detailed checklist of planned and completed work

## Reading Order

If you are new to the project, read in this order:

1. [../README.md](../README.md)
2. [installation.md](getting-started/installation.md)
3. [configuration.md](getting-started/configuration.md)
4. [usage.md](getting-started/usage.md)
5. [system-overview.md](architecture/system-overview.md)

If you are changing code, add [workspace.md](project/workspace.md) and [testing.md](operations/testing.md) next.
