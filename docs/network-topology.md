# Network Topology and Routing

## Mesh Topology

PIM forms a **partial mesh** over Wi-Fi Direct. Each node maintains direct connections to nearby peers within radio range, and the collective of these links creates the mesh.

```
                    ┌────────┐
        ┌──────────│ Node E │──────────┐
        │          │(relay) │          │
        │          └────────┘          │
        │                              │
   ┌────┴───┐                     ┌────┴───┐
   │ Node A │─────────────────────│ Node D │
   │(client)│                     │(gateway)│
   └────┬───┘                     └────┬───┘
        │                              │
        │          ┌────────┐          │
        └──────────│ Node B │──────────┘
                   │(relay) │
                   └───┬────┘
                       │
                  ┌────┴───┐
                  │ Node C │
                  │(client)│
                  └────────┘
```

Key properties:

- **Dynamic**: Nodes join and leave as devices move in and out of range
- **Heterogeneous**: Nodes have different roles (client, relay, gateway)
- **Partial**: Not every node is connected to every other node — only those within Wi-Fi Direct range (~200m)
- **Multi-path**: Multiple routes may exist between any two nodes

## Wi-Fi Direct Group Formation

Wi-Fi Direct requires a **Group Owner (GO)** and one or more **Group Clients (GC)**. This is a constraint of the protocol.

### Strategy: Rotating Group Ownership

PIM manages Wi-Fi Direct groups to maximize mesh connectivity:

1. **Gateway nodes prefer to be Group Owners** — they are high-value targets that many nodes want to reach, and GO status lets them accept multiple connections.

2. **Relay nodes negotiate GO status** based on:
    - Number of current connections (prefer nodes with fewer, to balance load)
    - Battery level
    - Proximity to gateway (nodes closer to gateways benefit from being GOs)

3. **Client nodes are typically Group Clients** — they initiate connections to nearby GOs.

### Multi-Group Connectivity

A single device can participate in multiple Wi-Fi Direct groups (hardware permitting):

```
Group 1 (GO: Node B)          Group 2 (GO: Node D)
┌──────────────────┐          ┌──────────────────┐
│  Node A (GC)     │          │  Node B (GC)     │
│  Node C (GC)     │          │  Node E (GC)     │
│  Node B (GO)  ───┼──bridge──┼─▶ Node D (GO)   │
└──────────────────┘          └──────────────────┘
```

Node B acts as a GC in Group 2 while being the GO of Group 1, bridging the two groups. This is how multi-hop paths are formed.

## IP Addressing

Each node in the mesh receives an internal IP address from a private range:

```
Mesh subnet:   10.77.0.0/16
Node A:        10.77.0.1
Node B:        10.77.0.2
Gateway D:     10.77.0.4
```

### Address Assignment

- Gateway nodes act as lightweight DHCP within the mesh:
    - Each gateway manages a slice of the address space
    - On mesh join, a client requests an address from the nearest gateway
- Alternatively, addresses can be deterministically derived from the node ID:
    ```
    mesh_ip = 10.77. || node_id[0] . node_id[1]
    ```
    With collision resolution if needed.

### Routing to the Internet

The gateway node's mesh-internal IP is set as the **default gateway** for the `pim0` interface:

```
# On client Node A
ip route add default via 10.77.0.4 dev pim0 metric 600
```

The metric ensures PIM is used only when no higher-priority interface (real Wi-Fi, Ethernet) is available — or it can be set as primary if desired.

## Routing Algorithm

### Distance-Vector Routing (MVP)

For the initial implementation, PIM uses a **distance-vector** algorithm inspired by RIP, adapted for mesh dynamics.

Each node maintains a routing table:

```
┌─────────────┬──────────┬──────┬────────────┬────────────┐
│ Destination │ Next Hop │ Hops │ Is Gateway │ Last Updated│
├─────────────┼──────────┼──────┼────────────┼────────────┤
│ Node B      │ Node B   │ 1    │ No         │ 1711408200 │
│ Node D      │ Node B   │ 2    │ Yes        │ 1711408200 │
│ Node E      │ Node E   │ 1    │ No         │ 1711408195 │
└─────────────┴──────────┴──────┴────────────┴────────────┘
```

#### Route Advertisement

Nodes periodically broadcast their routing table to direct peers:

```
RouteAdvertisement {
    origin: NodeId,
    sequence: u64,
    entries: [
        { destination: NodeId, hops: u8, is_gateway: bool },
        ...
    ],
    signature: Ed25519Signature,
}
```

#### Route Update Rules

When a node receives an advertisement from a neighbor:

1. For each entry, compute `new_hops = entry.hops + 1`
2. If the destination is unknown → add route with `next_hop = sender`
3. If `new_hops < current_hops` → update to shorter path
4. If the sender is the current next_hop and hops changed → update (the path through this neighbor got longer/shorter)
5. Discard if `new_hops >= max_hops` (default: 10)

#### Split Horizon with Poison Reverse

To prevent routing loops:

- A node does **not** advertise a route back to the neighbor it learned it from (split horizon)
- Alternatively, it advertises the route with `hops = infinity` (poison reverse)

#### Triggered Updates

In addition to periodic advertisements, nodes send immediate updates when:

- A new peer is discovered
- A peer disconnects (routes through it become invalid)
- A route's hop count changes significantly

### Gateway-Oriented Routing

Since the primary use case is internet access, routing is **gateway-oriented**:

- Routes toward gateway nodes are prioritized
- Each node tracks the shortest path to each known gateway
- When forwarding an internet-bound packet, the node selects the nearest gateway
- If multiple gateways are equidistant, the node can load-balance or select by capacity

### Future: Link-State Routing

For larger meshes (Phase 3+), a link-state approach (similar to OSPF) may be more efficient:

- Each node floods its local link state to all nodes
- Every node computes a full topology map
- Dijkstra's algorithm selects optimal paths
- More bandwidth overhead but faster convergence and loop-free routing

## Packet Forwarding

When a node receives a mesh frame:

```rust
fn handle_frame(frame: MeshFrame) {
    if frame.dst == self.node_id {
        // Packet is for us — deliver to TUN (or gateway engine)
        deliver_local(frame);
    } else if frame.ttl > 0 {
        // Forward to next hop
        frame.ttl -= 1;
        let next = routing_table.lookup(frame.dst);
        transport.send(next, frame);
    } else {
        // TTL expired — drop
        drop(frame);
    }
}
```

### TTL (Time-To-Live)

Every mesh frame carries a TTL field (default: 10). It decrements at each hop. This prevents packets from looping indefinitely if routing has a transient loop.

## Store-and-Forward

When the next hop is temporarily unreachable (e.g., the peer is briefly out of range):

1. The node buffers the frame in a bounded queue
2. Periodically retries delivery
3. If the peer reconnects within the buffer timeout (default: 30s), the frame is delivered
4. If the timeout expires, the frame is dropped and the route is invalidated

This enables resilience against brief connectivity interruptions common in mobile scenarios.

## Mesh Join Procedure

When a new node starts PIM:

```
1. Start PIM daemon
   └─▶ Create TUN interface pim0 (no IP yet)

2. Discovery broadcasts presence
   └─▶ "I am Node X, capabilities: [client]"

3. Nearby peer responds
   └─▶ Handshake + key exchange (see encryption-and-security.md)

4. Receive routing table from peer
   └─▶ Learn about gateway nodes and other peers

5. Request mesh IP from gateway
   └─▶ Gateway assigns 10.77.x.x

6. Configure pim0 with assigned IP
   └─▶ ip addr add 10.77.0.5/16 dev pim0

7. Set default route via gateway
   └─▶ ip route add default via 10.77.0.4 dev pim0

8. Node is now online
   └─▶ All IP traffic flows through the mesh
```

## Topology Maintenance

### Heartbeats

Each pair of directly connected peers exchanges heartbeat messages:

- Interval: every 5 seconds
- Timeout: 15 seconds (3 missed heartbeats → peer considered dead)
- Heartbeats also carry lightweight routing hints (e.g., gateway reachability changes)

### Graceful Departure

A node leaving the mesh broadcasts a `Goodbye` message:

- Peers immediately remove routes through the departing node
- Triggers route recalculation

### Partition Handling

If the mesh partitions (a group of nodes becomes unreachable):

- Routes to unreachable nodes expire after `route_expiry_s` (default: 300s)
- If the partition heals, discovery and route advertisements rebuild connectivity
- Buffered frames for unreachable destinations are dropped after timeout
