# Wire Protocol

## Overview

PIM uses a custom binary protocol for communication between peers over Wi-Fi Direct. The protocol is designed to be compact, extensible, and suitable for carrying IP packets across the mesh.

All multi-byte integers are **big-endian**. All frames are **authenticated and encrypted** at the transport layer (see [security.md](security.md)).

## Frame Structure

Every message between peers is a **TransportFrame** — the unit of transmission over a Wi-Fi Direct link.

```
TransportFrame (encrypted, between direct peers)
┌────────────────────────────────────────────────────┐
│ magic: u16          = 0x504D ("PM")                │  2 bytes
│ version: u8         = 1                            │  1 byte
│ frame_type: u8      (see Frame Types)              │  1 byte
│ length: u32         (payload length in bytes)      │  4 bytes
│ nonce: [u8; 12]     (AES-GCM nonce)               │  12 bytes
│ payload: [u8]       (encrypted, variable length)   │  N bytes
│ tag: [u8; 16]       (AES-GCM auth tag)            │  16 bytes
└────────────────────────────────────────────────────┘
Total overhead: 36 bytes + payload
```

After transport-layer decryption, the payload is interpreted according to `frame_type`.

## Frame Types

```
0x01  Handshake         Handshake messages (init, response, confirm)
0x02  Data              Mesh data frame (carries IP packets)
0x03  RouteUpdate       Routing table advertisement
0x04  Heartbeat         Keepalive between direct peers
0x05  Control           Mesh control messages (IP assignment, goodbye, etc.)
0x06  Fragment          Fragment of a larger frame
```

## Mesh Data Frame (0x02)

The primary frame type — carries IP packets through the mesh.

```
MeshDataFrame (decrypted payload of TransportFrame)
┌────────────────────────────────────────────────────┐
│ src_id: [u8; 16]     (originating node ID)         │  16 bytes
│ dst_id: [u8; 16]     (destination node ID          │  16 bytes
│                       or gateway ID for internet)   │
│ session_id: u32      (for tracking request/response)│  4 bytes
│ ttl: u8              (time-to-live, decrements)     │  1 byte
│ flags: u8            (see Data Flags)               │  1 byte
│ payload_len: u16     (e2e encrypted payload size)   │  2 bytes
│ payload: [u8]        (E2E encrypted IP packet)      │  N bytes
└────────────────────────────────────────────────────┘
Header overhead: 40 bytes
```

### Data Flags

```
bit 0:  IS_FRAGMENT       Payload is a fragment, not a complete packet
bit 1:  IS_LAST_FRAGMENT  This is the last fragment in a sequence
bit 2:  REQUIRES_ACK      Sender expects an acknowledgment
bit 3:  IS_ACK            This frame is an acknowledgment
bit 4:  IS_INTERNET       Destination is the internet (dst_id = gateway)
bits 5-7: reserved
```

## Handshake Frame (0x01)

Used during the peer handshake protocol.

```
HandshakeFrame
┌────────────────────────────────────────────────────┐
│ handshake_type: u8   (0=Init, 1=Response, 2=Confirm)│  1 byte
│ sender_pub: [u8; 32]  (Ed25519 public key)          │  32 bytes
│ ephemeral_pub: [u8; 32] (X25519 ephemeral public)   │  32 bytes
│ nonce: [u8; 32]       (random nonce)                 │  32 bytes
│ signature: [u8; 64]   (Ed25519 signature)            │  64 bytes
│ extra: [u8]           (type-specific, variable)      │  N bytes
└────────────────────────────────────────────────────┘
```

For `handshake_type = 2` (Confirm), the frame contains an HMAC instead of the key fields:

```
HandshakeConfirm
┌────────────────────────────────────────────────────┐
│ handshake_type: u8   = 2                           │  1 byte
│ hmac: [u8; 32]       (HMAC-SHA256 of transcript)   │  32 bytes
└────────────────────────────────────────────────────┘
```

## Route Update Frame (0x03)

```
RouteUpdateFrame
┌────────────────────────────────────────────────────┐
│ origin_id: [u8; 16]   (advertising node)           │  16 bytes
│ sequence: u64          (monotonic sequence number)  │  8 bytes
│ entry_count: u16       (number of route entries)    │  2 bytes
│ entries: [RouteEntry]  (repeated)                   │  N × 18 bytes
│ signature: [u8; 64]    (Ed25519 over all above)     │  64 bytes
└────────────────────────────────────────────────────┘

RouteEntry
┌────────────────────────────────────────────────────┐
│ destination: [u8; 16]  (target node ID)            │  16 bytes
│ hops: u8               (distance in hops)          │  1 byte
│ flags: u8              (bit 0: is_gateway)          │  1 byte
└────────────────────────────────────────────────────┘
```

## Heartbeat Frame (0x04)

```
HeartbeatFrame
┌────────────────────────────────────────────────────┐
│ sender_id: [u8; 16]                                │  16 bytes
│ timestamp: u64         (unix millis)               │  8 bytes
│ gateway_hops: u8       (hops to nearest gateway,   │  1 byte
│                         0xFF = no gateway known)    │
│ load: u8               (0-255, forwarding load)    │  1 byte
└────────────────────────────────────────────────────┘
```

## Control Frame (0x05)

Multipurpose control messages:

```
ControlFrame
┌────────────────────────────────────────────────────┐
│ control_type: u8                                   │  1 byte
│ body: [u8]            (type-dependent)             │  N bytes
└────────────────────────────────────────────────────┘
```

### Control Types

```
0x01  IpRequest       Client requests a mesh IP address
0x02  IpAssign        Gateway assigns an IP to a client
0x03  Goodbye         Node is leaving the mesh
0x04  Rekey           Initiate session rekeying
0x05  Ping            Mesh-level ping (for latency measurement)
0x06  Pong            Response to Ping
```

### IpRequest

```
IpRequest
┌────────────────────────────────────────────────────┐
│ requester_id: [u8; 16]                             │  16 bytes
└────────────────────────────────────────────────────┘
```

### IpAssign

```
IpAssign
┌────────────────────────────────────────────────────┐
│ assigned_ip: [u8; 4]   (e.g., 10.77.0.5)          │  4 bytes
│ subnet_mask: u8         (e.g., 16)                 │  1 byte
│ gateway_ip: [u8; 4]     (gateway's mesh IP)        │  4 bytes
│ lease_seconds: u32      (how long this IP is valid) │  4 bytes
└────────────────────────────────────────────────────┘
```

### Goodbye

```
Goodbye
┌────────────────────────────────────────────────────┐
│ departing_id: [u8; 16]                             │  16 bytes
│ reason: u8              (0=shutdown, 1=moving)      │  1 byte
└────────────────────────────────────────────────────┘
```

## Fragmentation

IP packets can exceed the optimal frame size for Wi-Fi Direct. PIM fragments large packets:

```
Max payload per frame:  1400 bytes (configurable, based on MTU)
```

When a packet exceeds this limit:

```
Fragment header (prepended to each fragment's payload)
┌────────────────────────────────────────────────────┐
│ fragment_id: u32       (unique per fragmented packet)│  4 bytes
│ fragment_offset: u16   (byte offset in original)     │  2 bytes
│ total_length: u32      (original packet total size)  │  4 bytes
└────────────────────────────────────────────────────┘
```

The `IS_FRAGMENT` and `IS_LAST_FRAGMENT` flags in the data frame header indicate fragmentation state.

Reassembly at the destination:

1. Collect fragments by `fragment_id`
2. Order by `fragment_offset`
3. When all fragments received (total bytes == `total_length`), deliver
4. Timeout incomplete assemblies after 10 seconds

## Reliability

PIM is **best-effort by default** (like IP). Higher-layer protocols (TCP) handle retransmission. However, for mesh control messages and routing updates, PIM provides optional acknowledgments:

- Set `REQUIRES_ACK` flag on important frames
- Receiver responds with `IS_ACK` frame containing the same `session_id`
- Sender retransmits if no ACK within 500ms, up to 3 retries
- Used for: routing updates, control frames, fragmented data

## Serialization

Frames are serialized as raw bytes (no Protobuf or JSON overhead for the data path). The Rust implementation uses:

- `byteorder` crate for endian-aware integer encoding
- `bytes` crate for zero-copy buffer management
- Manual serialization for maximum control and minimal allocation

For configuration and debugging, a JSON representation exists but is never used on the wire.

## Example: Full Packet On-Wire

An IP packet from client Node A, destined for the internet, traversing through relay Node B to gateway Node D:

```
On the wire between Node A → Node B:

TransportFrame {
  magic: 0x504D,
  version: 1,
  frame_type: 0x02 (Data),
  length: 1476,
  nonce: [A-B session nonce],
  payload: AES-GCM-Encrypt(session_key_AB, {
    MeshDataFrame {
      src_id: [Node A ID],
      dst_id: [Node D ID],
      session_id: 42,
      ttl: 9,
      flags: 0b00010000 (IS_INTERNET),
      payload_len: 1436,
      payload: E2E_Encrypt(gateway_pub_key, {
        original IP packet (e.g., TCP SYN to 93.184.216.34:443)
      })
    }
  }),
  tag: [16-byte auth tag]
}
```
