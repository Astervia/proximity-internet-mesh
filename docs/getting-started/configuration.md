# Configuration

PIM uses a single TOML file, defaulting to `/etc/pim/pim.toml`. The shared config model lives in `pim-core` and is used by both the CLI and the daemon.

The fastest way to create a starter file is with the CLI:

```bash
pim config generate client --output /etc/pim/pim.toml
```

## Top-Level Sections

```toml
[node]
[interface]
[discovery]
[transport]
[routing]
[gateway]
[security]
[[peers]]
```

## Field Reference

### `[node]`

- `name`: human-readable node name
- `data_dir`: directory for persistent local state such as keys and runtime data

### `[interface]`

- `name`: TUN interface name, default `pim0`
- `mtu`: interface MTU, default `1400`
- `mesh_ip`: mesh CIDR or `auto`

Notes:

- Static lab setups in this repository use explicit CIDRs.
- The daemon also contains `IpRequest` and `IpAssign` support for dynamic assignment when `mesh_ip = "auto"`, but that depends on reachable gateway behavior at runtime rather than a purely local startup path.

### `[discovery]`

- `broadcast_interval_ms`: how often discovery advertisements are sent
- `peer_timeout_ms`: how long a peer may stay unseen before timing out

### `[transport]`

- `type`: backend name, currently `tcp`
- `listen_port`: local inbound port, default `9100`

### `[routing]`

- `max_hops`: maximum accepted route length
- `algorithm`: routing algorithm name, currently distance-vector oriented
- `route_expiry_s`: time before learned routes expire

### `[gateway]`

- `enabled`: whether the node acts as an internet gateway
- `nat_interface`: internet-facing interface used for NAT
- `max_connections`: maximum tracked gateway connections

### `[security]`

- `key_file`: path to the Ed25519 private key file
- `require_encryption`: whether unencrypted sessions are rejected

### `[[peers]]`

- `address`: peer transport address such as `relay:9100` or `192.168.1.20:9100`
- `label`: optional operator-facing label

## Minimal Client Example

```toml
[node]
name = "client-a"
data_dir = "/var/lib/pim"

[interface]
name = "pim0"
mesh_ip = "10.77.0.100/24"
mtu = 1400

[transport]
type = "tcp"
listen_port = 9100

[security]
key_file = "/var/lib/pim/node.key"
require_encryption = true

[[peers]]
mechanism = "tcp"
address = "gateway:9100"
label = "gateway"
```

## Minimal Gateway Example

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
nat_interface = "eth0"
max_connections = 200

[security]
key_file = "/var/lib/pim/node.key"
require_encryption = true
```

## Minimal Relay Example

```toml
[node]
name = "relay-b"
data_dir = "/var/lib/pim"

[interface]
name = "pim0"
mesh_ip = "10.77.0.10/24"
mtu = 1400

[transport]
type = "tcp"
listen_port = 9100

[security]
key_file = "/var/lib/pim/node.key"
require_encryption = true

[[peers]]
mechanism = "tcp"
address = "gateway:9100"
label = "gateway"
```

## Operational Guidance

- Use static mesh IPs for deterministic labs and Docker tests.
- Put node keys and writable runtime state under `/var/lib/pim` for service-style deployments.
- Gateways need `gateway.enabled = true` and a valid external `nat_interface`.
- Static peers are still the most explicit and easiest way to bring up predictable topologies.
