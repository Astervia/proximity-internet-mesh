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

## 2024-08-16 - [Secure File Creation for Configs]
**Vulnerability:** The CLI's configuration initialization logic used a non-atomic `path.exists()` check before creating the `config.toml` file with `create(true).truncate(true)`. This creates a Time-of-Check to Time-of-Use (TOCTOU) vulnerability where an attacker could place a symlink at the target path between the check and the file creation.
**Learning:** Checking for file existence before creation using separate operations is prone to race conditions and symlink attacks.
**Prevention:** Use atomic operations like `OpenOptions::new().create_new(true)` when creating sensitive files that shouldn't be overwritten, and handle the resulting `ErrorKind::AlreadyExists` to provide safe, race-free user warnings.
## 2026-04-23 - [Prevent TOCTOU via Atomic File Creation]
**Vulnerability:** The daemon's config generation (`pim-cli/src/main.rs`) and identity key generation (`pim-crypto/src/identity.rs`) checked `path.exists()` before calling `create(true).truncate(true)`. This creates a Time-of-Check to Time-of-Use (TOCTOU) race condition where an attacker could replace the file with a symlink between the check and the use, potentially leading to arbitrary file overwrites or key theft.
**Learning:** Checking for file existence before creating a sensitive file is inherently racy and insecure.
**Prevention:** Always rely on the filesystem to enforce exclusivity. Use `OpenOptions::new().create_new(true)` to atomically create a file only if it does not already exist, and handle `ErrorKind::AlreadyExists` errors gracefully.
## 2026-04-23 - [Prevent DoS via Panic on Invalid Nonce Length]
**Vulnerability:** In `pim-crypto`, the `decrypt` and `decrypt_in_place_detached` methods used an unsafe `.unwrap()` on the result of `try_into()` when parsing a slice (`[8..12]`) into an array. If an attacker could feed malformed, truncated, or otherwise corrupted frames to these methods, the program could panic and crash, leading to a Denial of Service (DoS).
**Learning:** Relying on `.unwrap()` during conversion of dynamic length slices from network or external boundaries can trigger runtime panics.
**Prevention:** Always use safe slice operations (like `.get()`) with appropriate error mapping (`.ok_or()`) and `.try_into().map_err()` instead of `.unwrap()` to ensure robust error handling without crashing the program.

## 2026-05-09 - [Secure File Permissions for Atomic Replace]
**Vulnerability:** The daemon's broadcast config was persisted using `std::fs::write` to a temporary file before renaming it over the original file. Since `std::fs::write` falls back to the system's default `umask`, the temporary file could be created world-readable and world-writable. The POSIX `rename` operation preserves the permissions of the source file, causing the sensitive configuration file to end up with insecure permissions.
**Learning:** When performing atomic file writes using a temporary file and `rename`, the temporary file must be created with explicitly strict permissions (e.g., `0o600`), because its permissions will become the permissions of the final file.
**Prevention:** Explicitly configure `std::fs::OpenOptions` with `OpenOptionsExt::mode(0o600)` on Unix systems when creating temporary files for atomic replacement, avoiding reliance on system umasks.
