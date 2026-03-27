# Testing Guide

## Overview

PIM testing is organized in three tiers:

| Tier | Scope | Runs in | Speed |
|------|-------|---------|-------|
| Unit tests | Single function / struct | `cargo test` | Fast (ms) |
| Component tests | Single crate, mocked dependencies | `cargo test -p <crate>` | Fast (ms) |
| Integration tests | Multi-node, real networking | Docker Compose | Slow (seconds) |

Every feature must have unit tests. Features involving networking or TUN devices must also have Docker integration tests.

---

## Unit and Component Tests

### Conventions

- Test files live alongside source code: `src/foo.rs` has tests in `#[cfg(test)] mod tests { ... }` at the bottom
- Test helpers shared within a crate go in `src/test_utils.rs` (behind `#[cfg(test)]`)
- Test helpers shared across crates go in `crates/pim-core/src/test_utils.rs` (feature-gated with `features = ["test-utils"]`)
- Use `assert_eq!`, `assert!(matches!(...))`, and `#[should_panic]` where appropriate
- Use `tokio::test` for async tests

### Example: Crypto Unit Test

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_produces_matching_session_keys() {
        let alice = Identity::generate();
        let bob = Identity::generate();

        let alice_hs = Handshaker::new(&alice);
        let bob_hs = Handshaker::new(&bob);

        let init = alice_hs.initiate();
        let response = bob_hs.respond(&init).unwrap();
        let alice_session = alice_hs.finalize(&response).unwrap();
        let bob_session = bob_hs.finalize_from_init(&init).unwrap();

        assert_eq!(alice_session.key_bytes(), bob_session.key_bytes());
    }

    #[test]
    fn tampered_ciphertext_fails_decryption() {
        let cipher = SessionCipher::new(&random_key());
        let encrypted = cipher.encrypt(b"hello world");
        let mut tampered = encrypted.clone();
        tampered.ciphertext[0] ^= 0xFF;
        assert!(cipher.decrypt(&tampered).is_err());
    }
}
```

### Example: Protocol Codec Test

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use bytes::BytesMut;

    #[test]
    fn mesh_data_frame_round_trip() {
        let frame = MeshDataFrame {
            src_id: NodeId::random(),
            dst_id: NodeId::random(),
            session_id: 42,
            ttl: 10,
            flags: DataFlags::IS_INTERNET,
            payload: vec![0xDE, 0xAD, 0xBE, 0xEF],
        };

        let mut buf = BytesMut::new();
        frame.encode(&mut buf);

        let decoded = MeshDataFrame::decode(&mut buf).unwrap();
        assert_eq!(frame.src_id, decoded.src_id);
        assert_eq!(frame.session_id, decoded.session_id);
        assert_eq!(frame.payload, decoded.payload);
    }

    #[test]
    fn decode_rejects_truncated_frame() {
        let mut buf = BytesMut::from(&[0x50, 0x4D, 0x01][..]);  // magic + version, truncated
        assert!(TransportFrame::decode(&mut buf).is_err());
    }
}
```

### Example: Routing Unit Test

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_shorter_route() {
        let mut table = RoutingTable::new();
        let gateway = NodeId::random();
        let relay_a = NodeId::random();
        let relay_b = NodeId::random();

        // Learn gateway via relay_a (2 hops)
        table.apply_update(&relay_a, &route_update(gateway, 2, true));
        assert_eq!(table.lookup(&gateway).unwrap().hops, 3); // 2 + 1

        // Learn gateway via relay_b (1 hop) — shorter
        table.apply_update(&relay_b, &route_update(gateway, 1, true));
        assert_eq!(table.lookup(&gateway).unwrap().hops, 2); // 1 + 1
        assert_eq!(table.lookup(&gateway).unwrap().next_hop, relay_b);
    }

    #[test]
    fn split_horizon_excludes_learned_routes() {
        let mut table = RoutingTable::new();
        let gateway = NodeId::random();
        let relay = NodeId::random();

        table.apply_update(&relay, &route_update(gateway, 1, true));

        let advert = table.generate_advertisement_for(&relay);
        assert!(!advert.entries.iter().any(|e| e.destination == gateway));
    }
}
```

---

## Docker Integration Tests

### Architecture

Each PIM node runs in its own Docker container. Containers are connected via Docker bridge networks that simulate Wi-Fi Direct links. The PIM daemon uses `TcpTransport` inside Docker (since Wi-Fi Direct isn't available in containers).

```
┌─────────────────────────────────────────────────────────┐
│                    Docker Compose                        │
│                                                          │
│  ┌──────────┐    net_ab    ┌──────────┐    net_bd    ┌──────────┐
│  │ client-a │◄────────────►│ relay-b  │◄────────────►│gateway-d │
│  │          │              │          │              │          │──► internet
│  └──────────┘              └──────────┘              └──────────┘
│                                │
│                            net_bc
│                                │
│                           ┌──────────┐
│                           │ client-c │
│                           └──────────┘
│                                                          │
└─────────────────────────────────────────────────────────┘
```

Docker networks simulate point-to-point links. By giving each link its own network, we control exactly which nodes can see each other — mirroring the physical topology of Wi-Fi Direct range.

### Dockerfile

```dockerfile
FROM rust:1.87-slim AS builder

WORKDIR /src
COPY . .
RUN cargo build --workspace --release

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    iproute2 \
    iptables \
    iputils-ping \
    curl \
    dnsutils \
    tcpdump \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /src/target/release/pim-daemon /usr/local/bin/pim-daemon
COPY --from=builder /src/target/release/pim-cli /usr/local/bin/pim

ENTRYPOINT ["pim-daemon"]
```

### Docker Compose Topologies

All topology files live in `tests/docker/`. Each file defines a different test scenario.

#### `tests/docker/compose-2node.yml` — Single-Hop

```yaml
services:
  gateway:
    build: ../..
    container_name: pim-gateway
    cap_add:
      - NET_ADMIN      # required for TUN and iptables
    sysctls:
      - net.ipv4.ip_forward=1
    volumes:
      - ./configs/gateway.toml:/etc/pim/config.toml:ro
    command: ["--config", "/etc/pim/config.toml"]
    networks:
      mesh_link:
        ipv4_address: 172.20.0.10
      internet:    # gateway has internet access

  client:
    build: ../..
    container_name: pim-client
    cap_add:
      - NET_ADMIN
    volumes:
      - ./configs/client.toml:/etc/pim/config.toml:ro
    command: ["--config", "/etc/pim/config.toml"]
    networks:
      mesh_link:
        ipv4_address: 172.20.0.11
    # no 'internet' network — client has no direct internet

networks:
  mesh_link:
    driver: bridge
    ipam:
      config:
        - subnet: 172.20.0.0/24
  internet:
    driver: bridge
```

#### `tests/docker/compose-4node.yml` — Multi-Hop with Alternate Paths

```yaml
services:
  gateway:
    build: ../..
    container_name: pim-gateway
    cap_add: [NET_ADMIN]
    sysctls: ["net.ipv4.ip_forward=1"]
    volumes: ["./configs/gateway.toml:/etc/pim/config.toml:ro"]
    command: ["--config", "/etc/pim/config.toml"]
    networks:
      link_rg:
        ipv4_address: 172.21.0.10
      internet: {}

  relay:
    build: ../..
    container_name: pim-relay
    cap_add: [NET_ADMIN]
    volumes: ["./configs/relay.toml:/etc/pim/config.toml:ro"]
    command: ["--config", "/etc/pim/config.toml"]
    networks:
      link_cr:
        ipv4_address: 172.22.0.10
      link_rg:
        ipv4_address: 172.21.0.11

  relay2:
    build: ../..
    container_name: pim-relay2
    cap_add: [NET_ADMIN]
    volumes: ["./configs/relay2.toml:/etc/pim/config.toml:ro"]
    command: ["--config", "/etc/pim/config.toml"]
    networks:
      link_cr2:
        ipv4_address: 172.23.0.10
      link_rg:
        ipv4_address: 172.21.0.12

  client:
    build: ../..
    container_name: pim-client
    cap_add: [NET_ADMIN]
    volumes: ["./configs/client.toml:/etc/pim/config.toml:ro"]
    command: ["--config", "/etc/pim/config.toml"]
    networks:
      link_cr:
        ipv4_address: 172.22.0.11
      link_cr2:
        ipv4_address: 172.23.0.11

networks:
  link_cr:
    driver: bridge
    ipam:
      config: [{subnet: 172.22.0.0/24}]
  link_cr2:
    driver: bridge
    ipam:
      config: [{subnet: 172.23.0.0/24}]
  link_rg:
    driver: bridge
    ipam:
      config: [{subnet: 172.21.0.0/24}]
  internet:
    driver: bridge
```

This topology gives the client two paths to the gateway:
```
client → relay  → gateway
client → relay2 → gateway
```

### Writing Integration Tests

Integration tests are Rust tests in `tests/integration/` that shell out to `docker compose` to manage the test topology. They use the `bollard` crate (Docker API client) or simple `std::process::Command` calls.

#### Test Harness Pattern

```rust
// tests/integration/helpers/mod.rs

use std::process::Command;
use std::time::Duration;
use std::thread;

pub struct MeshTestbed {
    compose_file: String,
    project_name: String,
}

impl MeshTestbed {
    pub fn new(compose_file: &str) -> Self {
        let project_name = format!("pim-test-{}", rand::random::<u16>());
        Self {
            compose_file: compose_file.to_string(),
            project_name,
        }
    }

    /// Start all containers. Blocks until they're running.
    pub fn up(&self) {
        let status = Command::new("docker")
            .args(["compose", "-f", &self.compose_file, "-p", &self.project_name, "up", "-d", "--build"])
            .status()
            .expect("failed to run docker compose up");
        assert!(status.success(), "docker compose up failed");

        // Wait for daemons to initialize
        thread::sleep(Duration::from_secs(5));
    }

    /// Execute a command inside a running container and return stdout.
    pub fn exec(&self, service: &str, cmd: &[&str]) -> String {
        let output = Command::new("docker")
            .args(["compose", "-f", &self.compose_file, "-p", &self.project_name, "exec", "-T", service])
            .args(cmd)
            .output()
            .expect("failed to exec in container");
        String::from_utf8_lossy(&output.stdout).to_string()
    }

    /// Execute a command, return (exit_code, stdout, stderr).
    pub fn exec_full(&self, service: &str, cmd: &[&str]) -> (i32, String, String) {
        let output = Command::new("docker")
            .args(["compose", "-f", &self.compose_file, "-p", &self.project_name, "exec", "-T", service])
            .args(cmd)
            .output()
            .expect("failed to exec in container");
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).to_string(),
            String::from_utf8_lossy(&output.stderr).to_string(),
        )
    }

    /// Stop and remove a single service (simulates node failure).
    pub fn kill(&self, service: &str) {
        Command::new("docker")
            .args(["compose", "-f", &self.compose_file, "-p", &self.project_name, "kill", service])
            .status()
            .expect("failed to kill service");
    }

    /// Restart a previously killed service.
    pub fn restart(&self, service: &str) {
        Command::new("docker")
            .args(["compose", "-f", &self.compose_file, "-p", &self.project_name, "up", "-d", service])
            .status()
            .expect("failed to restart service");
        thread::sleep(Duration::from_secs(3));
    }

    /// Add network partition between two services via iptables.
    pub fn partition(&self, service: &str, block_ip: &str) {
        self.exec(service, &["iptables", "-A", "INPUT", "-s", block_ip, "-j", "DROP"]);
        self.exec(service, &["iptables", "-A", "OUTPUT", "-d", block_ip, "-j", "DROP"]);
    }

    /// Remove network partition.
    pub fn heal_partition(&self, service: &str, block_ip: &str) {
        self.exec(service, &["iptables", "-D", "INPUT", "-s", block_ip, "-j", "DROP"]);
        self.exec(service, &["iptables", "-D", "OUTPUT", "-d", block_ip, "-j", "DROP"]);
    }

    /// Get container logs for a service.
    pub fn logs(&self, service: &str) -> String {
        let output = Command::new("docker")
            .args(["compose", "-f", &self.compose_file, "-p", &self.project_name, "logs", service])
            .output()
            .expect("failed to get logs");
        String::from_utf8_lossy(&output.stdout).to_string()
    }
}

impl Drop for MeshTestbed {
    fn drop(&mut self) {
        // Always clean up containers
        let _ = Command::new("docker")
            .args(["compose", "-f", &self.compose_file, "-p", &self.project_name, "down", "-v", "--remove-orphans"])
            .status();
    }
}
```

#### Example: Single-Hop Internet Access Test

```rust
// tests/integration/phase1_single_hop.rs

mod helpers;
use helpers::MeshTestbed;

#[test]
fn client_reaches_internet_through_gateway() {
    let testbed = MeshTestbed::new("tests/docker/compose-2node.yml");
    testbed.up();

    // Verify mesh connectivity: client can ping gateway's mesh IP
    let (code, stdout, _) = testbed.exec_full("client", &["ping", "-c", "3", "-W", "2", "10.77.0.1"]);
    assert_eq!(code, 0, "client cannot ping gateway mesh IP: {}", stdout);

    // Verify internet access: client can curl through the mesh
    let (code, stdout, stderr) = testbed.exec_full("client", &["curl", "-s", "-o", "/dev/null", "-w", "%{http_code}", "--max-time", "10", "http://example.com"]);
    assert_eq!(stdout.trim(), "200", "expected HTTP 200, got: {} (stderr: {})", stdout, stderr);

    // Verify DNS works through mesh
    let (code, stdout, _) = testbed.exec_full("client", &["dig", "+short", "example.com"]);
    assert_eq!(code, 0, "DNS resolution failed: {}", stdout);
    assert!(!stdout.trim().is_empty(), "DNS returned empty result");
}

#[test]
fn daemon_shuts_down_cleanly() {
    let testbed = MeshTestbed::new("tests/docker/compose-2node.yml");
    testbed.up();

    // Send SIGTERM to daemon inside client container
    testbed.exec("client", &["pim", "down"]);

    // Verify TUN interface is removed
    let (_, stdout, _) = testbed.exec_full("client", &["ip", "link", "show", "pim0"]);
    assert!(stdout.contains("does not exist") || stdout.is_empty(),
        "pim0 should be removed after shutdown");
}
```

#### Example: Multi-Hop and Route Failover Test

```rust
// tests/integration/phase2_multi_hop.rs

mod helpers;
use helpers::MeshTestbed;
use std::thread;
use std::time::Duration;

#[test]
fn client_reaches_internet_through_relay() {
    let testbed = MeshTestbed::new("tests/docker/compose-4node.yml");
    testbed.up();

    // Client has no direct link to gateway — must go through relay
    let (code, stdout, stderr) = testbed.exec_full("client",
        &["curl", "-s", "-o", "/dev/null", "-w", "%{http_code}", "--max-time", "15", "http://example.com"]);
    assert_eq!(stdout.trim(), "200", "HTTP through multi-hop failed: {} {}", stdout, stderr);
}

#[test]
fn traffic_reroutes_on_relay_failure() {
    let testbed = MeshTestbed::new("tests/docker/compose-4node.yml");
    testbed.up();

    // Verify initial connectivity
    let (code, _, _) = testbed.exec_full("client",
        &["curl", "-s", "-o", "/dev/null", "-w", "%{http_code}", "--max-time", "10", "http://example.com"]);
    assert_eq!(code, 0, "initial connectivity failed");

    // Kill relay (client should failover to relay2)
    testbed.kill("relay");

    // Wait for heartbeat timeout + route reconvergence
    thread::sleep(Duration::from_secs(20));

    // Traffic should now flow through relay2
    let (code, stdout, _) = testbed.exec_full("client",
        &["curl", "-s", "-o", "/dev/null", "-w", "%{http_code}", "--max-time", "15", "http://example.com"]);
    assert_eq!(stdout.trim(), "200", "failover to relay2 failed");
}

#[test]
fn relay_cannot_read_e2e_payload() {
    let testbed = MeshTestbed::new("tests/docker/compose-4node.yml");
    testbed.up();

    // Start tcpdump on relay, capturing mesh traffic
    testbed.exec("relay", &["sh", "-c",
        "timeout 10 tcpdump -i any -w /tmp/capture.pcap port 9100 &"]);

    // Send a known payload through the mesh
    testbed.exec("client", &["curl", "-s", "--max-time", "8", "http://example.com"]);

    thread::sleep(Duration::from_secs(3));

    // Read the capture and verify the payload is encrypted
    let (_, stdout, _) = testbed.exec_full("relay",
        &["sh", "-c", "xxd /tmp/capture.pcap | grep -c 'example.com' || echo 0"]);
    assert_eq!(stdout.trim(), "0", "relay could see plaintext payload — E2E encryption failed");
}
```

#### Example: Discovery and Mesh Join Test

```rust
// tests/integration/phase3_discovery.rs

mod helpers;
use helpers::MeshTestbed;
use std::thread;
use std::time::Duration;

#[test]
fn client_auto_discovers_and_joins_mesh() {
    let testbed = MeshTestbed::new("tests/docker/compose-2node.yml");

    // Start only the gateway first
    // (would need a modified compose or start services individually)
    testbed.up();

    // Give discovery time
    thread::sleep(Duration::from_secs(10));

    // Client should have a mesh IP
    let (_, stdout, _) = testbed.exec_full("client", &["pim", "status"]);
    assert!(stdout.contains("10.77."), "client did not receive mesh IP: {}", stdout);

    // Client should see the gateway as a peer
    let (_, stdout, _) = testbed.exec_full("client", &["pim", "peers"]);
    assert!(stdout.contains("gateway"), "client did not discover gateway: {}", stdout);
}

#[test]
fn node_rejoins_after_restart() {
    let testbed = MeshTestbed::new("tests/docker/compose-2node.yml");
    testbed.up();

    // Kill client
    testbed.kill("client");
    thread::sleep(Duration::from_secs(5));

    // Restart client
    testbed.restart("client");

    // Wait for re-discovery
    thread::sleep(Duration::from_secs(10));

    // Should be back online
    let (code, stdout, _) = testbed.exec_full("client",
        &["curl", "-s", "-o", "/dev/null", "-w", "%{http_code}", "--max-time", "10", "http://example.com"]);
    assert_eq!(stdout.trim(), "200", "client did not rejoin mesh after restart");
}
```

#### Example: Chaos / Resilience Test

```rust
// tests/integration/phase4_resilience.rs

mod helpers;
use helpers::MeshTestbed;
use std::thread;
use std::time::Duration;

#[test]
fn recovers_from_temporary_network_partition() {
    let testbed = MeshTestbed::new("tests/docker/compose-4node.yml");
    testbed.up();

    // Verify initial connectivity
    let (_, stdout, _) = testbed.exec_full("client",
        &["curl", "-s", "-o", "/dev/null", "-w", "%{http_code}", "--max-time", "10", "http://example.com"]);
    assert_eq!(stdout.trim(), "200");

    // Partition: block traffic between relay and gateway
    testbed.partition("relay", "172.21.0.10");

    thread::sleep(Duration::from_secs(5));

    // Traffic should reroute through relay2
    let (_, stdout, _) = testbed.exec_full("client",
        &["curl", "-s", "-o", "/dev/null", "-w", "%{http_code}", "--max-time", "15", "http://example.com"]);
    assert_eq!(stdout.trim(), "200", "did not reroute during partition");

    // Heal partition
    testbed.heal_partition("relay", "172.21.0.10");
    thread::sleep(Duration::from_secs(20));

    // Both paths should work again — verify routing table shows both relays
    let (_, stdout, _) = testbed.exec_full("client", &["pim", "routes"]);
    assert!(stdout.contains("relay") && stdout.contains("relay2"),
        "routing table did not recover: {}", stdout);
}
```

---

## Running Tests

### Unit Tests

```bash
cargo test --workspace
```

### Integration Tests (Docker)

```bash
# Build images first
docker compose -f tests/docker/compose-2node.yml build

# Run all integration tests
cargo test -p integration-tests

# Run a specific test
cargo test -p integration-tests -- client_reaches_internet_through_gateway

# Run with output (see container logs on failure)
cargo test -p integration-tests -- --nocapture
```

### CI Pipeline

```yaml
# .github/workflows/test.yml
jobs:
  unit-tests:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo test --workspace

  integration-tests:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo test -p integration-tests
    services:
      docker:
        image: docker:dind
```

---

## Test Topology Quick Reference

| Compose File | Nodes | Topology | Used For |
|---|---|---|---|
| `compose-2node.yml` | client, gateway | Single link | Phase 1: basic tunnel, encryption, NAT |
| `compose-3node.yml` | client, relay, gateway | Linear chain | Phase 2: multi-hop, forwarding, E2E |
| `compose-4node.yml` | client, relay, relay2, gateway | Diamond (2 paths) | Phase 2-4: failover, rerouting, resilience |
| `compose-5node-2gw.yml` | client, relay, gw1, gw2, relay2 | Two gateways | Phase 5: multi-gateway, load balancing |

---

## Debugging Failed Tests

### Container Logs

```bash
docker compose -f tests/docker/compose-4node.yml -p pim-test logs relay
```

### Shell Into a Running Container

```bash
docker compose -f tests/docker/compose-4node.yml -p pim-test exec client bash
```

### Inspect Mesh State Inside a Container

```bash
pim status          # connection state, mesh IP
pim peers           # discovered peers
pim routes          # routing table
ip addr show pim0   # TUN interface
ip route            # OS routing table
tcpdump -i pim0     # packets on TUN
tcpdump -i eth0 port 9100  # mesh frames on transport
```

### Network Simulation

Inside containers, use `tc` (traffic control) to simulate real-world conditions:

```bash
# Add 50ms latency to a link
tc qdisc add dev eth0 root netem delay 50ms

# Add 10% packet loss
tc qdisc add dev eth0 root netem loss 10%

# Add both
tc qdisc add dev eth0 root netem delay 50ms loss 5%

# Remove
tc qdisc del dev eth0 root
```
