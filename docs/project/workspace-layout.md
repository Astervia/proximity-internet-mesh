# Workspace Layout

## Workspace Layout

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

## Crate Responsibilities

### `pim-core`

Shared types and interfaces used across all crates.

```rust
// Key shared types
pub struct NodeId([u8; 16]);
pub struct MeshIp(Ipv4Addr);
pub struct PeerId { node_id: NodeId, public_key: PublicKey }

// Configuration
pub struct Config { ... }  // deserialized from TOML

// Common traits
pub trait FrameCodec {
    fn encode(&self, buf: &mut BytesMut);
    fn decode(buf: &mut BytesMut) -> Result<Self>;
}

// Error types
pub enum PimError { ... }
```

**Dependencies**: `serde`, `toml`, `bytes`, `thiserror`

### `pim-crypto`

All cryptographic operations.

```rust
// Identity
pub struct Identity {
    keypair: Ed25519Keypair,
    node_id: NodeId,
}
impl Identity {
    pub fn generate() -> Self;
    pub fn load(path: &Path) -> Result<Self>;
    pub fn save(&self, path: &Path) -> Result<()>;
}

// Handshake
pub struct Handshaker { ... }
impl Handshaker {
    pub fn initiate(&self) -> HandshakeInit;
    pub fn respond(&self, init: &HandshakeInit) -> HandshakeResponse;
    pub fn finalize(&self, resp: &HandshakeResponse) -> SessionKey;
}

// Session encryption
pub struct SessionCipher {
    key: Aes256GcmKey,
    nonce_counter: AtomicU32,
}
impl SessionCipher {
    pub fn encrypt(&self, plaintext: &[u8]) -> EncryptedFrame;
    pub fn decrypt(&self, frame: &EncryptedFrame) -> Result<Vec<u8>>;
}

// E2E encryption
pub fn e2e_encrypt(payload: &[u8], gateway_pub: &PublicKey) -> E2eFrame;
pub fn e2e_decrypt(frame: &E2eFrame, gateway_priv: &PrivateKey) -> Result<Vec<u8>>;
```

**Dependencies**: `x25519-dalek`, `ed25519-dalek`, `aes-gcm`, `hkdf`, `sha2`, `rand`

### `pim-protocol`

Wire protocol frame definitions and serialization.

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

// Serialization
impl FrameCodec for TransportFrame { ... }
impl FrameCodec for MeshDataFrame { ... }
```

**Dependencies**: `bytes`, `byteorder`, `bitflags`

### `pim-transport`

Abstracted transport layer with Wi-Fi Direct implementation.

```rust
#[async_trait]
pub trait Transport: Send + Sync {
    async fn send(&self, peer: &NodeId, frame: TransportFrame) -> Result<()>;
    async fn recv(&self) -> Result<(NodeId, TransportFrame)>;
    async fn connect(&self, peer_addr: PeerAddress) -> Result<()>;
    async fn disconnect(&self, peer: &NodeId) -> Result<()>;
    fn connected_peers(&self) -> Vec<NodeId>;
}

// Implementations
pub struct WifiDirectTransport { ... }
pub struct TcpTransport { ... }  // for development/testing
```

**Dependencies**: `tokio`, `async-trait`

### `pim-tun`

TUN device creation and management.

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

**Dependencies**: `tun`, `tokio`, `nix`

### `pim-routing`

Routing table and algorithm.

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

**Dependencies**: `tokio` (for timers)

### `pim-discovery`

Peer discovery and mesh join.

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

### `pim-gateway`

NAT engine for gateway nodes.

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

### `pim-daemon`

The main binary that wires everything together.

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

    // Spawn component tasks
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

User-facing CLI built with clap's derive API. It starts `pim-daemon`, stops it via PID-file-based signaling, and reads runtime status from config and `/run/pim.stats`. See [../getting-started/usage.md](../getting-started/usage.md) for the actual commands and flags.

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

**Dependencies**: `clap` (derive + `clap_complete`), `tokio`, `serde_json`, `anyhow`

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
