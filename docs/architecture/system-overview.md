# System Architecture

## Core Concept

Proximity Internet Mesh (PIM) presents itself as a **network adapter** on the host device. From the operating system's perspective, PIM is just another network interface — like Wi-Fi or Ethernet. Applications use standard TCP/IP networking and are completely unaware that their packets travel through a mesh of nearby devices.

```
┌────────────────────────────────────────┐
│           Applications                 │
│     (browser, curl, any app)           │
│         Normal TCP/IP usage            │
└──────────────┬─────────────────────────┘
               │  standard IP packets
┌──────────────▼─────────────────────────┐
│         OS Networking Stack            │
│     routes traffic to pim0 interface   │
└──────────────┬─────────────────────────┘
               │  raw IP packets
┌──────────────▼─────────────────────────┐
│         PIM Daemon (Rust)              │
│                                        │
│  ┌──────────┐  ┌────────┐  ┌────────┐ │
│  │   TUN    │  │ Router │  │ Crypto │ │
│  │ Device   │  │        │  │ Engine │ │
│  └────┬─────┘  └───┬────┘  └───┬────┘ │
│       └────────────┼────────────┘      │
│                    │                   │
│            ┌───────▼────────┐          │
│            │   Link Setup   │          │
│            │ (UDP / Wi-Fi   │          │
│            │ Direct / BT)   │          │
│            └───────┬────────┘          │
└────────────────────┼───────────────────┘
                     │  mesh frames
              ┌──────▼──────┐
              │  Peer Mesh  │
              │ (TCP over   │
              │ local links)│
              └─────────────┘
```

## User Flow

The experience mirrors connecting to a Wi-Fi network:

```
1. Start the adapter     →  $ pim up
                             Creates TUN interface `pim0`
                             Starts the PIM daemon

2. Connect to network    →  Discovery or link setup finds nearby peers
                             Handshake + key exchange
                             Routing table is built
                             pim0 gets an IP address (mesh-internal)

3. Now using PIM         →  OS routes traffic through pim0
                             Packets are encrypted, fragmented, routed through mesh
                             Gateway node NATs packets to the internet
                             To any app, it's just "the network"
```

Disconnecting is equally simple: `pim down` tears down the interface and leaves the mesh.

## Node Roles

### Client Node

- No internet access
- Runs the PIM daemon with a TUN interface
- Sends all outbound packets into the mesh
- Receives responses back through the mesh

### Relay Node

- Forwards mesh frames between peers
- Extends the reach of the mesh
- May or may not have internet access
- If it also has internet, it doubles as a gateway

### Gateway Node

- Has active internet connectivity
- Receives packets from the mesh
- Performs NAT: rewrites source address and forwards to the internet
- Returns responses back through the mesh to the originating client
- Advertises gateway capability during discovery

A single device can act as multiple roles simultaneously.

## Component Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│                          PIM Daemon                              │
│                                                                  │
│  ┌────────────┐  ┌────────────┐  ┌────────────┐  ┌───────────┐  │
│  │  TUN       │  │ Discovery  │  │  Routing   │  │  NAT /    │  │
│  │  Interface │  │  Service   │  │  Engine    │  │  Gateway  │  │
│  │  Manager   │  │            │  │            │  │  Engine   │  │
│  └─────┬──────┘  └─────┬──────┘  └─────┬──────┘  └─────┬─────┘  │
│        │               │               │               │        │
│  ┌─────┴───────────────┴───────────────┴───────────────┴─────┐  │
│  │                      Event Bus (tokio channels)           │  │
│  └─────┬─────────────────────────────────────────┬───────────┘  │
│        │                                         │              │
│  ┌─────┴──────────┐                     ┌────────┴───────────┐  │
│  │ Link Setup /   │                     │   Crypto Engine    │  │
│  │ Transport      │                     │                    │  │
│  │  Handoff       │                     │  (X25519 + AES)   │  │
│  └────────────────┘                     └────────────────────┘  │
└──────────────────────────────────────────────────────────────────┘
```

### TUN Interface Manager

- Creates and configures the `pim0` TUN device
- Reads outbound IP packets from the TUN file descriptor
- Writes inbound IP packets (responses) back into the TUN device
- Assigns the mesh-internal IP address to the interface
- Sets up OS routing rules so traffic flows through `pim0`

### Discovery Service

- Detects or accepts nearby PIM peers via UDP broadcast, Wi-Fi Direct, or Bluetooth PAN
- Exchanges capability advertisements:
    - Node ID (public key fingerprint)
    - Role (client / relay / gateway)
    - Supported transports
    - Hop distance to nearest gateway
- Maintains a **peer table** with liveness tracking (heartbeat / timeout)
- Emits events: `PeerJoined`, `PeerLeft`, `PeerUpdated`

### Routing Engine

- Maintains a routing table: `destination → next_hop`
- Implements distance-vector or link-state routing (see [networking-and-routing.md](networking-and-routing.md))
- Selects the best path toward a gateway node for outbound internet traffic
- Detects and avoids routing loops (TTL + split horizon)
- Recalculates routes when peers join or leave

### NAT / Gateway Engine

Active only on gateway nodes:

- Receives IP packets from the mesh destined for the internet
- Performs source NAT: replaces the mesh-internal source IP with the gateway's real IP
- Forwards the packet out through the gateway's real internet interface
- Tracks active connections (conntrack) to route responses back
- Rewrites destination on return packets and sends them back into the mesh

### Link Setup And Transport Manager

- Accepts peer endpoints from static config, UDP discovery, Wi-Fi Direct, or Bluetooth PAN
- Manages optional link-establishment mechanisms such as Wi-Fi Direct group formation
- Reuses the same TCP transport once a peer endpoint is reachable
- Handles connection lifecycle: setup, keepalive, reconnection
- Exposes a uniform `send(peer_id, frame)` / `recv() → (peer_id, frame)` interface
- Future: pluggable backends (TCP loopback for testing, Wi-Fi Aware, BLE adapters, etc.)

### Crypto Engine

- All cryptographic operations (see [security.md](security.md))
- Node identity: Ed25519 keypair
- Key exchange: X25519 Diffie-Hellman
- Packet encryption: AES-256-GCM per-hop or end-to-end
- Signature verification for routing messages

### Event Bus

- Internal async communication between components
- Built on `tokio::sync::mpsc` and `tokio::sync::broadcast` channels
- Decouples components for testability and modularity

## Packet Flow: Outbound Request

```
App sends TCP SYN to 93.184.216.34:443
        │
        ▼
OS routes packet to pim0 (default route or specific route)
        │
        ▼
TUN Manager reads raw IP packet from fd
        │
        ▼
Crypto Engine encrypts packet (end-to-end, keyed to gateway)
        │
        ▼
Routing Engine looks up path to gateway → next hop: Peer B
        │
        ▼
Packet wrapped in MeshFrame { src, dst, ttl, encrypted_payload }
        │
        ▼
Transport Manager sends frame to Peer B over the established direct link
        │
        ▼
Peer B (relay): decrements TTL, looks up next hop → Peer C
        │
        ▼
Peer C (gateway): decrypts, extracts IP packet, NATs, sends to internet
        │
        ▼
Response follows reverse path → arrives at TUN → delivered to app
```

## Async Runtime

Built on **Tokio**:

- Each component runs as one or more spawned tasks
- Non-blocking I/O for TUN reads, Wi-Fi Direct sockets, timers
- `CancellationToken` for coordinated graceful shutdown
- `JoinSet` for managing component task lifecycles

## Configuration

```toml
[node]
name = "my-device"
data_dir = "~/.pim"

[interface]
name = "pim0"
mtu = 1400
mesh_ip = "auto"          # assigned during mesh join, or static

[discovery]
broadcast_interval_ms = 5000
peer_timeout_ms = 30000

[transport]
type = "tcp"
listen_port = 9100

[wifi_direct]
enabled = false

[bluetooth]
enabled = false
interface = "auto"

[routing]
max_hops = 10
algorithm = "distance-vector"
route_expiry_s = 300

[gateway]
enabled = false            # set true on nodes with internet
nat_interface = "wlan0"    # real internet-facing interface
max_connections = 200

[security]
key_file = "~/.pim/node.key"
require_encryption = true
```
