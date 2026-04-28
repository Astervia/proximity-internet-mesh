# Wi-Fi Direct Transport

PIM uses Wi-Fi Direct as an optional **peer-finding** layer. The transport and
session logic remain the normal TCP path after discovery succeeds.

Platform backends:

- Linux uses `wpa_supplicant` P2P control via `wpa_cli`, forms a P2P group, and
  resolves the resulting peer IP from the group interface.
- macOS uses Bonjour DNS-SD on Apple's peer-to-peer Wi-Fi interface to advertise
  and discover the daemon's TCP listen port directly.

## Overview

```
     Device A                              Device B
     (any role)                            (any role)
          │                                    │
          │◀─── wpa_supplicant P2P scan ───────┤
          │     (p2p_find / p2p_peers)         │
          │                                    │
          │─── p2p_connect pbc ───────────────▶│
          │◀── GO/GC negotiation ──────────────┤
          │                                    │
          │  p2p-wlan0-0 (GO: 192.168.49.1)   │
          │  p2p-wlan0-0 (GC: DHCP-assigned)  │
          │                                    │
          │◀─── TCP connect :9100 ─────────────┤
          │◀─── Ed25519 handshake ─────────────┤
          │◀─── AES-256-GCM session ───────────┤
```

After discovery, the path is identical to a standard LAN connection.

## Linux Prerequisites

- `wpa_supplicant` compiled with `CONFIG_P2P=y` and running on the Wi-Fi interface.
- The user must have permission to run `wpa_cli` (typically the `netdev` group or
  root, depending on `wpa_supplicant` socket permissions).
- `ip` (iproute2) must be available for interface address resolution.

Check at startup:

```
wpa_cli -i wlan0 p2p_find   → should print "OK"
```

If this fails, PIM logs a warning and skips Wi-Fi Direct entirely.

## macOS Prerequisites

- no `wpa_cli` or `wpa_supplicant` setup is required
- the host must permit Bonjour peer-to-peer Wi-Fi service advertisement and browsing
- the daemon's TCP `listen_port` must be reachable once a peer is discovered

## Configuration

```toml
[wifi_direct]
enabled         = true    # false (default) → service not started
interface       = "wlan0" # physical Wi-Fi interface
go_intent       = 7       # 0–15; higher = more likely to become Group Owner
listen_channel  = 6       # P2P listen channel
op_channel      = 6       # P2P operating channel
connect_method  = "pbc"   # "pbc" (push-button) or "pin:<8-digit>"
```

The `listen_port` used for the TCP connection is taken from `[transport] listen_port`
(default `9100`), not from this section.

On macOS, only `enabled` is acted on directly. The Linux-specific tuning fields
remain in the shared config schema for compatibility but are ignored by the
Bonjour backend.

## Linux Group Roles and IP Addresses

wpa_supplicant negotiates which device becomes the **Group Owner (GO)** and which
becomes the **Group Client (GC)** using the `go_intent` value. The GO acts as a
soft AP and runs a built-in DHCP server:

| Role | Interface IP           | Peer IP                          |
| ---- | ---------------------- | -------------------------------- |
| GO   | `192.168.49.1` (fixed) | GC's DHCP-assigned address (ARP) |
| GC   | DHCP-assigned          | `192.168.49.1` (GO constant)     |

PIM detects its role by comparing its own interface IP to `192.168.49.1`.

macOS does not expose that Linux P2P group model. The backend resolves peer
socket addresses directly from Bonjour service discovery, so there is no GO/GC
role handling in the daemon on macOS.

## WifiDirectDiscovery Service

`WifiDirectDiscovery` runs as a background Tokio task started by the daemon when
`wifi_direct.enabled = true`.

Linux flow:

```
                 WifiDirectDiscovery (background task)
                 ─────────────────────────────────────
                 ┌─────────────────────────────────┐
poll (2 s)  ──▶ │  p2p_peers()                    │
                │  → for each new MAC:             │
                │      p2p_connect_pbc(mac)        │
                │      wait_for_group_iface (15 s) │
                │      WifiDirectGroup::from_iface │
                │      → peer_tx.send(SocketAddr)  │
                └─────────────────────────────────┘
                          │
                          ▼
                 run_wifidirect_consumer
                 ─────────────────────────────────────
                 ┌─────────────────────────────────┐
                 │  reconnect.register_discovered   │
                 │  initiate_peer_connection        │
                 │    └─▶ TCP handshake (same as   │
                 │        static / UDP peers)       │
                 └─────────────────────────────────┘
```

### Seen-MAC Deduplication

Once a MAC address triggers a connection attempt, it is added to `seen_macs`.
Subsequent `p2p_peers` polls that return the same MAC are ignored for the lifetime
of the service. If the peer goes away and re-appears, the existing group interface
will have been removed by wpa_supplicant, and the daemon's heartbeat timeout
(15 s) will have cleared the session — at which point a fresh connection can be
established if the user extends `seen_macs` expiry logic.

### Group Formation Timeout

After issuing `p2p_connect`, `WifiDirectDiscovery` polls `list_interfaces` every
500 ms for up to 15 s, looking for a new `p2p-*` interface. If none appears, the
attempt is logged as an error and the MAC is not re-tried in the current run (it
stays in `seen_macs`).

macOS flow:

```
                 WifiDirectDiscovery (background task)
                 ─────────────────────────────────────
                 ┌─────────────────────────────────┐
startup      ──▶ │  register _pimmesh._tcp        │
browse loop  ──▶ │  browse peer-to-peer services  │
                │  → for each new service:         │
                │      resolve host + port         │
                │      → peer_tx.send(SocketAddr)  │
                └─────────────────────────────────┘
                          │
                          ▼
                 run_wifidirect_consumer
```

macOS deduplicates by Bonjour service identity (`name@domain`) instead of peer MAC.

## Coexistence with LAN Discovery

Both mechanisms run in parallel:

```
UDP broadcast discovery ──▶ run_discovery_consumer ──▶ initiate_peer_connection
Wi-Fi Direct discovery  ──▶ run_wifidirect_consumer ──▶ initiate_peer_connection
Static [[peers]]        ──▶ initiate_peer_connection (at startup)
```

A peer found on the LAN and simultaneously via Wi-Fi Direct will produce two
`initiate_peer_connection` calls. The sessions map (`DaemonState::sessions`)
serializes insertion, so at most one session survives.

## Manual Hardware Test Procedure

### Setup

```toml
# gateway-wfd.toml
[node]
name = "gateway"
[gateway]
enabled = true
[wifi_direct]
enabled = true
go_intent = 15   # prefer GO role
```

```toml
# client-wfd.toml
[node]
name = "client"
[wifi_direct]
enabled = true
go_intent = 0    # prefer GC role
```

### Steps

```bash
# Device A — gateway
pim --config gateway-wfd.toml

# Device B — client
pim --config client-wfd.toml
```

### Expected Log Sequence

```
# Both devices (within ~10 s):
INFO  Wi-Fi Direct discovery starting on interface wlan0
INFO  Wi-Fi Direct: new peer discovered: aa:bb:cc:dd:ee:ff
INFO  Wi-Fi Direct: P2P group interface appeared: p2p-wlan0-0
INFO  Wi-Fi Direct: group formed (role=Go), peer addr=192.168.49.100:9100
INFO  Wi-Fi Direct: new peer addr — initiating connection
INFO  handshake complete, peer_id=…

# Client — after IpAssign:
INFO  mesh IP assigned: 10.X.X.Y/24
```

### Verification

```bash
# On client device:
ip addr show pim0          # should show 10.X.X.Y/24
ping 10.0.0.1              # reach gateway mesh IP
curl http://example.com    # internet access via gateway NAT
```

## Limitations

### No Per-Group Interface Cleanup

When the daemon exits, P2P group interfaces are not explicitly removed.
`wpa_supplicant` should clean them up on process restart, but a crashed daemon
may leave a stale `p2p-*` interface. Run `wpa_cli -i wlan0 p2p_group_remove p2p-wlan0-0`
to clean up manually.

This Linux-only limitation does not apply to the macOS Bonjour backend.

### Seen-MAC Expiry Not Implemented

`seen_macs` is never cleared. If a peer's MAC rotates (MAC randomization) it will
be treated as a new device on the next poll, but the old MAC will remain in the
set indefinitely.

### Single Group at a Time

The current implementation tracks only one group formation attempt at a time
(sequentially within `connect_and_emit`). Multiple simultaneous P2P connections
are possible in wpa_supplicant but not yet exploited here.

## Related Documents

- [discovery.md](discovery.md) — UDP broadcast discovery (LAN-based alternative)
- [security.md](security.md) — handshake and session establishment after group formation
- [overview.md](overview.md) — component architecture
