# Contributing

Thanks for your interest in `proximity-internet-mesh` (PIM). This guide covers
the local development loop, branch and commit conventions, and the test gates
a change must pass before merging.

## Prerequisites

- Rust toolchain capable of building the workspace (see `rust-toolchain.toml`
  if present, or the Rust version pinned in CI at `1.94.0`).
- Linux for full integration coverage (Docker labs); macOS supports the unit
  test suite and can run the daemon directly.
- See [docs/getting-started/installation.md](docs/getting-started/installation.md) for
  optional per-feature host packages (BlueZ, `wpa_supplicant`, `pfctl`, etc.).

## Local Loop

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

The `scripts/test-config-generators.sh` helper is also run in CI and should
pass locally:

```bash
scripts/test-config-generators.sh
```

Run the Docker integration suite (Linux only):

```bash
make docker-build
make test-all
```

Or run a single phase:

```bash
make test-single-hop          # phase 1 – single-hop connectivity
make test-ipv6        # phase 1 IPv6 variant
make test-multi-hop          # phase 2 – relay / routing
make test-peer-discovery          # phase 3 – discovery
make test-resilience          # phase 4 – resilience (SKIP_SLOW=1 by default)
make test-multi-gateway          # phase 5 – multi-gateway
make test-auto-discovery          # phase 7 – auto-discovery
make test-auto-ip-chain          # phase 8 – auto IP chain
make test-auth        # authorization flows
make test-debug-cli   # debug CLI smoke tests
make test-route-cli   # route CLI smoke tests
make test-bluetooth   # Bluetooth seam
make test-bluetooth-enx  # Bluetooth ENX seam
```

To run phase 4 with the full 6-minute NAT timeout test:

```bash
make test-resilience-full
```

## Branches

- `main` — protected; releases are cut from here.
- `feature/<short-name>` — new features.
- `fix/<short-name>` — bug fixes.
- `docs/<short-name>` — documentation-only changes.
- `chore/<short-name>` — build, CI, release machinery.

Work happens on a topic branch; a PR targets `main`.

## Commits

Use [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <imperative summary>

<body — wrap at 72 cols>
```

Types in active use: `feat`, `fix`, `docs`, `chore`, `refactor`, `test`,
`perf`, `style`.

Common scopes: `daemon`, `cli`, `rpc`, `protocol`, `routing`, `transport`,
`bluetooth`, `wifidirect`, `gateway`, `tun`, `scripts`, `tests`, `deps`,
`ci`, `release`.

## Pull Requests

A PR should:

- Link the issue it closes (`Closes #N`) when applicable.
- Include a "Test plan" section describing what was run.
- Pass all CI jobs triggered on `pull_request` to `main`:
  - `cargo fmt --all --check`
  - `cargo clippy --workspace --all-targets --locked -- -D warnings`
  - `cargo test --workspace --locked`
  - `scripts/test-config-generators.sh`
  - Platform validation on Linux and macOS
  - `cargo audit` (dependency security scan)
  - CodeQL SAST scan
- Keep `docs/` in sync — if behavior changes, update the relevant page under
  `docs/getting-started/`, `docs/architecture/`, or `docs/reference/`.

## Adding A New Connection Mechanism

Follow the workflow in
[`.agent/skills/add-connection-mechanism/SKILL.md`](.agent/skills/add-connection-mechanism/SKILL.md).
New transports must:

- live in their own crate under `crates/`,
- export an `async` interface compatible with `pim-transport`,
- be documented under `docs/architecture/transports/`,
- ship at least one Docker scenario or a hardware-test procedure.

## Reporting Issues And Disclosing Vulnerabilities

- Functional bugs: open a GitHub issue with reproduction steps and the daemon
  log captured with `RUST_LOG=info,pim=debug`.
- Security-sensitive issues: do not open a public issue. Open a private
  security advisory on GitHub instead.

## Code Of Conduct

Be respectful. Assume good intent. Disagree on technical merit, not on
identity. Maintainers reserve the right to remove comments or close threads
that violate this.
