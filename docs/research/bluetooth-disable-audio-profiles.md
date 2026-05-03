# Disabling Bluetooth audio profiles when serving NAP

## Why this exists

When `pim-daemon` brings up a Bluetooth NAP server (`serve_nap = true`), the
host BlueZ controller advertises every profile that `bluetoothd` has loaded
plugins for — including A2DP source/sink, AVRCP, HFP, and OBEX alongside NAP.
A peer paired for mesh networking can therefore use the same link to route
audio (the user observed this client → server). PIM exposes a network plane,
not a media plane, so the audio profiles are at best a confusing surface and
at worst a footgun for users running `pim-daemon` on a workstation that also
acts as their daily audio sink.

There is no per-controller D-Bus call in BlueZ that removes a UUID once
`bluetoothd` has registered it. Removing the profile must happen at one of
two layers below `bluetoothd`'s service objects:

1. The `bluetoothd` plugin loader (`--noplugin=audio,avrcp,hfp,…`).
2. The MGMT socket UUID registry (`btmgmt remove-uuid <uuid>` against `hci0`).

Both have caveats, listed below.

## Option A — `bluetoothd` plugin disable (system-wide)

Edit the systemd drop-in or `/etc/bluetooth/main.conf`:

```ini
# /etc/bluetooth/main.conf
[General]
Disable=Headset,Sink,Source,AVRCP
```

Or, on systemd hosts:

```bash
sudo systemctl edit bluetooth.service
# in the editor:
[Service]
ExecStart=
ExecStart=/usr/lib/bluetooth/bluetoothd --noplugin=audio --noplugin=hfp --noplugin=avrcp
```

**Pros**: clean, persistent, and works for every BlueZ client on the host
(not just `pim-daemon`).

**Cons**: kills audio for the whole machine, which is too blunt for users
who actually want their pim-host to also pair as a speaker.

## Option B — MGMT-socket UUID removal (per-daemon)

`btmgmt` (from `bluez-tools`) talks to the kernel MGMT socket directly:

```
btmgmt -i hci0 remove-uuid 0000110a-0000-1000-8000-00805f9b34fb   # A2DP source
btmgmt -i hci0 remove-uuid 0000110b-0000-1000-8000-00805f9b34fb   # A2DP sink
btmgmt -i hci0 remove-uuid 0000110e-0000-1000-8000-00805f9b34fb   # AVRCP
btmgmt -i hci0 remove-uuid 0000111e-0000-1000-8000-00805f9b34fb   # HFP HF
btmgmt -i hci0 remove-uuid 0000111f-0000-1000-8000-00805f9b34fb   # HFP AG
```

**Pros**: scope is the running daemon, not the whole host. The user can
still pair audio devices when `pim-daemon` is stopped (UUIDs come back when
BlueZ next registers them).

**Cons**: BlueZ may **re-register** these UUIDs whenever the audio plugin
sees a profile event (e.g. another paired audio device reconnects). Removal
is not sticky. Practical mitigation is to re-run `remove-uuid` periodically
or every time NAP is bounced, alongside the existing `discoverable on` /
`scan on` keepalive.

## Recommendation

Add Option B as a `pim-bluetooth::platform_impl::strip_audio_uuids` helper
gated on `serve_nap = true`. Re-fire on the same cadence as
`discoverable on` keepalive. Document Option A as the manual escape hatch
for users who never want PIM hosts to act as audio peers.

## Out of scope (do not implement here)

- macOS: IOBluetooth has no analogous API. macOS hosts that don't want to be
  audio peers should disable A2DP system-wide via `defaults write`.
- Windows: no production NAP path yet — revisit when one exists.
