## 2024-05-14 - Transport payload decryption zero-copy allocation optimization
**Learning:** Decrypting received TransportFrames created unnecessary memory pressure due to buffer allocation. In an async daemon doing heavy network I/O, allocating strings or slices in a hot path causes bottlenecks.
**Action:** Always favor using `AeadInPlace` in `aes-gcm` (or similar cipher crates) so you can directly decrypt the payload within its original slice over allocating copies of the slice for the cipher output.
