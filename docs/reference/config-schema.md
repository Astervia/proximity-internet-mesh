# Configuration Schema

This document is the canonical reference for `pim.toml`, the TOML configuration
file consumed by `pim-daemon` and `pim`. The struct backing each section lives
in `crates/pim-core/src/config/model.rs`. For task-oriented setup see
[../getting-started/configuration.md](../getting-started/configuration.md).

## File Location

By default `pim` looks for `/etc/pim/pim.toml`. The `--config <path>` flag
overrides this on every subcommand. See [cli.md](cli.md#pim) for per-subcommand
flag behaviour.

## Sections

The top-level `Config` struct defines ten sections. Sections marked **required**
have no `#[serde(default)]` attribute and must be present in the file;
everything else has a documented default and may be omitted entirely.

| Section | Required | Rust struct |
|---------|----------|-------------|
| `[node]` | **yes** | `NodeConfig` |
| `[interface]` | no | `InterfaceConfig` |
| `[discovery]` | no | `DiscoveryConfig` |
| `[transport]` | no | `TransportConfig` |
| `[routing]` | no | `RoutingConfig` |
| `[gateway]` | no | `GatewayConfig` |
| `[relay]` | no | `RelayConfig` |
| `[security]` | no | `SecurityConfig` |
| `[wifi_direct]` | no | `WifiDirectConfig` |
| `[bluetooth]` | no | `BluetoothConfig` |
| `[[peers]]` | no | `PeerConfig` / `PeerEndpointConfig` |

---

## `[node]`

Identity and persistent-state settings for the local node. This section is
**required** — the daemon will not start without it. The only mandatory field is
`name`; `data_dir` falls back to `~/.pim` when omitted.

| Key | Type | Default | Required | Notes |
|-----|------|---------|----------|-------|
| `name` | string | — | **yes** | Human-readable node name used in logs and operator output. |
| `data_dir` | string (path) | `~/.pim` | no | Directory for persistent state: generated keys, runtime metadata. Production deployments typically use `/var/lib/pim`. |

```toml
[node]
name = "my-node"
data_dir = "/var/lib/pim"
```

---

## `[interface]`

Settings for the Linux TUN interface that carries mesh IP traffic. Backed by
`InterfaceConfig`; the entire section is optional and all fields have defaults.

| Key | Type | Default | Required | Notes |
|-----|------|---------|----------|-------|
| `name` | string | `"pim0"` | no | Requested TUN interface name. macOS requires a `utunN` name (e.g. `utun0`). |
| `mtu` | integer | `1400` | no | Interface MTU in bytes. Keep this consistent across all nodes in the mesh. |
| `mesh_ip` | string | `"auto"` | no | Mesh IPv4 address as a CIDR (e.g. `"10.77.0.1/24"`) or the string `"auto"` to request automatic assignment from a gateway. |
| `mesh_ipv6` | optional string | _(none)_ | no | Optional mesh IPv6 CIDR assigned to the local TUN interface (e.g. `"fd77::10/64"`). Leave unset to run IPv4-only. |

```toml
[interface]
name = "pim0"
mesh_ip = "10.77.0.10/24"
mtu = 1400
# mesh_ipv6 = "fd77::10/64"
```

---

## `[discovery]`

UDP broadcast peer-discovery timing and policy. When `enabled = false` the
daemon connects only to statically configured `[[peers]]` entries.

| Key | Type | Default | Required | Notes |
|-----|------|---------|----------|-------|
| `enabled` | boolean | `true` | no | Enable or disable the discovery service entirely. |
| `port` | integer | `9101` | no | UDP port for sending and receiving discovery broadcasts. |
| `broadcast_interval_ms` | integer | `5000` | no | Interval between outgoing broadcasts, in milliseconds. |
| `peer_timeout_ms` | integer | `30000` | no | Time after which an unseen peer is considered stale, in milliseconds. |
| `connect_relays` | boolean | `true` | no | Automatically connect to discovered peers advertising relay capability. |
| `connect_gateways` | boolean | `true` | no | Automatically connect to discovered peers advertising gateway capability. |
| `shared_key` | optional string | _(none)_ | no | Optional 32-byte discovery group key encoded as 64 hex characters. Only nodes with the same key can decode each other's broadcasts. |

```toml
[discovery]
enabled = true
broadcast_interval_ms = 5000
peer_timeout_ms = 30000
# shared_key = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff"
```

---

## `[transport]`

Wire-transport configuration for direct peer connections. Currently the only
supported backend is `tcp`.

| Key | Type | Default | Required | Notes |
|-----|------|---------|----------|-------|
| `type` | string | `"tcp"` | no | Transport backend. Currently only `"tcp"` is implemented. |
| `listen_port` | integer | `9100` | no | TCP port this node listens on for inbound peer connections. |
| `max_reconnect_attempts` | integer | `20` | no | Maximum reconnect attempts per peer before giving up. |
| `connect_timeout_ms` | integer | `3000` | no | Timeout for outbound TCP connect attempts, in milliseconds. |

```toml
[transport]
type = "tcp"
listen_port = 9100
```

---

## `[routing]`

Route-propagation and route-aging settings for the distance-vector routing
engine.

| Key | Type | Default | Required | Notes |
|-----|------|---------|----------|-------|
| `max_hops` | integer | `10` | no | Maximum hop count accepted before a route is considered unusable. |
| `algorithm` | string | `"distance-vector"` | no | Routing algorithm identifier used for compatibility and diagnostics. |
| `route_expiry_s` | integer | `300` | no | Lifetime of learned routes before expiry, in seconds. |

```toml
[routing]
max_hops = 10
algorithm = "distance-vector"
route_expiry_s = 300
```

---

## `[gateway]`

Controls whether this node acts as an internet gateway with NAT. Gateway nodes
are implicitly relays regardless of the `[relay]` setting.

| Key | Type | Default | Required | Notes |
|-----|------|---------|----------|-------|
| `enabled` | boolean | `false` | no | Enable gateway and NAT behaviour. |
| `nat_interface` | string | `"eth0"` | no | Name of the internet-facing interface used for masquerading (e.g. `"eno1"`, `"eth0"`). Replace with the actual upstream interface on the host. |
| `max_connections` | integer | `200` | no | Maximum number of concurrent tracked gateway connections. |

```toml
[gateway]
enabled = true
nat_interface = "eth0"
max_connections = 200
```

---

## `[relay]`

Controls whether this node forwards traffic for other mesh peers. When
`enabled = true` the node acts as a relay. Gateway nodes (`gateway.enabled =
true`) are always relays regardless of this setting.

| Key | Type | Default | Required | Notes |
|-----|------|---------|----------|-------|
| `enabled` | boolean | `false` | no | Enable relay forwarding. |

```toml
[relay]
enabled = true
```

---

## `[security]`

Encryption policy and key-storage configuration. See
[../architecture/security.md](../architecture/security.md) for the meaning of
`require_encryption` and key-file format details.

| Key | Type | Default | Required | Notes |
|-----|------|---------|----------|-------|
| `key_file` | string (path) | `~/.pim/node.key` | no | Path to the Ed25519 private key file. The daemon generates this file on first startup if it does not exist. |
| `require_encryption` | boolean | `true` | no | When `true`, unencrypted peer sessions are rejected. |
| `authorization_policy` | string | `"allow_all"` | no | Policy applied after peer identity is authenticated. One of `"allow_all"`, `"allow_list"`, or `"trust_on_first_use"`. |
| `authorized_peers` | array of strings | `[]` | no | Explicitly authorized peer node IDs. Used only when `authorization_policy = "allow_list"`. |
| `trust_store_file` | string (path) | `~/.pim/trusted-peers.toml` | no | Persistent trust store used by `"trust_on_first_use"`. |

```toml
[security]
key_file = "/var/lib/pim/node.key"
require_encryption = true
authorization_policy = "allow_all"
trust_store_file = "/var/lib/pim/trusted-peers.toml"
# authorized_peers = ["0123456789abcdef0123456789abcdef"]
```

---

## `[wifi_direct]`

Wi-Fi Direct (IEEE 802.11 P2P) peer discovery and group-formation settings.
Disabled by default (opt-in). Requires `wpa_supplicant` compiled with P2P
support to be running and controlling the named interface.

Once a Wi-Fi Direct group is established the resulting IP address is handed to
the standard TCP transport, so all security, routing, and gateway logic applies
unchanged.

| Key | Type | Default | Required | Notes |
|-----|------|---------|----------|-------|
| `enabled` | boolean | `false` | no | Enable Wi-Fi Direct peer discovery. |
| `interface` | string | `"wlan0"` | no | Physical Wi-Fi interface to use for P2P operations. |
| `go_intent` | integer | `7` | no | Group Owner intent value (0–15). Higher values make this node more likely to become the Group Owner during negotiation. |
| `listen_channel` | integer | `6` | no | P2P listen channel number. |
| `op_channel` | integer | `6` | no | P2P operating channel number. |
| `connect_method` | string | `"pbc"` | no | Connection method: `"pbc"` (push-button) or `"pin:<8-digit-pin>"`. |

```toml
[wifi_direct]
enabled = true
interface = "wlan0"
go_intent = 7
listen_channel = 6
op_channel = 6
connect_method = "pbc"
```

---

## `[bluetooth]`

Bluetooth PAN link-establishment and supervision configuration. Disabled by
default (opt-in). This mechanism does not manage pairing — it waits for a
Bluetooth PAN interface to appear, learns peer IPs from the PAN neighbor table,
and hands them to the daemon so the standard TCP transport and handshake logic
can connect normally.

Static Bluetooth peers are configured under `[[peers]]` with
`mechanism = "bluetooth"`.

| Key | Type | Default | Required | Notes |
|-----|------|---------|----------|-------|
| `enabled` | boolean | `false` | no | Enable Bluetooth PAN link monitoring. |
| `interface` | string | `"auto"` | no | Preferred PAN-facing interface name or `"auto"` for runtime resolution. |
| `radio_discovery_enabled` | boolean | `true` | no | Enable radio-level Bluetooth discovery and pairing for new peers. |
| `device_name_prefix` | string | `"PIM-"` | no | Prefix used to identify PIM peers by Bluetooth device name. |
| `local_alias` | string | `""` | no | Local Bluetooth controller alias to advertise. Empty string means the alias is derived from the node name. |
| `connect_pan` | boolean | `true` | no | Allow outbound PAN/NAP connection attempts to discovered peers. |
| `serve_nap` | boolean | `false` | no | Start and supervise a local Linux NAP server process. |
| `nap_bridge` | string | `"br-bt"` | no | Linux bridge/interface to expose through the local NAP server. |
| `nap_bridge_addr` | string | `"192.168.44.1/24"` | no | IPv4 address/CIDR assigned to `nap_bridge` when the daemon manages it. |
| `dhcp_enabled` | boolean | `true` | no | Run a daemon-supervised DHCP server (dnsmasq) on `nap_bridge` when serving NAP. |
| `dhcp_range` | optional string | _(none)_ | no | Explicit DHCP range as `"start,end"`. When unset, derived automatically from `nap_bridge_addr`. |
| `dhcp_lease_time` | string | `"12h"` | no | DHCP lease time passed to dnsmasq (e.g. `"12h"`, `"infinite"`). |
| `dhcp_dns` | optional string | _(none)_ | no | Comma-separated DNS server list advertised to DHCP clients. When unset, inherited from the host's `/etc/resolv.conf` at runtime. |
| `request_dhcp` | boolean | `true` | no | Automatically request DHCP on the resolved PAN interface when acting as a PAN client (`connect_pan = true`, `serve_nap = false`). |
| `auto_discover_peers` | boolean | `true` | no | Automatically discover peer IPs from the PAN interface neighbor table. |
| `poll_interval_ms` | integer | `2000` | no | Poll interval while waiting for the PAN interface to become ready, in milliseconds. |
| `scan_interval_ms` | integer | `5000` | no | Poll interval for radio-level device scans, in milliseconds. |
| `peer_discovery_interval_ms` | integer | `2000` | no | Poll interval for automatic peer discovery after the interface is ready, in milliseconds. |
| `bluetoothctl_timeout_s` | integer | `15` | no | Timeout for `bluetoothctl` operations, in seconds. |
| `discoverable_timeout_s` | integer | `180` | no | How long the controller remains discoverable after startup, in seconds. |
| `startup_timeout_ms` | integer | `15000` | no | Maximum time to wait for the PAN interface to appear before giving up, in milliseconds. |

```toml
[bluetooth]
enabled = true
interface = "auto"
radio_discovery_enabled = true
device_name_prefix = "PIM-"
connect_pan = true
serve_nap = false
auto_discover_peers = true
poll_interval_ms = 2000
```

---

## `[[peers]]`

Statically configured peer targets. This is an array of tables — add one
`[[peers]]` block per peer. Optional; nodes can rely entirely on
`[discovery]` when this list is empty.

Each entry has a common `label` field plus mechanism-specific fields controlled
by the `mechanism` key (implemented as a TOML inline tag).

### Common fields

| Key | Type | Default | Required | Notes |
|-----|------|---------|----------|-------|
| `label` | string | `""` | no | Human-readable label for logs and operator output. |
| `mechanism` | string | — | **yes** | Connection mechanism. One of `"tcp"` or `"bluetooth"`. |

### `mechanism = "tcp"` fields

| Key | Type | Default | Required | Notes |
|-----|------|---------|----------|-------|
| `address` | string | — | **yes** | TCP address to connect to, e.g. `"192.168.1.1:9100"`. |

### `mechanism = "bluetooth"` fields

| Key | Type | Default | Required | Notes |
|-----|------|---------|----------|-------|
| `ip` | string | — | **yes** | IPv4 or IPv6 address reachable on the Bluetooth PAN interface. |

```toml
[[peers]]
mechanism = "tcp"
address = "192.168.1.20:9100"
label = "relay-a"

[[peers]]
mechanism = "bluetooth"
ip = "192.168.44.2"
label = "bt-gateway-a"
```

---

## See Also

- [config-examples/client-bt-only.toml](config-examples/client-bt-only.toml) — minimal Bluetooth-only client.
- [config-examples/gateway-bluetooth.toml](config-examples/gateway-bluetooth.toml) — Bluetooth NAP gateway.
- [../getting-started/configuration.md](../getting-started/configuration.md) — task-oriented walkthrough.
- [../architecture/security.md](../architecture/security.md) — meaning of `[security].require_encryption` and key files.
