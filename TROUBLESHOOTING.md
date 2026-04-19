# Troubleshooting

This page is the top-level index for operator troubleshooting notes, cleanup commands, and recovery procedures.

## Scope

Use this document to collect:

- runtime verification commands
- network and interface inspection commands
- safe shutdown and cleanup procedures
- feature-specific recovery notes for Bluetooth, Wi-Fi Direct, DHCP, routing, and gateway NAT

## Current Entries

- [Bluetooth gateway shutdown and cleanup](docs/operations/bluetooth-gateway-shutdown.md): how to stop a Bluetooth gateway cleanly, when to escalate to `SIGKILL`, and which bridge, route, and `iptables` cleanup commands are safe.
- [Bluetooth NAP bridge address already assigned](docs/troubleshooting/bluetooth-nap-bridge-address-already-assigned.md): what to do when `pim-daemon` cannot assign `nap_bridge_addr` to `br-bt` because the bridge already exists or is already configured.
- [Bluetooth DHCP client unavailable](docs/troubleshooting/bluetooth-dhcp-client-unavailable.md): repeated `WARN pim_bluetooth: Bluetooth DHCP client unavailable` on a PAN client — `dhclient` is not installed, or `request_dhcp` should be disabled.

## Common First Step: stop the daemon cleanly

For Bluetooth bridge, NAT, or stale-interface recovery, stop `pim-daemon`
with `SIGTERM` first and give it time to run its shutdown cleanup:

```bash
sudo pkill -TERM -x pim-daemon
```

Wait for the daemon to exit before deleting bridges, changing routes, or
restarting with the same config. A simple check:

```bash
pgrep -ax pim-daemon || echo "pim-daemon has exited"
```

If the command prints only `pim-daemon has exited`, the daemon is gone.
If it prints one or more matching processes, wait a few more seconds and
re-check before moving on to manual cleanup.

## Suggested Future Sections

- Bluetooth PAN discovery and pairing failures
- DHCP lease acquisition and `dnsmasq` validation
- Wi-Fi Direct startup and `wpa_cli` checks
- TUN interface and route-installation failures
- Gateway NAT and internet-forwarding checks
- Docker lab debugging and container network inspection

## Quick Command Areas To Grow

When adding entries here or under `docs/operations/`, prefer commands that quickly answer one of these questions:

- is the daemon still running
- is the TUN interface present and up
- did the host route table change as expected
- did the optional feature create the expected interface, bridge, or lease
- did firewall or NAT rules get installed and removed cleanly
