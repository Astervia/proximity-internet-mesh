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

## 2024-06-26 - [Secure File Creation for Config Generation]
**Vulnerability:** The CLI's config generation (`cmd_config_generate`) checked `path.exists()` before calling `std::fs::OpenOptions::create(true).truncate(true)`. This Time-of-Check to Time-of-Use (TOCTOU) vulnerability allowed a symlink attack where an attacker could create a symlink at the target path after the check but before the open, causing the CLI to overwrite an arbitrary file.
**Learning:** Checking file existence separately from opening the file creates a race condition for file creation, especially when running with elevated privileges.
**Prevention:** Explicitly use `create_new(true)` on `std::fs::OpenOptions` (or `tokio::fs::OpenOptions`) to enforce `O_CREAT | O_EXCL` at the OS level, making the check and creation atomic. Catch the `AlreadyExists` error to handle the case where the file is already present.
