# Implementation Plan

Actionable checklist organized by phase. Every item must have corresponding tests before it's considered done. See [../operations/testing.md](../operations/testing.md) for the overall test strategy and [../operations/docker-labs.md](../operations/docker-labs.md) for multi-node testing with Docker.

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
    - [x] Replayed nonce is rejected
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
    - [x] Peer sending heartbeats stays in peer table
    - [x] Stopped peer is removed after timeout
    - [x] `Goodbye` triggers immediate removal
    - [ ] **Docker (4 containers)**: kill a container → other nodes detect loss within 15s, routes update
    - [ ] **Docker (3 containers)**: restart killed container → re-discovers and rejoins mesh

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

## Phase 5 — Multi-Gateway and Load Balancing ✓

### 5.1 Multi-Gateway Routing ✓

- [x] `gateway_score(hops, load, rtt_ms) -> u32`: composite score (100/hop + load/2 + rtt_ms/10)
- [x] `best_gateway_entry()` internal helper used by `nearest_gateway()` and `nearest_gateway_route()`
- [x] `all_gateways()` sorted by composite score (best first)
- [x] Failover: `remove_peer()` → routes via dead peer removed → `nearest_gateway_route()` returns next-best
- [x] **Tests**: `load_aware_selection_prefers_less_loaded_gateway`, `failover_to_second_gateway_when_first_removed`, `all_gateways_sorted_by_score`
- [ ] **Docker**: kill preferred gateway → failover; restore → may shift back

### 5.2 Load-Aware Routing ✓

- [x] `RouteTableEntry.gateway_load: u8` field in routing table
- [x] `update_gateway_load(gw_id, load)` — called by daemon heartbeat handler for direct gateway peers
- [x] `run_heartbeats` wires real load: delta of `packets_forwarded` over 10s interval, normalized to 0–255
- [x] **Tests**: `load_aware_selection_prefers_less_loaded_gateway`, `update_gateway_load_only_applies_to_gateway_entries`, load normalization tests (0, 127, 255)
- [ ] **Docker**: saturate one gateway → new flows go to the other

### 5.3 Gateway Health Probes ✓

- [x] `RouteTableEntry.rtt_ms: Option<u32>` — measured RTT per gateway
- [x] `update_gateway_rtt(gw_id, rtt_ms)` — called by Pong handler in event loop
- [x] `run_gateway_probes` (10s interval): sends `ControlFrame::Ping` to each direct gateway, records nonce+time in `pending_pings`
- [x] Pong handler: removes from `pending_pings`, computes elapsed RTT, updates routing table
- [x] Stale ping GC: entries older than 30s discarded on each probe cycle
- [x] **Tests**: `rtt_aware_selection_prefers_lower_latency_gateway`, `rtt_aware_selection_switches_at_high_latency`, `pending_pings_gc_removes_stale_entries`
- [ ] **Docker**: `tc netem` latency on one gateway → clients prefer the other

## Phase 6 — Security Hardening ✓

### 6.1 Routing Security ✓

- [x] Verify Ed25519 signatures on all routing updates (`pim-routing/src/signing.rs`)
- [x] Reject updates with invalid or missing signatures (daemon wires `verify_route_update` before `apply_update`)
- [x] Sequence number validation (reject old/replayed updates) (`peer_max_seq` in `RoutingTable`)
- [x] Anomaly detection: reject hops=0 claims for non-self destinations (`apply_update` anomaly check)
- [x] `peer_pubkeys` populated on both initiator and responder handshake paths
- [x] Route advertisements signed with node Ed25519 key before sending
- [x] **Tests**: sign/verify round-trip, tampered payload, wrong key, unsigned frame, replay rejection, origin mismatch, zero-hop anomaly, blacklist (40 routing tests)

### 6.2 Rate Limiting ✓

- [x] Per-peer token-bucket rate limiter (`pim-daemon/src/rate_limiter.rs`)
- [x] 500-frame burst capacity, 200 fps sustained refill rate
- [x] All incoming frames checked; excess frames dropped and `packets_dropped` incremented
- [x] Bucket cleared on `remove_peer`
- [x] **Tests**: burst allowed up to capacity, throttled after burst, independent per-peer tracking, remove-peer resets bucket (4 unit tests)

### 6.3 Peer Reputation ✓

- [x] `ReputationTracker` in `pim-daemon/src/reputation.rs` with per-peer failure/success scores
- [x] Heartbeat timeout → `record_failure`; score ≥ 10 → auto-blacklist in routing table
- [x] Pong received → `record_success` (decrement score)
- [x] `blacklist_peer` flushes all routes via that peer from `RoutingTable`
- [x] **Tests**: failure accumulation, blacklist threshold, success decay, floor-zero, pardon (5 unit tests)

## Phase 7 — Auto-Discovery Integration

Wire the fully-implemented `pim-discovery` crate into the running daemon so that nodes find each other by UDP broadcast with zero static configuration. Today `DiscoveryService` exists but is never spawned; the daemon connects only to static `[[peers]]`.

### 7.1 Discovery Config Extension (`pim-core`)

- [ ] Add `enabled: bool` (default `true`) to `DiscoveryConfig` — skip spawning when `false`
- [ ] Add `port: u16` (default `9101`) to `DiscoveryConfig` — allows overriding the hardcoded port
- [ ] Add `connect_relays: bool` (default `true`) to `DiscoveryConfig` — filter out relay-capable peers
- [ ] Add `connect_gateways: bool` (default `true`) to `DiscoveryConfig` — filter out gateway-capable peers
- [ ] Update `DiscoveryConfig::default()` to include all five fields
- [ ] Add new `RelayConfig` struct with `enabled: bool` (default `false`) and `impl Default`
- [ ] Add `pub relay: RelayConfig` to top-level `Config` with `#[serde(default)]`
- [ ] Document in comment on `peers` field that `[[peers]]` is now optional (zero-config startup)
- [ ] **Tests**:
    - [ ] `discovery_defaults_when_section_absent` — parse minimal config (only `[node]`), assert all five `DiscoveryConfig` fields at their defaults
    - [ ] `discovery_enabled_false_round_trips` — serialize `enabled = false`, re-parse, assert false survives
    - [ ] `discovery_custom_port_round_trips` — set `port = 19101`, serialize, re-parse, assert 19101
    - [ ] `relay_config_defaults_to_disabled` — parse minimal config, assert `relay.enabled == false`
    - [ ] `relay_enabled_true_parses` — parse TOML with `[relay]\nenabled = true`, assert true
    - [ ] `peers_section_is_optional` — parse config with no `[[peers]]`, assert `config.peers.is_empty()`
    - [ ] `config_round_trip_with_all_discovery_fields` — extend `FULL_CONFIG` constant with all new fields, assert round-trip

### 7.2 Capability Advertisement (`pim-daemon`)

- [ ] Add pure function `node_capabilities(config: &Config) -> NodeCapabilities`:
    - `gateway.enabled = true` → `CLIENT | RELAY | GATEWAY` (bits `0x07`)
    - `relay.enabled = true` (and not gateway) → `CLIENT | RELAY` (bits `0x03`)
    - else → `CLIENT` (bits `0x01`)
- [ ] Add `use pim_discovery::{DiscoveryService, NodeCapabilities};` import (crate already in workspace `Cargo.toml`)
- [ ] **Tests**:
    - [ ] `gateway_config_yields_gateway_caps` — `gateway.enabled = true` → `is_gateway() && is_relay() && is_client()`
    - [ ] `relay_config_yields_relay_caps` — `relay.enabled = true`, gateway off → `is_relay() && is_client() && !is_gateway()`
    - [ ] `client_config_yields_client_caps_only` — both disabled → `is_client() && !is_relay() && !is_gateway()`
    - [ ] `gateway_caps_bits_are_correct` — gateway config → `caps.bits() == 0x07`

### 7.3 Daemon Discovery Integration (`pim-daemon`)

- [ ] Extend `ReconnectManager` with `discovered_addrs: Mutex<HashSet<SocketAddr>>` field
- [ ] Add `ReconnectManager::register_discovered(&self, addr: SocketAddr)` — insert into discovered set
- [ ] Add `ReconnectManager::is_reconnectable_addr(&self, peer_id: &NodeId) -> Option<SocketAddr>` — checks both configured and discovered sets via `addr_by_peer`
- [ ] Update `remove_peer()` to use `is_reconnectable_addr` so discovered peers also reconnect with exponential backoff
- [ ] Add `discovery_config: pim_core::DiscoveryConfig` field to `DaemonState`
- [ ] Extract peer connection initiation into `async fn initiate_peer_connection(state: Arc<DaemonState>, peer_addr: SocketAddr)` — refactor `main()` static-peer loop to call this
- [ ] Add `async fn run_discovery_consumer(state: Arc<DaemonState>, mut new_peer_rx: mpsc::Receiver<PeerRecord>)`:
    - Skip `record.node_id == state.self_id` (own advertisement — defense-in-depth)
    - Skip if `state.sessions.read().await.contains_key(&record.node_id)` (deduplication)
    - Skip CLIENT-only peers (`!is_relay() && !is_gateway()`)
    - Apply `connect_gateways` / `connect_relays` config filters
    - `reconnect.register_discovered(record.listen_addr)`
    - Call `initiate_peer_connection(state.clone(), record.listen_addr)`
- [ ] In `main()`, when `config.discovery.enabled`:
    - Derive `pubkey` from `identity.signing_key().verifying_key().to_bytes()`
    - Build `DiscoveryService::new(self_id, pubkey, node_capabilities(&config), listen_port)` with builder overrides from config
    - Spawn `discovery_svc.run(cancel.clone())`
    - Spawn `run_discovery_consumer(state.clone(), new_peer_rx)`
    - Log `info!` on enabled/disabled
- [ ] **Tests**:
    - [ ] `discovered_relay_triggers_connection_attempt` — relay `PeerRecord` into consumer → transport `connect` called with peer's addr
    - [ ] `discovered_gateway_triggers_ip_request` — gateway peer + `request_dynamic_ip` → `IpRequest` sent after handshake
    - [ ] `duplicate_discovery_does_not_reconnect` — peer already in sessions → no second connect call
    - [ ] `client_only_peer_is_skipped` — `NodeCapabilities::client()` (bits `0x01`) peer → no connect call
    - [ ] `self_advertisement_is_ignored` — `node_id == self_id` → no connect call
    - [ ] `discovery_disabled_in_config_skips_spawning` — `enabled = false` in config → `config.discovery.enabled == false`

### 7.4 Docker Test: Zero-Config Auto-Discovery

- [ ] New compose `docker/compose/phase7-auto-discovery.yml`: bridge `172.34.0.0/24`, **no `depends_on`** between services
    - `gateway` service: `172.34.0.10`, `gateway-p7.toml`, `NET_ADMIN + NET_RAW`, `ip_forward=1`
    - `relay` service: `172.34.0.20`, `relay-p7.toml`, `NET_ADMIN`
    - `client` service: `172.34.0.30`, `client-p7.toml`, `NET_ADMIN`
- [ ] `docker/configs/gateway-p7.toml`: `gateway.enabled = true`, `[discovery] enabled = true port = 9101`, **no `[[peers]]`**
- [ ] `docker/configs/relay-p7.toml`: `[relay] enabled = true`, mesh IP `10.77.0.10/24`, **no `[[peers]]`**
- [ ] `docker/configs/client-p7.toml`: `mesh_ip = "auto"`, `[discovery] enabled = true`, **no `[[peers]]`**
- [ ] `docker/tests/test-phase7.sh`:
    - [ ] **7A** `all_nodes_discover_each_other`: start all → wait 20s → `pim status --verbose` shows `peers>=1` on all three nodes
    - [ ] **7B** `client_auto_ip_configured`: `pim0` UP → `ip addr` shows `10.77.0.*` → `ping 10.77.0.1` succeeds
    - [ ] **7C** `internet_via_discovered_relay`: `curl http://example.com` succeeds from client through discovered relay+gateway chain
    - [ ] **7D** `dns_through_mesh`: `nslookup google.com` resolves from client
    - [ ] **7E** `late_joiner_discovers_and_joins`: stop client → wait 5s → restart → wait 20s → mesh IP assigned, ping + curl work
    - [ ] **7F** `relay_loss_and_recovery`: stop relay → wait 20s → gateway peer count drops → restart relay → wait 25s → client pings gateway, relay shows peers

## Acceptance Criteria Summary

Every checklist item above must meet these criteria before being marked done:

1. **Unit tests pass** — covering the happy path, edge cases, and error conditions
2. **Integration tests pass** — multi-container Docker tests where applicable
3. **No panics** — all error paths return `Result`, no `unwrap()` in non-test code
4. **Logging** — key operations emit `tracing` events at appropriate levels
5. **Documentation** — public API has doc comments

## Phase 8 — Wi-Fi Direct Transport

Add Wi-Fi Direct (IEEE 802.11 P2P) as an optional peer-finding layer. After a P2P group is formed the existing `TcpTransport` handles all communication — no new transport trait implementation is needed. TCP/LAN and Wi-Fi Direct discovery run in parallel and are fully additive.

**Prerequisite:** `wpa_supplicant` compiled with P2P support must be running on the device.

### 8.1 Config Extension (`pim-core/src/config.rs`)

- [x] Add `WifiDirectConfig` struct: `enabled`, `interface`, `go_intent`, `listen_channel`, `op_channel`, `connect_method`
- [x] Add `#[serde(default)] pub wifi_direct: WifiDirectConfig` to `Config`
- [x] Add `impl Default for WifiDirectConfig` with all defaults
- [x] Re-export `WifiDirectConfig` from `pim-core/src/lib.rs`
- [x] Update `FULL_CONFIG` test constant with `[wifi_direct]` section
- [x] **Tests**:
    - [x] `wifi_direct_defaults_to_disabled`
    - [x] `wifi_direct_enabled_round_trips`
    - [x] `wifi_direct_custom_interface_parses`
    - [x] `wifi_direct_go_intent_default_is_neutral`
    - [x] `config_round_trip_with_wifi_direct_section`

### 8.2 New Crate `pim-wifidirect`

- [x] Create `crates/pim-wifidirect/` with `Cargo.toml`
- [x] Add to workspace `members` and workspace dependencies in root `Cargo.toml`
- [x] `src/wpa_cli.rs` — `WpaCliController`: async wrapper around `wpa_cli` subprocesses
    - `p2p_find`, `p2p_stop_find`, `p2p_peers`, `p2p_peer_info`
    - `p2p_connect_pbc`, `p2p_connect_pin`, `p2p_group_add`, `p2p_group_remove`
    - `list_interfaces`, `iface_ipv4`, `arp_peers_on_iface`
    - Pure parsing helpers: `parse_p2p_peers`, `parse_p2p_peer_info`, `parse_inet_addr`, `parse_arp_table`, `parse_interface_list`
- [x] `src/group.rs` — `WifiDirectGroup`: role, own_ip, peer_ip resolution
    - `GroupRole` enum: `Go`, `Gc`
    - `GO_INTERFACE_IP` constant (`192.168.49.1`)
    - `WifiDirectGroup::from_iface`: polls interface IP, resolves peer IP
- [x] `src/lib.rs` — `WifiDirectDiscovery`: orchestrates find → connect → group formation
    - `WifiDirectDiscovery::new(node_name, config, listen_port) -> (Self, Receiver<SocketAddr>)`
    - `WifiDirectDiscovery::run(cancel)` — polling loop, emits `SocketAddr` per formed group
- [x] **Tests** (all pure unit tests, no subprocess invoked):
    - [x] `wpa_cli_p2p_peers_parses_empty_output`
    - [x] `wpa_cli_p2p_peers_parses_single_mac`
    - [x] `wpa_cli_p2p_peers_parses_multiple_macs`
    - [x] `wpa_cli_p2p_peers_ignores_header_lines`
    - [x] `p2p_peer_info_parses_device_name`
    - [x] `p2p_peer_info_returns_error_when_device_name_missing`
    - [x] `parse_inet_addr_extracts_ip`
    - [x] `parse_inet_addr_returns_none_for_no_ip`
    - [x] `parse_arp_table_extracts_peer_on_iface`
    - [x] `parse_arp_table_returns_empty_for_wrong_iface`
    - [x] `parse_interface_list_extracts_interfaces`
    - [x] `group_ip_parsed_from_go_interface`
    - [x] `group_ip_parsed_from_gc_interface`
    - [x] `group_peer_ip_gc_uses_go_constant`
    - [x] `group_peer_ip_go_reads_arp_table`
    - [x] `discovery_new_returns_receiver`
    - [x] `discovery_skips_already_seen_mac`

### 8.3 Daemon Integration (`pim-daemon/src/main.rs`)

- [x] Add `pim-wifidirect = { workspace = true }` to `pim-daemon/Cargo.toml`
- [x] Add `use pim_wifidirect::WifiDirectDiscovery` import
- [x] Add `async fn run_wifidirect_consumer(state: Arc<DaemonState>, mut addr_rx: mpsc::Receiver<SocketAddr>)`
    - `reconnect.register_discovered(addr)` — enables reconnect-on-loss
    - `initiate_peer_connection(state.clone(), addr)` — reuses Phase 7 function
- [x] In `main()`, when `config.wifi_direct.enabled`: build `WifiDirectDiscovery`, spawn `run()` and `run_wifidirect_consumer`
- [x] **Tests**:
    - [x] `wifidirect_disabled_config_skips_spawning`
    - [x] `wifidirect_enabled_config_exposes_interface_and_port`
    - [x] `wifidirect_addr_registered_for_reconnect`
    - [x] `wifidirect_coexists_with_udp_discovery`
    - [x] `wifidirect_discovery_construction_from_config`

### 8.4 Testing

- **Unit tests**: `cargo test --workspace` — all tests pass (no subprocess invoked)
- **Docker tests**: Not applicable — Docker containers cannot access physical Wi-Fi hardware. No `test-p8` target.
- **Manual hardware test**: See `docs/architecture/transports/wifi-direct.md` for the step-by-step procedure on two Linux devices with Wi-Fi hardware.
