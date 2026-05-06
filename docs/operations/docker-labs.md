# Docker Integration Testing Guide

Multi-node tests that require real network interfaces, TUN devices, and live
internet access cannot run in a standard `cargo test` context. This guide
covers how those tests are structured, how to run them, how to write new ones,
and tracks which scenarios are implemented. It also covers smaller
hardware-adjacent seam tests that run in Docker without requiring real radios.

## Prerequisites

| Tool              | Min version | Notes                                  |
| ----------------- | ----------- | -------------------------------------- |
| Docker Engine     | 24.x        | `docker --version`                     |
| Docker Compose v2 | 2.20        | `docker compose version`               |
| `tc` / `iproute2` | any         | only for 5.3 latency tests             |
| internet access   | —           | gateway containers NAT to the internet |

Containers need `CAP_NET_ADMIN` (and `CAP_NET_RAW` on gateways) to create TUN
devices and manipulate iptables. The compose files add these automatically.

## Repository Layout

```
Dockerfile                       — multi-stage Rust build + slim runtime image
Makefile                         — all test and stack-management targets
docker/
  entrypoint.sh                  — container entrypoint (runs pim-daemon)
  configs/
    gateway.toml                 — gateway node (mesh 10.77.0.1/24)
    gateway1.toml                — first gateway for phase-5 tests
    gateway2.toml                — second gateway for phase-5 tests
    relay.toml                   — relay → gateway
    relay1.toml                  — first relay for routing tests
    relay2.toml                  — second relay for routing tests
    relay-multigateway.toml      — relay → gateway1 + gateway2
    client.toml                  — client → gateway (direct)
    client-relay.toml            — client → relay (multi-hop)
    client-dual-relay.toml       — client → relay1 + relay2
    client2.toml                 — second client
  compose/
    bluetooth-seam.yml          — single-node Bluetooth fake-sysfs seam test
    bluetooth-seam-enx.yml      — single-node Bluetooth dynamic-enx fallback seam
    single-hop.yml        — gateway + client
    multi-hop-relay.yml             — gateway + relay + client
    multi-hop-routing.yml           — gateway + relay1 + relay2 + client
    peer-discovery.yml         — gateway + relay + client (peer lifecycle)
    resilience.yml        — gateway + client (network disruption)
    flow-control.yml      — gateway + flood-sender
    multi-gateway.yml      — gateway1 + gateway2 + relay + client
    mesh-broadcast.yml       — node-a → node-b → node-c → node-d chain
  tests/
    test-bluetooth.sh           — Bluetooth seam test runner
    test-bluetooth-enx.sh       — Bluetooth dynamic-enx seam test runner
    test-debug-cli.sh           — debug CLI output runner
    test-route-cli.sh           — split-default route CLI runner
    test-broadcast.sh           — daemon broadcast across a multi-hop chain
    test-messaging.sh           — pim-messaging plugin end-to-end (direct + routed)
    common.sh                    — shared assertion + JSON-RPC helpers
    test-single-hop.sh               — phase 1 test runner
    test-multi-hop.sh               — phase 2 test runner
    test-peer-discovery.sh               — phase 3 test runner
    test-resilience.sh               — phase 4 test runner
    test-multi-gateway.sh               — phase 5 test runner
docs/
  operations/docker-labs.md   — this file
```

## Network Topology

Each stack uses its own Docker bridge subnet for transport to avoid cross-phase
address collisions when multiple test networks exist on the same host.
The mesh uses `10.77.0.0/24` for TUN addresses.

| Role                                 | Transport IP                            | Mesh IP     | Config file                                |
| ------------------------------------ | --------------------------------------- | ----------- | ------------------------------------------ |
| gateway (phase 1 / 4)                | 172.30.0.10 / 172.34.0.10               | 10.77.0.1   | gateway.toml                               |
| gateway (phase 2 relay)              | 172.31.0.10                             | 10.77.0.1   | gateway.toml                               |
| gateway (phase 2 routing)            | 172.32.0.10                             | 10.77.0.1   | gateway.toml                               |
| gateway1 (phase 5)                   | 172.36.0.10                             | 10.77.0.1   | gateway1.toml                              |
| gateway2 (phase 5)                   | 172.36.0.11                             | 10.77.0.2   | gateway2.toml                              |
| relay (phase 2 relay / 5)            | 172.31.0.20 / 172.36.0.20               | 10.77.0.10  | relay.toml / relay-multigateway.toml       |
| relay1 (phase 2 routing / 3)         | 172.32.0.20 / 172.33.0.20               | 10.77.0.10  | relay1.toml / relay.toml                   |
| relay2 (phase 2 routing)             | 172.32.0.21                             | 10.77.0.11  | relay2.toml                                |
| client (phase 1 / 4 / 5)             | 172.30.0.30 / 172.34.0.30 / 172.36.0.30 | 10.77.0.100 | client.toml / client-relay.toml            |
| client (phase 2 relay / routing / 3) | 172.31.0.30 / 172.32.0.30 / 172.33.0.30 | 10.77.0.100 | client-relay.toml / client-dual-relay.toml |
| client2                              | 172.33.0.31                             | 10.77.0.101 | client2.toml                               |

### Phase 1 — Single-hop

```
  client (172.30.0.30)
    └──[TCP 9100]── gateway (172.30.0.10) ──── internet
  mesh: 10.77.0.100       mesh: 10.77.0.1
```

### Phase 2 — Multi-hop relay

```
  client ──[TCP 9100]── relay ──[TCP 9100]── gateway ──── internet
  10.77.0.100           10.77.0.10           10.77.0.1
```

### Phase 2 — Routing / failover (4 containers)

```
         ┌──── relay1 (10.77.0.10) ────┐
  client ┤                              ├── gateway ── internet
  .100   └──── relay2 (10.77.0.11) ────┘   .1
```

### Phase 5 — Multi-gateway

```
              ┌── gateway1 (10.77.0.1) ── internet
  client ── relay
              └── gateway2 (10.77.0.2) ── internet
  .100         .10
```

### Mesh broadcast / messaging — 4-node chain

```
  node-a ──[TCP]── node-b ──[TCP]── node-c ──[TCP]── node-d
  10.77.0.101      10.77.0.102      10.77.0.103      10.77.0.104
```

LAN UDP discovery is disabled. Each node only configures its left
neighbour as a static peer. Non-adjacent peers learn each other's
identity exclusively through routed `PeerInfo` broadcasts emitted by
the daemon's `messaging.broadcast` cycle, which is precisely what the
broadcast test validates.

## Quick Start

```bash
# Build the image once (cached on subsequent runs)
make docker-build

# Run all phases (slow tests skipped)
make test-all

# Run a single phase
make test-single-hop
make test-multi-hop
make test-debug-cli
make test-route-cli
make test-bluetooth
make test-broadcast
make test-messaging

# Interactive: bring up a stack and poke around
make up-single-hop
make sh-single-hop-client     # bash shell in the client container
pim status --verbose  # inside the container
make down-single-hop

# Tail logs while the stack is running
make logs-single-hop
```

Set `DUMP_LOGS_ON_FAIL=1` before running a test script to dump container logs
when an assertion fails:

```bash
DUMP_LOGS_ON_FAIL=1 make test-multi-hop
```

## Component Seam Tests

Not every Docker test needs a full mesh topology. For hardware-adjacent code,
prefer a smaller container scenario when the behavior under test is:

- startup wiring
- environment overrides
- fixture-driven readiness checks
- log or status-level handoff into the main daemon path

The Bluetooth seam tests follow this pattern. They start one daemon container,
mount a Bluetooth seam config, and set fake command paths for:

- `PIM_BLUETOOTH_BLUETOOTHCTL_COMMAND`
- `PIM_BLUETOOTH_BT_NETWORK_COMMAND`
- `PIM_BLUETOOTH_IP_COMMAND`
- `PIM_BLUETOOTH_SYSFS_ROOT`

The fake `bluetoothctl` script reports a nearby `PIM-` device, the fake
`bt-network` script marks the PAN interface as ready, and the fake `ip neigh`
script returns the resulting peer IP. The baseline seam keeps the PAN on
`bnep0`; the second seam leaves the configured `bnep0` unresolved and exposes a
live `enx*` interface instead. Together they assert that the daemon:

- radio-discovers and prepares a new Bluetooth peer
- auto-discovers the PAN neighbor IP
- hands the resulting address into the normal connection path
- falls back from a configured `bnep0` hint to a live `enx*` interface on Linux

## Debug CLI Test Lane

`make test-debug-cli` validates operator-facing output for the new `pim debug`
commands from the client container in `multi-gateway.yml`.

That single stack is enough to cover:

- direct peer visibility
- installed route visibility
- gateway ranking output
- route explanation for `internet`
- route explanation for a specific mesh IP
- non-empty discovery output, because discovery remains enabled by default

This keeps the test deterministic while still exercising a realistic mesh with
multiple gateways.

## Route CLI Test Lane

`make test-route-cli` reuses `single-hop.yml` to validate the operator
workflow around split-default routing from inside the client container:

- `pim route status` starts disabled
- `pim route on` installs both split-default routes
- `pim route status` reports the expected gateway and interface
- `pim route off` removes both routes again

This lane specifically protects the TUN and route-management behavior that the
daemon relies on after startup.

## Writing a New Docker Test

### 1. Add or reuse a compose file

If your test fits an existing topology, reuse the corresponding compose file.
If you need a new topology:

1. Copy the closest existing file in `docker/compose/`.
2. Adjust service names, configs, and IP addresses.
3. Keep the healthcheck pattern — tests use it to know when nodes are ready.
4. For seam tests, prefer a single service and drive fixtures through env vars or mounted files.

### 2. Add node configs if needed

Config files live in `docker/configs/`. Each file maps to one role. Use the
existing files as templates. Key rules:

- `mesh_ip` must be a static CIDR — `"auto"` is not supported.
- `key_file` should be `/var/lib/pim/node.key` (the data dir is writable).
- `[[peers]]` addresses use Docker service hostnames (`gateway:9100`, etc.).

### 3. Write assertions in a test script

Source `common.sh` and call the helpers:

```bash
source "$(dirname "$0")/common.sh"

COMPOSE_FILE="my-scenario.yml"

# Start the stack and wait for all containers to be healthy.
start_stack "$COMPOSE_FILE"
wait_all_healthy "$COMPOSE_FILE" 120

# Connectivity assertions
assert_ping "$COMPOSE_FILE" client "10.77.0.1" "client pings gateway"
assert_curl "$COMPOSE_FILE" client "http://example.com" "client reaches internet"
assert_dns  "$COMPOSE_FILE" client "example.com" "client resolves DNS"

# TUN interface assertions
assert_iface_up   "$COMPOSE_FILE" client pim0
assert_iface_addr "$COMPOSE_FILE" client "10.77.0.100"

# pim status assertions
assert_cmd_output "$COMPOSE_FILE" "routes" \
    in_svc "$COMPOSE_FILE" client pim status --verbose

# Raw command in container
in_svc "$COMPOSE_FILE" client ping -c 5 10.77.0.1

# Print pass/fail summary (non-zero exit on failure)
print_summary
```

### 4. Always clean up

Wrap the test in a `trap cleanup EXIT` so stacks are torn down even on failure:

```bash
cleanup() {
    stop_stack "$COMPOSE_FILE"
}
trap cleanup EXIT
```

### 5. Register the test in the Makefile

Add a target that depends on `docker-build` and calls your script. Add it to
`test-all` if it should run in CI.

### 6. Prefer Docker seams for hardware-adjacent logic

If the feature normally depends on host hardware, add a narrow seam first:

- fake sysfs tree for interface readiness
- mounted fixture files for discovery input
- explicit environment override for paths
- log assertions proving the daemon handed control to the normal code path

This repository's Bluetooth test is the reference example.

## Timing Guidelines

Timers are driven by the daemon's background task intervals:

| Task                         | Interval        |
| ---------------------------- | --------------- |
| Discovery broadcast          | 5 s             |
| Heartbeat                    | 5 s             |
| Peer liveness check          | 5 s             |
| Peer timeout                 | 15 s (3 missed) |
| Route advertisement          | 5 s             |
| Route convergence (2 cycles) | ~10 s           |
| Reconnect backoff max        | 30 s            |
| Buffer flush                 | 50 ms           |
| Buffer GC                    | 10 s            |
| Gateway probes               | 10 s            |
| Conntrack GC                 | 30 s            |
| TCP idle timeout             | 300 s           |
| UDP idle timeout             | 30 s            |
| ICMP idle timeout            | 10 s            |
| Bluetooth seam operstate flip| ~2 s in Docker  |

When writing sleep statements, add ~5 s of headroom on top of the calculated
interval.

## Scenario Checklist

The following is the canonical test checklist, mirroring the implementation
plan. Status: **[x] implemented** / **[ ] pending**.

### Phase 1 — Single-Hop Tunnel

| Test | Scenario                                       | Status             |
| ---- | ---------------------------------------------- | ------------------ |
| 1.5  | Two containers communicate over bridge network | [x] test-single-hop.sh |
| 1.6  | TUN interface up with correct address          | [x] test-single-hop.sh |
| 1.7  | Gateway resolves external DNS and reaches HTTP | [x] test-single-hop.sh |
| 1.8  | Client pings gateway mesh IP                   | [x] test-single-hop.sh |
| 1.8  | Client curl http://example.com through mesh    | [x] test-single-hop.sh |
| 1.8  | Client HTTPS through mesh                      | [x] test-single-hop.sh |
| 1.8  | Client DNS resolution through mesh             | [x] test-single-hop.sh |
| 1.8  | Daemon exits cleanly on SIGTERM                | [x] test-single-hop.sh |
| 1.9  | pim status reports running state               | [x] test-single-hop.sh |

### Component Seams

| Test | Scenario                                                        | Status                |
| ---- | --------------------------------------------------------------- | --------------------- |
| BT.1 | fake `bluetoothctl` reports a nearby `PIM-` peer                  | [x] test-bluetooth.sh |
| BT.2 | fake `bt-network` marks the PAN interface ready                   | [x] test-bluetooth.sh |
| BT.3 | daemon auto-discovers PAN peer IPs from fixture `ip neigh` output | [x] test-bluetooth.sh |
| BT.4 | Bluetooth handoff reaches the normal connection-initiation path   | [x] test-bluetooth.sh |

### Phase 2 — Multi-Hop Relay

| Test | Scenario                                             | Status                        |
| ---- | ---------------------------------------------------- | ----------------------------- |
| 2.1  | client → relay → gateway, curl internet              | [x] test-multi-hop.sh            |
| 2.1  | Frame with TTL=1 arriving at relay is dropped        | [ ] pending                   |
| 2.2  | Relay TUN carries only encrypted frames (pcap check) | [x] test-multi-hop.sh            |
| 2.3  | Routing table visible on each node (pim status)      | [x] test-multi-hop.sh            |
| 2.3  | Kill relay1 → traffic reroutes via relay2            | [x] test-multi-hop.sh            |
| 2.4  | 10 KB payload transferred intact                     | [x] test-multi-hop.sh (loopback) |
| 2.5  | 4-container full mesh connectivity                   | [x] test-multi-hop.sh            |

### Phase 3 — Discovery and Mesh Join

| Test | Scenario                                              | Status             |
| ---- | ----------------------------------------------------- | ------------------ |
| 3.1  | 3 nodes discover each other within 2 broadcast cycles | [x] test-peer-discovery.sh |
| 3.2  | Client auto-discovers, gets IP, pings gateway         | [x] test-peer-discovery.sh |
| 3.2  | 3-container: client → relay → gateway, internet       | [x] test-peer-discovery.sh |
| 3.3  | Kill container → peers detect within 15 s             | [x] test-peer-discovery.sh |
| 3.3  | Restart killed container → rejoin mesh                | [x] test-peer-discovery.sh |

### Phase 4 — Reliability and Performance

| Test | Scenario                                                    | Status                           |
| ---- | ----------------------------------------------------------- | -------------------------------- |
| 4.1  | Network disconnect → reconnect → mesh recovers              | [x] test-resilience.sh               |
| 4.1  | Reconnection establishes new session key                    | [ ] pending                      |
| 4.2  | Buffer frames during 5 s outage → delivered after reconnect | [x] test-resilience.sh               |
| 4.3  | Flood sender → backpressure, bounded memory                 | [x] test-resilience.sh               |
| 4.4  | TCP idle 4 min → still alive                                | [x] test-resilience.sh (SKIP_SLOW=0) |
| 4.4  | TCP idle 6 min → conntrack expired, new connection works    | [x] test-resilience.sh (SKIP_SLOW=0) |

### Phase 5 — Multi-Gateway and Load Balancing

| Test | Scenario                                               | Status                           |
| ---- | ------------------------------------------------------ | -------------------------------- |
| 5.1  | Kill preferred gateway → failover to gateway2          | [x] test-multi-gateway.sh               |
| 5.1  | Restore gateway1 → routing re-converges                | [x] test-multi-gateway.sh               |
| 5.2  | Saturate gateway1 → new flows observed on gateway2     | [x] test-multi-gateway.sh (heuristic)   |
| 5.3  | tc netem latency on gateway1 → client prefers gateway2 | [x] test-multi-gateway.sh (requires tc) |

### Daemon broadcast (mesh-essential identity hygiene)

| Test | Scenario                                                                | Status                  |
| ---- | ----------------------------------------------------------------------- | ----------------------- |
| BC.1 | 4-node chain converges so every node has 3 routes                        | [x] test-broadcast.sh   |
| BC.2 | Direct neighbour x25519 lands in `peers.list` after the Noise handshake | [x] test-broadcast.sh   |
| BC.3 | `peers.broadcast_identity_now` reports recipients = routes − 1           | [x] test-broadcast.sh   |
| BC.4 | After broadcast, `messages.send` to a 3-hop peer succeeds (keystore)     | [x] test-broadcast.sh   |
| BC.5 | Broadcast keystore round-trip is symmetric (`d → a` also works)          | [x] test-broadcast.sh   |

### pim-messaging plugin (end-to-end)

| Test | Scenario                                                                  | Status                  |
| ---- | ------------------------------------------------------------------------- | ----------------------- |
| MS.1 | `messages.send` to a direct neighbour → recipient decrypts the body       | [x] test-messaging.sh   |
| MS.2 | `messages.send` across 3 hops → routed delivery + ack                      | [x] test-messaging.sh   |
| MS.3 | Sender row transitions `pending` → `sent` → `delivered` after the ack      | [x] test-messaging.sh   |
| MS.4 | `messages.list_conversations` enriches rows with x25519 + name from keystore | [x] test-messaging.sh |
| MS.5 | `peers.forget` fans `on_peer_forgotten` and wipes the conversation         | [x] test-messaging.sh   |
| MS.6 | `messages.send` rejects empty bodies and unknown peers cleanly             | [x] test-messaging.sh   |

## Pending / Future Scenarios

The following scenarios are not yet automated and require manual validation or
additional tooling:

- **TTL drop verification**: inject a frame with TTL=1 at a relay and confirm
  it is dropped (requires a raw-frame injection tool or modified test binary).
- **Reconnect session key**: verify that after a reconnect the old session key
  can no longer decrypt messages (requires crypto-level introspection).
- **Packet capture cross-container**: assert that a pcap on the relay contains
  no plaintext IP headers while the client pings the internet.
- **Dynamic IP assignment** (Phase 3.2): the daemon currently requires a static
  `mesh_ip`; once `mesh_ip = "auto"` is supported the gateway IP-pool tests can
  be automated.

## Debugging Tips

### Daemon won't start

```bash
make up-single-hop
docker logs pim-single-hop-gw          # read daemon startup errors
docker exec -it pim-single-hop-gw bash
cat /etc/pim/pim.toml           # verify config was mounted
```

### TUN interface not coming up

The container needs `CAP_NET_ADMIN`. Verify it is present:

```bash
docker inspect pim-single-hop-gw | grep CapAdd
```

Also check that the kernel TUN module is loaded on the host:

```bash
lsmod | grep tun
# if missing:
modprobe tun
```

### Ping through mesh fails

1. Check that both TUN interfaces are UP: `ip link show pim0`
2. Check routing: `ip route show` — there should be a route to `10.77.0.0/24` via `pim0`
3. Tail the daemon log for errors: `RUST_LOG=debug make up-single-hop` then `make logs-single-hop`
4. Run `pim status --verbose` inside each container

### Container unhealthy / health check failing

The healthcheck waits for `/run/pim.pid` to exist and `pim0` to be UP.
Common causes of failure:

- Config file not mounted (volume path wrong)
- Missing `CAP_NET_ADMIN`
- Peer address unresolvable (hostname typo in `[[peers]]`)

### Viewing live stats

```bash
docker exec pim-single-hop-client pim status --verbose
# or watch it:
docker exec pim-single-hop-client bash -c 'while true; do pim status --verbose; sleep 5; done'
```

### Running a single assertion manually

```bash
source docker/tests/common.sh
assert_ping "single-hop.yml" client "10.77.0.1" "manual ping test"
print_summary
```
