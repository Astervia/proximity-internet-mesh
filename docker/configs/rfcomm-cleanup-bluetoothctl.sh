#!/bin/sh
# Fake `bluetoothctl` for the rfcomm-cleanup lab. Mounted into the
# container at /tmp/fake-bin/bluetoothctl. Handles the three calls
# the cleanup task makes: `devices Paired`, `info <addr>`, `remove
# <addr>`. Every other invocation is a no-op so a surprise extra
# command doesn't fail the lab in a confusing way.

# Strip the optional `--timeout N` prefix the daemon adds.
if [ "$1" = "--timeout" ]; then
    shift 2
fi

case "$1" in
    devices)
        if [ "$2" = "Paired" ] || [ -z "${2:-}" ]; then
            printf 'Device AA:BB:CC:DD:EE:FF PIM-test-stale\n'
        fi
        ;;
    info)
        printf 'Device %s (public)\n\tName: PIM-test-stale\n\tConnected: no\n\tPaired: yes\n' "$2"
        ;;
    remove)
        printf '%s remove %s\n' "$(date +%s)" "$2" >> /tmp/fake-bin/remove.log
        ;;
esac
exit 0
