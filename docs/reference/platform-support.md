# Platform Support

This page is the canonical platform-support reference for the workspace. The root
[README.md](../../README.md) carries a condensed view; this file carries the
full matrix, per-feature host requirements, and OS-specific guidance.

## Matrix

| Capability                    | Linux     | macOS         |
| ----------------------------- | --------- | ------------- |
| Client runtime                | Supported | Supported     |
| Relay runtime                 | Supported | Supported     |
| Gateway runtime and NAT       | Supported | Supported     |
| Wi-Fi Direct backend          | Supported | Supported     |
| Bluetooth PAN backend         | Supported | Supported     |
| `pim config generate client`  | Supported | Supported     |
| `pim config generate relay`   | Supported | Supported     |
| `pim config generate gateway` | Supported | Supported     |
| Docker integration labs       | Supported | Not supported |

## Per-OS Notes

### Linux

- TUN device via `/dev/net/tun` plus `iproute2`.
- Gateway NAT uses `iptables` and IPv4 forwarding.
- Bluetooth PAN uses BlueZ (`bluetoothctl`, `bt-network`).
- Bluetooth NAP serving uses `iproute2` to create and manage the NAP bridge (e.g., `br-bt`) and `iptables` for MASQUERADE and FORWARD rules from the Bluetooth subnet.
- DHCP on a Bluetooth NAP bridge requires `dnsmasq` when `[bluetooth].dhcp_enabled = true`.
- DHCP client on a Bluetooth PAN link requires `dhclient` when `[bluetooth].request_dhcp = true`.
- Wi-Fi Direct uses `wpa_supplicant` compiled with `CONFIG_P2P=y`, `wpa_cli`, and permission to talk to the `wpa_supplicant` control socket; `iproute2` is also required so the daemon can inspect the resulting P2P group interface.
- Docker integration labs require a Linux host with Docker Engine and Docker Compose v2, plus outbound internet access from the host so gateway containers can NAT traffic.

### macOS

- TUN device via `utunN` (e.g., `utun0`); privileges to create and manage the interface are required.
- Gateway NAT uses `pfctl` and `net.inet.ip.forwarding`; set `gateway.nat_interface` to the internet-facing host interface (e.g., `en0`).
- Bluetooth PAN uses `blueutil` for radio discovery and host Bluetooth PAN support.
- Wi-Fi Direct uses Bonjour peer-to-peer discovery instead of `wpa_supplicant`; no `wpa_cli` setup is required, and the host must allow Bonjour peer-to-peer Wi-Fi advertisement and browsing. Linux-specific tuning fields in `[wifi_direct]` are ignored on macOS.
- The generated gateway config template uses `utun0` for the mesh TUN and `en0` as the default NAT uplink.
- Docker integration labs are not supported.

## See Also

- [../getting-started/installation.md](../getting-started/installation.md) — install commands per OS.
- [../architecture/transports/wifi-direct.md](../architecture/transports/wifi-direct.md) — Wi-Fi Direct backend details.
- [../architecture/transports/bluetooth.md](../architecture/transports/bluetooth.md) — Bluetooth PAN backend details.
