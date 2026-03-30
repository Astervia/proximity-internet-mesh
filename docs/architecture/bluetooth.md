# Bluetooth PAN Connectivity

PIM treats Bluetooth as an optional **peer discovery and link-establishment**
mechanism, not a new wire transport. The current implementation targets Linux
Bluetooth PAN links and uses BlueZ helper commands to:

- advertise a PIM-specific Bluetooth alias
- scan for nearby Bluetooth devices
- pair and trust matching devices
- request a PAN/NAP link
- learn peer IPs from the resulting PAN neighbor table

Once that interface is up, PIM feeds the discovered peer IPs into the existing
`TcpTransport`, so the normal handshake, session, routing, and gateway logic
remain unchanged. Static peer IPs remain available as a fallback.

## Scope

- Bluetooth is opt-in via `[bluetooth] enabled = true`
- The daemon can perform radio-level device discovery via `bluetoothctl`
- The daemon can request PAN/NAP setup via `bt-network`
- The daemon waits for the PAN interface to become ready
- When ready, it discovers peer IPs and emits `SocketAddr`s using `[transport] listen_port`

This keeps Bluetooth aligned with the Wi-Fi Direct design: link setup first,
standard TCP connection second.

## Configuration

```toml
[transport]
listen_port = 9100

[bluetooth]
enabled = true
interface = "bnep0"
radio_discovery_enabled = true
device_name_prefix = "PIM-"
local_alias = "PIM-my-node"  # optional; defaults from node.name
auto_discover_peers = true
peer_addresses = []   # optional static fallback
poll_interval_ms = 2000
scan_interval_ms = 5000
peer_discovery_interval_ms = 2000
bluetoothctl_timeout_s = 15
discoverable_timeout_s = 180
startup_timeout_ms = 15000
```

When `radio_discovery_enabled = true`, the daemon uses `bluetoothctl` to scan
for nearby devices whose names match `device_name_prefix`. For each match it
attempts `pair`, `trust`, `connect`, and then `bt-network -c <mac> nap`.

When `auto_discover_peers = true`, the daemon also polls
`ip neigh show dev <interface>` and converts discovered PAN neighbor IPs into
`SocketAddr`s using `[transport] listen_port`. `peer_addresses` can still be
provided as a fallback or for environments where neighbor discovery is incomplete.

## Daemon Flow

```
Bluetooth PAN interface up
        │
        ▼
BluetoothDiscovery::run
        │
        ├─ bluetoothctl power/pairable/discoverable/system-alias
        ├─ bluetoothctl scan on
        ├─ bluetoothctl devices → filter names by prefix
        ├─ bluetoothctl pair/trust/connect <mac>
        ├─ bt-network -c <mac> nap
        ├─ waits for /sys/class/net/<interface>/operstate
        ├─ runs ip neigh show dev <interface>
        ├─ parses discovered neighbor IPs
        └─ emits SocketAddr(peer_ip, transport.listen_port)
                │
                ▼
run_bluetooth_consumer
        │
        ├─ reconnect.register_discovered(addr)
        └─ initiate_peer_connection(state, addr)
                 │
                 └─ TCP + authenticated handshake + session setup
```

## Docker-Testable Seam

For container-based testing, the daemon also honors:

```text
PIM_BLUETOOTH_SYSFS_ROOT=/path/to/fake/sysfs
PIM_BLUETOOTH_IP_COMMAND=/path/to/fake-ip
PIM_BLUETOOTH_BLUETOOTHCTL_COMMAND=/path/to/fake-bluetoothctl
PIM_BLUETOOTH_BT_NETWORK_COMMAND=/path/to/fake-bt-network
```

When set, `BluetoothDiscovery` reads `<root>/<interface>/operstate` instead of
the host's `/sys/class/net/<interface>/operstate`. This allows Docker tests to
simulate Bluetooth radio discovery and PAN setup by replacing `bluetoothctl`,
`bt-network`, and `ip neigh` with fixture scripts, plus flipping a plain file
from `down` to `up`, without requiring BlueZ or actual Bluetooth hardware.

## Coexistence

Bluetooth PAN is additive with the existing mechanisms:

```text
Static [[peers]]        ──▶ initiate_peer_connection
UDP discovery           ──▶ run_discovery_consumer ──▶ initiate_peer_connection
Wi-Fi Direct            ──▶ run_wifidirect_consumer ──▶ initiate_peer_connection
Bluetooth PAN           ──▶ run_bluetooth_consumer ──▶ initiate_peer_connection
```

If the same peer is reachable through more than one mechanism, duplicate
connection attempts still collapse onto the existing session and reconnect logic.

## Limitations

- The implementation shells out to `bluetoothctl` and `bt-network` rather than using D-Bus directly.
- Real-world success still depends on the host BlueZ stack and a PAN/NAP-capable peer.
- Automatic discovery depends on the Linux neighbor table for the PAN interface.
- Docker can test the orchestration seams, but not real RF discovery or PAN behavior.
- The readiness check is Linux-specific and depends on `/sys/class/net/<interface>/operstate`.

## Related Documents

- [system-overview.md](system-overview.md)
- [wifi-direct.md](wifi-direct.md)
- [discovery.md](discovery.md)
