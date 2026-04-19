# Bluetooth DHCP client unavailable

## Symptom

After a Bluetooth PAN client connects to a NAP gateway, the daemon logs one or
more warnings like:

```
WARN pim_bluetooth: Bluetooth DHCP client unavailable; continuing without DHCP
  (peer discovery still works via IPv6 link-local)
  interface=enx… dhclient=dhclient err=io error: No such file or directory (os error 2)
```

## Why it happens

When `connect_pan = true` and `serve_nap = false`, the client attempts to run
`dhclient` on the resolved PAN interface to obtain an IPv4 address from the
gateway's dnsmasq DHCP server.  `dhclient` defaults to the bare command name
`dhclient`, which is provided by the `isc-dhcp-client` package.  On systems
where that package is not installed the daemon logs this warning and continues
without IPv4 — peer discovery falls back to IPv6 link-local addresses.

`request_dhcp` defaults to `true`, so the warning appears even if the option is
absent from the config file.

## Fix A — install `dhclient` (recommended when IPv4 is needed)

```bash
# Debian / Ubuntu
sudo apt install isc-dhcp-client

# Fedora / RHEL
sudo dnf install dhcp-client
```

After installation, restart `pim-daemon`; it will acquire an IPv4 lease from
the gateway's DHCP pool (`192.168.44.x` with the default gateway config).

## Fix B — disable DHCP on the client (when IPv6 link-local is sufficient)

Add the following line to the `[bluetooth]` section of the **client** config:

```toml
request_dhcp = false
```

Peer discovery continues to work via IPv6 link-local.  Choose this option only
if you do not need the client to reach IPv4 services through the gateway.

## Verify

After applying either fix, restarting the daemon should produce no further
`Bluetooth DHCP client unavailable` warnings.  To confirm the client received
a lease (Fix A):

```bash
ip -br addr show dev <pan-interface>
# e.g. ip -br addr show dev enx6432a8144f4b
```

The interface should show a `192.168.44.x/24` address alongside any IPv6
link-local address.
