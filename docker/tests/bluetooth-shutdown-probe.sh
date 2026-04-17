#!/usr/bin/env bash
# In-container probe for test-bluetooth-shutdown.sh.
#
# Starts pim-daemon, waits for the NAP bridge + MASQUERADE rule to be
# installed, sends SIGTERM, then asserts that the daemon exits cleanly
# and that every resource it created is gone.
#
# Exits 0 on full cleanup, non-zero with a diagnostic log otherwise.

set -uo pipefail

CONFIG="/etc/pim/pim.toml"
BRIDGE="br-bt"
SUBNET="192.168.44.0/24"
NAT_IFACE="eth0"
LISTEN_PORT="9100"
IPTABLES="${PIM_BLUETOOTH_IPTABLES_COMMAND:-iptables}"

fail=0
fatal() { echo "[probe][FATAL] $*" >&2; fail=1; }
info()  { echo "[probe][INFO] $*"; }
ok()    { echo "[probe][OK]   $*"; }

# Start the daemon in the background and capture its PID directly so we
# don't depend on the pid-file race.
info "launching pim-daemon"
pim-daemon "$CONFIG" /run/pim.pid &
DAEMON_PID=$!
info "daemon pid=$DAEMON_PID"

# Wait up to 20s for the bridge to appear (proof the BT service initialised).
bridge_up=0
for _ in $(seq 1 40); do
    if ip link show "$BRIDGE" >/dev/null 2>&1; then
        bridge_up=1
        break
    fi
    sleep 0.5
done
if [ "$bridge_up" -ne 1 ]; then
    fatal "bridge $BRIDGE was never created"
    kill -KILL "$DAEMON_PID" 2>/dev/null || true
    exit 1
fi
ok "bridge $BRIDGE created"

# Wait for the MASQUERADE rule so we know install_bluetooth_masquerade ran.
masq_up=0
for _ in $(seq 1 20); do
    if "$IPTABLES" -t nat -C POSTROUTING -s "$SUBNET" -o "$NAT_IFACE" -j MASQUERADE >/dev/null 2>&1; then
        masq_up=1
        break
    fi
    sleep 0.5
done
if [ "$masq_up" -ne 1 ]; then
    fatal "MASQUERADE rule for $SUBNET was never installed"
fi
[ "$masq_up" -eq 1 ] && ok "MASQUERADE rule for $SUBNET installed"

# Confirm the listener is actually bound (otherwise a later "port released"
# assertion would be meaningless).
port_up=0
for _ in $(seq 1 20); do
    if ss -ltn "sport = :$LISTEN_PORT" 2>/dev/null | grep -q "LISTEN"; then
        port_up=1
        break
    fi
    sleep 0.5
done
if [ "$port_up" -ne 1 ]; then
    fatal "tcp/$LISTEN_PORT never entered LISTEN"
fi
[ "$port_up" -eq 1 ] && ok "tcp/$LISTEN_PORT is LISTENing"

# If setup failed we still exit early; otherwise move on to the teardown path.
if [ "$fail" -ne 0 ]; then
    kill -KILL "$DAEMON_PID" 2>/dev/null || true
    exit 1
fi

info "sending SIGTERM to pim-daemon"
kill -TERM "$DAEMON_PID"

# Wait up to 15s for the daemon to exit on its own. The production teardown
# timeout is 10s, so 15s leaves slack for test scheduling.
wait_timeout=30  # 30 * 0.5s = 15s
exited=0
for _ in $(seq 1 "$wait_timeout"); do
    if ! kill -0 "$DAEMON_PID" 2>/dev/null; then
        exited=1
        break
    fi
    sleep 0.5
done
if [ "$exited" -ne 1 ]; then
    fatal "daemon did not exit within 15s of SIGTERM"
    ps -fp "$DAEMON_PID" || true
    kill -KILL "$DAEMON_PID" 2>/dev/null || true
fi
[ "$exited" -eq 1 ] && ok "daemon exited after SIGTERM"

# Give the kernel a fraction of a second to finish tearing sockets down after
# process exit (TIME_WAIT does not prevent rebind because SO_REUSEADDR is on).
sleep 0.3

# ── Assert: bridge deleted ──────────────────────────────────────────────────
if ip link show "$BRIDGE" >/dev/null 2>&1; then
    fatal "bridge $BRIDGE still present after shutdown"
    ip link show "$BRIDGE" || true
else
    ok "bridge $BRIDGE removed"
fi

# ── Assert: MASQUERADE rule removed ─────────────────────────────────────────
if "$IPTABLES" -t nat -C POSTROUTING -s "$SUBNET" -o "$NAT_IFACE" -j MASQUERADE >/dev/null 2>&1; then
    fatal "MASQUERADE rule for $SUBNET still present"
    "$IPTABLES" -t nat -S POSTROUTING || true
else
    ok "MASQUERADE rule for $SUBNET removed"
fi

# ── Assert: FORWARD ACCEPT rule removed ─────────────────────────────────────
if "$IPTABLES" -C FORWARD -s "$SUBNET" -j ACCEPT >/dev/null 2>&1; then
    fatal "FORWARD ACCEPT rule for $SUBNET still present"
else
    ok "FORWARD ACCEPT rule for $SUBNET removed"
fi

# ── Assert: port 9100 released ──────────────────────────────────────────────
# After the daemon exits, the OS should have released the listening socket.
# Try to bind it ourselves: nc -l prints nothing and returns when the socket
# is closed; use a short-lived listener.
if ss -ltn "sport = :$LISTEN_PORT" 2>/dev/null | grep -q "LISTEN"; then
    fatal "tcp/$LISTEN_PORT is still held by some process"
    ss -ltnp "sport = :$LISTEN_PORT" || true
else
    ok "tcp/$LISTEN_PORT released"
fi

# Actually attempt a rebind to confirm the port is usable.
if timeout 2 nc -l -p "$LISTEN_PORT" </dev/null >/dev/null 2>&1 &
then
    NCPID=$!
    sleep 0.2
    if kill -0 "$NCPID" 2>/dev/null; then
        ok "tcp/$LISTEN_PORT successfully rebound"
        kill "$NCPID" 2>/dev/null || true
        wait "$NCPID" 2>/dev/null || true
    else
        fatal "nc -l on $LISTEN_PORT exited immediately (rebind failed)"
    fi
fi

if [ "$fail" -ne 0 ]; then
    echo "[probe] one or more cleanup checks failed"
    exit 1
fi
echo "[probe] all cleanup checks passed"
exit 0
