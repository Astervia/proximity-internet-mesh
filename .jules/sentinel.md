## 2026-04-09 - [Secure File Permissions for Private Keys]
**Vulnerability:** The node's private Ed25519 signing key (`node.key`) was written using `std::fs::write`, which falls back to the system's default `umask` and can leave the key world-readable.
**Learning:** Relying on default file permissions when writing sensitive cryptographic material creates a risk of exposing the key to other local users.
**Prevention:** Explicitly use `std::fs::OpenOptions` with `OpenOptionsExt::mode(0o600)` on Unix systems to ensure strict file access permissions when writing secrets.

## 2024-05-24 - [Secure File Permissions for Daemon Files]
**Vulnerability:** The daemon's `atomic_write` function used `tokio::fs::write`, which relies on the system's default `umask`. This function is used to write sensitive files like the trust store, potentially making them world-readable.
**Learning:** Relying on default file permissions when writing runtime configuration or trust stores creates a risk of exposing sensitive peer identities or network state to other local users.
**Prevention:** Explicitly use `tokio::fs::OpenOptions` with `mode(0o600)` on Unix systems and `tokio::io::AsyncWriteExt` to ensure strict file access permissions when performing asynchronous writes of sensitive data.
