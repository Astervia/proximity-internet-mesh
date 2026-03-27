# Implementation Phases

## Phase 1 — Single-Hop Tunnel (Foundation)

**Goal**: Two devices, one client and one gateway, connected via Wi-Fi Direct. IP packets flow from client through gateway to the internet.

This phase builds the vertical slice: every layer exists, but in its simplest form.

### Deliverables

1. **TUN interface management**
   - Create/destroy `pim0` TUN device
   - Read outbound IP packets from TUN fd
   - Write inbound IP packets to TUN fd
   - Assign static mesh IP and set up routing

2. **Transport over Wi-Fi Direct**
   - Establish a Wi-Fi Direct connection between two devices
   - Framed TCP or UDP socket over the Wi-Fi Direct link
   - Send/receive `TransportFrame` with length-prefixed framing

3. **Basic encryption**
   - Generate Ed25519 identity keypair on first run
   - Perform the handshake protocol between the two peers
   - Derive session key, encrypt/decrypt transport frames with AES-256-GCM

4. **Packet forwarding (gateway)**
   - Receive IP packets from the mesh
   - NAT using iptables/nftables (source NAT on the gateway's internet interface)
   - Return response packets through the mesh

5. **CLI**
   - `pim up` — start daemon, create TUN, connect to peer
   - `pim down` — tear down
   - `pim status` — show connection state

### Architecture (Phase 1)

```
Client                                   Gateway
┌──────────┐     Wi-Fi Direct       ┌──────────┐
│ pim0 TUN │◄─── encrypted ────────►│ pim0 TUN │
│          │     frames              │          │
│ daemon   │                        │ daemon   │──► internet
└──────────┘                        └──────────┘
```

### Key Decisions

- **Transport protocol**: TCP over Wi-Fi Direct's P2P socket for reliability, or UDP + custom reliability for lower latency. Start with TCP for simplicity.
- **NAT approach**: Use Linux `iptables` MASQUERADE on the gateway. The Rust daemon calls out to configure iptables rules.
- **TUN crate**: Use the `tun` crate (or `tun-tap`) for TUN device creation on Linux.

### Testing

- Two Linux machines (or VMs) with Wi-Fi Direct capability
- Fallback: TCP over localhost for development (mock transport)
- Test: `curl` through the mesh reaches the internet

---

## Phase 2 — Multi-Hop Relay

**Goal**: Introduce relay nodes. A client can reach a gateway through one or more intermediate relay hops.

### Deliverables

1. **Packet forwarding at relay nodes**
   - Receive mesh frames not destined for this node
   - Decrement TTL, look up next hop, re-encrypt, forward
   - Drop frames with TTL = 0

2. **Distance-vector routing**
   - Periodic route advertisement broadcasts
   - Route table construction and maintenance
   - Split horizon / poison reverse
   - Gateway-oriented route selection

3. **End-to-end encryption**
   - Client encrypts payload to gateway's public key
   - Relay nodes forward without decrypting inner payload
   - Gateway decrypts E2E layer after decrypting transport layer

4. **Multi-peer transport**
   - Transport Manager handles connections to multiple peers simultaneously
   - Route frames to the correct peer socket based on next-hop lookup

5. **Fragmentation and reassembly**
   - Fragment large IP packets before mesh transmission
   - Reassemble at destination node
   - Timeout and discard incomplete assemblies

### Architecture (Phase 2)

```
Client          Relay           Relay           Gateway
┌──────┐       ┌──────┐       ┌──────┐       ┌──────┐
│ pim  │◄─────►│ pim  │◄─────►│ pim  │◄─────►│ pim  │──► internet
└──────┘       └──────┘       └──────┘       └──────┘
  hop 1          hop 2          hop 3
```

### Testing

- 3-4 nodes (can be VMs or containers with mock transport)
- Verify: client can reach internet through 2+ hops
- Verify: removing a relay triggers route recalculation
- Verify: relay cannot read E2E-encrypted payloads

---

## Phase 3 — Discovery and Mesh Join

**Goal**: Nodes automatically discover nearby peers and join the mesh without manual configuration.

### Deliverables

1. **Peer discovery**
   - Broadcast presence advertisements over Wi-Fi Direct
   - Listen for advertisements from other nodes
   - Build and maintain peer table with capabilities

2. **Automatic mesh join**
   - On `pim up`, discover peers, handshake, receive routes
   - Request IP address from gateway
   - Configure TUN interface automatically
   - Full flow: start adapter → connect to network → using PIM

3. **Peer lifecycle management**
   - Heartbeat-based liveness detection
   - Graceful departure (`Goodbye` message)
   - Timeout-based removal of dead peers
   - Triggered route updates on peer join/leave

4. **Wi-Fi Direct group management**
   - Automatic Group Owner negotiation
   - Multi-group support for bridging mesh segments
   - Reconnection on group disruption

### Testing

- Start gateway, then start clients at different times
- Verify: clients auto-discover and join
- Verify: removing a node updates routing across the mesh
- Kill a node ungracefully → verify timeout-based cleanup

---

## Phase 4 — Reliability and Performance

**Goal**: Harden the system for real-world use. Improve throughput, latency, and resilience.

### Deliverables

1. **Connection management**
   - Automatic reconnection on transport failure
   - Backoff and retry with jitter
   - Connection pooling for high-traffic peers

2. **Store-and-forward**
   - Buffer frames when next hop is temporarily unavailable
   - Bounded queue with configurable timeout
   - Priority: control frames > data frames

3. **Flow control and backpressure**
   - Limit concurrent sessions per peer
   - TCP-like windowing for mesh-level flow control (optional)
   - Drop oldest/lowest-priority frames under congestion

4. **NAT improvements**
   - Connection tracking in userspace (reduce iptables dependency)
   - Handle connection expiry and cleanup
   - Support for UDP, TCP, and ICMP

5. **Performance optimization**
   - Zero-copy packet handling where possible
   - Batch frame sending
   - Minimize allocations in hot path
   - Profile and optimize crypto overhead

6. **Metrics and observability**
   - Track: packets forwarded, bytes transferred, active sessions, route count
   - Expose via `pim status --verbose`
   - Optional: Prometheus endpoint for monitoring

### Testing

- Stress test: sustained throughput through 3-hop path
- Chaos test: randomly kill/restart nodes during traffic
- Latency test: measure round-trip time vs hop count

---

## Phase 5 — Multi-Gateway and Load Balancing

**Goal**: Support multiple gateways. Distribute traffic for better performance and resilience.

### Deliverables

1. **Multi-gateway routing**
   - Routing table tracks multiple gateways and their distances
   - Select gateway based on: hop count, load, latency
   - Failover: if preferred gateway becomes unreachable, switch to next best

2. **Load-aware routing**
   - Gateways advertise current load in heartbeats
   - Clients consider load when selecting a gateway
   - Adaptive: shift traffic away from overloaded gateways

3. **Split traffic**
   - Different flows (by destination IP/port) can use different gateways
   - Enables parallel internet access through multiple gateways

4. **Gateway health monitoring**
   - Periodic end-to-end latency probes through each gateway
   - Detect degraded internet connectivity on a gateway
   - Deprioritize gateways with poor internet connectivity

---

## Phase 6 — Security Hardening

**Goal**: Protect against active attacks and adversarial nodes.

### Deliverables

1. **Peer reputation system**
   - Track packet delivery success rate per peer
   - Deprioritize or blacklist peers with suspicious behavior
   - Detect selective forwarding attacks

2. **Rate limiting**
   - Limit frames per second from any single peer
   - Prevent resource exhaustion attacks
   - Gateway: limit outbound requests per client

3. **Routing security**
   - Verify routing update signatures
   - Detect and reject routing anomalies (e.g., a node claiming 0 hops to everything)
   - Sequence number validation to prevent replay of old routes

4. **Optional: Onion routing**
   - Multi-layer encryption where each relay only knows prev/next hop
   - Protects metadata (who is talking to which gateway)
   - Significant performance tradeoff — optional feature

---

## Phase Summary

| Phase | Focus | Key Outcome |
|-------|-------|-------------|
| 1 | Single-hop tunnel | Two devices, IP packets flow through mesh to internet |
| 2 | Multi-hop relay | Packets traverse relay nodes, E2E encryption works |
| 3 | Discovery & join | Automatic mesh formation, seamless "connect to network" UX |
| 4 | Reliability | Production-grade resilience, performance, observability |
| 5 | Multi-gateway | Load balancing, failover, parallel internet access |
| 6 | Security hardening | Reputation, rate limiting, anti-attack measures |

## Development Environment

For phases 1-3, development can proceed using **TCP over localhost** as a mock transport, with Wi-Fi Direct integration tested separately. This allows rapid iteration without needing physical devices for every test.

```
Mock transport stack (dev):
  TUN ↔ Daemon ↔ TCP localhost ↔ Daemon ↔ TUN

Production transport stack:
  TUN ↔ Daemon ↔ Wi-Fi Direct ↔ Daemon ↔ TUN
```
