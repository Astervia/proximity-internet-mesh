# Bluetooth PAN Connectivity

PIM treats Bluetooth as an optional **peer discovery and link-establishment**
mechanism, not a new wire transport. The implementation supports Linux
Bluetooth PAN links through BlueZ helper commands and macOS Bluetooth PAN
through the host Bluetooth stack. At a high level it can:

- advertise a PIM-specific Bluetooth alias
- scan for nearby Bluetooth devices
- pair and trust matching devices
- request a PAN/NAP link
- learn peer IPs from the resulting PAN neighbor table

Once that interface is up, PIM feeds the discovered peer IPs into the existing
`TcpTransport`, so the normal handshake, session, routing, and gateway logic
remain unchanged. Static peer IPs remain available as a fallback.

The Linux daemon also supports an independent RFCOMM path. RFCOMM scans paired
devices by name prefix, opens a configured RFCOMM channel, exchanges PIM
identity frames, and bridges the RFCOMM byte stream to the local TCP transport
listener. That makes an RFCOMM peer behave like a normal authenticated PIM
session after the channel is established.

## Scope

- Bluetooth is opt-in via `[bluetooth] enabled = true`
- The daemon can perform radio-level device discovery via `bluetoothctl` on Linux or `blueutil` on macOS
- Linux requests PAN/NAP setup via `bt-network`; macOS uses the host stack's connection flow
- The daemon waits for the PAN interface to become ready
- When ready, it discovers peer IPs and emits `SocketAddr`s using `[transport] listen_port`
- RFCOMM is opt-in via `[bluetooth_rfcomm] enabled = true` and does not require a PAN interface

This keeps Bluetooth aligned with the Wi-Fi Direct design: link setup first,
standard TCP connection second.

## Configuration

```toml
[transport]
listen_port = 9100

[bluetooth]
enabled = true
interface = "auto"         # runtime hint on Linux; use "bridge0" on macOS
radio_discovery_enabled = true
device_name_prefix = "PIM-"
local_alias = "PIM-my-node"  # optional; defaults from node.name
connect_pan = true
serve_nap = false
nap_bridge = "br-bt"
auto_discover_peers = true
poll_interval_ms = 2000
scan_interval_ms = 5000
peer_discovery_interval_ms = 2000
bluetoothctl_timeout_s = 15
discoverable_timeout_s = 180
startup_timeout_ms = 15000

[bluetooth_rfcomm]
enabled = false
channel = 22
device_name_prefix = "PIM-"
outbound_enabled = true
poll_interval_ms = 30000
bridge_to_tcp = true
```

`interface` is now a preferred hint on Linux rather than a fixed requirement.
When set to `"auto"`, the daemon prefers a ready configured interface if one
exists, then falls back to live `bnep*` or `enx*` PAN interfaces.

When `connect_pan = true` and `radio_discovery_enabled = true`, the daemon
scans for nearby devices whose names match `device_name_prefix`. Linux uses
`bluetoothctl` followed by `bt-network -c <mac> nap`. macOS uses `blueutil`
for inquiry, pairing, and connection against the host Bluetooth stack.

When `serve_nap = true` on Linux, the daemon supervises `bt-network -s nap
<nap_bridge>` so a gateway can expose a local NAP service without a separate
operator-managed helper process.

When `auto_discover_peers = true`, the daemon also polls the host neighbor
table and converts discovered PAN neighbor IPs into `SocketAddr`s using
`[transport] listen_port`. Linux uses `ip neigh show dev <interface>`, while
macOS uses `arp -an -i <interface>`. Static Bluetooth peers can also be
declared under `[[peers]]` with `mechanism = "bluetooth"` for environments
where neighbor discovery is incomplete.

When `[bluetooth_rfcomm].enabled = true`, Linux also binds the configured
RFCOMM channel and optionally scans paired devices with `bluetoothctl devices
Paired`. Matching peers are dialed on that channel. After the RFCOMM
hello/hello-ack identity exchange, `bridge_to_tcp = true` forwards bytes to
`127.0.0.1:<transport.listen_port>`, reusing the normal PIM transport and
security path. `bridge_to_tcp = false` leaves the service in discovery-only
mode.

## Daemon Flow

```
Bluetooth PAN interface up
        │
        ▼
BluetoothDiscovery::run
        │
        ├─ Linux: bluetoothctl power/pairable/discoverable/system-alias
        ├─ Linux: bluetoothctl scan on
        ├─ Linux/macOS: device discovery → filter names by prefix
        ├─ Linux/macOS: pair/connect <mac>
        ├─ Linux: optionally starts bt-network -s nap <bridge>
        ├─ Linux: bt-network -c <mac> nap when connect_pan = true
        ├─ Linux: resolves a ready PAN interface from configured / bnep* / enx*
        ├─ macOS: waits for ifconfig <interface> to become active
        ├─ Linux: runs ip neigh show dev <interface>
        ├─ macOS: runs arp -an -i <interface>
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

RFCOMM flow:

```
Paired Bluetooth device whose name matches prefix
        │
        ▼
RfcommService
        │
        ├─ bind configured RFCOMM channel
        ├─ outbound loop: bluetoothctl devices Paired
        ├─ dial matching peer on configured channel
        ├─ accept inbound peer channels
        ├─ exchange hello / hello-ack identity JSON
        └─ bridge RFCOMM bytes to 127.0.0.1:[transport].listen_port
                │
                ▼
TCP transport listener
        │
        └─ authenticated handshake + normal session setup
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
Bluetooth RFCOMM        ──▶ RFCOMM bridge ──▶ TCP listener ──▶ normal session
```

If the same peer is reachable through more than one mechanism, duplicate
connection attempts still collapse onto the existing session and reconnect logic.

## Limitations

- The implementation shells out to `bluetoothctl` and `bt-network` rather than using D-Bus directly.
- Real-world success still depends on the host BlueZ stack and a PAN/NAP-capable peer.
- Automatic discovery depends on the platform neighbor table for the PAN interface.
- Docker can test the orchestration seams, but not real RF discovery or PAN behavior.
- The readiness check is platform-specific: Linux resolves a ready interface from `/sys/class/net`, while macOS uses `ifconfig <interface>`.

## Related Documents

- [overview.md](../overview.md)
- [wifi-direct.md](wifi-direct.md)
- [discovery.md](../discovery.md)
