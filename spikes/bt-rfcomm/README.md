# spikes/bt-rfcomm — RFCOMM auto-discovery spike

> **Status:** functional spike, 2026-04-30. Proves Mac (Tahoe 26.4)
> auto-discovers Linux (Ubuntu 24.04) and vice-versa over Bluetooth
> Classic RFCOMM, exchanging identity via a JSON handshake.
>
> **Why this exists:** Phase 7 originally targeted L2CAP CoC over BLE.
> Empirical testing showed BT-PAN is dead in macOS Tahoe (Apple removed
> client-side too — Mac sees Linux only as audio device, no PAN service).
> RFCOMM/SPP over BR/EDR Classic, however, works perfectly: bytes flow
> in both directions on the existing BR/EDR pairing. This spike is the
> empirical foundation for replacing the L2CAP-CoC plan with an RFCOMM
> path.

## What's here

| File | Role |
|---|---|
| [`PROTOCOL.md`](PROTOCOL.md) | Wire format, handshake spec, discovery mechanics |
| [`mac/pim-bt-rfcomm-mac.swift`](mac/pim-bt-rfcomm-mac.swift) | Mac sidecar (Swift + IOBluetooth). Polls paired devices, opens RFCOMM, accepts inbound |
| [`linux/pim-bt-rfcomm-linux.py`](linux/pim-bt-rfcomm-linux.py) | Linux daemon (Python 3 stdlib). RFCOMM listener + outbound discovery |

Both speak the same wire protocol (4-byte BE length-prefix + UTF-8 JSON
payload) and emit newline-delimited JSON events on stdout. Output stream
is suitable to be piped into `pim-daemon` IPC consumer in a follow-up.

## Run

### Linux side

```bash
sudo apt install bluez bluez-tools                          # if not already
sudo sdptool add --channel=22 SP                             # one-time per boot
sudo hciconfig hci0 piscan                                  # discoverable + scannable
sudo python3 spikes/bt-rfcomm/linux/pim-bt-rfcomm-linux.py \
    --name=PIM-gateway \
    --gateway
```

If `sdptool` complains about not finding the SDP server, ensure
`bluetoothd` is running with `--compat`:

```bash
sudo sed -i 's|^ExecStart=.*bluetoothd$|& --compat|' \
    /lib/systemd/system/bluetooth.service
sudo systemctl daemon-reload
sudo systemctl restart bluetooth
```

### Mac side

```bash
cd spikes/bt-rfcomm/mac
swiftc -framework IOBluetooth -O -o pim-bt-rfcomm-mac pim-bt-rfcomm-mac.swift
./pim-bt-rfcomm-mac --name=PIM-pepe --poll=10
```

Mac requires that the parent terminal app has Bluetooth permission
granted in System Settings → Privacy & Security → Bluetooth. The first
launch may trigger a TCC prompt against the terminal.

## Prerequisites for both sides

- BR/EDR pairing already done via `bluetoothctl pair` (Linux) and System
  Settings → Bluetooth (Mac). The two devices must be paired before this
  spike can establish RFCOMM channels.
- Both peer names match the prefix filter (default `PIM-`).
- The Linux RFCOMM channel is `1` (SPP convention). Mac connects to the
  same channel.

## Expected output

When both sides run, each emits a `discovered` event when the handshake
completes. Sample:

```json
{"event":"boot","name":"PIM-pepe","node_id":"...","prefix":"PIM-",...}
{"event":"scan_attempt","bd_addr":"00-15-83-3D-0A-57","name":"PIM-gateway","channel":1}
{"event":"discovered","peer":{"bd_addr":"00-15-83-3D-0A-57","caps":["mesh-v1","gateway-v1"],"name":"PIM-gateway","node_id":"...","platform":"linux","since":"2026-04-30T..."}}
```

`lost` events fire on disconnect. `peer_error` for protocol errors.

## Limitations of this spike

- **Identity is randomised per-process** unless `--node-id` is passed
  explicitly. Production integration must read NodeId from
  `pim-core::Identity`.
- **No retry on `open_failed`**. The Mac sidecar logs failures but does
  not back off; production should add exponential backoff per peer.
- **Single RFCOMM channel (1)**. If the SPP channel is already claimed
  by another app, conflicts. Production may use SDP discovery to pick a
  free channel.
- **Frame payload is JSON only**. Production transport will tunnel
  `pim-protocol::TransportFrame` bytes inside the same length-prefixed
  framer (handshake stays JSON, post-handshake bytes are opaque).
- **No encryption beyond what BR/EDR already provides** (Bluetooth link
  encryption from pairing). The PIM crypto layer
  (`pim-crypto::Handshake`) runs on top in production.
- **Linux uses `bluetoothctl devices Paired`** for outbound discovery,
  which depends on BlueZ ≥ 5.62. Older BlueZ versions need
  `bluetoothctl paired-devices` instead.

## Migration path to `pim-bluetooth` crate

The Linux Python should be replaced by a `pim-bluetooth::rfcomm` Rust
module that:

- Opens the listening socket via `nix::sys::socket` with `AF_BLUETOOTH`
  + `SOCK_STREAM` + `BTPROTO_RFCOMM` (libc constants 31, 1, 3 respectively
  on Linux), or via the `bluer` crate if it gains Classic RFCOMM support.
- Spawns a tokio task per connection, identical in shape to the Python
  acceptor loop.
- Hooks into `pim-discovery::PeerTable` so RFCOMM-discovered peers
  surface in the same UI surfaces as TCP/BNEP peers.

The Mac Swift binary stays as a Tauri sidecar (`tools/pim-bt-rfcomm-mac/`)
because IOBluetooth is the only public macOS path for BR/EDR Classic
sockets. It will gain a Unix-socket IPC channel to `pim-daemon` (instead
of stdout-only events).

A new `Transport::BluetoothRfcomm` variant in
`pim-protocol::TransportKind` will replace the planned `BluetoothCoc`
variant, since RFCOMM is what actually works on macOS Tahoe.

## Why RFCOMM over L2CAP CoC

| Property | RFCOMM (this spike) | L2CAP CoC over BLE (original Phase 7 plan) |
|---|---|---|
| Works on macOS Tahoe | ✓ confirmed empirically | unknown — needs separate spike |
| Re-pairing required | no — uses existing BR/EDR | yes — needs LE pairing |
| Throughput | ~700 kbps (BR/EDR 2.1 EDR) | ~100 KB/s (LE 2M PHY) |
| API age / stability | IOBluetooth since 10.2 | CBL2CAPChannel since iOS 11 |
| Linux interop | `socket(AF_BLUETOOTH, ..., BTPROTO_RFCOMM)` | `bluer::l2cap::Stream` (LE) |
| MTU | 667 bytes negotiated | up to ~1024 bytes |
| Profile registration | SPP via `sdptool add SP` | dynamic PSM in advertising |
| Pairing UI | macOS System Settings → Bluetooth | needs new pairing flow |

RFCOMM wins on every practical axis: reuses existing pairing, higher
throughput, simpler stack, no new pairing UI. The only drawback is that
macOS deprecates IOBluetooth in spirit (not formally) — but it remains
the only public Classic API and is unlikely to disappear soon.
