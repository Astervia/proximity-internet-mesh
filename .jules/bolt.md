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

## 2024-05-17 - Discovery advertisement encryption zero-copy allocation optimization

**Learning:** Discovery advertisement serialization/deserialization for encrypted format used standard `.encrypt` and `.decrypt` from the AEAD cipher, resulting in repeated allocation of `Vec<u8>`. For a system that handles heavy network I/O, these continuous runtime allocations can introduce bottlenecks.
**Action:** Use `AeadInPlace::encrypt_in_place_detached` and `decrypt_in_place_detached` operating directly on stack buffers, preserving zero-copy operations across the codebase.

## 2024-05-16 - Zero-copy transport framing

**Learning:** Data and Transport frames were constantly taking ownership of byte buffers via `.to_vec()` instead of using zero-copy `bytes::Bytes`. This caused numerous heap allocations per forwarded packet on relay nodes, unnecessarily straining memory bandwidth.
**Action:** Replace `Vec<u8>` payloads in protocol frame structs (`TransportFrame`, `MeshDataFrame`, `FragmentFrame`) with `bytes::Bytes` to allow zero-copy parsing and forwarding, drastically cutting allocations per packet.
## 2024-05-18 - Avoid allocations on uniquely owned `bytes::Bytes` with `try_into_mut()`

**Learning:** `bytes::Bytes` holds a reference-counted buffer but its contents cannot be mutated via `.to_vec()` without forcing a clone and full allocation. However, if the network code owns the sole reference to the buffer (e.g. immediately after decoding), `.try_into_mut()` can safely reclaim mutable access to the underlying `BytesMut` with zero allocations. In high-throughput network daemons like PIM, allocating memory for every frame on the read path becomes a major performance bottleneck.
**Action:** When a frame buffer needs to be mutated directly (like in `decrypt_in_place_detached`), avoid `.to_vec()`. Instead, attempt to use `.try_into_mut()` when we know the `Bytes` object is uniquely owned to recover zero-copy mutability.
## 2026-05-08 - Fragment reassembly vector allocation optimization

**Learning:** Reassembling fragmented mesh packets created unnecessary memory pressure due to redundant heap allocations. The `try_reassemble` function was allocating a `vec![0u8; self.total_length]` on *every* fragment insertion attempt, even when the fragment stream was incomplete (causing N redundant allocations for a packet needing N fragments).
**Action:** When optimizing iterative data reassembly, perform a fast O(N) validation pass over the fragment map to ensure data completeness (e.g., all chunks are present and contiguous) *before* allocating the target heap vector. This reduces allocations per packet from N to 1.

## 2026-05-09 - Zero-copy packet fragmentation
**Learning:** `fragment_packet` previously took a `&[u8]` slice and forced memory allocation for every fragmented packet via `Bytes::copy_from_slice()`. For large packets traversing the mesh, this creates massive memory pressure due to repeated allocation at every network boundary. Passing the original `Bytes` buffer lets us utilize `.slice(offset..end)` to create cheap references.
**Action:** When designing API boundaries for networking, protocol layers, and fragmentation, pass payloads as `bytes::Bytes` by value instead of `&[u8]`. This preserves the ability to perform O(1) zero-copy slicing (`payload.slice()`) downstream and avoids unnecessary O(N) memory reallocations like `Bytes::copy_from_slice()`.

## 2026-05-09 - Pre-allocate Vector capacities in hot paths when populating collections

**Learning:** `Vec::new()` allocates no heap memory until the first element is pushed, at which point it dynamically allocates and then periodically reallocates. In hot network paths like `fragment_packet`, where we know we will push items, this leads to unnecessary reallocation overhead. However, it is a mistake to aggressively apply `Vec::with_capacity()` to collections that often remain empty (e.g., error queues or retry buffers on the "happy path"), as this will force an unnecessary heap allocation where `Vec::new()` would have remained zero-cost.
**Action:** Always pre-allocate exact `Vec` capacities (e.g., using `Vec::with_capacity()`) over `Vec::new()` when the final size or an upper bound is known in advance *and* the vector is guaranteed or highly likely to be populated. Avoid `Vec::with_capacity()` for paths that usually remain empty.

## 2026-05-20 - Rusqlite statement caching optimization

**Learning:** Repeatedly executing identical static SQL queries using `conn.prepare()` creates unnecessary overhead in performance-sensitive paths (e.g., messaging and peer lookups) because it forces SQLite to parse and compile the SQL string on every call.
**Action:** Prefer `rusqlite`'s `conn.prepare_cached()` over `conn.prepare()` for static SQL strings to leverage the connection's built-in LRU statement cache and minimize query parsing latency.
