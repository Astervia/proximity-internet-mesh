# Gateway Usage

This guide shows how to run a PIM node as a gateway and how to choose the right
interfaces for different peer connectivity mechanisms.

The key distinction is:

- `gateway.nat_interface`: the real internet-facing interface used for NAT
- `transport.listen_port`: the TCP port peers connect to after discovery
- `[wifi_direct].interface`: the physical Wi-Fi interface used for P2P
- `[bluetooth].interface`: a preferred Bluetooth PAN interface hint; use `"auto"` on Linux unless you need to pin a specific interface

## Before You Start

The gateway host should have:

- working internet access on a normal host interface such as `eth0`, `enp3s0`, or `wlan0`
- privileges to create a TUN interface and configure NAT, usually `root` or `CAP_NET_ADMIN`
- a writable config file such as `/etc/pim/pim.toml`

Generate a starter gateway config:

```bash
pim config generate gateway --output /etc/pim/pim.toml
```

On macOS, use a `utunN` name such as `utun0` for `[interface].name`, choose a real uplink such as `en0` for `[gateway].nat_interface`, and install `blueutil` if this gateway should use Bluetooth PAN radio discovery. Wi-Fi Direct can also be enabled there; macOS uses Bonjour peer-to-peer discovery instead of Linux `wpa_supplicant` controls.

## Discover the Internet-Facing Interface

The most reliable way to discover the interface PIM should use for NAT is to ask
the kernel how it would reach a public IP.

Check the route to `1.1.1.1` on Linux:

```bash
ip route get 1.1.1.1
```

Typical output:

```text
1.1.1.1 via 192.168.1.1 dev wlan0 src 192.168.1.44 uid 1000
```

In this example, the correct NAT interface is `wlan0`.

On macOS, the equivalent command is:

```bash
route -n get default
```

Look for the `interface:` line and use that value for `gateway.nat_interface`.

You can extract just the interface name:

```bash
ip route get 1.1.1.1 | sed -n 's/.* dev \([^ ]*\) .*/\1/p'
```

Confirm that the interface has an IPv4 address on Linux:

```bash
ip -4 -o addr show dev wlan0
```

On macOS, the equivalent check is:

```bash
ifconfig en0
```

PIM requires a usable IPv4 address on `gateway.nat_interface` during startup.

## List Candidate Network Interfaces

List all network interfaces:

```bash
ip -br link
```

Show IPv4 addresses:

```bash
ip -br -4 addr
```

Show the current default route:

```bash
ip route show default
```

These commands are useful when the route-to-`1.1.1.1` check shows a VPN,
cellular interface, or some other path you do not want to use for gateway NAT.

## Static TCP Peer Connectivity

This is the simplest gateway setup. Peers connect directly to a known TCP
address.

Example config:

```toml
[node]
name = "gateway-a"
data_dir = "/var/lib/pim"

[interface]
name = "pim0"
mesh_ip = "10.77.0.1/24"
mtu = 1400

[transport]
type = "tcp"
listen_port = 9100

[gateway]
enabled = true
nat_interface = "wlan0"
max_connections = 200

[security]
key_file = "/var/lib/pim/node.key"
require_encryption = true
```

Start it:

```bash
sudo pim up --config /etc/pim/pim.toml
```

Check status:

```bash
sudo pim status --verbose
```

## UDP Discovery Plus Gateway Auto-Connect

If you want nearby nodes to discover the gateway automatically over the local
LAN, leave discovery enabled and allow gateway connections.

Example additions:

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

- the gateway advertises itself on the discovery UDP port
- peers that permit gateway connections can initiate a normal TCP session to `transport.listen_port`
- discovery does not replace the transport; it only finds peers

## Wi-Fi Direct Gateway

Use Wi-Fi Direct when the gateway should form P2P Wi-Fi groups and let peers
connect through the resulting link.

First, identify the Wi-Fi interface:

```bash
iw dev
```

Typical output includes interface names such as `wlan0` or `wlp2s0`.

Confirm the interface is up:

```bash
ip link show wlan0
```

On Linux, PIM uses the physical Wi-Fi interface in `[wifi_direct].interface`. On
macOS, the OS owns the peer-to-peer Wi-Fi control path, so `[wifi_direct].interface`
is accepted for config compatibility but ignored by the runtime backend.

Example gateway config:

```toml
[gateway]
enabled = true
nat_interface = "eth0"

[wifi_direct]
enabled = true
interface = "wlan0"
go_intent = 12
listen_channel = 6
op_channel = 6
connect_method = "pbc"
```

Operational notes:

- `gateway.nat_interface` is still the real internet uplink, often `eth0` or `wlan0`
- Linux uses `[wifi_direct].interface` as the radio for P2P group formation
- macOS advertises and discovers the gateway over Bonjour peer-to-peer Wi-Fi using the TCP `listen_port`
- macOS ignores `go_intent`, `listen_channel`, `op_channel`, and `connect_method`
- these may be the same physical uplink on some systems, but do not assume that
- Linux requires `wpa_supplicant` with P2P support already running on that interface

Useful checks:

```bash
iw dev
wpa_cli -i wlan0 p2p_find
```

If a P2P group is formed, Linux often creates an additional interface such as
`p2p-wlan0-0`. PIM discovers and uses that group interface automatically after
formation; you configure the parent Wi-Fi interface, not the transient `p2p-*`
name.

On macOS, no transient `p2p-*` interface is managed by PIM. Discovery is driven
through Bonjour peer-to-peer service advertisement and resolution instead.

## Bluetooth PAN Gateway

Use Bluetooth when peers are expected to connect over a Bluetooth PAN link and
then hand off to the normal TCP transport.

First, inspect Bluetooth controllers and PAN interfaces.

List controllers:

```bash
bluetoothctl list
```

Show controller state on Linux:

```bash
bluetoothctl show
```

List current network interfaces and look for the PAN interface:

```bash
ip -br link
```

If a PAN interface already exists, inspect it. On macOS, use `bridge0` or the host's Bluetooth PAN bridge if it differs:

```bash
ip -4 -o addr show dev <pan-iface>
ip neigh show dev <pan-iface>
```

Example gateway config:

```toml
[gateway]
enabled = true
nat_interface = "eth0"

[bluetooth]
enabled = true
interface = "auto" # use "bridge0" on macOS unless the host exposes a different PAN bridge
radio_discovery_enabled = true
device_name_prefix = "PIM-"
local_alias = "PIM-gateway-a"
connect_pan = false
serve_nap = true
nap_bridge = "br-bt"
# IPv4 address/CIDR assigned to nap_bridge when the daemon creates it.
nap_bridge_addr = "192.168.44.1/24"
# Run a daemon-supervised dnsmasq DHCP server on the bridge.
dhcp_enabled = true
# dhcp_range = "192.168.44.10,192.168.44.200" # optional; derived from nap_bridge_addr when unset.
dhcp_lease_time = "12h"
# dhcp_dns = "1.1.1.1,8.8.8.8"   # optional; inherits /etc/resolv.conf when unset.
# Gateway serves DHCP, so it doesn't need to request one itself.
request_dhcp = false
auto_discover_peers = true
poll_interval_ms = 2000
scan_interval_ms = 5000
peer_discovery_interval_ms = 2000
bluetoothctl_timeout_s = 15
discoverable_timeout_s = 180
startup_timeout_ms = 15000
```

Operational notes:

- `[bluetooth].interface` is a preferred interface hint on Linux; `"auto"` lets the daemon resolve a ready `bnep*`, `enx*`, or configured bridge interface dynamically
- `serve_nap = true` starts a daemon-managed Linux NAP server; the daemon auto-creates `nap_bridge` (e.g. `br-bt`), brings it up, and assigns `nap_bridge_addr` when missing
- when `dhcp_enabled = true`, the daemon supervises a `dnsmasq` DHCP server on the bridge so PAN clients get an address automatically; the DHCP range and DNS servers are derived from `nap_bridge_addr` and `/etc/resolv.conf` when not set explicitly
- when `gateway.enabled = true`, the daemon installs iptables MASQUERADE/FORWARD rules from the derived Bluetooth subnet to `gateway.nat_interface` so client traffic can reach the internet
- `connect_pan = true` lets a Linux gateway also act as an outbound PAN client to reach peer gateways
- on PAN clients (`serve_nap = false`, `request_dhcp = true`), the daemon runs `dhclient -d -v <interface>` automatically once the PAN interface comes up
- PIM waits for a PAN-facing interface to become ready
- Linux uses `bluetoothctl`; macOS uses the host Bluetooth stack and expects `blueutil` for radio discovery and pairing automation
- once the PAN link exists, PIM reads neighbor entries from `ip neigh show dev <interface>` on Linux or `arp -an -i <interface>` on macOS
- discovered neighbor IPs are then used as normal TCP peer targets on `transport.listen_port`

For environments where radio discovery is not enough, you can also declare
static Bluetooth peers:

```toml
[[peers]]
mechanism = "bluetooth"
ip = "192.168.44.2"
label = "bt-relay-a"
```

Useful checks:

```bash
bluetoothctl devices
ip -br link
ip neigh show dev <pan-iface>
```

## Mixed-Mechanism Gateway

A gateway can combine mechanisms. For example:

- UDP discovery on the LAN
- Wi-Fi Direct for nearby devices
- Bluetooth PAN for short-range fallback
- static TCP peers for known relays

Example:

```toml
[gateway]
enabled = true
nat_interface = "eth0"

[discovery]
enabled = true
connect_relays = true
connect_gateways = true

[wifi_direct]
enabled = true
interface = "wlan0"

[bluetooth]
enabled = true
interface = "auto"

[[peers]]
mechanism = "tcp"
address = "192.168.10.20:9100"
label = "relay-a"
```

All of these still converge on the same TCP transport, handshake, routing, and
gateway dataplane once a peer address becomes reachable.

## Minimal Gateway Bring-Up Checklist

1. Discover the internet-facing interface with `ip route get 1.1.1.1`.
2. Confirm that interface has IPv4 connectivity with `ip -4 -o addr show dev <iface>`.
3. If using Wi-Fi Direct, identify the Wi-Fi radio with `iw dev`.
4. If using Bluetooth PAN, identify the active PAN interface with `ip -br link` and inspect it with `ip neigh show dev <pan-iface>`.
5. Set `gateway.enabled = true` and `gateway.nat_interface = "<iface>"`.
6. Enable only the peer mechanisms you actually intend to use.
7. Start the daemon with `sudo pim up --config /etc/pim/pim.toml`.
8. Verify runtime health with `sudo pim status --verbose`.

## Troubleshooting

If the gateway fails to start:

- verify `gateway.nat_interface` exists: `ip link show <iface>`
- verify it has IPv4: `ip -4 -o addr show dev <iface>`
- verify the route to the internet still uses that interface: `ip route get 1.1.1.1`

If Wi-Fi Direct does not discover peers:

- verify the Wi-Fi interface name with `iw dev`
- verify `wpa_supplicant` is managing that interface
- check whether a `p2p-*` group interface appeared after negotiation

If Bluetooth peers do not appear:

- verify Bluetooth is powered: `bluetoothctl show`
- verify the PAN interface exists: `ip -br link`
- verify neighbor discovery sees peers on the active PAN interface: `ip neigh show dev <pan-iface>`

## Related Docs

- [configuration.md](configuration.md)
- [usage.md](usage.md)
- [../architecture/transports/wifi-direct.md](../architecture/transports/wifi-direct.md)
- [../architecture/transports/bluetooth.md](../architecture/transports/bluetooth.md)
