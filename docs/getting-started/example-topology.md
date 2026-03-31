# Example Topology

This document gives a concrete example of how to configure and reason about a small mesh.

It is intentionally practical. The goal is to make the runtime easier to picture, not just to list config fields.

## Example Topology

We will use three nodes:

- `gateway-a`: has internet access
- `relay-b`: forwards traffic between neighbors
- `client-c`: has no internet access and depends on the mesh

```text
client-c  <->  relay-b  <->  gateway-a  <->  internet
```

In the current implementation, peer links are configured statically with `[[peers]]`.

## Addressing Model

One simple lab subnet is:

- `gateway-a` mesh IP: `10.77.0.1/24`
- `relay-b` mesh IP: `10.77.0.2/24`
- `client-c` mesh IP: `10.77.0.3/24`

Transport listen port:

- all nodes listen on `9100`

Current implementation note:
The daemon can start with `mesh_ip = "auto"` and request an assignment from a gateway, but the most deterministic way to run a lab is still to use explicit static mesh CIDRs on every node.

## Example Gateway Config

```toml
[node]
name = "gateway-a"

[interface]
name = "pim0"
mesh_ip = "10.77.0.1/24"
mtu = 1400

[transport]
listen_port = 9100

[gateway]
enabled = true
nat_interface = "eth0"

[security]
key_file = "~/.pim/gateway-a.key"

[[peers]]
mechanism = "tcp"
address = "10.0.0.2:9100"
```

What this means:

- create `pim0`
- assign `10.77.0.1/24`
- listen for direct peer connections on `:9100`
- behave as a gateway and try to enable NAT through `eth0`
- connect to `relay-b` at `10.0.0.2:9100`

## Example Relay Config

```toml
[node]
name = "relay-b"

[interface]
name = "pim0"
mesh_ip = "10.77.0.2/24"
mtu = 1400

[transport]
listen_port = 9100

[gateway]
enabled = false

[security]
key_file = "~/.pim/relay-b.key"

[[peers]]
mechanism = "tcp"
address = "10.0.0.1:9100"

[[peers]]
mechanism = "tcp"
address = "10.0.0.3:9100"
```

What this means:

- `relay-b` is not the internet edge
- it participates in routing and forwarding
- it bridges the client and the gateway through two direct TCP sessions

## Example Client Config

```toml
[node]
name = "client-c"

[interface]
name = "pim0"
mesh_ip = "10.77.0.3/24"
mtu = 1400

[transport]
listen_port = 9100

[gateway]
enabled = false

[security]
key_file = "~/.pim/client-c.key"

[[peers]]
mechanism = "tcp"
address = "10.0.0.2:9100"
```

What this means:

- the client has a normal-looking network interface named `pim0`
- the client can send IP traffic through the mesh
- the client depends on routing advertisements to learn that `gateway-a` is the internet exit

## What Happens During Startup

For each node, the runtime picture is:

1. `pim up --config <file>` starts `pim-daemon`.
2. The daemon loads config and identity.
3. The daemon creates `pim0`.
4. The daemon starts listening on TCP port `9100`.
5. The daemon dials the configured peers.
6. Each direct link performs the authenticated handshake.
7. Heartbeats and route advertisements begin.
8. The client eventually learns a path to the gateway.

## What Happens When The Client Sends A Packet

Suppose `client-c` runs:

```bash
curl https://example.com
```

Then the system looks like this:

```text
1. client-c OS sends packet into pim0
2. client-c daemon reads raw packet
3. client-c routing picks gateway-a via relay-b
4. client-c optionally E2E-encrypts the IP packet for gateway-a
5. client-c sends the mesh frame to relay-b
6. relay-b decrypts the hop layer, sees dst=gateway-a, decrements TTL, forwards
7. gateway-a decrypts, NATs, and sends to the internet
8. response comes back to gateway-a
9. gateway-a reverse-NATs and sends back through relay-b
10. client-c writes packet back into pim0
11. curl receives the response
```

## Example Frame Progression

It helps to separate the packet into three nested views.

## View 1. Original IP packet

```text
src = 10.77.0.3
dst = 93.184.216.34
```

## View 2. Mesh packet

```text
MeshDataFrame {
  src_id = client-c,
  dst_id = gateway-a,
  ttl = 8,
  flags = IS_INTERNET | IS_E2E,
  payload = <IP packet bytes or gateway-encrypted bytes>
}
```

## View 3. Direct transport packet

```text
TransportFrame {
  frame_type = Data,
  nonce = ...,
  payload = <session-encrypted MeshDataFrame bytes>,
  tag = ...
}
```

The key idea is:

- applications think in IP packets
- the mesh thinks in `MeshDataFrame`s
- direct neighbors exchange `TransportFrame`s

## How To Read The Roles

## Client

Responsibilities:

- source of host traffic
- reads and writes `pim0`
- chooses a gateway route
- optionally encrypts traffic for the gateway

It does not:

- provide internet egress
- perform NAT for other nodes

## Relay

Responsibilities:

- maintain direct peer sessions
- receive route advertisements
- forward mesh packets based on `dst_id`
- drop packets with expired TTL or no route

It does not:

- terminate the internet path
- decrypt gateway E2E payloads

## Gateway

Responsibilities:

- advertise itself as reachable internet egress
- publish gateway metadata in heartbeats
- decrypt E2E payloads meant for it
- NAT traffic in and out of the internet

It does not:

- need to know application semantics
- need special client-side code

## Operational Tips

- Start by validating direct TCP reachability between peers before debugging routing.
- If packets do not leave the mesh, check whether a gateway route was learned.
- If packets reach the gateway but not the internet, check NAT setup and interface permissions.
- If the daemon refuses to start, verify that `mesh_ip` is explicit and that `/dev/net/tun` is available.

## Current Gaps To Keep In Mind

- automatic peer discovery exists as a crate but is not yet the default path
- dynamic mesh IP assignment exists in protocol and gateway code but is not yet the startup path
- the transport backend is TCP today, even though the architecture documents discuss Wi-Fi Direct as the target transport
- return-path delivery on the gateway is functional but still simpler than a final IP-to-node ownership design

These are not just future ideas. They are important for understanding why the current setup is more explicit than the long-term architecture diagrams suggest.
