# Agent Conventions

These conventions are specific to this repository and should guide agent work.

## Architectural Rules

1. Keep `TcpTransport` as the transport unless the task explicitly requires a new transport backend.
2. Prefer adding new peer-finding or link-setup mechanisms that feed the existing daemon connection path.
3. Reuse `initiate_peer_connection()` for newly discovered neighbor addresses when possible.
4. Treat peer discovery, link establishment, handshake, routing, and gateway behavior as separate layers.
5. Avoid bypassing the existing handshake/session path in `pim-daemon`.

## Current Connectivity Model

- `pim-transport` provides the direct peer transport abstraction and current TCP backend.
- `pim-discovery` provides LAN UDP discovery and emits `PeerRecord`.
- `pim-wifidirect` provides Wi-Fi Direct peer-finding and emits `SocketAddr`.
- `pim-daemon` owns connection initiation, handshake, reconnect, routing, and runtime orchestration.

## Preferred Extension Pattern

When adding a new connection mechanism such as Bluetooth P2P:

1. Add a dedicated crate if the mechanism has distinct OS integration or process control needs.
2. Keep its public contract narrow.
3. Emit either:
    - `SocketAddr` when the mechanism produces an IP-reachable peer endpoint, or
    - a clearly scoped connection descriptor if a new adaptation layer is unavoidable.
4. Integrate it in `pim-daemon` similarly to Wi-Fi Direct: spawn service, consume results, call connection setup.
5. Add config under `pim-core::Config` with an opt-in `enabled` flag and mechanism-specific settings.

## Testing Expectations

At minimum, changes should include:

- config parsing tests
- service construction tests
- daemon integration tests where practical
- docs updates when behavior or topology assumptions change

## Documentation Expectations

If the connectivity model changes, update:

- `docs/architecture/packet-flow.md`
- `docs/architecture/system-overview.md`
- `docs/architecture/wifi-direct.md` if the new mechanism changes comparative positioning
- `README.md` if operator-facing setup changes
