# Implementation Plan

Actionable checklist organized by phase. Every item must have corresponding tests before it's considered done. See [testing-guide.md](testing-guide.md) for multi-node testing with Docker.

---

## Phase 1 — Single-Hop Tunnel

Two containers: one client, one gateway. Client sends IP packets through the mesh, gateway NATs them to the internet.

### 1.1 Project Scaffolding

- [x] Initialize Cargo workspace with `crates/` layout
- [x] Create crate skeletons: `pim-core`, `pim-crypto`, `pim-protocol`, `pim-transport`, `pim-tun`, `pim-gateway`, `pim-daemon`
- [x] Set up workspace dependencies in root `Cargo.toml`
- [x] Add `tracing` + `tracing-subscriber` logging to all crates
- [ ] Create `Dockerfile` and `docker-compose.yml` for multi-node testing
- [x] **Tests**: workspace compiles, `cargo test --workspace` passes (empty tests)

### 1.2 Core Types (`pim-core`)

- [x] `NodeId` — 16-byte identifier, derived from public key
- [x] `MeshIp` — wrapper around `Ipv4Addr` for mesh-internal addresses
- [x] `Config` — TOML-deserialized configuration struct (includes `PeerConfig`)
- [x] `PimError` — unified error enum with `thiserror`
- [x] `FrameCodec` trait — `encode(&self, buf) / decode(buf) -> Result<Self>`
- [x] **Tests**: `NodeId` generation, `Config` round-trip serialization, error display

### 1.3 Cryptography (`pim-crypto`)

- [x] `Identity::generate()` — create Ed25519 keypair, derive `NodeId`
- [x] `Identity::save()` / `Identity::load()` — persist to / read from file
- [x] `Handshaker::initiate()` — produce `HandshakeInit` (ephemeral X25519 key + Ed25519 signature)
- [x] `Handshaker::respond()` — verify init, produce `HandshakeResponse`
- [x] `Handshaker::finalize()` — derive shared secret via X25519, run HKDF-SHA256 → `SessionKey`
- [x] `HandshakeConfirm` — HMAC-SHA256 transcript verification
- [x] `SessionCipher::encrypt()` — AES-256-GCM with incrementing nonce counter
- [x] `SessionCipher::decrypt()` — verify tag, decrypt
- [x] Nonce counter overflow detection (reject after 2^32 frames)
- [x] **Tests**:
  - [x] Identity generate → save → load round-trip
  - [x] Full handshake between two identities produces matching session keys
  - [x] Encrypt → decrypt round-trip
  - [x] Tampered ciphertext fails decryption
  - [ ] Replayed nonce is rejected
  - [x] Wrong key fails decryption
  - [x] Nonce counter overflow triggers error

### 1.4 Wire Protocol (`pim-protocol`)

- [x] `TransportFrame` — magic, version, frame_type, length, nonce, encrypted payload, tag
- [x] `MeshDataFrame` — src_id, dst_id, session_id, ttl, flags, payload
- [x] `HandshakeFrame` — init / response / confirm variants
- [x] `ControlFrame` — IpRequest, IpAssign, Goodbye, Ping/Pong
- [x] `HeartbeatFrame` — sender_id, timestamp, gateway_hops, load
- [x] `FrameCodec` implementations for all frame types
- [x] Length-prefixed framing for stream transports (TCP): 4-byte length prefix before each `TransportFrame`
- [x] **Tests**:
  - [x] Encode → decode round-trip for every frame type
  - [x] Reject truncated frames
  - [x] Reject invalid magic / version
  - [x] Reject frames exceeding max size
  - [x] Fuzz: random bytes don't panic decoder (returns error)
  - [x] Verify exact byte layout against protocol spec

### 1.5 TCP Transport (`pim-transport`)

- [x] `Transport` trait — async `send`, `recv`, `connect`, `disconnect`, `connected_peers`
- [x] `TcpTransport` — implementation for development and Docker testing
  - [x] Listener mode: bind, accept connections, perform framed reads
  - [x] Client mode: connect to address, perform framed writes
  - [x] Bidirectional: every connection handles both send and recv
  - [x] Connection tracking: map `NodeId` → TCP stream
  - [x] Graceful disconnect and cleanup
- [x] **Tests**:
  - [x] Two `TcpTransport` instances send/recv frames over loopback
  - [x] Connect → disconnect → reconnect lifecycle
  - [x] Multiple concurrent connections
  - [x] Sending to disconnected peer returns error
  - [ ] **Docker**: two containers communicate over bridge network

### 1.6 TUN Interface (`pim-tun`)

- [x] `TunInterface::create(name)` — create TUN device via ioctl
- [x] `TunInterface::set_ip(addr, mask)` — configure IP address
- [x] `TunInterface::set_mtu(mtu)` — set MTU
- [x] `TunInterface::up()` / `down()` — bring interface up/down
- [x] `TunInterface::read_packet()` — async read from TUN fd
- [x] `TunInterface::write_packet()` — async write to TUN fd
- [x] OS route configuration: set default route via gateway's mesh IP
- [x] **Tests** (require `CAP_NET_ADMIN` — run in Docker):
  - [ ] Create TUN → verify interface exists via `ip link`
  - [ ] Set IP → verify via `ip addr`
  - [ ] Write packet into TUN → read it back from TUN (loopback test)
  - [ ] Destroy TUN → verify interface removed

### 1.7 Gateway / NAT Engine (`pim-gateway`)

- [x] `GatewayEngine::new(internet_iface)` — initialize NAT engine
- [x] `translate_outbound(packet)` — rewrite source IP/port, add to conntrack
- [x] `translate_inbound(packet)` — look up conntrack, rewrite destination IP/port
- [x] Conntrack table with TTL-based expiry
- [x] `cleanup_expired()` — periodic conntrack sweep
- [x] iptables/nftables MASQUERADE rule setup (shell out to `iptables`)
- [x] TCP, UDP, ICMP support
- [x] **Tests**:
  - [x] Outbound translation creates conntrack entry, rewrites source
  - [x] Inbound response matches conntrack, rewrites destination
  - [x] Expired entries are cleaned up
  - [x] Unknown inbound packet (no conntrack) is dropped
  - [ ] **Docker**: gateway container can NAT traffic from mesh IP to external DNS (e.g., resolve `example.com`)

### 1.8 Daemon — Single-Hop Assembly (`pim-daemon`)

- [x] Wire together: TUN + Transport + Crypto + Gateway
- [x] TUN read loop: read IP packet → encrypt (hop-by-hop) → send via transport
- [x] Transport recv loop: receive frame → decrypt → write to TUN (or forward to gateway)
- [x] Gateway path: decrypt → NAT outbound → send to internet → NAT inbound → encrypt → send back
- [x] Graceful shutdown via `CancellationToken` + signal handler
- [x] Static configuration: peer address, mesh IPs, roles hardcoded in config
- [ ] **Tests**:
  - [ ] **Docker (2 containers)**: client pings gateway's mesh IP → pong received
  - [ ] **Docker (2 containers)**: client runs `curl http://example.com` through mesh → 200 response
  - [ ] **Docker (2 containers)**: client DNS resolution works through mesh
  - [ ] Daemon shuts down cleanly on SIGTERM (TUN removed, connections closed)

### 1.9 CLI (`pim-cli`)

- [x] `pim up --config <path>` — start daemon in foreground (or background with `--daemon`)
- [x] `pim down` — send SIGTERM to running daemon
- [x] `pim status` — connect to daemon (unix socket or pid file), show state
- [ ] **Tests**:
  - [ ] `pim up` creates TUN interface and starts listening
  - [ ] `pim down` removes TUN interface
  - [ ] `pim status` reports connection state accurately

---

## Phase 2 — Multi-Hop Relay

### 2.1 Packet Forwarding

- [x] Relay logic: if `dst_id != self.node_id` → decrement TTL, look up next hop, re-encrypt, forward
- [x] TTL enforcement: drop frame and log when TTL reaches 0
- [x] Forwarding metrics: count packets forwarded, dropped
- [ ] **Tests**:
  - [ ] **Docker (3 containers)**: client → relay → gateway, client curls internet successfully
  - [ ] Frame with TTL=1 arriving at relay is dropped (not forwarded)
  - [ ] Frame with TTL=0 is dropped immediately
  - [ ] Relay does not decrypt E2E payload (verify via instrumentation)

### 2.2 End-to-End Encryption

- [x] `e2e_encrypt(ip_packet, gateway_public_key)` — encrypt payload to gateway
- [x] `e2e_decrypt(e2e_frame, gateway_private_key)` — gateway decrypts
- [x] Client obtains gateway public key during initial connection (exchanged in config or discovery)
- [x] Relay nodes pass E2E-encrypted payload through without modification
- [x] **Tests**:
  - [x] E2E encrypt → decrypt round-trip
  - [x] Relay cannot decrypt E2E payload (attempt with relay's key fails)
  - [ ] **Docker (3 containers)**: packet capture on relay shows only encrypted payload
  - [x] Tampered E2E payload is rejected by gateway

### 2.3 Distance-Vector Routing (`pim-routing`)

- [x] `RoutingTable::apply_update()` — process incoming route advertisement
- [x] `RoutingTable::generate_advertisement()` — produce advertisement for neighbors
- [x] `RoutingTable::nearest_gateway()` — find shortest path to any gateway
- [x] `RoutingTable::lookup(dst)` — find next hop for destination
- [x] `RoutingTable::expire_stale(max_age)` — remove old entries
- [x] `RoutingTable::remove_routes_via(peer)` — invalidate routes through a dead peer
- [x] Split horizon: don't advertise a route back to the peer it was learned from
- [x] Poison reverse: advertise route with hops=infinity to the source peer
- [x] Periodic advertisement broadcast (configurable interval)
- [x] Triggered updates on topology change
- [x] **Tests**:
  - [x] 3-node chain: routes converge to correct hop counts
  - [x] Remove middle node: routes through it are invalidated
  - [x] Add new node: routing table updates within one advertisement cycle
  - [x] Routing loop detection: A→B→C→A is prevented by TTL and split horizon
  - [x] Gateway preference: client selects route with fewest hops to a gateway
  - [x] Stale route expiry after configured timeout
  - [ ] **Docker (4 containers)**: verify routing table state on each node via `pim routes`
  - [ ] **Docker (4 containers)**: kill a relay mid-test → traffic reroutes via alternate path

### 2.4 Fragmentation and Reassembly

- [x] Fragment large IP packets exceeding mesh MTU
- [x] Fragment header: fragment_id, fragment_offset, total_length
- [x] `IS_FRAGMENT` and `IS_LAST_FRAGMENT` flags in data frame
- [x] Reassembly buffer: collect fragments, reorder, deliver when complete
- [x] Reassembly timeout: discard incomplete fragments after 10 seconds
- [x] **Tests**:
  - [x] Fragment a 4000-byte packet into 3 fragments, reassemble → matches original
  - [x] Out-of-order fragments are reassembled correctly
  - [x] Duplicate fragment is ignored
  - [x] Missing fragment triggers timeout and discard
  - [ ] **Docker (2 containers)**: transfer a 10KB payload through mesh → arrives intact

### 2.5 Multi-Peer Transport

- [x] `TcpTransport` handles multiple concurrent peer connections
- [x] Connection map: `NodeId → TcpStream`
- [x] Send to specific peer by `NodeId`
- [x] Receive from any connected peer, tagged with sender `NodeId`
- [x] **Tests**:
  - [x] Node with 3 connected peers can send/recv to/from each independently
  - [x] Disconnecting one peer doesn't affect others
  - [ ] **Docker (4 containers)**: full mesh connectivity verified

---

## Phase 3 — Discovery and Mesh Join

### 3.1 Peer Discovery (`pim-discovery`)

- [x] `DiscoveryService::broadcast_presence()` — send advertisement over transport
- [x] `DiscoveryService::handle_advertisement()` — add/update peer table
- [x] `PeerTable` — tracks peers with capabilities, last_seen, public keys
- [x] Advertisement message: node_id, public_key, capabilities (client/relay/gateway), listen address
- [x] Periodic broadcast at configurable interval
- [x] **Tests**:
  - [x] Two nodes discover each other via advertisements
  - [x] Capabilities are correctly parsed and stored
  - [ ] **Docker (3 containers)**: all three discover each other within 2 broadcast cycles

### 3.2 Automatic Mesh Join

- [x] On `pim up`: discover → handshake → receive routes → request IP → configure TUN
- [x] IP request/assign via `ControlFrame` (IpRequest / IpAssign)
- [x] Gateway assigns IP from its pool (`IpPool`), tracks leases
- [x] Client configures TUN with assigned IP and routes
- [x] **Tests**:
  - [ ] **Docker (2 containers)**: start gateway, then start client → client auto-discovers, gets IP, can ping gateway
  - [ ] **Docker (3 containers)**: start gateway, start relay, start client → client discovers relay, relay has route to gateway, client reaches internet
  - [x] IP assignment is unique per client
  - [x] Client reconnects and gets the same IP (lease renewal)

### 3.3 Peer Lifecycle

- [x] Heartbeat send/receive between direct peers (5s interval)
- [x] Peer timeout after 3 missed heartbeats (15s)
- [x] `Goodbye` message on graceful shutdown
- [x] Route table cleanup when a peer is removed
- [x] Triggered route updates on peer join/leave
- [ ] **Tests**:
  - [ ] Peer sending heartbeats stays in peer table
  - [ ] Stopped peer is removed after timeout
  - [ ] `Goodbye` triggers immediate removal
  - [ ] **Docker (4 containers)**: kill a container → other nodes detect loss within 15s, routes update
  - [ ] **Docker (3 containers)**: restart killed container → re-discovers and rejoins mesh

---

## Phase 4 — Reliability and Performance

### 4.1 Connection Resilience

- [x] Auto-reconnect on transport failure with exponential backoff + jitter
- [x] Detect connection loss via heartbeat timeout or socket error
- [x] Re-handshake after reconnection (new session key)
- [x] Fixed transport-key/NodeId mismatch: `rename_peer` after handshake so real NodeId is used
- [ ] **Tests**:
  - [ ] **Docker**: `iptables -A DROP` between two containers, then remove → reconnects automatically
  - [ ] Reconnection establishes new session key (old key doesn't work)
  - [x] Backoff increases between retries (`backoff_base_grows_exponentially`, jitter-range tests)

### 4.2 Store-and-Forward

- [x] Bounded buffer for frames when next hop is temporarily unavailable (`send_buffer.rs`)
- [x] Priority queue: control frames > route updates > data frames (`Priority` enum)
- [x] Configurable timeout (default: 30s); capacity per peer: 256 frames
- [x] `flush_send_buffer` drains on reconnect (both initiator and responder sides)
- [x] `run_buffer_gc` background task expires stale frames every 10s
- [x] `send_frame_buffered` replaces direct sends in control, mesh-data, and route-advert paths
- [ ] **Tests**:
  - [ ] Buffer frames during 5s outage → delivered after reconnect (Docker)
  - [x] Buffer overflow drops lowest priority frames first (`peer_buffer_overflow_*`)
  - [x] Frames buffered beyond timeout are dropped (`peer_buffer_expire_*`, `send_buffer_expire_all_*`)

### 4.3 Flow Control

- [x] `TcpTransport::send` uses `try_send` (non-blocking); returns `TransportError::Congested` when write queue full
- [x] `should_buffer_under_congestion(frame_type)`: Control/Route/Handshake → buffered; Data/Heartbeat → dropped
- [x] `send_frame_buffered` handles `Congested` with priority-based policy; increments `congestion_drops` counter
- [x] `run_buffer_flush` (50 ms interval): re-sends buffered frames for connected peers, handles congestion recovery
- [x] Memory usage bounded: write queue (64 frames) + send buffer (256 frames) per peer
- [ ] **Tests**:
  - [x] `send_returns_congested_when_write_queue_full` (transport integration test)
  - [x] `congestion_drop_policy_is_priority_based` — Control/Route buffered, Data/HB dropped
  - [ ] Flood sender → receiver applies backpressure, no OOM (Docker)

### 4.4 NAT Improvements ✓

- [x] Userspace conntrack with per-protocol idle timeouts: TCP 5min, UDP 30s, ICMP 10s
- [x] ICMP echo (id-based) conntrack alongside TCP/UDP port-based conntrack
- [x] `cleanup_expired()` + `run_conntrack_gc` background task (30s interval, gateway-only)
- [x] **Tests** (unit):
  - [x] `icmp_outbound_and_inbound_round_trip`
  - [x] `conntrack_tcp_expires_after_idle_timeout` (301s)
  - [x] `conntrack_udp_expires_after_idle_timeout` (31s)
  - [x] `conntrack_icmp_expires_after_idle_timeout` (11s)
  - [x] `conntrack_tcp_not_expired_within_timeout` (299s, still alive)
  - [x] `port_released_after_conntrack_expiry`
- [ ] **Docker**: TCP connection through mesh, idle for 4 min → still works; idle for 6 min → conntrack expired

### 4.5 Observability ✓

- [x] `DaemonState` counters: `packets_forwarded`, `bytes_forwarded`, `packets_dropped`, `start_time`
- [x] Counters incremented in event loop: TUN→mesh send, relay forward, destination delivery; drops on TTL/no-route/no-session
- [x] `run_stats_writer` background task (5s interval): writes `/run/pim.stats` as key=value text (atomic rename)
- [x] `pim status --verbose`: reads and pretty-prints `/run/pim.stats`
- [x] `format_stats(...)` pure function for testable stats formatting
- [x] `parse_stats_str(...)` in CLI for testable stats parsing
- [x] **Tests** (6 new):
  - [x] `format_stats_contains_all_keys` — all 8 metrics present
  - [x] `packets_forwarded_counter_increments` — 100 increments → 100
  - [x] `bytes_forwarded_counter_accumulates` — sum of payload sizes
  - [x] `parse_stats_str_extracts_key_value_pairs` — 4 pairs parsed correctly
  - [x] `parse_stats_str_skips_malformed_lines` — bad lines ignored
  - [x] `parse_stats_str_empty_input` — empty → empty vec

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
