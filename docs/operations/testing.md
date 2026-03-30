# Testing

PIM uses two practical test layers in this repository: Rust tests for crate-level behavior and Docker-based end-to-end scenarios for multi-node networking.

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

## Notes

- Docker scenarios are the source of truth for current end-to-end behavior in this repository.
- Some phase-4 checks are intentionally slow; `make test-p4` skips the longest timeout path by default, while `make test-p4-full` runs it.
- If you need failing container logs automatically, run a script with `DUMP_LOGS_ON_FAIL=1`.
