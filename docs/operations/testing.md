# Testing

PIM uses two practical test layers in this repository: Rust tests for crate-level behavior and Docker-based end-to-end scenarios for multi-node networking.

GitHub Actions also validates the supported macOS path separately from the Linux Docker labs. The CI matrix builds and tests the workspace on both Linux and macOS, then runs `scripts/test-config-generators.sh` to confirm the generated config guidance matches the host platform.

## Test Layers

### Unit and crate-level tests

Run with Cargo:

```bash
cargo test --workspace
```

These cover logic that does not require a full network lab, including:

- protocol encode/decode behavior
- cryptographic handshakes and ciphertext validation
- routing-table update rules
- fragmentation and reassembly
- gateway translation logic

The convenience target is:

```bash
make test-unit
```

### Docker integration tests

Run through the `Makefile`:

```bash
make test-p1
make test-p2
make test-p3
make test-p4
make test-p5
make test-debug-cli
make test-route-cli
make test-bluetooth
make test-all
```

These tests use the files under:

- `docker/compose/`
- `docker/configs/`
- `docker/tests/`

They validate real daemon startup, TUN interface handling, peer connectivity, routing convergence, NAT, failover, resilience, and multi-gateway behavior. They also cover hardware-adjacent seams that can be simulated inside a container, such as the Bluetooth fake-sysfs path.

## Phase Coverage

- `test-p1`: single-hop client to gateway path
- `test-p2`: relay forwarding, routing convergence, and failover
- `test-p3`: discovery and peer lifecycle
- `test-p4`: resilience, buffering, congestion, and slow NAT timeout checks
- `test-p5`: dual-gateway failover and gateway selection behavior
- `test-debug-cli`: `pim debug` output from the client view in the dual-gateway Docker lab
- `test-route-cli`: `pim route on|status|off` flow in the single-hop Docker lab
- `test-bluetooth`: Bluetooth PAN automatic peer discovery seam using fake sysfs and `ip neigh` fixtures in Docker

## Local Workflow

Fast feedback loop:

```bash
make test-unit
```

Broader validation after daemon, protocol, routing, or gateway changes:

```bash
make docker-build
make test-p2
```

Hardware-adjacent connectivity work without real radios:

```bash
make test-bluetooth
```

Full regression pass:

```bash
make test-all
```

## Manual Inspection

For interactive debugging, start a stack and inspect containers directly:

```bash
make up-p1
make logs-p1
make sh-p1-client
pim status --verbose
make down-p1
```

## macOS Validation

The supported macOS scope is narrower than Linux:

- supported: client, relay, and gateway roles, native `utunN` interface naming, config generation, workspace build, and unit tests
- not supported: Wi-Fi Direct and Docker lab workflows

CI covers the supported macOS build and config-generation path, but host-level runtime validation still needs a real macOS machine with the privileges required to create a TUN interface and update routes. Bluetooth PAN on macOS also needs the host Bluetooth stack plus `blueutil` for radio discovery and pairing automation.

Recommended manual smoke checks on macOS:

```bash
cargo build --workspace --release
cargo test --workspace
scripts/test-config-generators.sh
sudo target/release/pim config generate client --output /tmp/pim-client.toml --force
sudo target/release/pim config generate relay --output /tmp/pim-relay.toml --force
sudo target/release/pim config generate gateway --output /tmp/pim-gateway.toml --force
sudo target/release/pim up --config /tmp/pim-client.toml
sudo target/release/pim status --verbose
sudo target/release/pim down
```

Gateway smoke check on macOS:

```bash
sudo target/release/pim-daemon --config /tmp/pim-gateway.toml
```

That command should bring up the TUN plus gateway dataplane on macOS when the host has a usable uplink interface and the privileges needed for `pfctl` and `sysctl`.

## Notes

- Docker scenarios are the source of truth for current end-to-end behavior in this repository.
- Some phase-4 checks are intentionally slow; `make test-p4` skips the longest timeout path by default, while `make test-p4-full` runs it.
- If you need failing container logs automatically, run a script with `DUMP_LOGS_ON_FAIL=1`.
