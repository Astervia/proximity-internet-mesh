# Agent Conventions

These conventions are specific to this repository and should guide agent work.

## General Rules

1. Preserve crate boundaries unless the task clearly justifies reshaping them.
2. Prefer the narrowest viable change over cross-workspace rewrites.
3. Keep shared types, config models, and error vocabulary in `pim-core`.
4. Keep transport, protocol, crypto, routing, gateway, discovery, and CLI concerns separated.
5. Put runtime orchestration in `pim-daemon`, not in leaf crates.
6. Keep operator-facing behavior explicit in config and docs.
7. Default new features to opt-in when they change runtime behavior or dependencies.

## Repo Design Bias

The repository is organized around:

- small crates with clear responsibilities
- a daemon that composes those crates into the running system
- Linux-first runtime assumptions for TUN, routing, and gateway behavior
- Docker-based end-to-end validation for multi-node behavior

When changing code, look for the existing layer that should own the behavior:

- `pim-core` for config, shared types, and common errors
- `pim-protocol` for wire-format changes
- `pim-crypto` for identity, handshake, and encryption behavior
- `pim-transport` for direct peer link behavior
- `pim-discovery` for peer-finding behavior
- `pim-routing` for path selection and advertisements
- `pim-gateway` for NAT and internet-edge behavior
- `pim-tun` for host-network integration
- `pim-daemon` for service wiring and async runtime coordination
- `pim-cli` for operator workflows

## Change Strategy

For most tasks:

1. Trace the current path end to end before editing.
2. Identify the owning crate and the integration points around it.
3. Change the smallest public surface that solves the task.
4. Preserve backward compatibility unless the task explicitly requires a break.
5. Use `./docs` for broader project context instead of recreating that context in `.agent/`.
6. Update docs when behavior, config, or operator workflow changes.

## Testing Expectations

At minimum, changes should include:

- focused crate-level tests for the changed behavior
- workspace-level verification when integration surfaces change
- explicit note of any gaps when full validation is not practical

Prefer:

- `cargo test --workspace` for general regression coverage
- targeted crate tests during iteration
- Docker phase tests when daemon, routing, discovery, transport, or gateway behavior changes

## Documentation Expectations

Update the closest user-facing or architecture-facing document when the behavior changes:

- `README.md` for install, runtime, or operator workflow changes
- `docs/project/` for workspace structure or implementation-plan changes
- `docs/architecture/` for protocol, packet flow, transport, discovery, routing, or security changes
- `docs/operations/` for testing, deployment, or debugging workflow changes

When looking for context before editing, start with `.agent/core/DOCS_MAP.md`
and follow into `./docs` rather than expanding the agent folder with duplicate
background material.
