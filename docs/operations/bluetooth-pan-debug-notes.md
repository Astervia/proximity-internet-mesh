# Bluetooth PAN Debug Notes

This note captures what we learned while debugging a two-node Bluetooth-only
setup:

- PC A: gateway node
- PC B: client node
- Linux on both hosts
- PIM Bluetooth enabled
- no LAN discovery, no Wi-Fi Direct, no static TCP peers

## Summary

The current Bluetooth implementation in PIM is a PAN-assisted discovery path,
not a native Bluetooth transport. On Linux it does the following:

1. discovers nearby Bluetooth devices by alias
2. attempts `bt-network -c <mac> nap`
3. waits for a local PAN-facing interface to become ready
4. reads neighbor IPs from that interface
5. hands those IPs into the normal TCP transport

That means a successful PIM Bluetooth session still depends on a working
Bluetooth PAN/NAP link and a usable local interface with neighbor visibility.

## What We Observed

### 1. Linux-to-Linux auto-connect does not work with two stock generated configs

With both hosts running the generated Bluetooth configs, both daemons behaved as
PAN clients. Each side was prepared to call `bt-network -c <peer> nap`, but the
repo does not currently start a PAN/NAP server role on Linux.

Observed failure on the gateway:

```text
Bluetooth radio discovery failed: bt-network failed: Network service is not supported by this device
Bluetooth PAN interface did not become ready before timeout
```

Implication:

- the peer was visible over Bluetooth radio discovery
- pairing/discovery alone was not enough
- no PAN/NAP service was available on the remote host
- no PAN interface was created locally
- therefore no peer IP was discovered

### 2. External NAP service on the gateway is enough, but the gateway daemon must not also act as a PAN client

After starting an external NAP server on the gateway with `bt-network -s nap`,
the gateway daemon still failed if its PIM Bluetooth watcher remained enabled.
The daemon continued trying outbound `bt-network -c <peer> nap`, which is the
wrong role for that host in this setup.

Implication:

- when one host provides the NAP service externally, the PIM Bluetooth watcher
  on that host should be disabled or switched into a server-aware mode
- the gateway can still accept the normal PIM TCP session without running the
  Bluetooth watcher locally

### 3. The PAN client interface name was not `bnep0`

On the client, the generated config expected:

```toml
[bluetooth]
interface = "bnep0"
```

But after a manual PAN connect, Linux did not expose `bnep0`. Instead, the
connection showed up on a dynamically named interface:

```text
enx6432a8144f4b
```

That interface name matched the remote device MAC and persisted only while the
PAN link existed.

Implication:

- hardcoding `bnep0` is not reliable on Linux
- the current config model assumes a stable interface name that may not exist
- the runtime needs dynamic PAN interface resolution

### 4. PAN-facing interfaces can disappear when the connection is torn down

When the PAN connection was removed, the client-side interface disappeared. That
means the interface name cannot be treated as permanent state.

Implication:

- the daemon cannot assume a configured PAN interface will always exist
- interface lookup must happen dynamically during connect and reconnect

### 5. "Bluetooth connected" at the OS level is not enough for PIM

We observed cases where the peer was visible in `bluetoothctl devices` and
`bt-network` reported the service as connected, but PIM still had no peers.

What was still missing in those states:

- no stable PAN interface name in config
- or no interface that PIM was watching
- or no neighbor entry visible on the interface PIM used

Implication:

- PIM peer formation depends on the full chain:
  radio discovery -> PAN link -> local interface -> neighbor IP -> TCP session
- any missing step leaves `pim debug peers` empty

## Key Conclusions

### 1. A peer may need to act as both PAN client and PAN server

In practice, a Linux host can be capable of both roles, and the runtime should
resolve that automatically. The current implementation assumes a PAN client
behavior only.

Desired behavior:

- a host should be able to expose NAP and also initiate outbound PAN connects
- if both peers support both roles, they should converge on a single working
  PAN link automatically instead of requiring manual role selection

### 2. The PAN client interface name must not be hardcoded

This was the clearest implementation gap from the session.

Desired behavior:

- the daemon should discover the active PAN interface dynamically
- it should not require `bnep0` specifically
- it should handle dynamically named Linux interfaces such as `enx*`

## Proposed Changes

### 1. Add an explicit NAP server mode to the Bluetooth config

Extend `[bluetooth]` with a Linux-only server option, for example:

```toml
[bluetooth]
enabled = true
serve_nap = true
nap_bridge = "br-bt"
```

Expected behavior:

- on startup, the daemon ensures the bridge exists or validates it
- starts `bt-network -s nap <bridge>`
- keeps the server process alive under daemon lifecycle management
- avoids the need for manual external PAN setup on gateway hosts

### 2. Resolve the PAN interface dynamically

Replace the current fixed-interface assumption with runtime resolution.

Suggested Linux strategy:

1. if the configured interface exists and is ready, use it
2. otherwise scan live interfaces and prefer `bnep*`
3. if no `bnep*` exists, accept a PAN-like `enx*` interface associated with the
   connected Bluetooth peer
4. once selected, use that interface for operstate checks and neighbor lookup
5. re-resolve on disconnect instead of caching permanently

This should apply both to readiness checks and neighbor discovery.

### 3. Separate Bluetooth roles inside the runtime

The runtime should distinguish:

- radio discovery
- PAN server role
- PAN client role
- PAN interface observation

Right now these are folded into one watcher. Splitting them would make the
behavior clearer and avoid cases where a host providing NAP also tries to
connect outward unnecessarily.

### 4. Improve logs around PAN role and interface selection

Current logs are useful but not sufficient when interface resolution fails.

Additional logs should include:

- whether this host is acting as PAN server, client, or both
- which interface was selected dynamically
- which interfaces were considered and rejected
- whether neighbor discovery returned zero entries on an otherwise-ready PAN link

### 5. Add a real host-level Bluetooth PAN test plan

The current seam tests validate orchestration, but not real Linux PAN behavior.
We should add manual or hardware-backed validation steps for:

- Linux gateway serving NAP
- Linux client connecting to NAP
- dynamic interface naming (`bnep*` vs `enx*`)
- reconnect after PAN interface disappearance
- two hosts both capable of PAN client/server roles

## Recommended Near-Term Implementation Order

1. Add dynamic PAN interface resolution on Linux.
2. Add daemon-managed NAP server mode for gateway nodes.
3. Refactor Bluetooth runtime into explicit client/server/interface-monitor roles.
4. Extend docs and test coverage around real host behavior.

## Operator Guidance Until Code Changes Land

For the current codebase:

- do not assume two generated Linux Bluetooth configs will auto-form a working
  PIM link
- provide a NAP service on one host manually if using Linux Bluetooth PAN
- disable the PIM Bluetooth watcher on the host serving NAP externally
- on the PAN client host, inspect the real interface name after connect
- if the active interface is not `bnep0`, update config to the actual interface
  name before retrying
