# Skill: Add Connection Mechanism

Use this skill when adding a new peer connection mechanism such as Bluetooth P2P.

## Purpose

Extend how peers discover or establish neighbor links without destabilizing the
existing transport, handshake, routing, and gateway layers.

In this repository, Wi-Fi Direct is the reference pattern: it adds a new way to
find or form a direct link, then hands a reachable peer endpoint to the daemon.

## Default Assumption

For Bluetooth P2P, assume the preferred design is:

- Bluetooth creates or exposes a direct neighbor link
- that link yields an IP-reachable endpoint or a narrow adapter into one
- the daemon still uses the normal connection path after link establishment

Do not start by replacing `TcpTransport` unless the code or platform constraints
force that conclusion.

## Inputs To Gather

Before editing, inspect:

- `crates/pim-core/src/config.rs`
- `crates/pim-daemon/src/main.rs`
- `crates/pim-transport/src/lib.rs`
- `crates/pim-transport/src/tcp.rs`
- `crates/pim-wifidirect/src/lib.rs`
- `docs/architecture/wifi-direct.md`

Optional helper:

- `.agent/tools/inspect-connectivity-surface.sh`

## Workflow

1. Decide which layer the new mechanism belongs to.
    - Discovery only
    - Link setup plus address acquisition
    - True new transport backend

2. Prefer the smallest viable extension.
    - If Bluetooth can produce an IP neighbor, mirror Wi-Fi Direct.
    - If Bluetooth needs a non-IP channel, define the minimal new abstraction needed.

3. Add config in `pim-core`.
    - Add a new config struct with `enabled: bool`
    - Include mechanism-specific fields only when justified
    - Default to disabled

4. Add an integration crate if needed.
    - Example: `pim-bluetooth`
    - Keep the public API narrow and daemon-oriented
    - Prefer emitting `SocketAddr` or a small connection descriptor

5. Wire the daemon.
    - Start the mechanism only when enabled
    - Add a dedicated consumer task
    - Reuse `initiate_peer_connection()` if possible

6. Handle failure explicitly.
    - timeouts
    - missing system dependencies
    - duplicate peers
    - reconnect semantics
    - cleanup on shutdown

7. Update docs and tests.

## Bluetooth-Specific Design Questions

Resolve these early:

- Does Bluetooth provide an IP interface in the target environment?
- Is the intended path Bluetooth PAN, BLE GATT, RFCOMM, or another mode?
- Can the resulting link supply a stable peer IP and port?
- Does pairing happen out of band or under daemon control?
- What Linux userspace dependency is required: `bluetoothctl`, D-Bus, BlueZ APIs, or something else?

If the answer is "Bluetooth yields an IP link", then model it after Wi-Fi Direct.
If not, document why a new transport or adapter is required.

## Expected Output

- chosen architecture
- code changes
- test coverage
- explicit limitations
- doc updates

## Done Criteria

The work is done when:

- config is wired and documented
- the daemon can start the new mechanism conditionally
- discovered or established peers enter the existing connection flow, or a justified replacement exists
- tests cover config and construction
- docs describe how the mechanism relates to TCP, UDP discovery, and Wi-Fi Direct
