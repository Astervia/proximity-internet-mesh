# iOS Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the Rust workspace cross-compile for iOS targets (device + simulator), expose a stable C ABI for a future NEPacketTunnelProvider extension, and document the iOS target architecture — resolving **Milestone 1** of issue #70.

**Architecture:** Audit each existing crate, add `#[cfg(target_os = "ios")]` gates to stub out Linux/macOS-only host integrations (TUN/dev/net/tun, wpa_cli, bluetoothctl, iproute2), and introduce a new leaf crate `pim-ios-ffi` with a minimal opaque-handle C ABI. The extension-side Swift code, the NEPacketTunnelFlow bridge, and client/relay runtime wiring land in follow-up plans — this plan stops at "the workspace compiles for `aarch64-apple-ios` and `aarch64-apple-ios-sim`, produces a `PimCore.xcframework`, and the architecture is documented."

**Tech Stack:** Rust (workspace unchanged), `cargo` with `--target`, `lipo` + `xcodebuild -create-xcframework`, a hand-written C header (no uniffi/cbindgen in Plan 1 — the FFI surface is three functions).

**Scope boundary:**
- **In:** Per-crate iOS platform gates; new `pim-ios-ffi` staticlib crate exposing `pim_ffi_version()`, `pim_ffi_start()`, `pim_ffi_stop()` (start/stop are stubs in this plan — they return a handle and release it, but do not yet drive the daemon); `scripts/build-ios.sh` producing a universal XCFramework; `docs/architecture/ios.md`; `make ios-check` smoke target.
- **Out:** NEPacketTunnelProvider Swift extension, Xcode app project, real packet flow, relay accept loop, discovery on iOS. Those belong to Plan 2 (Milestones 2–4) and Plan 3 (Milestone 5). See "Follow-up plans" at the bottom.
- **Out:** The `pim-daemon` and `pim-cli` binary crates stay Linux/macOS-only — we do **not** try to make them compile for iOS. Only library crates need to be iOS-clean.

**Prerequisites the executor must confirm before Task 1:**
- `xcode-select -p` returns a Developer directory.
- `rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios` completes.
- Working directory is a fresh worktree of the repo on a branch like `ios-foundation`.

---

## File Structure

Files created:
- `docs/architecture/ios.md` — architecture decision record.
- `crates/pim-ios-ffi/Cargo.toml`
- `crates/pim-ios-ffi/src/lib.rs` — three C-ABI entry points + a test.
- `crates/pim-ios-ffi/include/pim_ios_ffi.h` — hand-written C header consumed by Swift.
- `scripts/build-ios.sh` — cross-compile Rust for the three iOS triples and produce `target/ios/PimCore.xcframework`.

Files modified:
- `Cargo.toml` (workspace root) — add `pim-ios-ffi` to `members` + `workspace.dependencies`.
- `crates/pim-tun/src/lib.rs` — add iOS branch to the `platform` module.
- `crates/pim-bluetooth/src/lib.rs` — gate Linux-only code so iOS builds a no-op stub.
- `crates/pim-wifidirect/src/lib.rs` — gate Linux-only code the same way.
- `crates/pim-wifidirect/src/wpa_cli.rs` — gate Linux-only code.
- `crates/pim-wifidirect/src/group.rs` — gate Linux-only code.
- `Makefile` — add `ios-check` target.
- `README.md` — add a short iOS section pointing at `docs/architecture/ios.md`.

Each library crate keeps one responsibility; the iOS gates are mechanical stubs that mirror the existing `#[cfg(not(any(target_os = "linux", target_os = "macos")))]` block in `pim-tun`.

---

## Task 1: Architecture decision document

**Files:**
- Create: `docs/architecture/ios.md`

- [ ] **Step 1: Write `docs/architecture/ios.md`**

```markdown
# iOS Target Architecture

## Status

Accepted — 2026-04-24. Resolves Milestone 1 of issue #70.

## Context

`pim-daemon` integrates with the host through Linux (`/dev/net/tun`, `iptables`,
`ip route`, `wpa_cli`, `bluetoothctl`) and macOS (`utunN` via `SYSPROTO_CONTROL`,
`ifconfig`, `route`). None of those surfaces exist on iOS:

- A user-space process cannot open `/dev/net/tun` or `SYSPROTO_CONTROL`. Packet
  IO is only available to a **Packet Tunnel Provider** extension, via
  `NEPacketTunnelFlow.readPackets` / `writePackets`.
- The extension process is sandboxed and memory-limited (≈ 50 MB on iOS 15+,
  and real-world reports show sub-15 MB failures on some iOS 17.x patch
  versions — we design for 15 MB).
- Routing is declared once via `NEPacketTunnelNetworkSettings.IPv4Settings`
  (`includedRoutes` / `excludedRoutes` / `mtu`); there is no runtime `ip route`
  equivalent.
- Wi-Fi Direct (IEEE 802.11 P2P) is not exposed on iOS; Bluetooth PAN is not
  exposed to third-party apps. Proximity transports must be rebuilt on
  `MultipeerConnectivity` or plain Bonjour + `Network.framework`.
- Gateway/NAT mode is explicitly a non-goal — `iptables` has no iOS analogue.

## Decision

PIM on iOS is split into three layers:

1. **Rust core (unchanged crates)** — `pim-core`, `pim-crypto`, `pim-protocol`,
   `pim-routing`, `pim-transport`, `pim-discovery`, `pim-gateway`. These are
   platform-independent and compile for `aarch64-apple-ios` with no changes
   beyond dependency audits.

2. **Rust platform glue** — `pim-tun`, `pim-bluetooth`, `pim-wifidirect`. Each
   gains an `#[cfg(target_os = "ios")]` branch that either returns
   `TunError::Unavailable`/equivalent or is a no-op stub. Host integration
   code (shell-outs to `ip`, `bluetoothctl`, `wpa_cli`) stays compiled out on
   iOS.

3. **Rust ↔ Swift bridge** — new crate `pim-ios-ffi` (crate-type `staticlib`,
   `cdylib`). It exposes a tiny opaque-handle C ABI:
   - `pim_ffi_version() -> *const c_char`
   - `pim_ffi_start(config_json: *const c_char, err: *mut *mut c_char) -> *mut PimHandle`
   - `pim_ffi_stop(handle: *mut PimHandle)`
   The Swift side (lands in Plan 2) consumes a hand-written header and links
   against the XCFramework produced by `scripts/build-ios.sh`.

## Build pipeline

`scripts/build-ios.sh`:

1. Confirms `xcode-select -p` and required `rustup` targets.
2. Runs `cargo build --release -p pim-ios-ffi --target aarch64-apple-ios`.
3. Runs the same for `aarch64-apple-ios-sim` and `x86_64-apple-ios`.
4. Uses `lipo -create` to fuse the two simulator static libs into one fat lib.
5. Runs `xcodebuild -create-xcframework` with the device lib + fused simulator
   lib + the `pim_ios_ffi.h` header, producing `target/ios/PimCore.xcframework`.

## Binary crates

`pim-daemon` and `pim-cli` stay Linux/macOS-only. We do not attempt to build
them for iOS — a Packet Tunnel extension has no `main()` and does not run a
daemon process in the traditional sense. Instead, Plan 2 introduces a
`pim-runtime` library refactor that moves the event loop out of `pim-daemon`'s
`main.rs` so the extension can drive it via `pim_ffi_start`.

## What this plan does not decide

- NEPacketTunnelFlow ↔ PacketIO bridging design (deferred to Plan 2).
- Whether relay mode is viable under the 50 MB memory cap (Plan 3 benchmarks).
- Whether discovery uses `MultipeerConnectivity`, Bonjour, or stays
  static-peer-only on iOS (Plan 4).
```

- [ ] **Step 2: Commit**

```bash
git add docs/architecture/ios.md
git commit -m "docs: record iOS target architecture decision (issue #70, milestone 1)"
```

---

## Task 2: Prove the iOS targets are installed

No file changes — this task gates the rest of the plan. If it fails, stop and tell the user.

- [ ] **Step 1: Verify toolchain**

Run:
```bash
xcode-select -p
rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios
rustup target list --installed | grep -E 'apple-ios'
```

Expected: the last command prints all three targets on separate lines. If any target is missing, the plan cannot proceed; report it to the user rather than working around it.

- [ ] **Step 2: Baseline iOS compile failures**

Run:
```bash
cargo check --target aarch64-apple-ios -p pim-core -p pim-crypto -p pim-protocol -p pim-routing -p pim-transport -p pim-discovery -p pim-gateway 2>&1 | tail -40
```

Expected: succeeds (these crates already have no OS-specific code). Note the command and output — it becomes the regression baseline for Task 10.

Run:
```bash
cargo check --target aarch64-apple-ios -p pim-tun -p pim-bluetooth -p pim-wifidirect 2>&1 | tail -40
```

Expected: **fails** with errors like `cannot find function do_ioctl` on `pim-tun`, shell-out references on the other two. Record the failing crates — Tasks 3–5 fix each one.

---

## Task 3: Gate pim-tun for iOS

**Files:**
- Modify: `crates/pim-tun/src/lib.rs:749-796`

`pim-tun` already has a `#[cfg(not(any(target_os = "linux", target_os = "macos")))]` stub module. We make it explicit for iOS so the `TunError::Unavailable` path is tested, and we add a test that the crate at least compiles on iOS.

- [ ] **Step 1: Write the failing test**

Append to the top-level `#[cfg(test)] mod tests` block at `crates/pim-tun/src/lib.rs`:

```rust
    #[cfg(target_os = "ios")]
    #[tokio::test]
    async fn ios_stub_reports_unavailable() {
        let err = TunInterface::create("utun0").unwrap_err();
        assert!(matches!(err, TunError::Unavailable));
    }
```

Add the `tokio` test-dependency feature. Modify `crates/pim-tun/Cargo.toml`:

```toml
[dev-dependencies]
tokio = { workspace = true, features = ["macros", "rt"] }
```

- [ ] **Step 2: Run test on Linux/macOS — it should be compiled out**

Run: `cargo test -p pim-tun --quiet`
Expected: existing tests pass; the new test is silently skipped because of the `cfg` gate (not yet running under iOS).

- [ ] **Step 3: Extend the fallback platform module to name iOS explicitly**

Change the existing line 749:
```rust
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
```
to:
```rust
#[cfg(any(target_os = "ios", not(any(target_os = "linux", target_os = "macos"))))]
```

This makes the stub explicitly cover iOS while still catching other unsupported platforms.

- [ ] **Step 4: Verify iOS compile**

Run: `cargo check --target aarch64-apple-ios -p pim-tun`
Expected: succeeds with no errors.

Run: `cargo check -p pim-tun` (native build sanity)
Expected: still succeeds.

- [ ] **Step 5: Commit**

```bash
git add crates/pim-tun/src/lib.rs crates/pim-tun/Cargo.toml
git commit -m "feat(pim-tun): compile iOS stub returning Unavailable (issue #70)"
```

---

## Task 4: Gate pim-bluetooth for iOS

**Files:**
- Modify: `crates/pim-bluetooth/src/lib.rs`

`pim-bluetooth` shells out to `bluetoothctl`, runs `bt-network`, and inspects `bnepN` interfaces via sysfs — none available on iOS. The real constructor signature is:

```rust
// crates/pim-bluetooth/src/lib.rs:78-86
pub fn new(
    config: BluetoothConfig,
    listen_port: u16,
    static_targets: Vec<SocketAddr>,
) -> Result<(Self, mpsc::Receiver<SocketAddr>), BluetoothError>
```

It already returns `Result`, so the iOS variant can simply return `Err(BluetoothError::Unavailable)` — we add that variant first. The crate's internal async helpers (`run`, `prepare_controller`, `discover_devices`, `pair_and_request_pan`, `interface_is_ready`, `discover_neighbor_targets`, `run_bt_network`) only run after a successful `new`, so the iOS build never reaches them — but they still need to compile, so we gate the Linux-only `impl` block.

- [ ] **Step 1: Add the `Unavailable` error variant**

Edit `crates/pim-bluetooth/src/lib.rs:38` (the `pub enum BluetoothError` block). Add, after the `CommandFailed` variant:

```rust
    /// Bluetooth is not supported on the host platform (e.g. iOS).
    #[error("Bluetooth is not available on this platform")]
    Unavailable,
```

- [ ] **Step 2: Write the failing iOS stub test**

Append to the end of `crates/pim-bluetooth/src/lib.rs` (after the existing tests):

```rust
#[cfg(all(test, target_os = "ios"))]
mod ios_stub_tests {
    use super::*;

    #[test]
    fn new_returns_unavailable_on_ios() {
        let cfg = pim_core::BluetoothConfig::default();
        let result = BluetoothDiscovery::new(cfg, 9100, Vec::new());
        match result {
            Err(BluetoothError::Unavailable) => {}
            _ => panic!("expected BluetoothError::Unavailable on iOS"),
        }
    }
}
```

- [ ] **Step 3: Split the `impl BluetoothDiscovery` block**

At `crates/pim-bluetooth/src/lib.rs:74`, change:

```rust
impl BluetoothDiscovery {
```

to:

```rust
#[cfg(not(target_os = "ios"))]
impl BluetoothDiscovery {
```

Add at the bottom of the file (before the `#[cfg(test)] mod tests` block):

```rust
#[cfg(target_os = "ios")]
impl BluetoothDiscovery {
    /// iOS stub — Bluetooth PAN is not available to third-party apps.
    pub fn new(
        _config: BluetoothConfig,
        _listen_port: u16,
        _static_targets: Vec<SocketAddr>,
    ) -> Result<(Self, mpsc::Receiver<SocketAddr>), BluetoothError> {
        Err(BluetoothError::Unavailable)
    }
}
```

- [ ] **Step 4: Gate the `DEFAULT_*` path constants and command helpers**

The constants `DEFAULT_SYSFS_ROOT`, `DEFAULT_IP_COMMAND`, `DEFAULT_BLUETOOTHCTL_COMMAND`, `DEFAULT_BT_NETWORK_COMMAND` (and any private free functions like `run_command`, `run_command_on` that shell out) must also be gated. Grep for them:

```bash
grep -nE '^(const DEFAULT_|fn run_command|fn parse_)' crates/pim-bluetooth/src/lib.rs
```

For each result, add `#[cfg(not(target_os = "ios"))]` immediately above the declaration. The iOS impl block added in Step 3 does not reference any of them.

- [ ] **Step 5: Verify**

```bash
cargo check --target aarch64-apple-ios -p pim-bluetooth
cargo test -p pim-bluetooth --quiet
```

Expected: first command finishes clean; second runs the existing tests (which are Linux/macOS conditional and may be skipped on macOS — do not regress the count).

- [ ] **Step 4: Verify**

Run: `cargo check --target aarch64-apple-ios -p pim-bluetooth`
Expected: succeeds.

Run: `cargo test -p pim-bluetooth --quiet`
Expected: native tests still pass; iOS stub test is gated out.

- [ ] **Step 6: Commit**

```bash
git add crates/pim-bluetooth/src/lib.rs
git commit -m "feat(pim-bluetooth): compile no-op iOS stub (issue #70)"
```

---

## Task 5: Gate pim-wifidirect for iOS

**Files:**
- Modify: `crates/pim-wifidirect/src/lib.rs`
- Modify: `crates/pim-wifidirect/src/wpa_cli.rs`
- Modify: `crates/pim-wifidirect/src/group.rs`

The relevant real signatures:

```rust
// crates/pim-wifidirect/src/lib.rs:68
pub fn new(config: WifiDirectConfig, listen_port: u16) -> (Self, mpsc::Receiver<SocketAddr>)

// crates/pim-wifidirect/src/lib.rs (later)
pub async fn run(self, cancel: CancellationToken)
```

`new` is infallible and `run` already logs + returns early when `wpa_cli` is missing. Wi-Fi Direct is not exposed to iOS apps at all, so on iOS the crate can keep the same public signatures but (a) have `run` return immediately and (b) stop the `wpa_cli`/`group` modules' external-command code from being compiled.

- [ ] **Step 1: Split `impl WifiDirectDiscovery` in `lib.rs`**

At `crates/pim-wifidirect/src/lib.rs:68` change `impl WifiDirectDiscovery {` to:

```rust
#[cfg(not(target_os = "ios"))]
impl WifiDirectDiscovery {
```

Add a parallel iOS-only impl at the bottom of `crates/pim-wifidirect/src/lib.rs` (before `#[cfg(test)] mod tests`):

```rust
#[cfg(target_os = "ios")]
impl WifiDirectDiscovery {
    /// iOS stub — Wi-Fi Direct is not available to third-party apps.
    pub fn new(
        config: WifiDirectConfig,
        listen_port: u16,
    ) -> (Self, tokio::sync::mpsc::Receiver<std::net::SocketAddr>) {
        let (peer_tx, peer_rx) = tokio::sync::mpsc::channel(1);
        let ctrl = crate::wpa_cli::WpaCliController::new(&config.interface);
        (
            Self { ctrl, config, listen_port, peer_tx },
            peer_rx,
        )
    }

    /// iOS stub — returns immediately; no Wi-Fi Direct is available.
    pub async fn run(self, _cancel: tokio_util::sync::CancellationToken) {
        tracing::debug!("Wi-Fi Direct unavailable on iOS — discovery is a no-op");
    }
}
```

Note: the iOS `new` still constructs a real `WpaCliController` because the struct field is typed. That is fine — the controller's `new` is infallible and does not actually talk to `wpa_supplicant` until a method is called. Confirm this in Step 2 before proceeding.

- [ ] **Step 2: Verify WpaCliController::new is side-effect-free**

```bash
sed -n '31,60p' crates/pim-wifidirect/src/wpa_cli.rs
```

Expected: `pub fn new(iface: &str) -> Self` that just constructs the struct — no `Command::new` at this point. If it does shell out, gate it the same way as Step 3 below.

- [ ] **Step 3: Gate the shell-out methods in `wpa_cli.rs`**

Every method on `WpaCliController` that calls `Command::new` (`p2p_find`, `p2p_peers`, `p2p_connect`, `p2p_stop_find`, `list_interfaces`, etc.) should be gated:

```bash
grep -nE 'pub (async )?fn' crates/pim-wifidirect/src/wpa_cli.rs
```

For each `impl` block in the file that contains shell-outs, prefix it:

```rust
#[cfg(not(target_os = "ios"))]
```

Leave `impl WpaCliController { pub fn new(iface: &str) -> Self { ... } }` available on all platforms — split it into two blocks if necessary so the constructor stays unconditional.

- [ ] **Step 4: Gate `group.rs` similarly**

```bash
grep -nE 'pub (async )?fn' crates/pim-wifidirect/src/group.rs
```

Methods on `WifiDirectGroup` that call `ip` / `ifconfig` / parse system state get the same `#[cfg(not(target_os = "ios"))]` treatment.

- [ ] **Step 5: Verify**

```bash
cargo check --target aarch64-apple-ios -p pim-wifidirect
cargo test -p pim-wifidirect --quiet
```

Expected: both green. Native test count unchanged.

- [ ] **Step 6: Commit**

```bash
git add crates/pim-wifidirect/
git commit -m "feat(pim-wifidirect): compile no-op iOS stub (issue #70)"
```

---

## Task 6: Confirm remaining library crates compile for iOS

No code changes expected. This task is a checkpoint that catches any cross-crate issue (missing dev-deps, incorrect `[target.'cfg(...)'.dependencies]`, etc.) before the FFI crate is introduced.

- [ ] **Step 1: Run the full iOS library build**

```bash
cargo check --target aarch64-apple-ios \
  -p pim-core -p pim-crypto -p pim-protocol -p pim-routing \
  -p pim-transport -p pim-discovery -p pim-gateway \
  -p pim-tun -p pim-bluetooth -p pim-wifidirect 2>&1 | tail -30
```

Expected: `Finished ...` with no errors.

- [ ] **Step 2: Same for the simulator target**

```bash
cargo check --target aarch64-apple-ios-sim \
  -p pim-core -p pim-crypto -p pim-protocol -p pim-routing \
  -p pim-transport -p pim-discovery -p pim-gateway \
  -p pim-tun -p pim-bluetooth -p pim-wifidirect 2>&1 | tail -30
```

Expected: `Finished ...`.

If a crate fails because one of its **dependencies** (e.g. `ed25519-dalek`, `nix`, `libc`) needs a feature gate, add the fix under its crate's `Cargo.toml` like:

```toml
[target.'cfg(target_os = "ios")'.dependencies]
# nothing yet — but this is where iOS-only deps would go
```

and re-run. Do not commit until both targets are green.

- [ ] **Step 3: Commit only if changes were required**

```bash
git add -p crates/
git commit -m "chore: make library crates iOS-clean (issue #70)" || echo "no changes needed"
```

---

## Task 7: Create pim-ios-ffi skeleton crate

**Files:**
- Create: `crates/pim-ios-ffi/Cargo.toml`
- Create: `crates/pim-ios-ffi/src/lib.rs`
- Create: `crates/pim-ios-ffi/include/pim_ios_ffi.h`
- Modify: `Cargo.toml` (workspace root, `members` + `workspace.dependencies`)

The crate exposes three C functions. `pim_ffi_start` and `pim_ffi_stop` are **stubs** in this plan — they allocate an opaque handle and free it. They do not yet drive the daemon; that is Plan 2.

- [ ] **Step 1: Add the crate to the workspace**

Edit `Cargo.toml` (workspace root). In the `members` array, after `"crates/pim-cli",`, add:

```toml
    "crates/pim-ios-ffi",
```

In the `[workspace.dependencies]` block, after `pim-wifidirect = { path = "crates/pim-wifidirect" }`, add:

```toml
pim-ios-ffi = { path = "crates/pim-ios-ffi" }
```

- [ ] **Step 2: Create `crates/pim-ios-ffi/Cargo.toml`**

```toml
[package]
name = "pim-ios-ffi"
version = "0.1.0"
edition = "2021"
publish = false

[lib]
name = "pim_ios_ffi"
# `staticlib` is what Xcode will link against inside the XCFramework; `rlib`
# lets the crate be depended on by other Rust code (tests, future runtime).
crate-type = ["staticlib", "rlib"]

[dependencies]
pim-core = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
thiserror = { workspace = true }
tracing = { workspace = true }

[dev-dependencies]
# (no dev deps yet — the handle-lifecycle test is sync)
```

- [ ] **Step 3: Write the failing test**

Create `crates/pim-ios-ffi/src/lib.rs`:

```rust
//! C ABI used by the iOS Packet Tunnel Provider extension to drive the PIM
//! core. Plan 1 ships stubs only — lifecycle is wired up in Plan 2.

use std::ffi::{c_char, CStr, CString};
use std::ptr;

/// Opaque handle returned by `pim_ffi_start`. Callers must treat the pointer
/// as a black box and pass it back to `pim_ffi_stop` exactly once.
pub struct PimHandle {
    // Placeholder — Plan 2 replaces this with a tokio runtime and a
    // DaemonState. Kept non-empty so the pointer identity is meaningful.
    _private: (),
}

/// Returns a NUL-terminated string owned by the library describing the
/// crate version. The caller must not free the returned pointer.
#[no_mangle]
pub extern "C" fn pim_ffi_version() -> *const c_char {
    // Safety: the CStr is a static literal — its pointer lives forever.
    const VERSION: &[u8] = concat!(env!("CARGO_PKG_VERSION"), "\0").as_bytes();
    VERSION.as_ptr() as *const c_char
}

/// Allocate and return a new handle. In Plan 1 this only validates the
/// config JSON; Plan 2 spawns the runtime.
///
/// # Safety
/// `config_json` must be a NUL-terminated UTF-8 C string.
/// If `err` is non-null and an error occurs, `*err` is set to a
/// library-owned NUL-terminated UTF-8 string that the caller must free
/// with `pim_ffi_free_string`.
#[no_mangle]
pub unsafe extern "C" fn pim_ffi_start(
    config_json: *const c_char,
    err: *mut *mut c_char,
) -> *mut PimHandle {
    if config_json.is_null() {
        set_error(err, "config_json is null");
        return ptr::null_mut();
    }
    let config_cstr = CStr::from_ptr(config_json);
    let config_str = match config_cstr.to_str() {
        Ok(s) => s,
        Err(_) => {
            set_error(err, "config_json is not valid UTF-8");
            return ptr::null_mut();
        }
    };
    // Validate shape: must parse as a JSON object. Plan 2 will feed this into
    // `pim_core::Config` (after adding a JSON adaptor).
    if let Err(e) = serde_json::from_str::<serde_json::Value>(config_str) {
        set_error(err, &format!("invalid config JSON: {e}"));
        return ptr::null_mut();
    }
    Box::into_raw(Box::new(PimHandle { _private: () }))
}

/// Release the handle returned by `pim_ffi_start`. Idempotent for null.
///
/// # Safety
/// `handle` must be either null or a pointer returned by `pim_ffi_start`
/// that has not yet been passed to `pim_ffi_stop`.
#[no_mangle]
pub unsafe extern "C" fn pim_ffi_stop(handle: *mut PimHandle) {
    if handle.is_null() {
        return;
    }
    drop(Box::from_raw(handle));
}

/// Free a string previously written to `*err` by `pim_ffi_start`.
///
/// # Safety
/// `s` must be either null or a pointer previously written by this library.
#[no_mangle]
pub unsafe extern "C" fn pim_ffi_free_string(s: *mut c_char) {
    if s.is_null() {
        return;
    }
    drop(CString::from_raw(s));
}

unsafe fn set_error(err: *mut *mut c_char, msg: &str) {
    if err.is_null() {
        return;
    }
    let c = match CString::new(msg) {
        Ok(c) => c,
        Err(_) => return,
    };
    *err = c.into_raw();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    #[test]
    fn version_is_non_empty() {
        let p = pim_ffi_version();
        assert!(!p.is_null());
        let s = unsafe { CStr::from_ptr(p) }.to_str().unwrap();
        assert!(!s.is_empty());
    }

    #[test]
    fn start_and_stop_round_trip() {
        let cfg = CString::new(r#"{"node":{"name":"test"}}"#).unwrap();
        let mut err: *mut c_char = ptr::null_mut();
        let handle = unsafe { pim_ffi_start(cfg.as_ptr(), &mut err as *mut _) };
        assert!(!handle.is_null(), "expected non-null handle, got err");
        assert!(err.is_null(), "did not expect an error string");
        unsafe { pim_ffi_stop(handle) };
    }

    #[test]
    fn start_rejects_invalid_json() {
        let cfg = CString::new("not valid json {{{").unwrap();
        let mut err: *mut c_char = ptr::null_mut();
        let handle = unsafe { pim_ffi_start(cfg.as_ptr(), &mut err as *mut _) };
        assert!(handle.is_null());
        assert!(!err.is_null());
        let msg = unsafe { CStr::from_ptr(err) }.to_str().unwrap().to_string();
        assert!(msg.contains("invalid config JSON"));
        unsafe { pim_ffi_free_string(err) };
    }

    #[test]
    fn stop_on_null_is_safe() {
        unsafe { pim_ffi_stop(ptr::null_mut()) };
    }
}
```

- [ ] **Step 4: Run the test — it should fail because the crate does not yet exist in the workspace graph**

Run: `cargo test -p pim-ios-ffi --quiet`
Expected: either "could not find package" (if workspace edit wasn't saved) or compile errors if this is the first build. Re-run after Steps 1–3 are saved.

- [ ] **Step 5: Make the test pass**

Run: `cargo test -p pim-ios-ffi --quiet`
Expected: 4 tests pass.

- [ ] **Step 6: Create the hand-written C header**

Create `crates/pim-ios-ffi/include/pim_ios_ffi.h`:

```c
#ifndef PIM_IOS_FFI_H
#define PIM_IOS_FFI_H

#ifdef __cplusplus
extern "C" {
#endif

/* Opaque handle — contents are private to the Rust side. */
typedef struct PimHandle PimHandle;

/* Returns a pointer to a library-owned NUL-terminated UTF-8 string.
 * Do not free. Lifetime is the lifetime of the library. */
const char *pim_ffi_version(void);

/* Start the PIM runtime with the given JSON config.
 * On success: returns a non-NULL PimHandle*.
 * On failure: returns NULL and, if `err_out` is non-NULL, writes a
 *             library-owned NUL-terminated UTF-8 string describing the
 *             error that the caller must release with
 *             pim_ffi_free_string(). */
PimHandle *pim_ffi_start(const char *config_json, char **err_out);

/* Stop and free a handle previously returned by pim_ffi_start.
 * Passing NULL is a no-op. Passing the same handle twice is undefined. */
void pim_ffi_stop(PimHandle *handle);

/* Free an error string produced by pim_ffi_start. NULL is a no-op. */
void pim_ffi_free_string(char *s);

#ifdef __cplusplus
}
#endif

#endif /* PIM_IOS_FFI_H */
```

- [ ] **Step 7: Verify iOS cross-compile**

```bash
cargo build --release --target aarch64-apple-ios -p pim-ios-ffi
cargo build --release --target aarch64-apple-ios-sim -p pim-ios-ffi
```

Expected: each produces `target/<triple>/release/libpim_ios_ffi.a`.

Confirm with:
```bash
ls -lh target/aarch64-apple-ios/release/libpim_ios_ffi.a
ls -lh target/aarch64-apple-ios-sim/release/libpim_ios_ffi.a
```

Expected: two files, each a few hundred KB to a few MB.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml crates/pim-ios-ffi/
git commit -m "feat(pim-ios-ffi): add C ABI stub for iOS extension (issue #70)"
```

---

## Task 8: Build the XCFramework

**Files:**
- Create: `scripts/build-ios.sh`

`xcodebuild -create-xcframework` refuses duplicate architectures, so the two simulator triples must be fused with `lipo` before being fed in.

- [ ] **Step 1: Write a verifiable script**

Create `scripts/build-ios.sh`:

```bash
#!/usr/bin/env bash
# Build PimCore.xcframework from pim-ios-ffi for iOS device + simulator.
# Resolves milestone 1 of issue #70.

set -euo pipefail

cd "$(dirname "$0")/.."

PKG="pim-ios-ffi"
LIB="libpim_ios_ffi.a"
PROFILE="release"
OUT_DIR="target/ios"
FRAMEWORK="${OUT_DIR}/PimCore.xcframework"
HEADERS="crates/${PKG}/include"

DEVICE_TRIPLE="aarch64-apple-ios"
SIM_ARM_TRIPLE="aarch64-apple-ios-sim"
SIM_X86_TRIPLE="x86_64-apple-ios"

require_tool() {
  command -v "$1" >/dev/null 2>&1 \
    || { echo "error: missing tool '$1'" >&2; exit 1; }
}

require_tool cargo
require_tool lipo
require_tool xcodebuild

for triple in "$DEVICE_TRIPLE" "$SIM_ARM_TRIPLE" "$SIM_X86_TRIPLE"; do
  if ! rustup target list --installed | grep -q "^${triple}$"; then
    echo "error: rustup target ${triple} is not installed. Run:" >&2
    echo "  rustup target add ${triple}" >&2
    exit 1
  fi
done

echo "==> building ${PKG} for ${DEVICE_TRIPLE}"
cargo build --release -p "${PKG}" --target "${DEVICE_TRIPLE}"

echo "==> building ${PKG} for ${SIM_ARM_TRIPLE}"
cargo build --release -p "${PKG}" --target "${SIM_ARM_TRIPLE}"

echo "==> building ${PKG} for ${SIM_X86_TRIPLE}"
cargo build --release -p "${PKG}" --target "${SIM_X86_TRIPLE}"

SIM_FAT_DIR="${OUT_DIR}/sim-fat"
mkdir -p "${SIM_FAT_DIR}"
echo "==> fusing simulator arches with lipo"
lipo -create \
  "target/${SIM_ARM_TRIPLE}/${PROFILE}/${LIB}" \
  "target/${SIM_X86_TRIPLE}/${PROFILE}/${LIB}" \
  -output "${SIM_FAT_DIR}/${LIB}"

echo "==> assembling XCFramework"
rm -rf "${FRAMEWORK}"
xcodebuild -create-xcframework \
  -library "target/${DEVICE_TRIPLE}/${PROFILE}/${LIB}" -headers "${HEADERS}" \
  -library "${SIM_FAT_DIR}/${LIB}"                    -headers "${HEADERS}" \
  -output "${FRAMEWORK}"

echo "==> done: ${FRAMEWORK}"
```

- [ ] **Step 2: Static-check the script syntax**

```bash
bash -n scripts/build-ios.sh
```

Expected: exits 0 with no output.

- [ ] **Step 3: Mark it executable and run end-to-end**

```bash
chmod +x scripts/build-ios.sh
./scripts/build-ios.sh
```

Expected: prints each `==>` line and finishes with `==> done: target/ios/PimCore.xcframework`. Then:

```bash
ls target/ios/PimCore.xcframework
```

Expected: shows `Info.plist`, `ios-arm64`, and `ios-arm64_x86_64-simulator` directories.

- [ ] **Step 4: Exclude build artefacts from git**

Append to `.gitignore` if the entry is not already present:

```
target/ios/
```

- [ ] **Step 5: Commit**

```bash
git add scripts/build-ios.sh .gitignore
git commit -m "build: script to assemble PimCore.xcframework for iOS (issue #70)"
```

---

## Task 9: Make target for iOS smoke-check

**Files:**
- Modify: `Makefile`

- [ ] **Step 1: Add the target**

Append to `Makefile` (after the last `.PHONY` line):

```make
# ── iOS ───────────────────────────────────────────────────────────────────────

.PHONY: ios-check ios-xcframework

IOS_TARGETS := aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios
IOS_CRATES  := pim-core pim-crypto pim-protocol pim-routing pim-transport \
               pim-discovery pim-gateway pim-tun pim-bluetooth pim-wifidirect \
               pim-ios-ffi

ios-check:
	@for t in $(IOS_TARGETS); do \
	  echo "==> cargo check --target $$t"; \
	  cargo check --target $$t $(addprefix -p ,$(IOS_CRATES)) || exit 1; \
	done

ios-xcframework:
	bash scripts/build-ios.sh
```

- [ ] **Step 2: Run it**

```bash
make ios-check
```

Expected: three `==> cargo check --target ...` banners, each followed by a `Finished` line from cargo. Exit code 0.

- [ ] **Step 3: Commit**

```bash
git add Makefile
git commit -m "build: add make ios-check and make ios-xcframework (issue #70)"
```

---

## Task 10: README pointer

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Add an iOS subsection under an existing section**

Locate the `## Current Scope` section and, immediately below it, add:

```markdown
## iOS Support (in progress)

iOS support is tracked in [issue #70](https://github.com/Astervia/proximity-internet-mesh/issues/70).
Milestone 1 (target architecture + cross-compile scaffolding) is resolved —
see [docs/architecture/ios.md](docs/architecture/ios.md). Build locally with:

```bash
rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios
make ios-check          # cross-compile sanity check
make ios-xcframework    # produces target/ios/PimCore.xcframework
```

The Xcode app and packet-tunnel extension that consume `PimCore.xcframework`
land in follow-up plans.
```

- [ ] **Step 2: Verify the README still renders**

```bash
head -40 README.md
grep -n "iOS Support" README.md
```

Expected: the new section appears; the document is still a valid markdown file.

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "docs(readme): point at iOS foundation (issue #70, milestone 1)"
```

---

## Task 11: Final end-to-end validation

No file changes. This task is the acceptance test for the plan.

- [ ] **Step 1: Clean workspace + full build**

```bash
cargo clean
cargo test --workspace --quiet
```

Expected: all existing tests still pass, including the four new `pim-ios-ffi` tests. Nothing broken on Linux/macOS.

- [ ] **Step 2: iOS cross-compile smoke**

```bash
make ios-check
```

Expected: three targets × all library crates check cleanly.

- [ ] **Step 3: XCFramework build**

```bash
make ios-xcframework
file target/ios/PimCore.xcframework/ios-arm64/libpim_ios_ffi.a
file target/ios/PimCore.xcframework/ios-arm64_x86_64-simulator/libpim_ios_ffi.a
```

Expected: the first is `current ar archive random library` for arm64, the second is a fat archive with arm64 + x86_64 slices.

- [ ] **Step 4: Acceptance sign-off**

The plan is complete when:
1. `cargo test --workspace` is green on the host.
2. `make ios-check` is green.
3. `make ios-xcframework` produces `target/ios/PimCore.xcframework` with both slices.
4. `docs/architecture/ios.md` exists.
5. No `pim-daemon` or `pim-cli` regression (those binaries still build on Linux + macOS).

Open the PR referencing issue #70 and mark **Milestone 1 only** — list milestones 2–6 as open for follow-up plans.

---

## Follow-up plans (do not include in Plan 1)

The issue is an umbrella covering six milestones. Plan 1 resolves Milestone 1
and unblocks the rest. Each of these should be written and executed as its
own plan, in order:

- **Plan 2 — Milestones 2 + 3 + 4: client-mode dataplane.** Introduce a
  `PacketIO` trait in `pim-tun` (async `read_packet` / `write_packet`), refactor
  `pim-daemon` into a `pim-runtime` library + thin binary, extend
  `pim-ios-ffi` with `read/write` callbacks that the Swift extension registers,
  add the Xcode app + `NEPacketTunnelProvider` extension target,
  `NEPacketTunnelNetworkSettings` with `includedRoutes`, and a one-hop e2e
  integration test routing `curl` through a test relay.
- **Plan 3 — Milestone 5: relay-mode dataplane.** Add an accept loop inside
  the extension (documenting iOS backgrounding constraints), benchmark
  memory against the 50 MB cap, and ship a relay-mode test lab.
- **Plan 4 — Milestone 6: discovery + integrations + test coverage.** Add
  a `MultipeerConnectivity` or Bonjour-backed discovery adapter feature-gated
  for iOS, an iOS-focused integration test harness, and CI coverage for
  `make ios-xcframework`.
