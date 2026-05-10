# Transports

The PIM daemon abstracts peer-to-peer connectivity behind a transport interface
(`pim-transport`). This folder explains the concrete transport implementations
that ship in the workspace today and their host-OS requirements. Each transport
is enabled independently via its own config section; the daemon runs them
additively when more than one is enabled.

## Architectural placement

`pim-daemon` is a **portable bridge between transports and the mesh
logic**. It speaks `pim-protocol::TransportFrame` over its local TCP
listener and runs the mesh routing, gateway, NAT, identity, and
security stack on top. It does not own the platform-specific socket
APIs that bring transport bytes in.

Each transport ships as one or more **wire-protocol specs** (in this
folder) plus implementations that conform to the spec. An
implementation lives wherever the platform's native socket API is
reachable:

- In-tree as a Rust crate when libc is enough (e.g.
  `pim-bluetooth` for Linux RFCOMM, `pim-discovery` for UDP
  broadcasts).
- As a sidecar binary when the platform requires a host stack (e.g.
  `ui/tools/pim-bt-rfcomm-mac/` for IOBluetooth).
- As a Tauri plugin when the platform locks the API behind a
  managed runtime (e.g. the Android Kotlin BluetoothPlugin behind
  Java `BluetoothSocket`).

The daemon never sees Bluetooth, Wi-Fi Direct, or any other transport
directly — it sees a TCP socket on `127.0.0.1:[transport].listen_port`
that some external bridge dropped a peer into. To add a new transport,
add a new wire-protocol doc here and one or more implementations that
satisfy it.

## Implementations

- [bluetooth.md](bluetooth.md) — Bluetooth PAN backend, Linux NAP/PANU roles, and Bluetooth RFCOMM direct-channel bridging.
- [bt-rfcomm-protocol.md](bt-rfcomm-protocol.md) — Cross-platform wire-protocol spec for the RFCOMM Hello/HelloAck handshake. Authoritative for the Linux Rust crate, the macOS Swift sidecar, and the forthcoming Android Kotlin plugin.
- [wifi-direct.md](wifi-direct.md) — Wi-Fi Direct backend on Linux (`wpa_supplicant` P2P) and macOS (Bonjour peer-to-peer), discovery flow, and capability matrix.

## Picking A Transport

| Goal | Recommended transport | Notes |
|------|-----------------------|-------|
| Local lab on a single host | TCP over loopback (configured in `[transport]`) | Default; no radio hardware required. |
| Two devices in radio range, no infrastructure | Wi-Fi Direct | Highest throughput; needs Wi-Fi P2P-capable hardware. |
| Very low power / always-on pairing | Bluetooth PAN or RFCOMM | PAN gives an IP link; RFCOMM bridges paired devices directly into the TCP listener. |
| Mixed mesh | Multiple transports enabled simultaneously | The daemon discovers peers per-transport and reuses the routing layer. |

For protocol-level frame layout shared across all transports, see [../protocol.md](../protocol.md). For discovery (UDP broadcast, Bonjour, P2P scans), see [../discovery.md](../discovery.md).
