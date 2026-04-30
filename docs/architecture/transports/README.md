# Transports

The PIM daemon abstracts peer-to-peer connectivity behind a transport interface
(`pim-transport`). This folder explains the concrete transport implementations
that ship in the workspace today and their host-OS requirements. Each transport
is enabled independently via its own config section; the daemon runs them
additively when more than one is enabled.

## Implementations

- [bluetooth.md](bluetooth.md) — Bluetooth PAN backend, Linux NAP/PANU roles, and Bluetooth RFCOMM direct-channel bridging.
- [wifi-direct.md](wifi-direct.md) — Wi-Fi Direct backend on Linux (`wpa_supplicant` P2P) and macOS (Bonjour peer-to-peer), discovery flow, and capability matrix.

## Picking A Transport

| Goal | Recommended transport | Notes |
|------|-----------------------|-------|
| Local lab on a single host | TCP over loopback (configured in `[transport]`) | Default; no radio hardware required. |
| Two devices in radio range, no infrastructure | Wi-Fi Direct | Highest throughput; needs Wi-Fi P2P-capable hardware. |
| Very low power / always-on pairing | Bluetooth PAN or RFCOMM | PAN gives an IP link; RFCOMM bridges paired devices directly into the TCP listener. |
| Mixed mesh | Multiple transports enabled simultaneously | The daemon discovers peers per-transport and reuses the routing layer. |

For protocol-level frame layout shared across all transports, see [../protocol.md](../protocol.md). For discovery (UDP broadcast, Bonjour, P2P scans), see [../discovery.md](../discovery.md).
