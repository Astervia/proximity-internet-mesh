# Bluetooth Gateway Shutdown & Cleanup

Notes for operators running `pim-daemon` as a Bluetooth gateway (configs
like `docs/reference/config-examples/gateway-bluetooth.toml`) when they need to
stop the daemon and bring the host back to a clean state.

## Normal shutdown: just send SIGTERM

As of the current build, `pim-daemon` tears down everything it created
when it receives `SIGTERM` or `Ctrl-C`:

- the NAP bridge (`br-bt`) is set down and deleted
- the Bluetooth MASQUERADE / FORWARD iptables rules are removed
- supervised child processes (`bt-network`, `dnsmasq`, `dhclient`) are killed
- the TCP listening socket on `transport.listen_port` is released
- peers receive a Goodbye frame and TCP sessions are closed
- the TUN interface (`pim0`) is brought down

Preferred command to stop the gateway:

```bash
sudo pkill -TERM -x pim-daemon
```

`-x` requires an exact process-name match, so you don't hit other tools
that happen to have "pim-daemon" in their argv (history, editors, etc.).
If you started `pim-daemon` yourself under `sudo`, its process name is
still `pim-daemon` — the CLI `pim up` wrapper `exec`s the daemon in place
with the appropriate name.

If you spawned multiple daemons with different configs and only want to
stop the gateway one, match by the config path the kernel sees:

```bash
sudo pkill -TERM -f 'pim-daemon .*gateway-bluetooth\.toml'
```

Give it up to ~10 seconds to finish teardown (the daemon caps the
Bluetooth teardown wait at 10 s). A quick verification:

```bash
ip link show br-bt 2>/dev/null        # should print nothing
sudo iptables -t nat -S POSTROUTING | grep 192.168.44   # should be empty
ss -ltn 'sport = :9100'                # should be empty
```

## If the daemon doesn't exit

If the daemon is genuinely stuck (kernel-level wedge, e.g. a blocking
ioctl into a wedged Bluetooth driver), SIGKILL is a last resort. **Don't
SIGKILL as a first move** — it bypasses all cleanup and is the reason
you previously had to delete `br-bt` by hand. Try SIGTERM first, give
it 10–15 seconds, and only then escalate.

```bash
# Try the clean path first.
sudo pkill -TERM -x pim-daemon
# Wait up to 15s for teardown to complete.
for i in $(seq 1 30); do
    pgrep -x pim-daemon >/dev/null || break
    sleep 0.5
done
# Only if still running:
if pgrep -x pim-daemon >/dev/null; then
    echo "SIGTERM didn't land; sending SIGKILL"
    sudo pkill -KILL -x pim-daemon
    # Manual cleanup is required because SIGKILL skips teardown.
    sudo ip link delete br-bt 2>/dev/null || true
    sudo iptables -t nat -D POSTROUTING -s 192.168.44.0/24 -o eno1 -j MASQUERADE 2>/dev/null || true
    sudo iptables -D FORWARD -s 192.168.44.0/24 -j ACCEPT 2>/dev/null || true
fi
```

Replace `eno1` with whatever `gateway.nat_interface` is in your config.
Replace `192.168.44.0/24` with the subnet derived from
`bluetooth.nap_bridge_addr`.

## A better replacement for the old cleanup snippet

The old cleanup snippet — reproduced here for reference —

```bash
sudo -v
PID=$(pgrep -fo '/usr/local/bin/pim-daemon .*gateway-bluetooth.toml')
sudo kill -TERM "$PID"
sleep 2
sudo kill -KILL "$PID" 2>/dev/null || true
sudo pkill -KILL -f '/usr/local/bin/pim-daemon .*gateway-bluetooth.toml' 2>/dev/null || true
sudo fuser -k 9100/tcp 2>/dev/null || true
sudo ip link set br-bt down
sudo ip link delete br-bt
```

has three properties that can leave your terminal in a weird state:

1. **`sudo fuser -k 9100/tcp`** sends SIGKILL to _every_ process holding
   TCP port 9100 in any way (listeners, established connections,
   anything in `TIME_WAIT` attached to a live fd). In practice this is
   almost always just pim-daemon, but it will also kill any other
   development server or debugger that happens to have bound the port.
   Worse, with `-k` and no `-s`, `fuser` defaults to SIGKILL, which
   takes no prisoners; if it catches a shell or something that shares
   stdio with your terminal session, you end up with a dangling pty.

2. **`sudo kill -KILL` only 2 s after SIGTERM** doesn't give the
   daemon enough time to finish its teardown. Before this release
   that didn't matter because there was no teardown to finish; now it
   matters, and a 2 s wait will often be enough for TCP to release but
   not enough for the bridge + iptables to be undone.

3. **No default-route rollback.** When the daemon was installing a
   default route through `pim0` and you SIGKILL it, the route stays.
   Your shell's DNS/HTTP traffic then tries to egress via a dead TUN,
   which looks to you like "my terminal is broken" — everything hangs
   on name resolution for a while. This is the most common cause of
   the "fucks up my terminal" symptom.

The replacement below avoids all three:

```bash
#!/usr/bin/env bash
# clean-pim-gateway.sh — terminate pim-daemon (gateway-bluetooth config)
# and, only if SIGTERM doesn't cut it, fall back to SIGKILL and manual
# resource cleanup.
set -u
MATCH='pim-daemon .*gateway-bluetooth\.toml'
NAP_SUBNET='192.168.44.0/24'
NAT_IFACE='eno1'   # ← match gateway.nat_interface from your config

sudo -v
# Use pgrep -f to match on the full argv, but not -o: if you have more
# than one matching daemon, kill all of them explicitly.
mapfile -t PIDS < <(pgrep -f "$MATCH" || true)
if [ ${#PIDS[@]} -eq 0 ]; then
    echo "no pim-daemon process matches /$MATCH/"
    exit 0
fi
echo "pids: ${PIDS[*]}"

sudo kill -TERM "${PIDS[@]}"

# Wait up to 15s for graceful teardown — the daemon bounds its Bluetooth
# cleanup at 10s internally.
for _ in $(seq 1 30); do
    alive=0
    for p in "${PIDS[@]}"; do
        kill -0 "$p" 2>/dev/null && alive=1
    done
    [ "$alive" -eq 0 ] && break
    sleep 0.5
done

# If anything survived, SIGKILL and clean up manually.
alive=0
for p in "${PIDS[@]}"; do kill -0 "$p" 2>/dev/null && alive=1; done
if [ "$alive" -ne 0 ]; then
    echo "graceful SIGTERM failed; escalating to SIGKILL + manual cleanup"
    sudo kill -KILL "${PIDS[@]}" 2>/dev/null || true

    # Bridge
    sudo ip link set br-bt down 2>/dev/null || true
    sudo ip link delete br-bt 2>/dev/null || true

    # iptables — only the rules the daemon installs
    sudo iptables -t nat -D POSTROUTING -s "$NAP_SUBNET" \
        -o "$NAT_IFACE" -j MASQUERADE 2>/dev/null || true
    sudo iptables -D FORWARD -s "$NAP_SUBNET" -j ACCEPT 2>/dev/null || true

    # Default route via pim0 — rare, but if the route is still pointing at
    # a dead TUN this is what makes the terminal "hang".
    if ip route show | grep -q 'default .* dev pim0'; then
        sudo ip route del default dev pim0 2>/dev/null || true
    fi

    # TUN interface
    ip link show pim0 >/dev/null 2>&1 && sudo ip link delete pim0 2>/dev/null || true
fi

# Deliberately *not* using `fuser -k 9100/tcp` — it would kill unrelated
# processes that happen to have the port open.
```

## What each cleanup action actually touches

When you run the SIGKILL-escalation path above (or the old snippet),
here is what each piece affects and whether it has side effects the
operator needs to be aware of.

### `ip link delete br-bt`

- Removes the NAP Linux bridge.
- If any Bluetooth client is still associated via `bt-network -c`, its
  PAN interface (`bnep0`) is orphaned and its DHCP lease on
  `192.168.44.0/24` goes nowhere. Restart `bt-network` on the client
  side (or simply let auto-discovery reconnect once the gateway is
  back up).
- **No host-wide network restart.** Interfaces unrelated to `br-bt`
  (your Wi-Fi, `eth0`/`eno1`, any docker bridges, VPNs, `lo`) are
  untouched. The "network manager restart" you may see in some older
  notes is not caused by this command — deleting a non-NAP bridge
  affects only that bridge.
- If you had other Bluetooth PAN clients bridged into `br-bt` via
  some external tool, they lose layer-2 connectivity to each other.
  In a PIM-only setup, there are no such clients.

### `iptables -t nat -D POSTROUTING -s 192.168.44.0/24 -o $NAT_IFACE -j MASQUERADE`

- Removes exactly one masquerade rule matching PIM's Bluetooth subnet.
- Clients that had an in-flight masqueraded connection (e.g. a TCP
  session from `192.168.44.0/24` out through `eno1`) will find return
  packets dropped. The connection state in `conntrack` still exists
  but can't match new inbound return packets without the MASQUERADE
  rule. Existing connections therefore die; new ones never start.
  No other NAT rules are affected.
- Packet forwarding itself is NOT disabled by this — `net.ipv4.ip_forward`
  stays at whatever it was set to. The daemon only _enables_ forwarding
  (via `sysctl -w net.ipv4.ip_forward=1`); it doesn't restore the
  previous value on shutdown. If you need strict cleanup, set it
  yourself: `sudo sysctl -w net.ipv4.ip_forward=0`.

### `iptables -D FORWARD -s 192.168.44.0/24 -j ACCEPT`

- Removes the companion FORWARD-ACCEPT rule the daemon installs
  alongside the MASQUERADE.
- If your FORWARD policy is `DROP` (the default on hardened hosts),
  Bluetooth clients lose internet the moment this rule is removed.
- If your FORWARD policy is `ACCEPT` (common on desktops), this
  rule removal has no observable effect; traffic continues to forward
  through the default policy.
- No other FORWARD rules are affected.

### `ip link delete pim0`

- Removes the TUN interface the daemon created.
- Any route whose `dev` is `pim0` becomes stale and is evicted
  automatically by the kernel. That includes any PIM mesh routes
  and any default-via-`pim0` route.
- Applications with open sockets bound to `pim0` get EBADF / ENETDOWN
  on their next I/O. In practice only `pim-daemon` itself holds the
  TUN fd, and by this point it's already gone.

### `ip route del default dev pim0`

- Removes the default route the daemon installed _before_ deleting
  `pim0` itself. Doing it in this order means you get back a working
  default route immediately (usually via Wi-Fi or Ethernet), without
  waiting for NetworkManager / systemd-networkd to notice the TUN is
  gone.
- This is the single most important step for "my terminal feels
  broken after pkill". Everything your shell does — DNS resolution,
  `curl`, package managers — will hang until a usable default route
  exists again.

### `fuser -k 9100/tcp` _(not recommended)_

- SIGKILL to every process holding port 9100 in any capacity.
- Unrelated to the Bluetooth subsystem; only matters because the
  daemon's old behaviour leaked the listening socket. With the
  current build the socket is released on SIGTERM, so this step
  is no longer necessary.
- If you ever have another tool on port 9100 (a test server, a
  tunnelled port forward, an SSH local-forward) it will be killed
  without warning. Prefer `pkill -x pim-daemon`, which targets only
  the daemon.

### `sysctl -w net.ipv4.ip_forward=0` _(optional)_

- Returns IP forwarding to its previous state. The daemon enables it
  but does not disable it on shutdown. If nothing else on your host
  depended on forwarding being on, you can restore it yourself.
- Disabling forwarding breaks any other in-flight forwarded traffic —
  docker networking on bridge mode, libvirt NAT networks, VPN
  gateways. Only do this if you are sure nothing else on the host
  needs it.

## What the daemon does NOT restore

These are things the daemon touches but does not return to their
original state, even on a clean SIGTERM. If you care about a
pristine host state, do them manually:

- `net.ipv4.ip_forward` (enabled, never disabled)
- Bluetooth controller state: `discoverable`, `pairable`, `alias`.
  The daemon changes these via `bluetoothctl` at startup and does
  not reset them. Run `bluetoothctl discoverable off; pairable off;
system-alias ""` if you need them back.
- Kernel conntrack entries for masqueraded flows. These expire
  naturally on timeout; `sudo conntrack -F` flushes them immediately
  if you have `conntrack-tools` installed.

## Why the old script could make your terminal feel broken

Putting the pieces together, the most likely cause of a wedged
terminal after running the old snippet is:

1. The daemon was installing a default route through `pim0`.
2. `kill -KILL` after only 2 s skipped the daemon's own route
   cleanup.
3. `ip link delete pim0` was never part of the snippet, so the
   dead TUN stuck around, but even after the TUN was deleted the
   default route stayed evicted without a replacement until the
   system network stack noticed.
4. Meanwhile your shell's next command tried to do DNS, hit the
   dead route, and blocked for 30–60 s.

The replacement script avoids this by (a) giving SIGTERM a chance
to clean up properly, and (b) if SIGKILL is needed, removing the
default route _and_ the TUN device before returning control to
the shell.
