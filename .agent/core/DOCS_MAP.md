# Docs Map

Use this file to navigate the broader repository documentation under `./docs`.
The `.agent/` folder should stay lightweight and refer to these docs instead of
duplicating them.

## Start Here

- `README.md`
    - current project scope
    - local build, install, and usage flow
- `docs/README.md`
    - top-level index for the documentation tree

## Architecture

- `docs/architecture/overview.md`
    - high-level system model and component relationships
- `docs/architecture/packet-flow.md`
    - packet path and runtime behavior
- `docs/architecture/protocol.md`
    - wire protocol framing and message structure
- `docs/architecture/routing.md`
    - routing behavior and forwarding model
- `docs/architecture/discovery.md`
    - peer discovery behavior
- `docs/architecture/security.md`
    - identity, trust, and crypto model
- `docs/architecture/transports/bluetooth.md`
    - Bluetooth PAN transport: NAP/PANU roles, bridge/DHCP setup, coexistence
- `docs/architecture/transports/wifi-direct.md`
    - current connectivity-specific reference design

## Project Structure

- `docs/project/workspace.md`
    - crate responsibilities, repository layout, and per-crate type signatures
- `docs/project/history.md`
    - historical phased delivery log; forward-looking items live in roadmap.md
- `docs/project/roadmap.md`
    - broader direction and future work

## Getting Started

- `docs/getting-started/installation.md`
    - install prerequisites and setup
- `docs/getting-started/configuration.md`
    - configuration model and examples
- `docs/getting-started/usage.md`
    - runtime and operator workflow
- `docs/getting-started/example-topology.md`
    - sample node layouts and roles

## Operations

- `docs/operations/testing.md`
    - Cargo and Docker validation workflow
- `docs/operations/docker-labs.md`
    - Docker lab details and usage

## Usage Guidance

- For implementation tasks, read the nearest section in `./docs` before editing.
- For review tasks, compare the code against the relevant docs and call out drift.
- If a topic is already covered in `./docs`, prefer linking to it over rewriting the same background in `.agent/`.
