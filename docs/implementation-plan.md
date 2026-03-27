# Implementation Plan

Actionable checklist organized by phase. Every item must have corresponding tests before it's considered done. See [testing-guide.md](testing-guide.md) for multi-node testing with Docker.

---

## Phase 1 — Single-Hop Tunnel

Two containers: one client, one gateway. Client sends IP packets through the mesh, gateway NATs them to the internet.

### 1.1 Project Scaffolding

- [ ] Initialize Cargo workspace with `crates/` layout
- [ ] Create crate skeletons: `pim-core`, `pim-crypto`, `pim-protocol`, `pim-transport`, `pim-tun`, `pim-gateway`, `pim-daemon`
- [ ] Set up workspace dependencies in root `Cargo.toml`
- [ ] Add `tracing` + `tracing-subscriber` logging to all crates
- [ ] Create `Dockerfile` and `docker-compose.yml` for multi-node testing
- [ ] **Tests**: workspace compiles, `cargo test --workspace` passes (empty tests)

### 1.2 Core Types (`pim-core`)

- [ ] `NodeId` — 16-byte identifier, derived from public key
- [ ] `MeshIp` — wrapper around `Ipv4Addr` for mesh-internal addresses
- [ ] `Config` — TOML-deserialized configuration struct
- [ ] `PimError` — unified error enum with `thiserror`
- [ ] `FrameCodec` trait — `encode(&self, buf) / decode(buf) -> Result<Self>`
- [ ] **Tests**: `NodeId` generation, `Config` round-trip serialization, error display

### 1.3 Cryptography (`pim-crypto`)

- [ ] `Identity::generate()` — create Ed25519 keypair, derive `NodeId`
- [ ] `Identity::save()` / `Identity::load()` — persist to / read from file
- [ ] `Handshaker::initiate()` — produce `HandshakeInit` (ephemeral X25519 key + Ed25519 signature)
- [ ] `Handshaker::respond()` — verify init, produce `HandshakeResponse`
- [ ] `Handshaker::finalize()` — derive shared secret via X25519, run HKDF-SHA256 → `SessionKey`
- [ ] `HandshakeConfirm` — HMAC-SHA256 transcript verification
- [ ] `SessionCipher::encrypt()` — AES-256-GCM with incrementing nonce counter
- [ ] `SessionCipher::decrypt()` — verify tag, decrypt
- [ ] Nonce counter overflow detection (reject after 2^32 frames)
- [ ] **Tests**:
  - [ ] Identity generate → save → load round-trip
  - [ ] Full handshake between two identities produces matching session keys
  - [ ] Encrypt → decrypt round-trip
  - [ ] Tampered ciphertext fails decryption
  - [ ] Replayed nonce is rejected
  - [ ] Wrong key fails decryption
  - [ ] Nonce counter overflow triggers error

### 1.4 Wire Protocol (`pim-protocol`)

- [ ] `TransportFrame` — magic, version, frame_type, length, nonce, encrypted payload, tag
- [ ] `MeshDataFrame` — src_id, dst_id, session_id, ttl, flags, payload
- [ ] `HandshakeFrame` — init / response / confirm variants
- [ ] `ControlFrame` — IpRequest, IpAssign, Goodbye, Ping/Pong
- [ ] `HeartbeatFrame` — sender_id, timestamp, gateway_hops, load
- [ ] `FrameCodec` implementations for all frame types
- [ ] Length-prefixed framing for stream transports (TCP): 4-byte length prefix before each `TransportFrame`
- [ ] **Tests**:
  - [ ] Encode → decode round-trip for every frame type
  - [ ] Reject truncated frames
  - [ ] Reject invalid magic / version
  - [ ] Reject frames exceeding max size
  - [ ] Fuzz: random bytes don't panic decoder (returns error)
  - [ ] Verify exact byte layout against protocol spec

### 1.5 TCP Transport (`pim-transport`)

- [ ] `Transport` trait — async `send`, `recv`, `connect`, `disconnect`, `connected_peers`
- [ ] `TcpTransport` — implementation for development and Docker testing
  - [ ] Listener mode: bind, accept connections, perform framed reads
  - [ ] Client mode: connect to address, perform framed writes
  - [ ] Bidirectional: every connection handles both send and recv
  - [ ] Connection tracking: map `NodeId` → TCP stream
  - [ ] Graceful disconnect and cleanup
- [ ] **Tests**:
  - [ ] Two `TcpTransport` instances send/recv frames over loopback
  - [ ] Connect → disconnect → reconnect lifecycle
  - [ ] Multiple concurrent connections
  - [ ] Sending to disconnected peer returns error
  - [ ] **Docker**: two containers communicate over bridge network

### 1.6 TUN Interface (`pim-tun`)

- [ ] `TunInterface::create(name)` — create TUN device via ioctl
- [ ] `TunInterface::set_ip(addr, mask)` — configure IP address
- [ ] `TunInterface::set_mtu(mtu)` — set MTU
- [ ] `TunInterface::up()` / `down()` — bring interface up/down
- [ ] `TunInterface::read_packet()` — async read from TUN fd
- [ ] `TunInterface::write_packet()` — async write to TUN fd
- [ ] OS route configuration: set default route via gateway's mesh IP
- [ ] **Tests** (require `CAP_NET_ADMIN` — run in Docker):
  - [ ] Create TUN → verify interface exists via `ip link`
  - [ ] Set IP → verify via `ip addr`
  - [ ] Write packet into TUN → read it back from TUN (loopback test)
  - [ ] Destroy TUN → verify interface removed

### 1.7 Gateway / NAT Engine (`pim-gateway`)

- [ ] `GatewayEngine::new(internet_iface)` — initialize NAT engine
- [ ] `translate_outbound(packet)` — rewrite source IP/port, add to conntrack
- [ ] `translate_inbound(packet)` — look up conntrack, rewrite destination IP/port
- [ ] Conntrack table with TTL-based expiry
- [ ] `cleanup_expired()` — periodic conntrack sweep
- [ ] iptables/nftables MASQUERADE rule setup (shell out to `iptables`)
- [ ] TCP, UDP, ICMP support
- [ ] **Tests**:
  - [ ] Outbound translation creates conntrack entry, rewrites source
  - [ ] Inbound response matches conntrack, rewrites destination
  - [ ] Expired entries are cleaned up
  - [ ] Unknown inbound packet (no conntrack) is dropped
  - [ ] **Docker**: gateway container can NAT traffic from mesh IP to external DNS (e.g., resolve `example.com`)

### 1.8 Daemon — Single-Hop Assembly (`pim-daemon`)

- [ ] Wire together: TUN + Transport + Crypto + Gateway
- [ ] TUN read loop: read IP packet → encrypt (hop-by-hop) → send via transport
- [ ] Transport recv loop: receive frame → decrypt → write to TUN (or forward to gateway)
- [ ] Gateway path: decrypt → NAT outbound → send to internet → NAT inbound → encrypt → send back
- [ ] Graceful shutdown via `CancellationToken` + signal handler
- [ ] Static configuration: peer address, mesh IPs, roles hardcoded in config
- [ ] **Tests**:
  - [ ] **Docker (2 containers)**: client pings gateway's mesh IP → pong received
  - [ ] **Docker (2 containers)**: client runs `curl http://example.com` through mesh → 200 response
  - [ ] **Docker (2 containers)**: client DNS resolution works through mesh
  - [ ] Daemon shuts down cleanly on SIGTERM (TUN removed, connections closed)

### 1.9 CLI (`pim-cli`)

- [ ] `pim up --config <path>` — start daemon in foreground (or background with `--daemon`)
- [ ] `pim down` — send SIGTERM to running daemon
- [ ] `pim status` — connect to daemon (unix socket or pid file), show state
- [ ] **Tests**:
  - [ ] `pim up` creates TUN interface and starts listening
  - [ ] `pim down` removes TUN interface
  - [ ] `pim status` reports connection state accurately

---

## Phase 2 — Multi-Hop Relay

### 2.1 Packet Forwarding

- [ ] Relay logic: if `dst_id != self.node_id` → decrement TTL, look up next hop, re-encrypt, forward
- [ ] TTL enforcement: drop frame and log when TTL reaches 0
- [ ] Forwarding metrics: count packets forwarded, dropped
- [ ] **Tests**:
  - [ ] **Docker (3 containers)**: client → relay → gateway, client curls internet successfully
  - [ ] Frame with TTL=1 arriving at relay is dropped (not forwarded)
  - [ ] Frame with TTL=0 is dropped immediately
  - [ ] Relay does not decrypt E2E payload (verify via instrumentation)

### 2.2 End-to-End Encryption

- [ ] `e2e_encrypt(ip_packet, gateway_public_key)` — encrypt payload to gateway
- [ ] `e2e_decrypt(e2e_frame, gateway_private_key)` — gateway decrypts
- [ ] Client obtains gateway public key during initial connection (exchanged in config or discovery)
- [ ] Relay nodes pass E2E-encrypted payload through without modification
- [ ] **Tests**:
  - [ ] E2E encrypt → decrypt round-trip
  - [ ] Relay cannot decrypt E2E payload (attempt with relay's key fails)
  - [ ] **Docker (3 containers)**: packet capture on relay shows only encrypted payload
  - [ ] Tampered E2E payload is rejected by gateway

### 2.3 Distance-Vector Routing (`pim-routing`)

- [ ] `RoutingTable::apply_update()` — process incoming route advertisement
- [ ] `RoutingTable::generate_advertisement()` — produce advertisement for neighbors
- [ ] `RoutingTable::nearest_gateway()` — find shortest path to any gateway
- [ ] `RoutingTable::lookup(dst)` — find next hop for destination
- [ ] `RoutingTable::expire_stale(max_age)` — remove old entries
- [ ] `RoutingTable::remove_routes_via(peer)` — invalidate routes through a dead peer
- [ ] Split horizon: don't advertise a route back to the peer it was learned from
- [ ] Poison reverse: advertise route with hops=infinity to the source peer
- [ ] Periodic advertisement broadcast (configurable interval)
- [ ] Triggered updates on topology change
- [ ] **Tests**:
  - [ ] 3-node chain: routes converge to correct hop counts
  - [ ] Remove middle node: routes through it are invalidated
  - [ ] Add new node: routing table updates within one advertisement cycle
  - [ ] Routing loop detection: A→B→C→A is prevented by TTL and split horizon
  - [ ] Gateway preference: client selects route with fewest hops to a gateway
  - [ ] Stale route expiry after configured timeout
  - [ ] **Docker (4 containers)**: verify routing table state on each node via `pim routes`
  - [ ] **Docker (4 containers)**: kill a relay mid-test → traffic reroutes via alternate path

### 2.4 Fragmentation and Reassembly

- [ ] Fragment large IP packets exceeding mesh MTU
- [ ] Fragment header: fragment_id, fragment_offset, total_length
- [ ] `IS_FRAGMENT` and `IS_LAST_FRAGMENT` flags in data frame
- [ ] Reassembly buffer: collect fragments, reorder, deliver when complete
- [ ] Reassembly timeout: discard incomplete fragments after 10 seconds
- [ ] **Tests**:
  - [ ] Fragment a 4000-byte packet into 3 fragments, reassemble → matches original
  - [ ] Out-of-order fragments are reassembled correctly
  - [ ] Duplicate fragment is ignored
  - [ ] Missing fragment triggers timeout and discard
  - [ ] **Docker (2 containers)**: transfer a 10KB payload through mesh → arrives intact

### 2.5 Multi-Peer Transport

- [ ] `TcpTransport` handles multiple concurrent peer connections
- [ ] Connection map: `NodeId → TcpStream`
- [ ] Send to specific peer by `NodeId`
- [ ] Receive from any connected peer, tagged with sender `NodeId`
- [ ] **Tests**:
  - [ ] Node with 3 connected peers can send/recv to/from each independently
  - [ ] Disconnecting one peer doesn't affect others
  - [ ] **Docker (4 containers)**: full mesh connectivity verified

---

## Phase 3 — Discovery and Mesh Join

### 3.1 Peer Discovery (`pim-discovery`)

- [ ] `DiscoveryService::broadcast_presence()` — send advertisement over transport
- [ ] `DiscoveryService::handle_advertisement()` — add/update peer table
- [ ] `PeerTable` — tracks peers with capabilities, last_seen, public keys
- [ ] Advertisement message: node_id, public_key, capabilities (client/relay/gateway), listen address
- [ ] Periodic broadcast at configurable interval
- [ ] **Tests**:
  - [ ] Two nodes discover each other via advertisements
  - [ ] Capabilities are correctly parsed and stored
  - [ ] **Docker (3 containers)**: all three discover each other within 2 broadcast cycles

### 3.2 Automatic Mesh Join

- [ ] On `pim up`: discover → handshake → receive routes → request IP → configure TUN
- [ ] IP request/assign via `ControlFrame` (IpRequest / IpAssign)
- [ ] Gateway assigns IP from its pool, tracks leases
- [ ] Client configures TUN with assigned IP and routes
- [ ] **Tests**:
  - [ ] **Docker (2 containers)**: start gateway, then start client → client auto-discovers, gets IP, can ping gateway
  - [ ] **Docker (3 containers)**: start gateway, start relay, start client → client discovers relay, relay has route to gateway, client reaches internet
  - [ ] IP assignment is unique per client
  - [ ] Client reconnects and gets the same or new IP

### 3.3 Peer Lifecycle

- [ ] Heartbeat send/receive between direct peers (5s interval)
- [ ] Peer timeout after 3 missed heartbeats (15s)
- [ ] `Goodbye` message on graceful shutdown
- [ ] Route table cleanup when a peer is removed
- [ ] Triggered route updates on peer join/leave
- [ ] **Tests**:
  - [ ] Peer sending heartbeats stays in peer table
  - [ ] Stopped peer is removed after timeout
  - [ ] `Goodbye` triggers immediate removal
  - [ ] **Docker (4 containers)**: kill a container → other nodes detect loss within 15s, routes update
  - [ ] **Docker (3 containers)**: restart killed container → re-discovers and rejoins mesh

---

## Phase 4 — Reliability and Performance

### 4.1 Connection Resilience

- [ ] Auto-reconnect on transport failure with exponential backoff + jitter
- [ ] Detect connection loss via heartbeat timeout or socket error
- [ ] Re-handshake after reconnection (new session key)
- [ ] **Tests**:
  - [ ] **Docker**: `iptables -A DROP` between two containers, then remove → reconnects automatically
  - [ ] Reconnection establishes new session key (old key doesn't work)
  - [ ] Backoff increases between retries

### 4.2 Store-and-Forward

- [ ] Bounded buffer for frames when next hop is temporarily unavailable
- [ ] Priority queue: control frames > route updates > data frames
- [ ] Configurable timeout (default: 30s)
- [ ] **Tests**:
  - [ ] Buffer frames during 5s outage → delivered after reconnect
  - [ ] Buffer overflow drops lowest priority frames first
  - [ ] Frames buffered beyond timeout are dropped

### 4.3 Flow Control

- [ ] Backpressure: limit concurrent in-flight frames per peer
- [ ] Drop policy under congestion (tail-drop or priority-based)
- [ ] **Tests**:
  - [ ] Flood sender → receiver applies backpressure, no OOM
  - [ ] Priority traffic survives congestion

### 4.4 NAT Improvements

- [ ] Userspace conntrack (remove iptables dependency for NAT logic)
- [ ] Handle TCP, UDP, ICMP correctly
- [ ] Connection expiry: TCP 5min idle, UDP 30s idle, ICMP 10s
- [ ] **Tests**:
  - [ ] **Docker**: TCP connection through mesh, idle for 4 min → still works; idle for 6 min → conntrack expired
  - [ ] ICMP ping through mesh works
  - [ ] UDP DNS queries through mesh work

### 4.5 Observability

- [ ] `pim status --verbose` — peer count, route count, packets forwarded, bytes transferred
- [ ] Structured tracing with `tracing` crate (spans for per-packet lifecycle)
- [ ] **Tests**:
  - [ ] After sending 100 packets, metrics report ~100 forwarded
  - [ ] `pim status` output is parseable and accurate

---

## Phase 5 — Multi-Gateway and Load Balancing

### 5.1 Multi-Gateway Routing

- [ ] Routing table tracks multiple gateways
- [ ] `nearest_gateway()` considers: hop count, load, latency
- [ ] Failover: switch to next-best gateway if current becomes unreachable
- [ ] **Tests**:
  - [ ] **Docker (5 containers, 2 gateways)**: client prefers closer gateway
  - [ ] **Docker**: kill preferred gateway → traffic fails over to second gateway
  - [ ] **Docker**: restore first gateway → traffic may shift back (configurable)

### 5.2 Load-Aware Routing

- [ ] Gateways report load in heartbeats
- [ ] Clients factor load into gateway selection
- [ ] **Tests**:
  - [ ] **Docker (2 gateways)**: saturate one gateway → new flows go to the other

### 5.3 Gateway Health Probes

- [ ] Periodic latency probes through each gateway
- [ ] Detect degraded internet on a gateway (high latency, packet loss)
- [ ] **Tests**:
  - [ ] **Docker**: add latency to one gateway's internet (`tc netem`) → clients prefer the other

---

## Phase 6 — Security Hardening

### 6.1 Routing Security

- [ ] Verify Ed25519 signatures on all routing updates
- [ ] Reject updates with invalid or missing signatures
- [ ] Sequence number validation (reject old/replayed updates)
- [ ] Anomaly detection: reject unreasonable claims (0 hops to everything)
- [ ] **Tests**:
  - [ ] Forged routing update is rejected
  - [ ] Replayed old update (old sequence number) is rejected
  - [ ] Node claiming 0 hops to all destinations is deprioritized

### 6.2 Rate Limiting

- [ ] Per-peer frame rate limit
- [ ] Gateway: per-client request rate limit
- [ ] **Tests**:
  - [ ] **Docker**: flood node with frames → excess frames are dropped, node stays healthy
  - [ ] Gateway limits a single client's outbound rate

### 6.3 Peer Reputation

- [ ] Track delivery success rate per peer
- [ ] Deprioritize peers with high drop rates
- [ ] Blacklist after sustained bad behavior
- [ ] **Tests**:
  - [ ] Peer that drops 50% of forwarded frames gets deprioritized
  - [ ] Blacklisted peer's routes are not used

---

## Acceptance Criteria Summary

Every checklist item above must meet these criteria before being marked done:

1. **Unit tests pass** — covering the happy path, edge cases, and error conditions
2. **Integration tests pass** — multi-container Docker tests where applicable
3. **No panics** — all error paths return `Result`, no `unwrap()` in non-test code
4. **Logging** — key operations emit `tracing` events at appropriate levels
5. **Documentation** — public API has doc comments
