# Crate Guide

> **See also:** [roadmap.md](roadmap.md) for the phased delivery view and
> [history.md](history.md) for the historical implementation checklist.

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

## Workspace Tree

PIM is organized as a Cargo workspace with multiple crates, each owning a clear slice of functionality.

```
proximity-internet-mesh/
├── Cargo.toml                  # workspace root
├── Cargo.lock
├── docs/                      # user, architecture, operations, and project docs
│
├── crates/
│   ├── pim-core/               # shared types, traits, config
│   ├── pim-crypto/             # encryption, keys, handshake
│   ├── pim-transport/          # transport abstraction and TCP backend
│   ├── pim-routing/            # routing table, distance-vector algorithm
│   ├── pim-tun/                # TUN device management
│   ├── pim-gateway/            # NAT engine, connection tracking
│   ├── pim-discovery/          # peer discovery, capability exchange
│   ├── pim-protocol/           # frame serialization/deserialization
│   ├── pim-daemon/             # main daemon binary, ties everything together
│   └── pim-cli/                # CLI binary (`pim`)
│
├── docker/                     # Compose labs, node configs, integration scripts
└── target/                     # Cargo build output
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

## Per-Crate Type Signatures

The signatures below are the primary public API surface for each crate. They complement the prose descriptions above.

### `pim-core`

```rust
pub struct NodeId([u8; 16]);
pub struct MeshIp(Ipv4Addr);
pub struct PeerId { node_id: NodeId, public_key: PublicKey }
pub struct Config { ... }  // deserialized from TOML

pub trait FrameCodec {
    fn encode(&self, buf: &mut BytesMut);
    fn decode(buf: &mut BytesMut) -> Result<Self>;
}

pub enum PimError { ... }
```

### `pim-crypto`

```rust
pub struct Identity {
    keypair: Ed25519Keypair,
    node_id: NodeId,
}
impl Identity {
    pub fn generate() -> Self;
    pub fn load(path: &Path) -> Result<Self>;
    pub fn save(&self, path: &Path) -> Result<()>;
}

pub struct Handshaker { ... }
impl Handshaker {
    pub fn initiate(&self) -> HandshakeInit;
    pub fn respond(&self, init: &HandshakeInit) -> HandshakeResponse;
    pub fn finalize(&self, resp: &HandshakeResponse) -> SessionKey;
}

pub struct SessionCipher {
    key: Aes256GcmKey,
    nonce_counter: AtomicU32,
}
impl SessionCipher {
    pub fn encrypt(&self, plaintext: &[u8]) -> EncryptedFrame;
    pub fn decrypt(&self, frame: &EncryptedFrame) -> Result<Vec<u8>>;
}

pub fn e2e_encrypt(payload: &[u8], gateway_pub: &PublicKey) -> E2eFrame;
pub fn e2e_decrypt(frame: &E2eFrame, gateway_priv: &PrivateKey) -> Result<Vec<u8>>;
```

### `pim-protocol`

```rust
pub struct TransportFrame {
    pub frame_type: FrameType,
    pub nonce: [u8; 12],
    pub payload: Vec<u8>,   // encrypted
    pub tag: [u8; 16],
}

pub struct MeshDataFrame {
    pub src_id: NodeId,
    pub dst_id: NodeId,
    pub session_id: u32,
    pub ttl: u8,
    pub flags: DataFlags,
    pub payload: Vec<u8>,   // E2E encrypted IP packet
}

pub struct RouteUpdateFrame { ... }
pub struct HeartbeatFrame { ... }
pub struct ControlFrame { ... }

impl FrameCodec for TransportFrame { ... }
impl FrameCodec for MeshDataFrame { ... }
```

### `pim-transport`

```rust
#[async_trait]
pub trait Transport: Send + Sync {
    async fn send(&self, peer: &NodeId, frame: TransportFrame) -> Result<()>;
    async fn recv(&self) -> Result<(NodeId, TransportFrame)>;
    async fn connect(&self, peer_addr: PeerAddress) -> Result<()>;
    async fn disconnect(&self, peer: &NodeId) -> Result<()>;
    fn connected_peers(&self) -> Vec<NodeId>;
}

pub struct TcpTransport { ... }  // current implementation
```

### `pim-tun`

```rust
pub struct TunInterface {
    name: String,
    fd: AsyncFd<RawFd>,
}

impl TunInterface {
    pub fn create(name: &str) -> Result<Self>;
    pub fn set_ip(&self, addr: Ipv4Addr, mask: u8) -> Result<()>;
    pub fn set_mtu(&self, mtu: u32) -> Result<()>;
    pub fn up(&self) -> Result<()>;
    pub fn down(&self) -> Result<()>;
    pub async fn read_packet(&self) -> Result<IpPacket>;
    pub async fn write_packet(&self, packet: &[u8]) -> Result<()>;
}
```

### `pim-routing`

```rust
pub struct RoutingTable {
    routes: HashMap<NodeId, RouteEntry>,
}

pub struct RouteEntry {
    pub destination: NodeId,
    pub next_hop: NodeId,
    pub hops: u8,
    pub is_gateway: bool,
    pub last_updated: Instant,
}

impl RoutingTable {
    pub fn lookup(&self, dst: &NodeId) -> Option<&RouteEntry>;
    pub fn nearest_gateway(&self) -> Option<&RouteEntry>;
    pub fn apply_update(&mut self, from: &NodeId, update: &RouteUpdateFrame);
    pub fn generate_advertisement(&self) -> RouteUpdateFrame;
    pub fn expire_stale(&mut self, max_age: Duration);
    pub fn remove_routes_via(&mut self, peer: &NodeId);
}
```

### `pim-gateway`

```rust
pub struct GatewayEngine {
    nat_table: ConnTrackTable,
    internet_iface: String,
}

pub struct ConnTrackTable {
    // Maps (mesh_src_ip, mesh_src_port, dst_ip, dst_port, proto)
    //   → (nat_port, last_seen)
    entries: HashMap<FiveTuple, NatEntry>,
}

impl GatewayEngine {
    pub fn new(internet_iface: &str) -> Result<Self>;
    pub fn translate_outbound(&mut self, packet: &mut IpPacket) -> Result<()>;
    pub fn translate_inbound(&mut self, packet: &mut IpPacket) -> Result<Option<MeshIp>>;
    pub fn cleanup_expired(&mut self);
}
```

### `pim-discovery`

```rust
pub struct DiscoveryService {
    peer_table: Arc<RwLock<PeerTable>>,
    transport: Arc<dyn Transport>,
    identity: Arc<Identity>,
}

pub struct PeerTable {
    peers: HashMap<NodeId, PeerInfo>,
}

pub struct PeerInfo {
    pub node_id: NodeId,
    pub public_key: PublicKey,
    pub capabilities: Capabilities,
    pub last_seen: Instant,
}

impl DiscoveryService {
    pub async fn run(&self, cancel: CancellationToken);
    pub async fn broadcast_presence(&self);
    pub async fn handle_advertisement(&self, adv: Advertisement);
}
```

### `pim-daemon`

```rust
#[tokio::main]
async fn main() {
    let config = Config::load()?;
    let identity = Identity::load_or_generate(&config)?;

    let tun = TunInterface::create(&config.interface.name)?;
    let transport = create_transport(&config)?;
    let routing = RoutingTable::new();
    let discovery = DiscoveryService::new(...);
    let gateway = if config.gateway.enabled {
        Some(GatewayEngine::new(&config.gateway.nat_interface)?)
    } else {
        None
    };

    let cancel = CancellationToken::new();
    tokio::select! {
        _ = run_tun_reader(tun, routing, crypto, transport) => {},
        _ = run_tun_writer(tun, crypto) => {},
        _ = run_frame_receiver(transport, routing, gateway, tun) => {},
        _ = discovery.run(cancel.clone()) => {},
        _ = run_route_advertiser(routing, transport, cancel.clone()) => {},
        _ = signal::ctrl_c() => { cancel.cancel(); },
    }
}
```

### `pim-cli`

```rust
#[derive(Parser)]
#[command(name = "pim", version, about, propagate_version = true)]
pub struct Cli {
    #[arg(short, long, global = true, default_value = "/etc/pim/config.toml")]
    pub config: PathBuf,

    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    Up(UpArgs),
    Down,
    Status(StatusArgs),
    Peers(PeersArgs),
    Routes(RoutesArgs),
    Config(ConfigArgs),
    Keygen(KeygenArgs),
    Diag(DiagArgs),
}
```

See [../getting-started/usage.md](../getting-started/usage.md) for the actual commands and flags.

## Key Workspace Dependencies

```toml
[workspace.dependencies]
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
toml = "0.8"
bytes = "1"
byteorder = "1"
thiserror = "2"
tracing = "0.1"
tracing-subscriber = "0.3"
clap = { version = "4", features = ["derive"] }
async-trait = "0.1"
bitflags = "2"

# Crypto
x25519-dalek = "2"
ed25519-dalek = { version = "2", features = ["rand_core"] }
aes-gcm = "0.10"
hkdf = "0.12"
sha2 = "0.10"
rand = "0.8"

# Networking
tun = "0.7"
nix = { version = "0.29", features = ["net", "ioctl"] }
```

## Build and Run

```bash
# Build everything
cargo build --workspace

# Run the daemon
cargo run -p pim-daemon -- --config config.toml

# CLI
cargo run -p pim-cli -- up
cargo run -p pim-cli -- status

# Run tests
cargo test --workspace

# Integration tests (requires root for TUN)
sudo cargo test -p tests --test integration
```
