# Connectivity Architecture Map

This map is for agent tasks that change peer connectivity behavior.

## Key Files

- `crates/pim-core/src/config.rs`
    - shared config model
    - transport config
    - Wi-Fi Direct config
- `crates/pim-transport/src/lib.rs`
    - `Transport` trait
    - `PeerAddress`
- `crates/pim-transport/src/tcp.rs`
    - current transport backend
- `crates/pim-discovery/src/lib.rs`
    - UDP LAN discovery entry point
- `crates/pim-daemon/src/main.rs`
    - transport startup
    - static peer bootstrapping
    - discovery consumers
    - handshake and reconnect
- `crates/pim-wifidirect/src/lib.rs`
    - Wi-Fi Direct discovery service
    - emits peer `SocketAddr`
- `docs/architecture/wifi-direct.md`
    - current reference design for non-LAN peer finding

## Current Flow

1. Daemon starts `TcpTransport`.
2. Static peers, UDP discovery, and Wi-Fi Direct discovery all produce candidate neighbors.
3. Candidates converge into `initiate_peer_connection()`.
4. The daemon performs handshake and learns the real peer identity.
5. Established sessions participate in routing and forwarding.

## Design Bias

The current codebase is biased toward:

- one transport path
- multiple discovery or link-establishment mechanisms
- shared handshake and routing logic

New connectivity work should preserve that layering unless there is a strong technical reason not to.
