# Encryption and Security

## Threat Model

PIM routes IP packets through untrusted intermediate nodes. Any relay in the path can observe, modify, drop, or inject frames. The security model must protect against:

| Threat | Protection |
|--------|-----------|
| Eavesdropping by relay nodes | End-to-end encryption (client ↔ gateway) |
| Packet tampering | Authenticated encryption (AES-256-GCM) |
| Replay attacks | Nonce tracking + session-bound keys |
| Node impersonation | Ed25519 identity keys + signed handshakes |
| Malicious gateway (data exfiltration) | Destination-aware encryption (TLS passthrough) |
| Routing table poisoning | Signed routing advertisements |
| Sybil attacks (fake nodes flooding mesh) | Peer reputation + rate limiting |
| Traffic analysis | Padding + optional dummy traffic (future) |

## Identity

Every node has a persistent **Ed25519 keypair** generated on first run:

```
Private key:  stored in ~/.pim/node.key (never leaves the device)
Public key:   serves as the node's identity
Node ID:      SHA-256(public_key)[0..16] — 16-byte fingerprint
```

The node ID is used in all mesh operations: discovery advertisements, routing tables, frame headers.

### Key Generation

```
ed25519_keypair() → (private_key, public_key)
node_id = sha256(public_key)[0..16]
```

On first startup, the daemon generates the keypair and writes it to the configured `key_file`. Subsequent startups load the existing key.

## Handshake Protocol

When two peers discover each other and establish a transport connection, they perform a handshake to authenticate and derive a shared session key.

```
    Initiator (A)                          Responder (B)
         │                                      │
         │─── HandshakeInit ───────────────────▶│
         │    { A.pub, A.ephemeral_pub,         │
         │      nonce_a, sig_a }                │
         │                                      │
         │◀── HandshakeResponse ────────────────│
         │    { B.pub, B.ephemeral_pub,         │
         │      nonce_b, sig_b }                │
         │                                      │
         │     Both sides compute:              │
         │     shared_secret = X25519(          │
         │       my_ephemeral_priv,             │
         │       their_ephemeral_pub)           │
         │                                      │
         │     session_key = HKDF-SHA256(       │
         │       shared_secret,                 │
         │       salt = nonce_a ‖ nonce_b,      │
         │       info = "pim-session-v1")       │
         │                                      │
         │─── HandshakeConfirm ────────────────▶│
         │    { HMAC(session_key, transcript) }  │
         │                                      │
         │◀── HandshakeConfirm ─────────────────│
         │    { HMAC(session_key, transcript) }  │
         │                                      │
         ▼   Session established                ▼
```

### Steps in Detail

1. **HandshakeInit**: Initiator sends its long-term public key, a freshly generated X25519 ephemeral public key, a random nonce, and an Ed25519 signature over (ephemeral_pub ‖ nonce) using its long-term key.

2. **HandshakeResponse**: Responder verifies the signature, then replies with its own long-term public key, ephemeral public key, nonce, and signature.

3. **Key Derivation**: Both sides perform X25519 Diffie-Hellman with their ephemeral private key and the other's ephemeral public key. The result is fed into HKDF-SHA256 to produce the session key.

4. **HandshakeConfirm**: Both sides send an HMAC of the full handshake transcript using the derived session key. This proves both sides derived the same key and prevents MitM.

### Properties

- **Forward secrecy**: Ephemeral keys are discarded after session establishment. Compromising a node's long-term key doesn't reveal past session traffic.
- **Mutual authentication**: Both sides prove possession of their long-term private key via signatures.
- **Replay resistance**: Fresh nonces ensure each handshake is unique.

## Encryption Layers

PIM uses two layers of encryption:

### 1. Hop-by-Hop Encryption (Transport Security)

Every frame sent between directly connected peers is encrypted with the session key derived during their handshake.

```
PlaintextFrame
    │
    ▼
AES-256-GCM Encrypt(session_key, nonce, frame_bytes)
    │
    ▼
EncryptedTransportFrame { nonce, ciphertext, tag }
```

This protects against passive observers on the Wi-Fi Direct link and ensures frame integrity between direct peers.

**Nonce management**: A 96-bit nonce, constructed as a 32-bit counter concatenated with a 64-bit random session prefix. The counter increments per frame per direction. If the counter reaches 2^32, the session must be rekeyed.

### 2. End-to-End Encryption (Payload Security)

The actual IP packet payload is encrypted from the client to the gateway, so relay nodes cannot read it.

```
Client                    Relay                    Gateway
  │                         │                         │
  │ E2E_Encrypt(payload,    │                         │
  │   gateway_pub_key)      │                         │
  │─── [e2e_encrypted] ────▶│─── [e2e_encrypted] ───▶│
  │                         │  (relay cannot decrypt) │ E2E_Decrypt()
  │                         │                         │
```

The client obtains the gateway's public key during route discovery. It performs X25519 with an ephemeral key to derive a per-packet or per-session encryption key.

**Scheme:**
```
ephemeral_priv, ephemeral_pub = x25519_keygen()
shared = X25519(ephemeral_priv, gateway_pub)
e2e_key = HKDF-SHA256(shared, salt=random, info="pim-e2e-v1")
ciphertext = AES-256-GCM(e2e_key, nonce, ip_packet)
e2e_frame = { ephemeral_pub, salt, nonce, ciphertext, tag }
```

The gateway decrypts using its long-term private key and the ephemeral public key from the frame.

### Combined Encryption Stack

A packet traversing a multi-hop path has this structure:

```
┌─────────────────────────────────────────────────┐
│  Transport Frame (hop-by-hop encrypted)         │
│  ┌───────────────────────────────────────────┐  │
│  │  Mesh Frame Header                        │  │
│  │  { src_id, dst_id, ttl, session_id, ... } │  │
│  │  ┌─────────────────────────────────────┐  │  │
│  │  │  E2E Encrypted Payload              │  │  │
│  │  │  { ephemeral_pub, nonce,            │  │  │
│  │  │    ciphertext(IP packet), tag }     │  │  │
│  │  └─────────────────────────────────────┘  │  │
│  └───────────────────────────────────────────┘  │
└─────────────────────────────────────────────────┘
```

At each hop:
1. Outer transport encryption is decrypted (hop-by-hop)
2. Mesh frame header is read for routing decisions
3. Inner E2E payload is **not** decrypted — passed through as-is
4. Frame is re-encrypted with the next hop's session key and forwarded

## TLS Passthrough

Since PIM operates at the IP layer, TLS connections from applications pass through the mesh end-to-end:

```
App ←──TLS──→ pim0 ←──mesh──→ gateway ←──TLS──→ remote server
```

The application's TLS session is between the app and the remote server. PIM encrypts the already-encrypted TLS packets for mesh transit. This means:
- The gateway cannot read HTTPS content (it only sees encrypted TLS bytes)
- Applications retain their own security guarantees
- PIM encryption protects metadata (destination IP, port) from relay nodes

## Routing Message Authentication

Routing advertisements and updates are signed with the sender's Ed25519 key:

```
RoutingUpdate {
    origin_id: NodeId,
    sequence: u64,
    routes: Vec<RouteEntry>,
    timestamp: u64,
    signature: Ed25519Signature,   // signs all fields above
}
```

Nodes verify signatures before accepting routing updates. This prevents:
- Route injection by malicious nodes
- Modification of routing updates in transit

## Key Rekeying

Session keys are rekeyed:
- After transmitting 2^32 frames (nonce counter exhaustion)
- After a configurable time interval (default: 1 hour)
- On demand if a security event is detected

Rekeying performs a new ephemeral X25519 exchange within the existing authenticated session.

## Cryptographic Primitives Summary

| Purpose | Algorithm |
|---------|-----------|
| Node identity | Ed25519 |
| Key exchange | X25519 (Curve25519 Diffie-Hellman) |
| Key derivation | HKDF-SHA256 |
| Symmetric encryption | AES-256-GCM |
| Hashing / fingerprints | SHA-256 |
| Message authentication | HMAC-SHA256 |
| Nonce generation | CSPRNG (OS-provided) |

## Rust Crate Choices

| Crate | Purpose |
|-------|---------|
| `ring` or `rustls` | Core crypto primitives |
| `x25519-dalek` | X25519 key exchange |
| `ed25519-dalek` | Ed25519 signatures |
| `aes-gcm` | AES-256-GCM encryption |
| `hkdf` + `sha2` | Key derivation |
| `rand` | CSPRNG for nonces and ephemeral keys |
