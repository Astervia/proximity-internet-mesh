# Client Usage

This guide shows how to run a PIM node as a client and how to choose the right
interfaces for different peer connectivity mechanisms.

The key distinction is:

- `[interface].name`: the local TUN interface, usually `pim0`
- `[interface].mesh_ip`: usually `"auto"` on clients so the gateway can assign it
- `transport.listen_port`: the TCP port used once a peer becomes reachable
- `[wifi_direct].interface`: the physical Wi-Fi interface used for P2P
- `[bluetooth].interface`: the Bluetooth PAN network interface, usually `bnep0`

## Before You Start

The client host should have:

- privileges to create a TUN interface and update routing, usually `root` or `CAP_NET_ADMIN`
- a writable config file such as `/etc/pim/pim.toml`
- at least one intended peer-connectivity path, such as LAN discovery, Wi-Fi Direct, Bluetooth PAN, or static peers

Generate a starter client config:

```bash
pim config generate client --output /etc/pim/pim.toml
```

## List Candidate Network Interfaces

List all network interfaces:

```bash
ip -br link
```

Show IPv4 addresses:

```bash
ip -br -4 addr
```

These commands help identify:

- the host uplink currently in use
- the Wi-Fi interface that should back Wi-Fi Direct
- whether a Bluetooth PAN interface such as `bnep0` already exists

If you want to see which interface the host would use to reach the public
internet, check:

```bash
ip route get 1.1.1.1
```

That route is not configured into the PIM client TOML the way it is for a
gateway, but it is still useful context when troubleshooting local connectivity.

## Basic Client Config

For most clients, start with automatic mesh IP assignment:

```toml
[node]
name = "client-a"
data_dir = "/var/lib/pim"

[interface]
name = "pim0"
mesh_ip = "auto"
mtu = 1400

[transport]
type = "tcp"
listen_port = 9100

[relay]
enabled = false

[security]
key_file = "/var/lib/pim/node.key"
require_encryption = true
```

Notes:

- `mesh_ip = "auto"` lets the daemon request an address from a reachable gateway
- a client does not need a `[gateway]` section enabled
- a plain client usually does not set `relay.enabled = true`

## UDP Discovery Plus Auto-Connect

If you want the client to discover nearby relays and gateways automatically over
the local LAN, enable discovery and allow both connection types.

```toml
[discovery]
enabled = true
port = 9101
broadcast_interval_ms = 5000
peer_timeout_ms = 30000
connect_relays = true
connect_gateways = true
```

Notes:

- the client listens for discovery advertisements on the UDP discovery port
- once a peer is discovered, the normal TCP transport still handles the session
- allowing both relays and gateways gives the client the most flexible bootstrap path

## Static TCP Peer Connectivity

This is the simplest client setup when you already know a relay or gateway
address.

Example:

```toml
[[peers]]
mechanism = "tcp"
address = "192.168.1.20:9100"
label = "relay-a"
```

This can be combined with discovery, Wi-Fi Direct, and Bluetooth PAN.

## Wi-Fi Direct Client

Use Wi-Fi Direct when the client should discover peers through a Wi-Fi P2P
radio link.

First, identify the Wi-Fi interface:

```bash
iw dev
```

Typical output includes interface names such as `wlan0` or `wlp2s0`.

Confirm the interface is up:

```bash
ip link show wlan0
```

PIM uses the physical Wi-Fi interface in `[wifi_direct].interface`.

Example client config:

```toml
[wifi_direct]
enabled = true
interface = "wlan0"
go_intent = 7
listen_channel = 6
op_channel = 6
connect_method = "pbc"
```

Operational notes:

- the client configures the parent Wi-Fi interface, not a transient `p2p-*` group interface
- `wpa_supplicant` with P2P support must already be running on that interface
- once a group forms, PIM uses the resulting IP path to open its normal TCP session

Useful checks:

```bash
iw dev
wpa_cli -i wlan0 p2p_find
```

## Bluetooth PAN Client

Use Bluetooth when the client should discover peers over a Bluetooth PAN link
and then hand off to the normal TCP transport.

First, inspect Bluetooth controllers and PAN interfaces.

List controllers:

```bash
bluetoothctl list
```

Show controller state:

```bash
bluetoothctl show
```

List current network interfaces and look for `bnep*`:

```bash
ip -br link
```

If a PAN interface already exists, inspect it:

```bash
ip -4 -o addr show dev bnep0
ip neigh show dev bnep0
```

Example client config:

```toml
[bluetooth]
enabled = true
interface = "bnep0"
radio_discovery_enabled = true
device_name_prefix = "PIM-"
local_alias = "PIM-client-a"
auto_discover_peers = true
poll_interval_ms = 2000
scan_interval_ms = 5000
peer_discovery_interval_ms = 2000
bluetoothctl_timeout_s = 15
discoverable_timeout_s = 180
startup_timeout_ms = 15000
```

Operational notes:

- `[bluetooth].interface` should be the PAN interface, usually `bnep0`
- PIM waits for that interface to become ready
- PIM can use `bluetoothctl` to scan for nearby devices whose names match `device_name_prefix`
- once the PAN link exists, PIM reads neighbor entries from `ip neigh show dev <interface>`
- discovered neighbor IPs are then used as normal TCP peer targets on `transport.listen_port`

For environments where radio discovery is not enough, you can also declare
static Bluetooth peers:

```toml
[[peers]]
mechanism = "bluetooth"
ip = "192.168.44.2"
label = "bt-gateway-a"
```

Useful checks:

```bash
bluetoothctl devices
ip link show bnep0
ip neigh show dev bnep0
```

## Mixed-Mechanism Client

A client can combine mechanisms. For example:

- UDP discovery on the LAN
- Wi-Fi Direct for nearby devices
- Bluetooth PAN for short-range fallback
- static TCP peers for known relays or gateways

Example:

```toml
[interface]
mesh_ip = "auto"

[discovery]
enabled = true
connect_relays = true
connect_gateways = true

[wifi_direct]
enabled = true
interface = "wlan0"

[bluetooth]
enabled = true
interface = "bnep0"

[[peers]]
mechanism = "tcp"
address = "192.168.10.20:9100"
label = "relay-a"
```

All of these still converge on the same TCP transport, handshake, routing, and
gateway-selection logic once a peer address becomes reachable.

## Minimal Client Bring-Up Checklist

1. List local interfaces with `ip -br link`.
2. If using Wi-Fi Direct, identify the Wi-Fi radio with `iw dev`.
3. If using Bluetooth PAN, identify the PAN interface with `ip -br link` and inspect it with `ip neigh show dev <bnepX>`.
4. Set `interface.mesh_ip = "auto"` unless you have a reason to force a static mesh CIDR.
5. Enable discovery if the client should auto-connect to nearby peers.
6. Enable only the peer mechanisms you actually intend to use.
7. Start the daemon with `sudo pim up --config /etc/pim/pim.toml`.
8. Verify runtime health with `sudo pim status --verbose`.

## Troubleshooting

If the client does not receive a mesh IP:

- verify `interface.mesh_ip = "auto"` if you expect dynamic assignment
- verify a reachable gateway exists
- verify discovery is enabled or that a static peer is configured

If Wi-Fi Direct does not discover peers:

- verify the Wi-Fi interface name with `iw dev`
- verify `wpa_supplicant` is managing that interface
- check whether a `p2p-*` group interface appeared after negotiation

If Bluetooth peers do not appear:

- verify Bluetooth is powered: `bluetoothctl show`
- verify the PAN interface exists: `ip link show bnep0`
- verify neighbor discovery sees peers: `ip neigh show dev bnep0`

## Related Docs

- [configuration.md](configuration.md)
- [usage.md](usage.md)
- [gateway-usage.md](gateway-usage.md)
- [../architecture/wifi-direct.md](../architecture/wifi-direct.md)
- [../architecture/bluetooth.md](../architecture/bluetooth.md)
