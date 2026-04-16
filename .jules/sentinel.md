## 2026-04-09 - [Secure File Permissions for Private Keys]
**Vulnerability:** The node's private Ed25519 signing key (`node.key`) was written using `std::fs::write`, which falls back to the system's default `umask` and can leave the key world-readable.
**Learning:** Relying on default file permissions when writing sensitive cryptographic material creates a risk of exposing the key to other local users.
**Prevention:** Explicitly use `std::fs::OpenOptions` with `OpenOptionsExt::mode(0o600)` on Unix systems to ensure strict file access permissions when writing secrets.

## 2024-05-24 - [Secure File Permissions for Daemon Files]
**Vulnerability:** The daemon's `atomic_write` function used `tokio::fs::write`, which relies on the system's default `umask`. This function is used to write sensitive files like the trust store, potentially making them world-readable.
**Learning:** Relying on default file permissions when writing runtime configuration or trust stores creates a risk of exposing sensitive peer identities or network state to other local users.
**Prevention:** Explicitly use `tokio::fs::OpenOptions` with `mode(0o600)` on Unix systems and `tokio::io::AsyncWriteExt` to ensure strict file access permissions when performing asynchronous writes of sensitive data.

## 2024-06-25 - [Secure File Permissions for System Files]
**Vulnerability:** The daemon's PID file (`pid_file`) was written using `std::fs::write`, which falls back to the system's default `umask`. Under a permissive umask (e.g., `0000`), this could leave the PID file world-writable, allowing unprivileged users to spoof the PID.
**Learning:** Relying on default file permissions when writing non-sensitive system files (like PID files) can still create security risks, such as local DoS or privilege escalation, if the files are used by other system services.
**Prevention:** Explicitly use `std::fs::OpenOptions` with `OpenOptionsExt::mode(0o644)` on Unix systems to ensure strict file access permissions (owner writable, group/others readable) when writing system files.

## 2026-04-09 - [Secure File Permissions for Private Keys: Symlink Arbitrary File Overwrite]
**Vulnerability:** The daemon used `std::fs::OpenOptions` with `create(true).truncate(true)` when saving the Ed25519 node private key. This creates a Time-of-Check to Time-of-Use (TOCTOU) symlink vulnerability, allowing an attacker to pre-create a symlink at the target path, causing the daemon to overwrite an arbitrary file (like `/etc/shadow`) with the private key upon file creation if running as root.
**Learning:** Using `O_CREAT | O_TRUNC` for sensitive files exposes the system to symlink-based arbitrary file overwrites.
**Prevention:** Explicitly use `create_new(true)` which leverages `O_CREAT | O_EXCL` when writing sensitive key files to fail securely if a file or symlink already exists at the target path.
