# Bluetooth RFCOMM wire protocol

> Wire format used by every RFCOMM-capable PIM peer. The protocol is
> the cross-platform contract; the implementation lives wherever the
> platform-specific Bluetooth socket lives.

## Architectural placement

PIM treats `pim-daemon` as **a portable bridge between transports and
the mesh logic** — routing, gateway, NAT, identity, security — and
nothing else. Wire-protocol bridges (RFCOMM, Wi-Fi Direct, future
transports) are external to the daemon. They live wherever the
platform's native socket API lives:

- The Linux `pim-bluetooth` crate is in-tree only because Linux's
  `AF_BLUETOOTH` socket is reachable from Rust without a host stack
  callout. It is **not** privileged over other platforms — it's just
  one implementation of the same wire contract.
- macOS uses a Swift sidecar that the desktop Tauri shell spawns,
  because `IOBluetooth` is the only public Classic-BT API on the
  platform.
- Android uses a Kotlin Tauri plugin, because Java `BluetoothSocket`
  is the only RFCOMM API exposed to apps.

Each implementation MUST be byte-compatible with this spec — same
framing, same JSON shape, same `mesh_tag` derivation. After Hello /
HelloAck, the channel is bridged to the daemon's local TCP transport
(`127.0.0.1:[transport].listen_port`); the daemon doesn't know or
care which transport the bytes came in on.

When pim-daemon adds a new transport (LoRa, Thread, Matter, …), it
adds **a new wire-protocol spec under `kernel/docs/architecture/transports/`**
and one or more sidecar/plugin implementations. The daemon itself
gains nothing transport-specific; it keeps speaking
`pim-protocol::TransportFrame` over a TCP socket.

## Implementations

| Platform | Location | Notes |
| --- | --- | --- |
| Linux   | `kernel/crates/pim-bluetooth/src/rfcomm/` (in-tree) | Raw `AF_BLUETOOTH` / `BTPROTO_RFCOMM` socket via `libc`. `#[cfg(target_os = "linux")]`. Could move to a sidecar binary if Linux ever grows a non-libc BT story; today the in-tree crate is just convenience. |
| macOS   | `ui/tools/pim-bt-rfcomm-mac/` | Swift sidecar binary spawned by the desktop Tauri shell. Uses `IOBluetooth`. |
| Android | `ui/src-tauri/gen/android/.../org/astervia/pim/BluetoothPlugin.kt` (forthcoming `RfcommSession.kt` helper) | Kotlin Tauri plugin. Uses Java `BluetoothSocket`. See `plans/android-port/phase-b-rfcomm-handshake.md`. |

## Frame format

Each application message is wrapped in a 4-byte big-endian length prefix
identical to `pim-protocol::LengthDelimitedCodec`:

```
+----------------+-------------------------+
| length: u32 BE | payload: utf-8 JSON     |
| (4 bytes)      | (length bytes)          |
+----------------+-------------------------+
```

- `length` is the byte count of `payload`, NOT including the prefix.
- Max length: **65 536 bytes**. Larger frames MUST be rejected with a
  `TooLarge` error and the channel torn down.
- A `length` of `0` is reserved for future framing extensions and MUST
  be rejected today.
- Multiple frames per channel: free, in order, reliable (RFCOMM
  guarantees in-order delivery).

Reference implementation:
[`crates/pim-bluetooth/src/rfcomm/frame.rs`](../../../crates/pim-bluetooth/src/rfcomm/frame.rs).

## Handshake

The peer that **opened** the RFCOMM channel sends `Hello` first. The
**accepting** peer replies with `HelloAck`. Both messages are JSON.

### `Hello` (initiator → acceptor)

```json
{
  "type": "hello",
  "v": 1,
  "node_id": "a3f2c8b4d9e6f7012a3f2c8b4d9e6f7012a3f2c8b4d9e6f7012a3f2c8b4d9e6f7",
  "name": "PIM-pepe",
  "platform": "macos",
  "caps": ["mesh-v1"],
  "mesh_tag": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
}
```

### `HelloAck` (acceptor → initiator)

```json
{
  "type": "hello-ack",
  "v": 1,
  "node_id": "b4e3d9c0a1f2e3d4b4e3d9c0a1f2e3d4b4e3d9c0a1f2e3d4b4e3d9c0a1f2e3d4",
  "name": "PIM-gatewaybtonly",
  "platform": "linux",
  "caps": ["mesh-v1", "gateway-v1"],
  "mesh_tag": "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210"
}
```

### Field semantics

| Field | Type | Required | Notes |
| --- | --- | --- | --- |
| `type` | `"hello"` \| `"hello-ack"` | yes | The implementation field is named `kind` in some codebases (e.g. Rust); the serde tag on the wire is always `type`. |
| `v` | `u8` | yes | Protocol version. Currently **`1`**. |
| `node_id` | string | yes | 32-character lowercase hex of the peer's NodeId (16 bytes). |
| `name` | string | yes | Human-readable node label (`[node].name` in pim.toml). |
| `platform` | string | yes | Free-form short label: `"linux"`, `"macos"`, `"android"`, etc. Used for diagnostics only. |
| `caps` | array of strings | yes | Capability flags advertised by the sender (`mesh-v1`, optionally `gateway-v1`/`relay-v1`/etc.). Future bumps add new strings; receivers MUST ignore unknown capabilities. |
| `mesh_tag` | string \| null | conditional | Lowercase-hex `HMAC-SHA256(mesh_handshake_key, node_id_hex)`. Present iff the sender is on a private mesh. Receivers on a private mesh require this field to be present and matching; receivers on the open mesh require it to be absent. **Open ↔ private is hard-rejected at the RFCOMM Hello layer, before any TCP bridge is set up.** |

Reference: [`pim-crypto::compute_rfcomm_hello_tag`](../../../crates/pim-crypto/src/lib.rs)
for the exact HMAC layout (key length, key derivation, ASCII encoding
of `node_id_hex`).

### Version mismatch

If the version (`v`) doesn't match, the side that detects the mismatch
sends:

```json
{"type":"error","code":"version_mismatch","peer_v":1,"our_v":2}
```

…and closes the channel. No retry — the peer must upgrade or downgrade.

## Discovery loop

Both sides emit a discovery event on stdout (or via the daemon's local
RPC stream) every time a peer's handshake completes successfully:

```json
{
  "event": "discovered",
  "peer": {
    "node_id": "...",
    "name": "PIM-...",
    "platform": "linux",
    "caps": ["mesh-v1", "gateway-v1"],
    "bd_addr": "00:15:83:3D:0A:57",
    "since": "2026-04-30T03:42:05Z"
  }
}
```

And on disconnect:

```json
{"event":"lost","peer":{"node_id":"..."},"reason":"channel_closed"}
```

These events are consumed by the parent daemon so it can update the
pim-ui Peers panel without the implementation having to know about RPC
plumbing.

## Identity persistence

Each implementation reads its `node_id` from a local file:

| Platform | Path |
| --- | --- |
| Linux | `[security].key_file` from pim.toml (default `/var/lib/pim/node.key`). |
| macOS | `~/Library/Application Support/pim/node.key` (32 bytes hex, owned by the daemon). |
| Android | App-private `getFilesDir()/node.key`, populated by the in-process daemon library. The Kotlin plugin obtains the hex via the JNI `nativeLocalIdentity` export. |

For standalone test harnesses, if no key exists, the implementation MAY
generate a random 32-byte hex string per process and persist nothing.

## RFCOMM channel & SDP

- Default channel: **22**. Channel 1 (the SPP convention) is reserved by
  BlueZ for its built-in Serial Port profile on most Linux installs and
  causes `bind()` to return `EADDRINUSE`. Channel 22 is in the dynamic
  RFCOMM range (1–30) but far from the SPP default — empirically free
  on common Linux + macOS + Android stacks.
- Linux registers an SDP record:
  ```bash
  sudo sdptool add --channel=22 SP
  ```
  Required for some BR/EDR stacks (notably modern Android) to discover
  the channel via UUID-based dialing. Re-run after every `bluetoothd`
  restart.
- The macOS Swift sidecar relies on direct channel-by-ID dial and
  doesn't need an SDP record.
- The Android Kotlin plugin tries hidden `createInsecureRfcommSocket(int)`
  first to dial channel 22 directly; falls back to SDP-resolved SPP
  UUID dial against the Linux SDP record above.

## Auto-discovery mechanics

**Linux** (`kernel/crates/pim-bluetooth/src/rfcomm/listener.rs` +
`outbound.rs`):

1. Bind RFCOMM listening socket on `[bluetooth_rfcomm].channel`.
2. Loop accepting; for each new connection, run handshake.
3. On `Hello` received, reply with `HelloAck`, emit `discovered`.
4. Independently scan paired devices via `bluetoothctl devices Paired`
   (when `[bluetooth_rfcomm].outbound_enabled = true`) and dial peers
   whose name starts with `[bluetooth_rfcomm].device_name_prefix`.

**macOS** (`ui/tools/pim-bt-rfcomm-mac/`):

1. Every `poll_interval_ms`, enumerate `IOBluetoothDevice.pairedDevices()`.
2. Filter to devices whose name starts with `PIM-`.
3. For each, if no active RFCOMM channel exists, attempt
   `openRFCOMMChannelAsync(channel = <configured>)`.
4. On open, send `Hello`, wait for `HelloAck`, emit `discovered`.
5. Also accept inbound RFCOMM channels on the same channel.

**Android** (`ui/src-tauri/gen/android/.../BluetoothPlugin.kt`):

1. On daemon start, the Tauri JS layer calls `ensurePimName` so the BT
   adapter advertises `PIM-android-<addr-tail>`.
2. `listenIncoming` binds an inbound listener (`SDP-only on API 33+`,
   raw channel via reflection on older APIs).
3. `listBondedDevices` returns the current paired-device list to the
   JS layer; the JS code calls `connect(address)` for each device whose
   name starts with `PIM-`.
4. `connect(address)` dials raw channel 22 first (hidden API), falling
   back to SDP-resolved SPP UUID then PIM_UUID lookups.
5. After the BT socket opens (in or out), `runRfcommSession` drives the
   Hello handshake before bridging bytes to `127.0.0.1:9100` (the
   in-process daemon's TCP transport).

## Post-handshake

Once the channel is in steady state post-handshake, the daemon pumps
`pim-protocol::TransportFrame` bytes through it (length-prefixed frames
reuse the same wire format — the Hello/HelloAck pair is just the first
two application frames). On Android and macOS this happens by bridging
the BT socket to `127.0.0.1:<transport.listen_port>`; on Linux it
happens the same way (the bridge is owned by the daemon's
[`bridge.rs`](../../../crates/pim-bluetooth/src/rfcomm/bridge.rs)).

Multiple peers concurrent: each peer = own RFCOMM channel = own task on
the listener / outbound thread.

## Versioning policy

The protocol version (`v`) bumps in lockstep across all three
implementations. The procedure:

1. Open a kernel-side spec change against this document and the
   `HelloMsg` struct in `kernel/crates/pim-bluetooth/src/rfcomm/session.rs`.
2. In parallel, update `ui/tools/pim-bt-rfcomm-mac` (Swift) and the
   Android Kotlin RFCOMM helper.
3. Land all three in the same release window. Older peers receive the
   `error: version_mismatch` envelope and disconnect cleanly.

Don't add new fields with default values without bumping `v`. Older
implementations parse JSON laxly today, but the policy is: **every
field change is a breaking change, and breaking changes bump `v`.**
