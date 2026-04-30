# RFCOMM auto-discovery protocol

> Wire format used by `pim-bt-rfcomm-mac` (Swift) ↔ `pim-bt-rfcomm-linux` (Python/Rust)
> over RFCOMM channel 1, on top of Bluetooth Classic BR/EDR pairing.

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
- Max length: 65 536 bytes. Larger frames MUST be rejected.
- Multiple frames per channel: free, in order, reliable (RFCOMM guarantees).

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
  "caps": ["mesh-v1"]
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
  "caps": ["mesh-v1", "gateway-v1"]
}
```

If the version (`v`) doesn't match, the side that detects mismatch sends
`{"type":"error","code":"version_mismatch","peer_v":1,"our_v":2}` and
closes the channel.

## Discovery loop

Both sides emit a discovery event on stdout (newline-delimited JSON)
every time a peer's handshake completes successfully:

```json
{"event":"discovered","peer":{"node_id":"...","name":"PIM-...","platform":"linux","caps":["mesh-v1","gateway-v1"],"bd_addr":"00:15:83:3D:0A:57","since":"2026-04-30T03:42:05Z"}}
```

And on disconnect:

```json
{"event":"lost","peer":{"node_id":"..."},"reason":"channel_closed"}
```

These events are consumed by the parent daemon (pim-daemon) via Unix socket
or stdout pipe.

## Identity persistence

Both sides read their `node_id` from a local file:

- Mac: `~/Library/Application Support/pim/node.key` (32 bytes hex, owned by the daemon)
- Linux: `/etc/pim/node.key` (same format)

For the standalone test harness, if no key exists, both sides generate a
random 32-byte hex string per process and persist nothing.

## RFCOMM channel & SDP

- Channel: **1** (SPP convention).
- Linux registers an SDP record: `sudo sdptool add --channel=1 SP`. This is
  required for some BR/EDR stacks to discover the SPP service; the Mac
  side can connect directly by channel ID even without SDP, but registration
  improves reliability.

## Auto-discovery mechanics

**Mac side (initiator + acceptor)**:
1. Every 30s, enumerate `IOBluetoothDevice.pairedDevices()`.
2. Filter to devices whose name starts with `PIM-`.
3. For each, if no active RFCOMM channel exists, attempt
   `openRFCOMMChannelAsync(channel=1)`.
4. On open, send `Hello`, wait for `HelloAck`, emit `discovered` event.
5. Maintain registered channel per peer; on close emit `lost`.
6. Also accept inbound RFCOMM channels (act as RFCOMM acceptor).

**Linux side (acceptor primarily)**:
1. Bind RFCOMM listening socket on channel 1.
2. Loop accepting; for each new connection, run handshake.
3. On `Hello` received, reply with `HelloAck`, emit `discovered`.
4. Optionally also scan paired devices and connect outbound (symmetric to Mac).

## Future hooks (not in this PoC)

- Once the channel is in steady state post-handshake, the daemon can pump
  `pim-protocol::TransportFrame` bytes through it (length-prefixed frames
  reuse the same wire format — the handshake is just the first two
  application frames).
- Multiple peers concurrent: each peer = own RFCOMM channel = own task.
