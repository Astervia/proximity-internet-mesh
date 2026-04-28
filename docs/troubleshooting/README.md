# Troubleshooting

This folder collects operator recovery notes, cleanup commands, and verification
procedures for the PIM daemon. Each page targets one symptom; this index lists
them and points at related operational procedures.

## Scope

Use this section to find:

- runtime verification commands
- network and interface inspection commands
- safe shutdown and cleanup procedures
- feature-specific recovery notes for Bluetooth, Wi-Fi Direct, DHCP, routing, and gateway NAT

## First Step: Stop The Daemon Cleanly

For Bluetooth bridge, NAT, or stale-interface recovery, stop `pim-daemon`
with `SIGTERM` first and let it run its shutdown cleanup:

```bash
sudo pkill -TERM -x pim-daemon
```

Wait for the daemon to exit before deleting bridges, changing routes, or
restarting with the same config:

```bash
pgrep -ax pim-daemon || echo "pim-daemon has exited"
```

If the command prints only `pim-daemon has exited`, the daemon is gone. If it
still prints matching processes, wait a few more seconds before manual cleanup.

## Entries

### Bluetooth

- [bluetooth-nap-bridge.md](bluetooth-nap-bridge.md) — `pim-daemon` cannot assign `nap_bridge_addr` to `br-bt` because the bridge already exists or is already configured.
- [bluetooth-dhcp-client.md](bluetooth-dhcp-client.md) — repeated `WARN pim_bluetooth: Bluetooth DHCP client unavailable` on a PAN client; `dhclient` is not installed or `request_dhcp` should be disabled.
- [../operations/bluetooth-gateway-shutdown.md](../operations/bluetooth-gateway-shutdown.md) — how to stop a Bluetooth gateway cleanly, when to escalate to `SIGKILL`, and which bridge / route / `iptables` cleanup commands are safe.

## Suggested Future Sections

- Bluetooth PAN discovery and pairing failures
- DHCP lease acquisition and `dnsmasq` validation
- Wi-Fi Direct startup and `wpa_cli` checks
- TUN interface and route-installation failures
- Gateway NAT and internet-forwarding checks
- Docker lab debugging and container network inspection

## Quick Diagnostic Questions

When adding entries here or under `../operations/`, prefer commands that quickly
answer one of these questions:

- is the daemon still running
- is the TUN interface present and up
- did the host route table change as expected
- did the optional feature create the expected interface, bridge, or lease
- did firewall or NAT rules get installed and removed cleanly
