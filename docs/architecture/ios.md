# iOS Target Architecture

## Status

Accepted — 2026-04-24. Resolves Milestone 1 of [issue #70][issue-70].

[issue-70]: https://github.com/Astervia/proximity-internet-mesh/issues/70

## Context

`pim-daemon` integrates with the host through Linux (`/dev/net/tun`, `iptables`,
`ip route`, `wpa_cli`, `bluetoothctl`) and macOS (`utunN` via `SYSPROTO_CONTROL`,
`ifconfig`, `route`). None of those surfaces exist on iOS:

- A user-space process cannot open `/dev/net/tun` or `SYSPROTO_CONTROL`. Packet
  IO is only available to a **Packet Tunnel Provider** extension, via
  `NEPacketTunnelFlow.readPackets` / `writePackets`.
- The extension process is sandboxed and memory-limited (≈ 50 MB on iOS 15+,
  with real-world reports of sub-15 MB caps on some iOS 17.x patch versions —
  the dataplane must be designed for 15 MB).
- Routing is declared once via `NEPacketTunnelNetworkSettings.IPv4Settings`
  (`includedRoutes` / `excludedRoutes` / `mtu`); there is no runtime
  `ip route` equivalent.
- Wi-Fi Direct (IEEE 802.11 P2P) is not exposed on iOS. Bluetooth PAN is not
  exposed to third-party apps either. Proximity transports have to be rebuilt
  on `MultipeerConnectivity` or Bonjour + `Network.framework`.
- Gateway/NAT mode is explicitly a non-goal of issue #70 — `iptables` has no
  iOS analogue and the sandbox forbids raw sockets.

## Decision

PIM on iOS is split into three layers:

1. **Platform-independent core (unchanged crates)** — `pim-core`,
   `pim-crypto`, `pim-protocol`, `pim-routing`, `pim-transport`,
   `pim-discovery`, `pim-gateway`. These crates already compile for
   `aarch64-apple-ios` with no source changes.

2. **Rust platform glue** — `pim-tun`, `pim-bluetooth`, `pim-wifidirect`.
   Each exposes an iOS branch that either returns
   `TunError::Unavailable` / `BluetoothError::Unavailable` /
   `WifiDirectError::Unavailable` or is a compile-time no-op. Host
   integration code (shell-outs to `ip`, `bluetoothctl`, `wpa_cli`) stays
   compiled out on iOS so the sandbox never sees it.

3. **Rust ↔ Swift bridge** — new crate `pim-ios-ffi` (`crate-type =
   ["staticlib", "rlib"]`). It exposes a tiny opaque-handle C ABI:
   - `pim_ffi_version() -> *const c_char`
   - `pim_ffi_start(config_json, err_out) -> *mut PimHandle`
   - `pim_ffi_stop(handle)`
   - `pim_ffi_free_string(s)`

   The Swift side (lands in a follow-up plan) consumes a hand-written header
   and links against the XCFramework produced by
   [`scripts/build-ios.sh`](../../scripts/build-ios.sh).

## Build pipeline

`scripts/build-ios.sh`:

1. Confirms `xcode-select -p` and required `rustup` targets
   (`aarch64-apple-ios`, `aarch64-apple-ios-sim`, `x86_64-apple-ios`).
2. Runs `cargo build --release -p pim-ios-ffi --target <triple>` for each
   of the three iOS targets.
3. Uses `lipo -create` to fuse the two simulator static libs into one
   multi-arch simulator lib.
4. Runs `xcodebuild -create-xcframework` with the device lib + fused
   simulator lib + the `pim_ios_ffi.h` header, producing
   `target/ios/PimCore.xcframework`.

The Makefile target `make ios-check` runs `cargo check` against all library
crates for each iOS triple, and `make ios-xcframework` wraps the build
script.

## Binary crates

`pim-daemon` and `pim-cli` stay Linux/macOS-only. They are deliberately **not**
cross-compiled for iOS:

- A Packet Tunnel extension has no `main()` and does not run a daemon process
  in the traditional sense.
- Getting the daemon to link cleanly on iOS would require a `pim-runtime`
  library refactor that belongs in a follow-up plan (see below).

## What this plan does not decide

- `NEPacketTunnelFlow` ↔ `PacketIO` bridging design (deferred to Plan 2).
- Whether relay mode is viable under the 50 MB memory cap
  (Plan 3 benchmarks).
- Whether discovery uses `MultipeerConnectivity`, Bonjour, or stays
  static-peer-only on iOS (Plan 4).

## Follow-up plans

- **Plan 2 (Milestones 2 + 3 + 4):** client-mode dataplane. Introduce a
  `PacketIO` trait in `pim-tun`, refactor `pim-daemon` into a `pim-runtime`
  library + thin binary, extend `pim-ios-ffi` with read/write callbacks that
  the Swift extension registers, add the Xcode app + `NEPacketTunnelProvider`
  extension target, configure `NEPacketTunnelNetworkSettings` with
  `includedRoutes`, and land a one-hop end-to-end integration test routing
  `curl` through a test relay.
- **Plan 3 (Milestone 5):** relay-mode dataplane. Add an accept loop inside
  the extension (documenting iOS backgrounding constraints), benchmark
  memory against the 50 MB cap, and ship a relay-mode test lab.
- **Plan 4 (Milestone 6):** discovery + integrations + test coverage.
  Add a `MultipeerConnectivity` or Bonjour-backed discovery adapter,
  feature-gated for iOS, plus CI coverage for `make ios-xcframework`.
