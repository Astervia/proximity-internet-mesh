# Bluetooth PAN Connectivity

PIM treats Bluetooth as an optional **link-establishment** mechanism, not a new
wire transport. The current implementation targets Linux Bluetooth PAN links
that expose an IP-capable interface such as `bnep0`. Once that interface is up,
PIM feeds the configured peer IPs into the existing `TcpTransport`, so the
normal handshake, session, routing, and gateway logic remain unchanged.

## Scope

- Bluetooth is opt-in via `[bluetooth] enabled = true`
- Pairing and PAN creation are out of scope for the daemon
- The daemon waits for the PAN interface to become ready
- When ready, it emits `SocketAddr`s using `[transport] listen_port`

This keeps Bluetooth aligned with the Wi-Fi Direct design: link setup first,
standard TCP connection second.

## Configuration

```toml
[transport]
listen_port = 9100

[bluetooth]
enabled = true
interface = "bnep0"
peer_addresses = ["192.168.44.2"]
poll_interval_ms = 2000
startup_timeout_ms = 15000
```

`peer_addresses` contains the remote IPs expected to be reachable over the PAN
link. The daemon combines each address with `[transport] listen_port` before
calling the existing connection path.

## Daemon Flow

```
Bluetooth PAN interface up
        │
        ▼
BluetoothDiscovery::run
        │
        ├─ waits for /sys/class/net/<interface>/operstate
        ├─ validates configured peer IPs
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
```

When set, `BluetoothDiscovery` reads `<root>/<interface>/operstate` instead of
the host's `/sys/class/net/<interface>/operstate`. This allows Docker tests to
simulate a Bluetooth PAN interface by flipping a plain file from `down` to `up`
without requiring BlueZ or actual Bluetooth hardware.

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

- Only pre-established Bluetooth PAN links are supported.
- The daemon does not invoke BlueZ, `bluetoothctl`, or D-Bus to pair devices.
- `peer_addresses` must be configured explicitly; there is no Bluetooth-side peer discovery yet.
- The readiness check is Linux-specific and depends on `/sys/class/net/<interface>/operstate`.

## Related Documents

- [system-overview.md](system-overview.md)
- [wifi-direct.md](wifi-direct.md)
- [discovery.md](discovery.md)
