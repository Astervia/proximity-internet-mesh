# Transports

The PIM daemon abstracts peer-to-peer connectivity behind a transport interface
(`pim-transport`). This folder explains the concrete transport implementations
that ship in the workspace today and their host-OS requirements. Each transport
is enabled independently via its own config section; the daemon runs them
additively when more than one is enabled.

## In-kernel vs out-of-kernel — the rule

`pim-daemon` is a **portable bridge between transports and the mesh
logic**. It speaks `pim-protocol::TransportFrame` over its local TCP
listener and runs the mesh routing, gateway, NAT, identity, and
security stack on top. It does not own the platform-specific socket
APIs that bring transport bytes in.

The rule for deciding where new (or existing) transport code lives:

> A piece of transport code **stays in the daemon (in-kernel)** if its
> wire/socket access is reachable from portable Rust — `tokio`,
> `libc` on Linux, `std::net`. It **lives outside the daemon
> (out-of-kernel)** when the wire access requires a platform-specific
> managed runtime (Java `BluetoothSocket`, `IOBluetooth`, Wi-Fi P2P
> managed APIs, NetworkExtension on iOS).
>
> Out-of-kernel implementations conform to a documented wire-protocol
> spec in this folder and bridge their post-handshake bytes into the
> daemon's TCP transport at `127.0.0.1:[transport].listen_port`. The
> daemon never needs to know which transport the bytes came in on.

Out-of-kernel implementations come in two shapes:

- **Sidecar binary** — a separate process the platform shell spawns
  (e.g. `ui/tools/pim-bt-rfcomm-mac/` for macOS IOBluetooth).
- **Tauri plugin** — Kotlin/Swift code linked into the mobile app
  shell, talking to the in-process daemon library via JNI helpers
  (e.g. the Android `BluetoothPlugin.kt` for Java `BluetoothSocket`).

To add a new transport, write a wire-protocol doc in this folder, then
ship one or more implementations that satisfy it.

### Platform-lifecycle hooks for in-kernel transports

Some in-kernel transports use portable sockets (UDP, raw libc) but
need a *lifecycle* hook from the platform shell — e.g. Android's
`WifiManager.MulticastLock` so UDP broadcast frames keep flowing when
the screen is off. The lifecycle hook lives at the platform layer
(Kotlin/Swift); the socket I/O stays in the kernel daemon.

Reference patterns:

- `pim-tun` adopts a TUN fd from the platform layer via the
  `PIM_TUN_FD` env var on Android (`VpnService.establish()` →
  `detachFd()` → in-process daemon library). Same pattern works for
  any kernel-owned interface that needs platform-specific
  fd-acquisition.
- `pim-discovery` UDP broadcast on Android needs the `MulticastLock`
  acquired by `PimVpnService.kt` for the lifetime of the daemon.
  Socket itself stays in `pim-discovery`.

If a transport needs a platform-lifecycle hook AND a wire-protocol
contract (e.g. a Wi-Fi P2P group formation step driven by Java
`WifiP2pManager` followed by a Hello-style identity exchange), split
the work: lifecycle bits in Kotlin/Swift, wire bytes either in the
sidecar/plugin or bridged in for the kernel to handle.

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
