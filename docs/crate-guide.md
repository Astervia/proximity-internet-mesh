# Crate Guide

This document explains the role of each crate in the workspace and how the crates fit together as one networking system.

The short version is:

```text
pim-cli
  -> starts pim-daemon
      -> uses pim-core for config and shared types
      -> uses pim-tun to create pim0
      -> uses pim-transport to connect peers
      -> uses pim-crypto to authenticate and encrypt
      -> uses pim-protocol to encode frames
      -> uses pim-routing to pick next hops
      -> uses pim-gateway on gateway nodes
      -> can later use pim-discovery for automatic peer discovery
```

## The Workspace At A Glance

| Crate           | What it does                                                    | When it matters most                        |
| --------------- | --------------------------------------------------------------- | ------------------------------------------- |
| `pim-core`      | Shared config, errors, and basic types                          | Everywhere                                  |
| `pim-crypto`    | Identity, handshake, session encryption, gateway E2E encryption | When a peer joins or data must be protected |
| `pim-protocol`  | Binary frame formats                                            | Every packet crossing the mesh              |
| `pim-transport` | Peer-to-peer transport abstraction, currently TCP               | Direct peer connections                     |
| `pim-tun`       | Linux TUN device management                                     | Host integration with the OS network stack  |
| `pim-routing`   | Distance-vector routing table and route advertisement logic     | Choosing the next hop                       |
| `pim-gateway`   | Userspace NAT and IP lease pool for gateway nodes               | Internet access through a gateway           |
| `pim-discovery` | UDP broadcast discovery of nearby peers                         | Automatic peer finding                      |
| `pim-daemon`    | Main runtime that glues all layers together                     | Actual system behavior                      |
| `pim-cli`       | User-facing commands like `pim up` and `pim down`               | Operations and local control                |

## Dependency Layers

```text
top level
  pim-cli
  pim-daemon

service layer
  pim-routing
  pim-gateway
  pim-discovery
  pim-transport
  pim-tun
  pim-crypto
  pim-protocol

shared foundation
  pim-core
```

## `pim-core`

General description:
`pim-core` is the shared foundation crate. It keeps the workspace aligned around one config model, one error vocabulary, and one definition of identity and frame encoding traits.

What it contains:

- `Config` and its sections for node, interface, transport, routing, gateway, security, and static peers
- `NodeId`, a 16-byte identifier derived from the node's public key
- `MeshIp`, a wrapper around the internal IPv4 address
- `FrameCodec`, the trait used by protocol frames for encode/decode
- `PimError`, the common error type used across crates

Why it matters:
Without `pim-core`, each crate would invent its own wire helpers and configuration shapes. This crate prevents that drift.

Example:

```rust
use pim_core::Config;

let cfg = Config::load("/etc/pim/pim.toml".as_ref())?;
println!("node {}", cfg.node.name);
println!("listen port {}", cfg.transport.listen_port);
```

## `pim-crypto`

General description:
`pim-crypto` provides identity, authenticated session setup between direct peers, hop-by-hop session encryption, and end-to-end encryption from a client to a gateway.

What it contains:

- `Identity` for loading or generating the Ed25519 identity key
- `Handshaker`, `HandshakeInit`, `HandshakeResponse`, `HandshakeConfirm`
- `SessionCipher` for per-peer encrypted transport frames
- `e2e_encrypt` and `e2e_decrypt` for internet-bound payloads
- `x25519_public_from_seed` to derive the gateway X25519 public key from the long-term identity seed

How it fits:

- Direct peers first prove identity and derive a shared session key.
- Once the session exists, every transport payload between those peers is encrypted.
- For internet-bound traffic, the original IP packet can also be encrypted again for the gateway, so relays forward opaque bytes.

Example:

```rust
use pim_crypto::{e2e_encrypt, x25519_public_from_seed};

let gateway_pub = x25519_public_from_seed(&gateway_seed);
let ciphertext = e2e_encrypt(ip_packet_bytes, &gateway_pub)?;
```

Current implementation note:
Session protection between neighbors is implemented and used by `pim-daemon`. Gateway E2E encryption is also implemented and enabled when the daemon knows the gateway X25519 key from heartbeats.

## `pim-protocol`

General description:
`pim-protocol` defines the binary language spoken inside the mesh. It turns Rust structs into bytes and bytes back into Rust structs.

What it contains:

- `TransportFrame`: outer encrypted frame exchanged between direct peers
- `MeshDataFrame`: inner mesh payload carrying source, destination, TTL, flags, and payload
- `HandshakeWireFrame`: handshake messages sent during session setup
- `RouteUpdateFrame`: signed distance-vector advertisements
- `HeartbeatFrame`: liveness, load, and nearest-gateway metadata
- `ControlFrame`: side-band messages like `IpRequest`, `IpAssign`, `Ping`, `Pong`, and `Goodbye`
- `FragmentFrame`, `fragment_packet`, and `Reassembler`
- `LengthDelimitedCodec` for stream-safe framing on transports like TCP

How it fits:
The daemon never writes raw structs to the network. It always wraps data in protocol frames. This crate is the contract between nodes.

Example:

```text
IP packet
  -> MeshDataFrame
  -> encrypted into TransportFrame
  -> length-delimited for TCP
  -> sent to the next peer
```

## `pim-transport`

General description:
`pim-transport` is the peer link layer. It abstracts how one node sends a `TransportFrame` to another node.

What it contains:

- `Transport` trait with `send`, `recv`, `connect`, `disconnect`, and `connected_peers`
- `PeerAddress`
- `TcpTransport`, the current implementation

How it works today:

- Each node listens on a TCP port.
- When a connection opens, the initiating side sends its temporary or real `NodeId`.
- The transport layer handles stream I/O, length framing, and per-peer write queues.
- The daemon handles higher-level identity confirmation through the handshake.

Why this split matters:
The transport crate is intentionally below routing and crypto policy. It just moves frames between immediate neighbors.

Example:

```rust
transport.connect(&PeerAddress { node_id, addr }).await?;
transport.send(&peer_id, frame).await?;
let (from_peer, frame) = transport.recv().await?;
```

Current implementation note:
The architecture docs mention Wi-Fi Direct as the target data plane, but the code currently uses TCP as the implemented transport backend.

## `pim-tun`

General description:
`pim-tun` is the boundary between the operating system and the mesh. It creates a Linux TUN interface and moves raw IP packets in and out of user space.

What it contains:

- `TunInterface::create`
- interface configuration helpers for IP, MTU, flags, and default route
- async packet read and write operations

How it fits:

- Outbound host traffic arrives as raw packets read from `pim0`.
- Inbound mesh traffic is written back into `pim0`, so applications see normal network packets.

Example:

```rust
let tun = TunInterface::create("pim0")?;
tun.set_ip("10.77.0.2".parse()?, 24)?;
tun.set_mtu(1400)?;
tun.up()?;
```

Operational note:
This crate is Linux-specific and requires `/dev/net/tun` plus privileges such as `CAP_NET_ADMIN`.

## `pim-routing`

General description:
`pim-routing` decides where packets should go next. It implements a distance-vector routing table with replay protection, split horizon with poison reverse, and gateway scoring.

What it contains:

- `RoutingTable`
- `RouteTableEntry`
- `UpdateResult`
- `gateway_score`
- `signing` helpers for route advertisements

How it fits:

- When direct peer sessions come up, the daemon adds them as one-hop routes.
- Peers exchange `RouteUpdateFrame`s.
- The routing table picks the best next hop toward a destination or the nearest gateway.
- Gateway choice considers hops, load, and measured RTT.

Example:

```text
Node A knows:
  Gateway G via B in 2 hops
  Gateway H via C in 3 hops

If G becomes overloaded or slow, gateway_score can make H the better route.
```

## `pim-gateway`

General description:
`pim-gateway` is the internet edge. It turns a mesh node into a gateway by performing userspace NAT and managing a pool of mesh-facing client IP leases.

What it contains:

- `GatewayEngine` for outbound and inbound NAT translation
- `IpPool` for handing out mesh-side IPv4 addresses
- conntrack and external port allocation logic

How it fits:

- A gateway receives an IP packet from the mesh.
- It rewrites source IP and source port to its external interface values.
- When a reply comes back, it reverses the translation and sends the packet back toward the original client.

Example:

```text
Client packet in mesh:
  src=10.77.0.2:43210 -> dst=1.1.1.1:443

Gateway after NAT:
  src=203.0.113.5:30042 -> dst=1.1.1.1:443

Reply after reverse NAT:
  src=1.1.1.1:443 -> dst=10.77.0.2:43210
```

Current implementation note:
The daemon already uses `GatewayEngine` for packet translation on gateway nodes. The reverse path still has a temporary limitation: it does not yet maintain a full IP-to-NodeId mapping, so return delivery is more approximate than the final design.

## `pim-discovery`

General description:
`pim-discovery` is the automatic peer-finding layer. It broadcasts compact advertisements over UDP and keeps a live peer table.

What it contains:

- `DiscoveryService`
- `DiscoveryAdvertisement`
- `NodeCapabilities`
- `PeerTable` and `PeerRecord`

How it fits:

- Each node periodically broadcasts who it is, what roles it can serve, and which TCP port it listens on.
- When a node hears a new advertisement, it can initiate a transport connection and start the handshake.

Example:

```text
Broadcast says:
  node_id = 9a12..44ef
  capabilities = gateway
  listen_port = 9100

Receiver learns:
  "connect to sender_ip:9100 and start handshake"
```

Current implementation note:
This crate is implemented but is not yet wired into `pim-daemon` startup. Today, the daemon connects to statically configured `[[peers]]` addresses.

## `pim-daemon`

General description:
`pim-daemon` is the real system process. It assembles all other crates into one runtime that creates the interface, connects peers, exchanges routes, forwards packets, and optionally acts as a gateway.

What it does:

- loads config and identity
- creates `pim0`
- starts the transport listener
- optionally enables gateway NAT
- connects to configured peers
- runs handshakes and establishes sessions
- forwards packets from TUN to mesh and mesh to TUN
- sends heartbeats and route advertisements
- tracks liveness, buffering, congestion, reputation, and rate limits

Why it matters:
The daemon is where the abstract architecture becomes an actual event loop.

Example:

```text
TUN packet arrives
  -> nearest gateway route lookup
  -> optional gateway E2E encryption
  -> MeshDataFrame
  -> per-peer session encryption
  -> TransportFrame
  -> TCP send to next hop
```

## `pim-cli`

General description:
`pim-cli` is the operator-facing command layer. It does not run the mesh itself; it starts, stops, and inspects the daemon.

What it contains:

- `pim up`
- `pim down`
- `pim status`

How it fits:

- `pim up` locates `pim-daemon` and runs it in the foreground or background.
- `pim down` signals the daemon to stop.
- `pim status` reports basic state from the PID file and config.

Example:

```bash
pim up --config ./client.toml
pim status --verbose
pim down
```

## Practical Reading Order

If you are new to the codebase, read the crates in this order:

1. `pim-core`
2. `pim-protocol`
3. `pim-crypto`
4. `pim-transport`
5. `pim-routing`
6. `pim-tun`
7. `pim-gateway`
8. `pim-daemon`
9. `pim-discovery`
10. `pim-cli`

That order mirrors the stack from low-level building blocks to the full system.
