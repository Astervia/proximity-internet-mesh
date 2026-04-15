## 2024-05-14 - Transport payload decryption zero-copy allocation optimization

**Learning:** Decrypting received TransportFrames created unnecessary memory pressure due to buffer allocation. In an async daemon doing heavy network I/O, allocating strings or slices in a hot path causes bottlenecks.
**Action:** Always favor using `AeadInPlace` in `aes-gcm` (or similar cipher crates) so you can directly decrypt the payload within its original slice over allocating copies of the slice for the cipher output.

## 2024-05-14 - E2E payload decryption zero-copy allocation optimization

**Learning:** Similar to TransportFrames, decrypting received E2E payloads created unnecessary memory pressure due to buffer allocation of `Vec<u8>`. In an async daemon doing heavy network I/O, allocating strings or slices in a hot path causes bottlenecks.
**Action:** Decrypt E2E payload within its original slice over allocating copies of the slice for the cipher output using `AeadInPlace` in `aes-gcm`.

## 2024-05-15 - E2E payload encryption zero-copy allocation optimization

**Learning:** E2E payload encryption created unnecessary memory pressure due to buffer allocation. In an async daemon doing heavy network I/O, allocating intermediate strings or slices in a hot path causes bottlenecks. We refactored `e2e_encrypt` to use `encrypt_in_place_detached` directly on the pre-allocated output buffer instead of allocating an intermediate vector for the ciphertext.
**Action:** Favor using `AeadInPlace` for encryption when appending to an existing or pre-allocated buffer to avoid unnecessary memory allocations.

## 2024-05-16 - Transport payload encryption zero-copy allocation optimization

**Learning:** Similar to our findings with E2E encryption, transport payload encryption created unnecessary memory pressure due to extra buffer allocations in `Session::encrypt_frame` and `transport_frame_from_encrypted`. In a hot path, this causes bottlenecks.
**Action:** Favor using `AeadInPlace::encrypt_in_place_detached` when you need to retain the original plaintext byte-slice structure but place it within a new wrapper (like `TransportFrame`), eliminating unnecessary temporary buffer creations.

## 2024-05-16 - Transport write buffer zero-copy allocation optimization

**Learning:** Transport write queues were creating unnecessary memory allocations in the hot path. Serializing frames via `BytesMut` then converting them using `.to_vec()` caused extra allocations on every outgoing packet. Using `freeze()` transforms `BytesMut` into `Bytes` with zero copying.
**Action:** Use `Bytes::freeze()` rather than `.to_vec()` to pass buffers between async tasks without unnecessary memory allocations.
