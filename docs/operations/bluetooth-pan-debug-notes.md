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
nap_bridge_addr = "192.168.44.1/24"
dhcp_enabled = true
```

Expected behavior (as of the resilient bring-up work):

- on startup, the daemon **auto-creates** `nap_bridge` with `ip link add … type bridge` if missing, brings it up, and assigns `nap_bridge_addr`; a missing bridge is no longer silently tolerated
- starts `bt-network -s nap <bridge>` with `kill_on_drop(true)` so the child is reaped on daemon exit
- keeps the server process alive under daemon lifecycle management and restarts it if it exits
- when `gateway.enabled = true`, installs iptables MASQUERADE/FORWARD rules from the Bluetooth subnet (derived from `nap_bridge_addr`) to `gateway.nat_interface`
- when `dhcp_enabled = true`, supervises a `dnsmasq` instance bound to the bridge so PAN clients get an IP, a default route, and DNS automatically; `dhcp_range` defaults to a safe pool derived from `nap_bridge_addr`, and `dhcp_dns` falls back to the nameservers listed in `/etc/resolv.conf`
- on PAN clients (`serve_nap = false`, `request_dhcp = true`), the daemon auto-runs `dhclient -d -v <interface>` once the PAN interface appears and restarts it if it dies
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

## Implementation Plan

### Phase 1. Remove the fixed-interface assumption

Goal:
make Linux Bluetooth PAN work when the active interface is created dynamically
as `bnep*` or `enx*` instead of matching the configured `bluetooth.interface`
exactly.

Primary code areas:

- `crates/pim-bluetooth/src/lib.rs`
- `crates/pim-core/src/config.rs`
- `crates/pim-daemon/src/main.rs`

Work items:

1. Introduce runtime PAN interface resolution in `pim-bluetooth`.
2. Replace direct reads of `self.config.interface` in readiness and neighbor
   discovery paths with a resolver that:
   - prefers the configured interface if it exists and is ready
   - otherwise scans `/sys/class/net`
   - prefers `bnep*` first
   - falls back to `enx*` when the interface is up and appears only after PAN
     setup
3. Re-resolve on every readiness/discovery cycle instead of assuming the same
   interface persists for the lifetime of the daemon.
4. Add logs for:
   - configured interface
   - resolved runtime interface
   - candidate interfaces considered
   - reason no interface qualified
5. Keep the existing `interface` field for now as a hint/default rather than as
   a hard requirement so current configs remain backward-compatible.

Acceptance criteria:

- a Linux client can connect successfully when the PAN interface appears as
  `enx*`
- `pim-bluetooth` can recover after the PAN interface disappears and later
  reappears with the same or different name
- logs show which interface was selected

Testing:

- add unit tests around interface selection and operstate filtering
- extend seam tests to simulate:
  - configured interface exists
  - configured interface missing but `bnep0` appears
  - configured interface missing but `enx*` appears
  - interface disappearance and re-resolution

### Phase 2. Add explicit Linux PAN server support

Goal:
let a node provide NAP service under daemon control instead of requiring an
external `bt-network -s nap` process.

Primary code areas:

- `crates/pim-core/src/config.rs`
- `crates/pim-bluetooth/src/lib.rs`
- `crates/pim-daemon/src/main.rs`
- `scripts/generate_client_full_config.sh`
- `scripts/generate_gateway_full_config.sh`

Suggested config additions:

```toml
[bluetooth]
enabled = true
serve_nap = true
nap_bridge = "br-bt"
connect_pan = true
```

Work items:

1. Extend `BluetoothConfig` with Linux-oriented role controls:
   - `serve_nap`
   - `nap_bridge`
   - `connect_pan`
2. In `pim-daemon`, start a managed NAP subprocess when `serve_nap = true`.
3. Ensure lifecycle handling covers startup, shutdown, and restart if the helper
   exits unexpectedly.
4. Validate the configured bridge early and fail with a clear log if the bridge
   is missing or unusable.
5. Update config generators so:
   - gateway-oriented templates can enable NAP serving
   - client-oriented templates can disable it by default
   - generated comments explain current Linux limitations clearly

Acceptance criteria:

- the gateway daemon can provide NAP without any separate operator-run helper
- the daemon does not attempt outbound `bt-network -c ... nap` when configured
  as server-only
- gateway and client generated configs no longer imply that both Linux nodes
  should behave as PAN clients

Testing:

- config round-trip tests for new fields
- daemon tests that verify the managed helper is started only when enabled
- generator tests updated to assert the new fields/comments

### Phase 3. Split Bluetooth runtime roles cleanly

Goal:
separate responsibilities that are currently folded into one watcher so server,
client, and interface-observation behavior can evolve independently.

Primary code areas:

- `crates/pim-bluetooth/src/lib.rs`
- `crates/pim-daemon/src/main.rs`
- `docs/architecture/bluetooth.md`

Work items:

1. Break the current watcher logic into distinct Linux role components:
   - radio discovery / pairing
   - PAN client connect requests
   - NAP server process management
   - PAN interface resolution and neighbor observation
2. Keep a thin orchestration layer in `BluetoothDiscovery::run` or replace it
   with a clearer controller object.
3. Make outbound connect attempts conditional on role config instead of being
   implied whenever radio discovery is on.
4. Preserve the existing downstream contract: emit `SocketAddr`s into the normal
   TCP transport path.

Acceptance criteria:

- server-only, client-only, and dual-role behavior are explicit in code and logs
- hosts serving NAP no longer accidentally perform the wrong outbound role
- future Bluetooth fixes do not require editing one monolithic loop

Testing:

- focused tests per role component
- integration test covering a dual-role configuration without duplicate or
  conflicting connect attempts

### Phase 4. Update operator-facing defaults and docs

Goal:
remove misleading defaults and document the actual Linux Bluetooth PAN model.

Primary code areas:

- `scripts/generate_client_full_config.sh`
- `scripts/generate_gateway_full_config.sh`
- `scripts/test-config-generators.sh`
- `docs/architecture/bluetooth.md`
- `docs/getting-started/client-usage.md`
- `docs/getting-started/gateway-usage.md`

Work items:

1. Stop presenting `bnep0` as a universally reliable Linux default.
2. Document that `bluetooth.interface` is a preferred hint and that runtime
   selection may choose another PAN-facing interface.
3. Document role-based examples:
   - client-only
   - gateway serving NAP
   - dual-role experimental mode
4. Update operator guidance to explain what log lines confirm:
   - NAP server started
   - PAN client connected
   - runtime interface resolved
   - neighbor discovery succeeded or returned zero peers

Acceptance criteria:

- generated configs and docs no longer instruct operators to rely on `bnep0`
- gateway docs describe managed NAP mode instead of requiring a manual external
  helper

### Phase 5. Add host-level validation

Goal:
prove the Linux Bluetooth PAN path on real hardware, not only through seam
tests.

Work items:

1. Create a repeatable manual validation checklist for two Linux hosts.
2. Record exact commands, expected logs, and expected peer states.
3. Validate these scenarios:
   - gateway serves NAP, client connects
   - dynamic interface arrives as `bnep*`
   - dynamic interface arrives as `enx*`
   - PAN interface disappears and later reconnects
   - dual-role hosts converge without manual role changes
4. Capture failure signatures and corresponding remediation guidance for
   operators.

Acceptance criteria:

- the project has one documented real-host validation path that can be run
  before release
- Bluetooth PAN regressions can be checked against concrete expected behavior

## Recommended Delivery Order

1. Phase 1 first, because dynamic interface resolution fixes the clearest
   current runtime failure and is mostly localized to `pim-bluetooth`.
2. Phase 2 next, because Linux nodes still need a daemon-managed NAP role to
   avoid the current manual external helper workaround.
3. Phase 3 after that, once the runtime has the needed behavior and can be
   cleanly refactored around explicit roles.
4. Phase 4 alongside or immediately after Phase 2 so generated configs stop
   teaching the broken path.
5. Phase 5 before calling the Linux Bluetooth PAN path production-ready.

## Operator Guidance Until Code Changes Land

For the current codebase:

- do not assume two generated Linux Bluetooth configs will auto-form a working
  PIM link
- provide a NAP service on one host manually if using Linux Bluetooth PAN
- disable the PIM Bluetooth watcher on the host serving NAP externally
- on the PAN client host, inspect the real interface name after connect
- if the active interface is not `bnep0`, update config to the actual interface
  name before retrying
