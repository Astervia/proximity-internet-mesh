# Transports

The PIM daemon abstracts peer-to-peer connectivity behind a transport interface
(`pim-transport`). This folder explains the concrete transport implementations
that ship in the workspace today, their host-OS requirements, and how the
daemon chooses between them.

## Implementations

- [bluetooth.md](bluetooth.md): Bluetooth PAN backend, NAP/PANU roles, and the bridge/DHCP setup the daemon manages on Linux gateways.
- [wifi-direct.md](wifi-direct.md): Wi-Fi Direct backend on Linux (`wpa_supplicant` P2P) and macOS (Bonjour peer-to-peer), discovery flow, and capability matrix.

## Picking A Transport

| Goal | Recommended transport | Notes |
|------|-----------------------|-------|
| Local lab on a single host | TCP over loopback (configured in `[transport]`) | Default; no radio hardware required. |
| Two devices in radio range, no infrastructure | Wi-Fi Direct | Highest throughput; needs Wi-Fi P2P-capable hardware. |
| Very low power / always-on pairing | Bluetooth PAN | Lower bandwidth; pairs well with NAP-mode gateways. |
| Mixed mesh | Multiple transports enabled simultaneously | The daemon discovers peers per-transport and reuses the routing layer. |

For protocol-level frame layout shared across all transports, see [../protocol.md](../protocol.md). For discovery (UDP broadcast, Bonjour, P2P scans), see [../discovery.md](../discovery.md).
