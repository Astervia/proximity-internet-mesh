# macOS Bluetooth: state of the world (2026)

**Status:** research, not yet implemented
**Audience:** future agents and contributors picking up the macOS
Bluetooth integration. **Read this before doing more research** — most
of the questions you might ask have been answered here, with citations.
**Read this before writing more code** — there is a known-dead-end
implementation path that this doc explicitly maps out.

---

## TL;DR

1. **Apple removed Bluetooth PAN from macOS in Monterey (12.0, late 2021)
   and never restored it.** Confirmed via Apple Senior Advisor responses,
   reverse-engineering of `bluetoothd`, and live verification on macOS
   26.4 (Tahoe).
2. **No public, private, or reverse-engineerable verb exists in modern
   macOS to make a Mac act as a Bluetooth PAN client (PANU).** All ten
   plausible workarounds were investigated; every one is dead.
3. **The daemon's current macOS Bluetooth path is unreachable** — it
   shells out to `bluetoothctl` (BlueZ, Linux-only) which fails on
   power-on at startup, and even if patched, the BNEP-to-network-
   interface plumbing it depends on does not exist in macOS user-space.
4. **The recommended replacement is "L2CAP as transport":** pair via
   `IOBluetoothDevicePair`, open an L2CAP channel on a custom PSM, run
   the daemon's existing transport protocol over that channel, route
   IP through the existing `utun` device. This sidesteps PAN, BNEP,
   bridge0, and Network Extension entitlements entirely.
5. **There is an existing `feature/macOS-bluetooth` branch on origin**
   with WIP implementation — review it before continuing to ensure it
   isn't going down a doomed path (the obvious-looking PAN approach).

---

## 1. The state of macOS Bluetooth PAN

### What Apple shipped historically

Pre-Monterey (i.e. Big Sur 11 and earlier), macOS supported Bluetooth
Personal Area Networking via two surfaces:

- **PANU client**: System Preferences → Network → "+" → Bluetooth PAN.
  Created a `Bluetooth PAN` network service that bound BNEP-over-L2CAP
  to a kernel network interface (typically `en7`).
- **NAP server**: System Preferences → Sharing → Internet Sharing →
  "To computers using: Bluetooth PAN". Created a `bridge0` interface,
  ran BNEP-NAP, served DHCP to clients.

The plumbing lived in `bluetoothd` (Apple's user-space Bluetooth
daemon, licensed Bluetopia stack from MindTree / CSR) and a
`BluetoothPAN` SystemConfiguration agent, with a private kernel
interface for instantiating the network device.

### What Apple removed in Monterey 12.0

- The `Bluetooth PAN` entry in System Preferences → Network: **gone**.
- The `Bluetooth PAN` option in Internet Sharing: **gone**.
- The `BluetoothPAN` SystemConfiguration agent: **removed**.
- The "Connect to Network" submenu under paired devices in the
  Bluetooth menu bar: **gone**.

`bluetoothd` itself **still has BNEP/PANU compiled in** (verified on
macOS 26.4 via `strings`: `BNEP`, `PANU`, `OI_BNEP_*`,
`OI_PAN_USER_IFCREATE_ERROR`, `OI_PAN_DEVICE_CONNECTED`, etc.) but
exposes no XPC contract for a userspace client to start a PANU role
and instantiate a network interface. The protocol code is dead weight,
likely retained because the same daemon image ships across iOS / macOS
/ visionOS and BNEP is exercised on iOS for adjacent flows.

### Citations

- Apple Senior Advisor confirming removal, 17 Dec 2021 — [discussions.apple.com/thread/253312557](https://discussions.apple.com/thread/253312557)
- Same removal still in effect on Ventura (13) — [discussions.apple.com/thread/254918402](https://discussions.apple.com/thread/254918402)
- Same on Sequoia (15) — [discussions.apple.com/thread/255822875](https://discussions.apple.com/thread/255822875)
- The Register coverage of the workaround landscape, Feb 2024 — [theregister.com/2024/02/07/horndis_android_mac/](https://www.theregister.com/2024/02/07/horndis_android_mac/)

---

## 2. Workarounds investigated — all dead

Each of these was independently verified against current macOS and
recent (2023–2026) reverse-engineering / community findings.

### 2.1 Menu-bar "Connect to Network" submenu — dead

Removed in Monterey 12.0. No `bridge100` / `en7` is created because the
menu item itself doesn't exist. `osascript` / GUI-scripting / Accessibility
APIs cannot click something that isn't there.

### 2.2 `BluetoothPAN.framework` private framework — never existed

There has never been a top-level framework with that exact name on any
shipped macOS. Verified via `/System/Library/PrivateFrameworks` listing
on macOS 26.4. The PAN logic historically lived inside `IOBluetoothUI`
and the (now-removed) Bluetooth pref pane.

### 2.3 Private `IOBluetoothDevice` methods — none exist for PAN

Class-dump of `IOBluetooth 7.0.0d93` (current era, via
[w0lfschild/macOS_headers](https://github.com/w0lfschild/macOS_headers/blob/master/macOS/Frameworks/IOBluetooth/7.0.0d93/IOBluetoothDevice.h))
shows **no** `openPANServiceConnection`, `requestNetworkAccess`, or
`openConnection:withPanID:`. Connection APIs are L2CAP-PSM-based
(`openL2CAPChannelSync:withPSM:`) and RFCOMM-based.

### 2.4 `networksetup` / `scutil` — no Bluetooth port to bind

`networksetup -listallhardwareports` on Monterey+ does not list a
Bluetooth port. `networksetup -createnetworkservice "Bluetooth PAN"
Bluetooth` only works if a hardware port named `Bluetooth` exists,
which it does not. Verified locally.

### 2.5 `bluetoothd` XPC verbs — BNEP linked but unbridged

`bluetoothd` exposes `com.apple.bluetoothd`,
`com.apple.bluetoothd-central`, `com.apple.bluetooth.xpc`. Strings dump
shows core daemon plumbing for advertisers, GATT, classic pairing,
audio routing — but **no PAN-connect / NAP-client message names**. The
internal Bluetopia BNEP/PANU symbols exist but no XPC contract asks
the daemon to start a PANU role and instantiate a kernel network
interface. No public reverse-engineering project (HackTricks,
[Hacking IOBluetooth by colemancda](https://colemancda.github.io/2018/03/25/Hacking-IOBluetooth),
InternalBlue, jiska/Frankenstein) has documented a PAN verb
post-Monterey.

### 2.6 Reading existing `bridge0` after Internet Sharing toggle — toggle is gone

Pre-Monterey, Internet Sharing → Bluetooth PAN created `bridge0` running
BNEP-NAP. **That toggle was removed in Monterey** alongside the PAN
client UI, and remains absent on Ventura/Sonoma/Sequoia/Tahoe. Apple
Community thread 255822875 (Oct 2024, macOS 15.1) confirms users see
a yellow-warning "service is currently unavailable" on Bluetooth
Sharing entries. Even if you saw `bridge0` on a Mac, it would be the
**Thunderbolt Bridge**, not Bluetooth.

### 2.7 Third-party kexts / System Extensions — none exist for PAN client

After exhaustive search of GitHub, App Store, MacUpdate, and commercial
vendors: **no DriverKit, kext, System Extension, or NetworkExtension
Provider implements Bluetooth PAN client (PANU) on Sonoma/Sequoia.**

- **EasyTether** ([mobile-stream.com](http://www.mobile-stream.com/easytether/drivers.html))
  installs `EasyTetherUSBEthernet.kext`. Despite "Bluetooth" branding,
  the Mac driver creates a **virtual Ethernet interface tunneled over
  RFCOMM** (proprietary protocol) — not standard BNEP/PAN. Also requires
  Reduced Security boot on Apple Silicon Sonoma+.
- **HoRNDIS** ([github.com/jwise/HoRNDIS](https://github.com/jwise/HoRNDIS))
  is USB-RNDIS only, not Bluetooth — explicitly cited as "the workaround
  *because* PAN is gone."

### 2.8 Reverse-tether tools — all abandoned Bluetooth on modern macOS

Speedify's KB article on "Android Bluetooth tethering on Mac" is
explicitly stale; they direct users to USB or Wi-Fi for Sonoma+. No app
in 2024–2026 achieves Android phone hotspot → Mac via Bluetooth on
Sonoma/Sequoia. All commercial reverse-tether apps moved to USB-RNDIS,
Wi-Fi hotspot, or proprietary RFCOMM tunnels (EasyTether-style).

### 2.9 AppleScript / Automator / Shortcuts — nothing to invoke

No "Connect to Network" entry in the Bluetooth menu in Monterey+, so
System Events GUI scripting can't target it. Shortcuts.app ships
`Bluetooth On/Off` and `Set Wi-Fi`, no PAN action. AppleScript
dictionaries for `Bluetooth File Exchange`, `System Events`, and the
System Settings extensions expose no PAN verb.

### 2.10 BNEP kernel extension — never shipped

Apple **never** shipped a standalone `IOBluetoothBNEP.kext` /
`AppleBNEP.kext`. Verified `kextstat | grep -i bnep` on macOS 26.4 —
none loaded. Historic Big Sur kextstat dumps don't list one either.
BNEP was always implemented in user space inside `bluetoothd`,
bridging to the kernel via a private interface owned by the daemon and
the (now-removed) PrefPane.

---

## 3. The current daemon's macOS path is also dead

`crates/pim-bluetooth/src/platform_impl.rs` on macOS:

- **`prepare_controller`** calls `bluetoothctl --power 1`. `bluetoothctl`
  is BlueZ — Linux-only. On macOS the binary doesn't exist; the watcher
  fails immediately with `bluetoothctl failed: Failed to switch
  bluetooth power on in 10 seconds` every ~16s.
- **`discover_devices`** also shells to `bluetoothctl --inquiry`, same
  failure.
- **`pair_and_request_pan`** shells to `bluetoothctl --pair` /
  `--connect`, same failure.
- **`resolve_pan_interfaces`** runs `ifconfig bridge0` and assumes a
  pre-existing Bluetooth bridge — but that bridge can only exist if the
  user manually enabled Internet Sharing → Bluetooth, **which is the
  toggle Apple removed**. So even with the BlueZ calls patched out,
  this code path can never observe the network interface it expects.

**Both ends of the macOS pipeline are broken**, and patching either in
isolation accomplishes nothing. The `bluetoothctl` calls are at least
honest about being broken (loud error log). The `bridge0` ARP-table
read fails silently and the watcher just sits forever waiting for an
interface that will never appear.

In `pim-ui` we have already defaulted `[bluetooth].enabled = false` on
macOS in the generated config (see `src-tauri/src/daemon/default_config.rs`
in pim-ui#main) to silence the loop. That is a **cover-up, not a fix.**

---

## 4. Recommended path: L2CAP as transport

Stop trying to reanimate Bluetooth PAN on macOS. The PAN profile was a
bridge between Bluetooth and the OS network stack; on macOS that
bridge no longer exists. **Build a new bridge** that uses what macOS
*does* expose: pairing + L2CAP channels.

### 4.1 Architecture

```
ANTES (Linux-only, broken on macOS):
  BT inquiry → pair → bt-network → BNEP iface → DHCP → IP → TCP transport
                                   └─── doesn't exist on macOS ────┘

DEPOIS (cross-platform):
  BT inquiry → pair (auto-confirm) → L2CAP channel(PSM 0x1011) → pim frames direct
```

The daemon already has a TCP transport that runs after a peer's IP
becomes known via PAN. Replace the "PAN provides IP" step on macOS
with "L2CAP provides a stream" — and run the existing pim transport
framing **directly over L2CAP**, no IP-over-Bluetooth needed. IP frames
the daemon mints itself flow through the existing `utun` device.

### 4.2 Component breakdown

#### Inside `proximity-internet-mesh` (this repo)

- **`crates/pim-bluetooth/src/macos/`** — new module replacing the
  existing macOS stubs in `platform_impl.rs`. Sub-modules:
  - `inquiry.rs` — `IOBluetoothDeviceInquiry` filtering by name prefix
    (`PIM-*`) or, ideally, by an SDP service UUID we register on the
    Linux side
  - `pair.rs` — `IOBluetoothDevicePair` with a delegate that
    auto-replies `replyUserConfirmation:YES` to silence numeric-comparison
    prompts. Pairing is **silent** if the Linux peer registers an
    SSP agent with `NoInputNoOutput` capability — see §6.
  - `l2cap.rs` — `openL2CAPChannelAsync:withPSM:` on a fixed PSM
    (proposal: `0x1011`; **NOT** `0x000F` which is BNEP) + bridging
    the channel into a `tokio` byte-stream
  - `runloop.rs` — dedicated thread running `CFRunLoopRun()` (Cocoa
    callbacks don't fire without an active run loop; `dispatch_main`
    is not a substitute — confirmed via [users.rust-lang.org thread 87824](https://users.rust-lang.org/t/mac-iobluetooth-binding-no-delegate-calls/87824))

- **`crates/pim-bluetooth/src/linux/l2cap_server.rs`** — `AF_BLUETOOTH
  SOCK_SEQPACKET` listener on the same PSM. Linux side can run alongside
  (or instead of) the existing BNEP NAP server — they don't conflict.

- **`crates/pim-transport/src/l2cap.rs`** — new transport variant
  parallel to TCP. The existing `Transport` trait should already
  abstract enough that L2CAP slots in.

#### Inside `pim-ui` (separate repo)

- **`Info.plist`** already has `NSBluetoothAlwaysUsageDescription` (added
  by pim-ui#main 2026-04-28). Required on macOS 13+ — TCC denies access
  silently otherwise.
- **LaunchAgent broker** — `IOBluetooth` callbacks **do not fire** for
  LaunchDaemon-launched processes on Sonoma+ (Apple Forums thread
  [738748](https://developer.apple.com/forums/thread/738748)). The
  Bluetooth-touching code must run in a user-session LaunchAgent and
  proxy state changes to the privileged daemon over the existing Unix
  socket. This is a non-trivial privilege-model change.

### 4.3 Why this works

- **No PAN profile needed** — we don't pretend to be a network
  interface, we just transport bytes
- **No BNEP needed** — pim's own framing replaces it
- **No `bridge0` needed** — IP routes through the existing `utun`
- **No Network Extension entitlement needed** — Network Extensions are
  for apps that *create* tun/tap devices; we already have utun via
  the daemon's own existing path
- **No kext needed** — pure user-space code on both ends
- **macOS API surface is fully public** — `IOBluetoothDevicePair`,
  `IOBluetoothDeviceInquiry`, `IOBluetoothDevice.openL2CAPChannelAsync`
  are all in the public IOBluetooth framework (deprecated since macOS
  12 but functional on 15 / 16 with no signal of removal)

### 4.4 Why this isn't already done

The L2CAP-as-transport replacement is **1–2 weeks of focused work**:

1. Cocoa run-loop thread + Rust ↔ Obj-C bridging via `objc2-io-bluetooth`
2. Pairing automation with delegate auto-confirm
3. L2CAP channel I/O wired into tokio
4. SDP service registration on Linux + parsing on macOS
5. Transport trait wiring on both ends
6. Privilege-model adjustment in pim-ui (LaunchAgent broker)

Plus testing across macOS 13/14/15/16 and BlueZ versions 5.66+. None of
the steps are speculative — all use documented public APIs — but the
volume is real.

---

## 5. Implementation roadmap

### Phase 1 — proof of concept (1 week)

- Vertical slice: macOS Mac pairs with a Linux peer (manual
  `bluetoothctl pairable on; default-agent` on Linux side), opens an
  L2CAP channel on PSM `0x1011`, exchanges a single test frame, closes.
- No tokio integration yet — synchronous `IOBluetoothL2CAPChannel`
  delegate prints bytes. Linux side: `socat
  BLUETOOTH-L2CAP-LISTEN:0x1011 STDIO`.
- **Deliverable**: a 200-line proof binary that exits 0 with bytes
  flowing.

### Phase 2 — transport integration (3 days)

- Wrap the L2CAP channel in `tokio::io::AsyncRead/AsyncWrite`.
- Plug into `pim-transport` so the daemon can mint TCP-style mesh
  framing on top.
- Replace the Linux `socat` end with the daemon's own L2CAP server.

### Phase 3 — pairing automation (2 days)

- `IOBluetoothDevicePair` delegate that auto-confirms numeric
  comparison.
- Linux SSP agent registered as `NoInputNoOutput` so Just Works
  pairing kicks in without any user prompt.
- Test against BlueZ 5.66+ on Linux peers.

### Phase 4 — discovery + service identity (2 days)

- SDP service UUID registered on Linux side; macOS inquiry filters by
  it instead of by name prefix. Eliminates the "any device named
  `PIM-*` is treated as a peer" attack surface.
- Periodic re-scan with `IOBluetoothDeviceInquiry`.

### Phase 5 — pim-ui broker (3 days)

- LaunchAgent in user session that owns IOBluetooth code (TCC works
  here; one-time prompt at first run).
- Unix-socket protocol between the agent and the privileged daemon —
  ideally reuse the existing `pim.sock` JSON-RPC plumbing rather than
  invent a new channel.
- TCC consent dialog wiring (`NSBluetoothAlwaysUsageDescription`
  already in Info.plist).

### Phase 6 — hardening + multi-version testing (3 days)

- macOS 13 / 14 / 15 / 16 verification of every step.
- Pairing reliability on flaky links (peer goes out of range mid-flow,
  L2CAP closes, etc.).
- Reconnect / backoff strategy.

**Total**: ~15 working days for a single focused engineer.

---

## 6. Linux gateway side requirements

Already mostly correct; the only caveat is the SSP agent capability.

```bash
# Packages
apt install bluez bluez-tools        # bt-network in bluez-tools

# Kernel — if you keep BNEP-NAP for legacy clients
modprobe bnep
ip link add name pan0 type bridge
ip link set pan0 up
ip addr add 192.168.44.1/24 dev pan0
dnsmasq --interface=pan0 --bind-interfaces \
        --dhcp-range=192.168.44.10,192.168.44.50,12h --no-daemon
bt-network -s nap pan0

# For the L2CAP-as-transport path — pair flow only.
# NoInputNoOutput is the magic word: it makes SSP fall back to "Just
# Works" so the macOS initiator never sees a numeric-comparison
# prompt and our Rust delegate has nothing to auto-confirm.
bluetoothctl <<EOF
power on
discoverable on
pairable on
agent NoInputNoOutput
default-agent
EOF
```

Equivalent over D-Bus (no `bt-network`):

```python
bus.call("org.bluez", "/org/bluez/hci0",
         "org.bluez.NetworkServer1", "Register", "ss", "nap", "pan0")
```

Sources: [BlueZ network profile source](https://github.com/bluez/bluez/tree/master/profiles/network),
[fraggod 2015 PAN setup writeup](https://blog.fraggod.net/2015/03/28/bluetooth-pan-network-setup-with-bluez-5x.html)
(still valid through BlueZ 5.7x).

---

## 7. macOS gotchas (the parts that bit during research)

- **TCC blocks LaunchDaemons even as root** for IOBluetooth callbacks
  on Sonoma+. `registerForConnectNotifications:` returns a notification
  object but no callbacks fire. (Apple Forums thread [738748](https://developer.apple.com/forums/thread/738748).)
  This is the entire reason the implementation must run in a
  LaunchAgent in user session, not in the privileged daemon.
- **`NSBluetoothAlwaysUsageDescription` is mandatory** on macOS 13+
  ([Apple docs](https://developer.apple.com/documentation/BundleResources/Information-Property-List/NSBluetoothAlwaysUsageDescription))
  for any process touching IOBluetooth. Without it, the system prompt
  is suppressed and Bluetooth scan returns empty silently.
- **macOS as initiator advertises `DisplayYesNo` IO capability** —
  cannot be changed via public API. The peer's IO capability decides
  the SSP method. With Linux as `NoInputNoOutput`, SSP picks Just
  Works; with a peer that requests numeric comparison, you'd see a
  prompt unless you auto-confirm via `replyUserConfirmation:YES` in
  your delegate.
- **`blueutil` doesn't help.** Verified via [README + source](https://github.com/toy/blueutil).
  `blueutil --connect <id>` opens an ACL connection but cannot pick a
  service profile. There is no `blueutil --service PANU` flag and
  cannot be one — the underlying IOBluetooth APIs don't expose PAN
  profile activation.
- **Out-of-Band pairing is not available** on macOS via public API.
  Apple Developer Forums explicitly say "Out Of Band pairing is
  currently not available for iOS apps" — same private-only situation
  on macOS. (Forums thread [758122](https://developer.apple.com/forums/thread/758122).)
- **Don't try to spoof a paired device by writing
  `/private/var/root/Library/Preferences/com.apple.bluetoothd.plist`.**
  The link-key location is documented (e.g. [thread 8135590](https://discussions.apple.com/thread/8135590))
  but the approach requires SIP off / Full Disk Access, breaks across
  macOS updates, and `bluetoothd` validates plist contents before
  honoring them. Last-resort hack at best.

---

## 8. Existing Bluetooth implementation work — branch review

The `feature/macOS-bluetooth` branch (origin/feature/macOS-bluetooth)
has prior implementation work. **Before extending it, verify it isn't
going down the dead PAN path.** Specifically check whether it:

- ✅ Uses `IOBluetoothDevice.openL2CAPChannelAsync:withPSM:` for transport
- ✅ Pairs via `IOBluetoothDevicePair` with auto-confirm
- ✅ Routes IP frames through `utun` rather than expecting `bridge0`
- ❌ Tries to reanimate `bridge0` / Internet Sharing toggle
- ❌ Calls into private `BluetoothPAN.framework`
- ❌ Patches `bluetoothd` plist files
- ❌ Requires a kernel extension or Network Extension entitlement

If the branch goes the L2CAP-as-transport route, this doc supports that
direction. If it goes the PAN-revival route, sections 1–3 above
explain why that work cannot succeed and should be redirected.

---

## 9. References

### Apple
- [IOBluetooth — Apple Developer Documentation](https://developer.apple.com/documentation/iobluetooth)
- [IOBluetoothDevice](https://developer.apple.com/documentation/iobluetooth/iobluetoothdevice)
- [IOBluetoothDevicePair](https://developer.apple.com/documentation/iobluetooth/iobluetoothdevicepair)
- [Bluetooth on OS X (archive)](https://developer.apple.com/library/archive/documentation/DeviceDrivers/Conceptual/Bluetooth/BT_Bluetooth_On_MOSX/BT_Bluetooth_On_MOSX.html)
- [NSBluetoothAlwaysUsageDescription](https://developer.apple.com/documentation/BundleResources/Information-Property-List/NSBluetoothAlwaysUsageDescription)

### Apple community / forums
- [Apple Forums #738748 — IOBluetoothDevice notifications fail for daemons (Sonoma TCC)](https://developer.apple.com/forums/thread/738748)
- [Apple Forums #758122 — Out-Of-Band pairing not available](https://developer.apple.com/forums/thread/758122)
- [Apple Community 253312557 — PAN missing on new MacBook Pro (Senior Advisor confirms removal)](https://discussions.apple.com/thread/253312557)
- [Apple Community 254918402 — PAN still missing in Ventura](https://discussions.apple.com/thread/254918402)
- [Apple Community 255822875 — Can't turn Bluetooth Sharing on (Sequoia 15.1)](https://discussions.apple.com/thread/255822875)
- [Apple Community 8135590 — link-key plist location High Sierra+](https://discussions.apple.com/thread/8135590)

### Reverse engineering & headers
- [colemancda — Hacking IOBluetooth](https://colemancda.github.io/2018/03/25/Hacking-IOBluetooth)
- [w0lfschild — IOBluetoothDevice.h 7.0.0d93](https://github.com/w0lfschild/macOS_headers/blob/master/macOS/Frameworks/IOBluetooth/7.0.0d93/IOBluetoothDevice.h)
- [phracker/MacOSX-SDKs IOBluetoothDevice.h 10.6](https://github.com/phracker/MacOSX-SDKs/blob/master/MacOSX10.6.sdk/System/Library/Frameworks/IOBluetooth.framework/Versions/A/Headers/objc/IOBluetoothDevice.h)
- [onmyway133/Runtime-Headers IOBluetoothDevice.h 10.12](https://github.com/onmyway133/Runtime-Headers/blob/master/macOS/10.12/IOBluetooth.framework/IOBluetoothDevice.h)
- [AppleBluetooth/IOBluetoothFamily reverse-engineering](https://github.com/AppleBluetooth/IOBluetoothFamily)

### Rust crates
- [objc2-io-bluetooth (docs.rs)](https://docs.rs/objc2-io-bluetooth/latest/objc2_io_bluetooth/) — public IOBluetooth bindings; no PAN/BNEP types because none exist publicly
- [users.rust-lang.org — IOBluetooth no delegate calls thread](https://users.rust-lang.org/t/mac-iobluetooth-binding-no-delegate-calls/87824) — confirms CFRunLoop requirement

### Tools / projects
- [toy/blueutil — README + source](https://github.com/toy/blueutil)
- [lapfelix/BluetoothConnector — pair/connect ACL on macOS](https://github.com/lapfelix/BluetoothConnector)
- [jwise/HoRNDIS — USB-RNDIS workaround](https://github.com/jwise/HoRNDIS)
- [The Register — HoRNDIS coverage 2024-02-07](https://www.theregister.com/2024/02/07/horndis_android_mac/)

### BlueZ / Linux
- [BlueZ network profile source](https://github.com/bluez/bluez/tree/master/profiles/network)
- [BlueZ PAN setup writeup, fraggod 2015](https://blog.fraggod.net/2015/03/28/bluetooth-pan-network-setup-with-bluez-5x.html) — still valid

### TCC / privacy
- [HackTricks macOS TCC](https://angelica.gitbook.io/hacktricks/macos-hardening/macos-security-and-privilege-escalation/macos-security-protections/macos-tcc)
- [Chris Paynter — daemons blocked by TCC](https://chrispaynter.medium.com/what-to-do-when-your-macos-daemon-gets-blocked-by-tcc-dialogues-d3a1b991151f)
