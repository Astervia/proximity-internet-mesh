# Networking Flow

This document explains the system as a set of layers and then walks through the runtime flow from startup to packet forwarding.

It focuses on what the code does today, and it calls out places where the architecture is broader than the currently wired implementation.

## The Layers

## 1. Host Integration Layer

Purpose:
Make the mesh look like a normal network interface to the operating system.

Crates:

- `pim-tun`
- parts of `pim-daemon`

What happens:

- the daemon creates a Linux TUN interface such as `pim0`
- it assigns the mesh IP and MTU
- it brings the interface up
- client nodes may add a default route through the gateway mesh IP

Why it matters:
Applications do not need mesh-specific logic. They just send IP packets, and the OS routes those packets into `pim0`.

Example:

```text
curl https://example.com
  -> Linux creates a normal TCP/IP packet
  -> route points at pim0
  -> daemon reads that packet from the TUN device
```

## 2. Peer Link Layer

Purpose:
Create direct node-to-node links and move encrypted transport frames between neighbors.

Crates:

- `pim-transport`
- `pim-crypto`
- `pim-protocol`

What happens:

- each node listens for TCP peer connections
- peers connect using configured addresses today
- a handshake authenticates peers and derives a shared session key
- every direct-peer payload is then encrypted into a `TransportFrame`

Why it matters:
This is the secure tunnel between adjacent peers in the mesh.

Example:

```text
Node A connects to Node B:9100
  -> sends temporary transport identity
  -> handshake confirms real node identity
  -> both sides derive a shared session key
  -> normal mesh traffic can now flow
```

## 3. Mesh Control Layer

Purpose:
Exchange metadata needed to keep the mesh healthy.

Crates:

- `pim-protocol`
- `pim-routing`
- parts of `pim-daemon`
- `pim-discovery` for future automatic joins

What happens:

- heartbeat frames confirm liveness
- route updates advertise reachable destinations and gateways
- ping and pong refine gateway quality estimates
- goodbye messages let peers remove routes quickly

Why it matters:
Without the control plane, data forwarding would have no route selection, no liveness view, and no graceful cleanup.

Example:

```text
Heartbeat from a gateway includes:
  gateway_hops = 0
  load = 17
  gw_x25519_pub = ...

That tells neighbors:
  "I am a gateway, I am lightly loaded, and here is the public key clients need for E2E encryption."
```

## 4. Mesh Data Layer

Purpose:
Move raw IP packets hop by hop across the mesh.

Crates:

- `pim-protocol`
- `pim-routing`
- `pim-crypto`
- `pim-daemon`

What happens:

- the original IP packet is wrapped in a `MeshDataFrame`
- the frame carries `src_id`, `dst_id`, `ttl`, and flags
- large payloads can be fragmented and reassembled
- relay nodes decrement TTL and forward toward the next hop

Why it matters:
This is the actual packet forwarding plane.

Example:

```text
MeshDataFrame
  src_id = client node
  dst_id = gateway node
  ttl = 8
  flags = IS_INTERNET | IS_E2E
  payload = encrypted IP packet
```

## 5. Gateway Layer

Purpose:
Connect the mesh to the internet.

Crates:

- `pim-gateway`
- parts of `pim-daemon`

What happens:

- the gateway receives a mesh-delivered IP packet
- if the packet is E2E encrypted, the gateway decrypts it
- the gateway rewrites source IP and port using userspace NAT
- the packet is written toward the external network
- reply traffic is reverse-translated and sent back into the mesh

Why it matters:
The mesh itself is not the internet. Gateway logic is the bridge.

## 6. Discovery Layer

Purpose:
Find peers without static configuration.

Crates:

- `pim-discovery`

What happens:

- nodes broadcast small UDP advertisements
- receivers learn `node_id`, `public_key`, capabilities, and the transport listen port
- new peers can then be connected automatically

Current status:
Implemented as a crate, but not yet integrated into `pim-daemon`. Current daemon startup still depends on static `[[peers]]` configuration.

## End-To-End Startup Flow

This is the runtime sequence in the current daemon.

## Step 1. Load config and identity

The daemon:

- reads the TOML config
- writes its PID file
- loads or generates the Ed25519 identity
- derives its `NodeId`
- derives its own X25519 public key for gateway E2E use

Example:

```toml
[node]
name = "relay-b"

[interface]
name = "pim0"
mesh_ipv4_prefix = "10.77.0.0/16"
mesh_ipv6_prefix = "fd77::/64"
mtu = 1400

[transport]
listen_port = 9100

[gateway]
enabled = false

[[peers]]
mechanism = "tcp"
address = "10.0.0.1:9100"
```

The daemon derives its mesh IPv4 + IPv6 from `self_id` plus the
configured prefixes via [`pim_core::derive_mesh_ipv4`] /
[`derive_mesh_ipv6`](../../crates/pim-core/src/mesh_address.rs). There
is no `mesh_ip = "auto"` step and no gateway round-trip — the address
is final before the TUN comes up.

## Step 2. Create and configure the TUN interface

The daemon creates the TUN device and configures:

- interface name
- IPv4 address
- prefix length
- MTU
- link-up state

For a client, this is the point where the host gets a mesh-facing interface like `pim0`.

## Step 3. Start the transport listener

The daemon binds the configured TCP port and starts accepting direct peer connections.

At this point, the node is reachable, but it still does not trust any peer until the higher-level handshake finishes.

## Step 4. Gateway-only setup

If `gateway.enabled = true`, the daemon:

- builds a `GatewayEngine`
- attempts to enable forwarding and MASQUERADE rules whose source CIDR is the entire `mesh_ipv4_prefix`

This makes the node capable of translating mesh traffic to internet traffic.

## Step 5. Connect to configured peers

Today, the daemon loops over `[[peers]]` entries and initiates direct TCP connections.

For each configured peer:

- connect the TCP transport
- create a temporary transport key
- start the handshake initiator
- rename the transport entry to the real peer `NodeId` after the handshake succeeds

Why the temporary ID exists:
The transport layer needs a map key before the peer's real identity has been proven.

## Step 6. Establish secure sessions

After TCP connect, both peers run the handshake from `pim-crypto`.

The result is a per-peer session with:

- peer `NodeId`
- send cipher
- receive cipher

After this step, transport payloads between those two neighbors are encrypted.

## Step 7. Build routing knowledge

Once peers exist, the daemon starts periodic control tasks:

- route advertisements
- heartbeats
- liveness checks
- buffered-send cleanup and retry

Over time, the routing table learns:

- which nodes are directly reachable
- which nodes are gateways
- how many hops away a gateway is
- which next hop is currently best

## Outbound Packet Flow

This is the path from a client application to the internet.

## Step 1. Application sends traffic

Example:

```text
Firefox or curl opens a TLS connection to 93.184.216.34:443
```

Linux creates an IP packet and routes it to `pim0`.

## Step 2. Daemon reads the raw packet from TUN

The daemon receives the packet bytes through `pim-tun`.

At this point, the packet is still an ordinary IP packet. It is not yet a mesh packet.

## Step 3. Daemon chooses a gateway path

The daemon asks `pim-routing` for the nearest gateway route.

Possible outcomes:

- a best gateway and next hop are known
- no route exists, and the daemon may fall back to any directly connected peer
- no usable peer exists, so the packet is dropped

## Step 4. Optional end-to-end encryption to the gateway

If the daemon knows the gateway X25519 public key from heartbeats:

- it encrypts the IP packet with `e2e_encrypt`
- it sets `DataFlags::IS_E2E`

Relays cannot decrypt this payload. Only the gateway can.

## Step 5. Wrap as mesh data

The daemon wraps the payload in a `MeshDataFrame`:

- `src_id` is the local node
- `dst_id` is the chosen gateway
- `ttl` starts at a value like `8`
- flags mark internet-bound and possibly E2E-encrypted payloads

If the payload is too large, it is fragmented first.

## Step 6. Encrypt for the next hop and send

The `MeshDataFrame` bytes are encrypted with the direct session to the next hop and placed in a `TransportFrame`.

Then the transport layer sends the frame over the TCP socket.

At this point there are two different protections:

- direct-peer session encryption for the whole hop
- optional inner E2E encryption for the gateway payload itself

## Relay Flow

When an intermediate relay receives a `TransportFrame`:

1. It decrypts the frame using the direct session with the sender.
2. It decodes the inner `MeshDataFrame`.
3. If `dst_id` is not itself, it checks TTL and route lookup.
4. It re-encrypts the same mesh payload for the next hop.
5. It forwards the packet.

Important detail:
Relays can inspect the mesh header, but if `IS_E2E` is set they should not be able to read the original internet packet payload.

## Gateway Delivery Flow

When the destination node is the gateway:

1. It decrypts the direct-peer transport frame.
2. It reassembles fragments if necessary.
3. If `IS_E2E` is set, it decrypts the inner payload.
4. It passes the resulting IP packet into `GatewayEngine`.
5. `GatewayEngine` rewrites source IP and source port.
6. The translated packet is written out through the gateway host networking path.

Example:

```text
Client side:
  10.77.0.2:43123 -> 8.8.8.8:53

Gateway after NAT:
  192.0.2.10:30017 -> 8.8.8.8:53
```

## Return Traffic Flow

For reply packets from the internet:

1. The gateway reads the returning packet.
2. `GatewayEngine` looks up the conntrack entry.
3. The destination is rewritten back to the original mesh client IP and port.
4. The daemon sends the packet back into the mesh.
5. The client node eventually writes the packet back into `pim0`.
6. The local application receives what looks like a normal network response.

Current limitation:
The reverse path does not yet maintain a full IP-to-`NodeId` ownership table, so return delivery logic is still simpler than the final intended design.

## Control Messages In Practice

These protocol messages are important when reading logs or debugging behavior.

## `HeartbeatFrame`

Used for:

- liveness
- gateway distance
- gateway load
- learning the gateway X25519 public key

## `RouteUpdateFrame`

Used for:

- telling neighbors which destinations are reachable
- marking which destinations are gateways
- poisoning routes that became invalid
- replay-safe, signed route exchange

## `ControlFrame::Ping` and `Pong`

Used for:

- measuring gateway RTT
- improving gateway selection quality

## `ControlFrame::Goodbye`

Used for:

- fast peer removal during graceful shutdown

## `ControlFrame::IpRequest` and `IpAssign` (removed)

These two control frames previously implemented gateway-driven mesh-IP
allocation. They were deleted alongside the per-gateway `IpPool` once
mesh addresses became deterministic from each `NodeId` — see
[`pim_core::derive_mesh_ipv4`](../../crates/pim-core/src/mesh_address.rs).
Tag values `0x01` / `0x02` are reserved on the wire so a daemon
receiving a legacy frame from an old peer surfaces a clean decode
error instead of aliasing future tags.

## Failure And Recovery Behavior

A few implementation details are worth knowing:

- if a configured peer disappears, reconnect uses exponential backoff with jitter
- congested peer queues can cause low-priority data drops
- route loops are limited by TTL and reduced by split horizon with poison reverse
- stale peers are removed by heartbeat timeout logic
- route advertisements use sequence numbers to reject replayed updates

## Mental Model

If you want one compact model of the system, use this:

```text
OS packet
  -> TUN
  -> mesh wrapper
  -> route selection
  -> per-hop encryption
  -> next peer
  -> zero or more relays
  -> gateway decrypt + NAT
  -> internet
  -> reverse NAT
  -> reverse mesh path
  -> TUN
  -> application
```

That is the core runtime story of the project.
