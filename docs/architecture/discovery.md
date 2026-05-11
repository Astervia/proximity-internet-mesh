# Peer Discovery

Discovery is how PIM nodes find each other without prior configuration. Each node
broadcasts a compact advertisement packet over UDP; any node on the same LAN that
receives it can immediately initiate an authenticated connection.

## Overview

```
     Node A (gateway)                    Node B (relay)                Node C (client)
     172.33.0.10                         172.33.0.20                   172.33.0.30
          │                                    │                             │
          │◀──── "PIMD" broadcast ─────────────┤                             │
          │      every 5 s to                  │◀──── "PIMD" broadcast ──────┤
          │      255.255.255.255:9101           │      every 5 s              │
          │                                    │                             │
          │─────────────────────── "PIMD" broadcast every 5 s ─────────────▶│
          │                                    │                             │
          │  ←─ TCP handshake ─────────────────┤                             │
          │  ←─ TCP handshake ─────────────────┼─────────────────────────────┤
```

Each broadcast is a fixed 56-byte UDP packet. No DNS, no central registry,
no pre-shared configuration beyond the subnet.

## Advertisement Wire Format

```
Offset  Len  Field
──────  ───  ────────────────────────────────────────────────────────────────
     0    4  magic           — 0x50494D44 ("PIMD"), rejects non-PIM traffic
     4    1  version         — 0x01, bumped on format changes
     5   16  node_id         — SHA-256(Ed25519 public key)[0..16]
    21   32  public_key      — Ed25519 verifying key (32 bytes)
    53    1  capabilities    — bitfield (see below)
    54    2  listen_port     — big-endian u16, TCP transport port
    ──  ───
    56       total
```

No length field or checksum is needed — the packet is always exactly 56 bytes.
The magic prefix acts as a discriminator so that stray UDP traffic from other
applications on port 9101 is silently discarded.

The `listen_addr` used for connecting is **not** in the packet. It is derived at
the receiver by combining the UDP source IP with the advertised `listen_port`:

```
listen_addr = SocketAddr::new(udp_from.ip(), ad.listen_port)
```

This means nodes behind a NAT will advertise the correct port but the IP seen
by the receiver will be their LAN address — which is exactly what is needed for
same-subnet connectivity.

## Node Capabilities

Capabilities are a single-byte bitfield that describes what roles a node
supports simultaneously.

```
Bit 0 (0x01)  CLIENT   — originates traffic and requests internet access
Bit 1 (0x02)  RELAY    — forwards mesh frames for other nodes
Bit 2 (0x04)  GATEWAY  — performs NAT to the internet
Bits 3–7      reserved
```

A node may hold any combination:

| Config                   | Bits | Description                                     |
| ------------------------ | ---- | ----------------------------------------------- |
| `gateway.enabled = true` | 0x07 | Internet gateway — implicitly also relay/client |
| `relay.enabled = true`   | 0x03 | Relay — forwards for others, also a client      |
| _(default)_              | 0x01 | Client only — originates traffic                |

The daemon derives its own capabilities at startup from the loaded configuration:

```
fn node_capabilities(config: &Config) -> NodeCapabilities {
    if config.gateway.enabled      → NodeCapabilities::gateway()   // 0x07
    else if config.relay.enabled   → NodeCapabilities::relay()     // 0x03
    else                           → NodeCapabilities::client()    // 0x01
}
```

Receivers use capabilities to decide whether to connect:

- **Gateway** (`0x04` set) — connect; will supply IP assignments and internet access.
- **Relay** (`0x02` set, `0x04` clear) — connect if `discovery.connect_relays = true`.
- **Client only** (`0x01` only) — never connected to; ignored by the consumer.

## Discovery Service

`DiscoveryService` runs as a background Tokio task. It owns a UDP socket and three
concurrent loops driven by a `tokio::select!`:

```
                      DiscoveryService (background task)
                      ────────────────────────────────────
                      ┌────────────────────────────────┐
broadcast_interval ──▶│  broadcast_presence()          │
        (5 s)         │  → send 56 bytes to            │
                      │    255.255.255.255:9101         │
                      └────────────────────────────────┘
                      ┌────────────────────────────────┐
UDP recv_from     ──▶ │  handle_advertisement()        │
                      │  → validate magic/version      │
                      │  → ignore own node_id          │
                      │  → derive listen_addr          │
                      │  → PeerTable::upsert()         │
                      │  → if new → new_peer_tx.send() │
                      └────────────────────────────────┘
                      ┌────────────────────────────────┐
gc_interval       ──▶ │  PeerTable::expire_stale()     │
      (peer_timeout/2)│  → remove silent peers         │
                      └────────────────────────────────┘
```

### UDP Socket Configuration

The socket binds to `0.0.0.0:<discovery_port>` (default 9101) and enables
`SO_BROADCAST`. Broadcasts are sent to `255.255.255.255:<discovery_port>`.

One socket serves both sending and receiving. The receive buffer is 72 bytes
(56 + 16 spare) so the allocation is negligible.

### New-Peer Notification

`DiscoveryService::new` returns a `(service, mpsc::Receiver<PeerRecord>)` pair:

```
let (svc, new_peer_rx) = DiscoveryService::new(self_id, pubkey, caps, port);
```

When a previously-unknown peer's advertisement is received, `upsert` returns
`true` and the `PeerRecord` is forwarded on `new_peer_tx`. Subsequent
advertisements from the same node refresh `last_seen` in the `PeerTable` but
do **not** re-notify the daemon — duplicates are suppressed at the source.

## Peer Table

`PeerTable` is an in-memory `HashMap<NodeId, PeerRecord>` protected by an async
`Mutex` inside `DiscoveryService`.

```rust
struct PeerRecord {
    node_id:      NodeId,
    public_key:   [u8; 32],
    capabilities: NodeCapabilities,
    listen_addr:  SocketAddr,
    last_seen:    Instant,
}
```

Key operations:

| Method              | Effect                                                    |
| ------------------- | --------------------------------------------------------- |
| `upsert(record)`    | Insert new or refresh `last_seen`; returns `true` if new  |
| `get(node_id)`      | Look up a peer by NodeId                                  |
| `expire_stale(age)` | Remove peers not seen within `age`; returns their NodeIds |
| `all()`             | Iterate all current peers                                 |
| `remove(node_id)`   | Explicit removal                                          |

Expiry runs at `peer_timeout / 2` (default every 15 s) to ensure a peer with
a 30 s timeout is noticed within roughly one GC cycle after going silent.

## Daemon Integration

The daemon spawns two tasks around `DiscoveryService`:

```
main()
  ├─ tokio::spawn  DiscoveryService::run(cancel)
  │                  └─▶ UDP broadcast + receive loop
  │
  └─ tokio::spawn  run_discovery_consumer(state, new_peer_rx)
                     └─▶ filter + connect to discovered peers
```

### Discovery Consumer

`run_discovery_consumer` receives `PeerRecord`s from the channel and applies
a multi-stage filter before initiating a connection:

```
For each PeerRecord received on new_peer_rx:
  1. record.node_id == self_id?       → skip (own broadcast, defense-in-depth)
  2. sessions.contains(record.node_id)?  → skip (already connected)
  3. !caps.is_relay() && !caps.is_gateway()?  → skip (client-only peer)
  4. caps.is_gateway() && !config.connect_gateways?  → skip (disabled by config)
  5. caps.is_relay() && !caps.is_gateway() && !config.connect_relays? → skip
  6. reconnect.register_discovered(addr)
  7. initiate_peer_connection(state, addr)
```

Step 2 provides deduplication: `DiscoveryService` suppresses repeat
notifications for known peers via `upsert`, but a brief race between the
static-peer connect path and an arriving broadcast could still produce a
duplicate. The sessions-map check at step 2 is the authoritative guard.

### Connection Initiation

`initiate_peer_connection` follows the same path as static `[[peers]]`:

```
initiate_peer_connection(state, peer_addr)
  │
  ├─ transport.connect(PeerAddress { node_id: random_temp_key, addr: peer_addr })
  │     └─▶ TCP SYN to peer_addr
  │
  ├─ handshake_initiator(state, temp_key, rx)
  │     └─▶ X25519 + Ed25519 three-way handshake (see security.md)
  │         → derives real NodeId from peer's public key
  │         → establishes AES-256-GCM session cipher
  │
  └─ reconnect.register(real_peer_id, peer_addr)
       └─▶ enables reconnect-on-loss for this peer
```

A random temporary `NodeId` is used as the transport key so that simultaneous
outbound connections to different peers do not collide in the transport map.
The real `NodeId` is substituted after the handshake response is received.

### Reconnect-on-Loss

The `ReconnectManager` tracks two address sets:

- `configured_addrs` — static `[[peers]]` entries, never changes at runtime.
- `discovered_addrs` — addresses registered by `run_discovery_consumer`.

When `remove_peer` fires (heartbeat timeout or `Goodbye` message), it calls
`is_reconnectable_addr` which checks both sets. Discovered peers therefore
receive the same exponential-backoff reconnect treatment as static peers.

```
remove_peer(peer_id)
  └─▶ is_reconnectable_addr(peer_id)
        ├─ configured_addrs.contains(addr)?  → reconnect
        └─ discovered_addrs.contains(addr)?  → reconnect
```

Re-discovery also cooperates with reconnection. When a silent peer is eventually
removed from `PeerTable` by `expire_stale` (at ~30 s), the next broadcast from
that peer will re-trigger a `new_peer_tx` notification — at which point the
sessions map no longer contains the peer (heartbeat timeout at 15 s already
cleared it), so the consumer initiates a fresh connection.

## Configuration Reference

```toml
[discovery]
enabled              = true    # false → service not started; static [[peers]] only
port                 = 9101    # UDP broadcast port
broadcast_interval_ms = 5000   # how often to send own advertisement
peer_timeout_ms      = 30000   # how long before an unseen peer is expired
connect_relays       = true    # auto-connect to discovered relay peers
connect_gateways     = true    # auto-connect to discovered gateway peers

[relay]
enabled = false   # true → advertise RELAY capability; node forwards for others
```

`[[peers]]` remains valid alongside discovery. Static peers are connected at
startup; discovered peers are added dynamically. The two mechanisms are
independent and additive.

## Timing Relationships

```
t=0        daemon starts, UDP socket bound
t=0..5s    first broadcast fires on the first interval tick
t=5s       discovery converges with direct neighbours after ~1 cycle
t=10s      routing tables exchanged; gateway reachable via relay after ~2 cycles
t=15s      peer liveness timeout (3 × 5 s heartbeat interval)
t=30s      DiscoveryService expires a peer that has gone silent
```

Because the liveness timeout (15 s) is shorter than the discovery expiry
(30 s), a peer that goes silent will be removed from the session map before it
is removed from the `PeerTable`. When the peer returns and broadcasts again,
re-discovery fires cleanly.

## Limitations

### LAN-Local Only

UDP broadcast to `255.255.255.255` is confined to a single broadcast domain.
Nodes on different subnets — separated by a router — cannot discover each other
this way.

Current workarounds:

- **Static `[[peers]]`**: configure at least one peer's address per subnet boundary.
- **Relay bridging**: a dual-homed relay node hears broadcasts on both subnets and
  bridges connectivity manually via its static config.

### No Authentication at the Broadcast Layer

Discovery advertisements are unauthenticated. Any node on the LAN can craft a
packet that appears to be a PIM advertisement. The defence is at the transport
layer: the subsequent TCP handshake performs mutual Ed25519 authentication. A
spoofed advertisement will at worst cause a failed connection attempt.

### Client-Only Nodes Are Not Connected To

The consumer skips `CLIENT`-only peers (`capabilities == 0x01`). This means two
client nodes will not directly connect to each other even if they discover each
other. All client traffic flows through relays and gateways. This is intentional:
clients are end-users, not infrastructure.

## Future Directions

### Cross-Subnet Discovery (Bootstrap Nodes)

A well-known bootstrap address (hostname or IP) could be added to the config.
The daemon would contact it at startup to receive a peer list for nodes outside
the local broadcast domain.

```toml
[discovery]
bootstrap = ["pim-bootstrap.example.com:9101"]
```

### mDNS / DNS-SD

An mDNS backend would allow discovery without a fixed broadcast address,
integrating better with existing LAN service-discovery infrastructure.

### Gateway Quality Advertisement

Gateways could include load and internet latency metrics in the advertisement,
allowing clients to prefer the best gateway without a full Ping/Pong cycle:

```
# hypothetical extended advertisement
load_pct:      u8   # 0–255 forwarding load
internet_rtt:  u16  # measured RTT to 8.8.8.8 in ms
```

This would complement the existing RTT-probe mechanism (Phase 5) by making
initial gateway selection smarter before any probes are exchanged.

## Related Documents

- [security.md](security.md) — handshake and session establishment after discovery
- [routing.md](routing.md) — route exchange that follows a successful connection
- [protocol.md](protocol.md) — wire format for all ControlFrame variants (note: IpRequest / IpAssign were removed when mesh addresses became deterministic from NodeId)
- [overview.md](overview.md) — placement of discovery in the overall component architecture
