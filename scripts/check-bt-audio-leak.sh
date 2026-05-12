#!/usr/bin/env bash
# check-bt-audio-leak.sh — manual hardware regression test for the BlueZ
# A2DP/HFP audio-profile leak that motivated the L2CAP CoC transport.
#
# Story:
#   RFCOMM bonds on Linux pull in BlueZ's A2DP/HFP profiles by default
#   (visible as `bluez_card.*` cards in PulseAudio). After pairing an
#   Android peer over PIM-RFCOMM, the Android playback would route its
#   audio to the Linux machine — surprising and unwanted. The fix is to
#   ship L2CAP CoC as the default Bluetooth transport (LE has no
#   auto-registered audio profiles) and demote RFCOMM to a fallback.
#
# This script is the manual gate for the L2CAP CoC plan's audio-leak
# acceptance criterion. It is intentionally not in CI — it needs paired
# hardware on both ends and an operator to drive playback.
#
# Usage:
#   bash scripts/check-bt-audio-leak.sh /path/to/pim.toml
#
# Exit codes:
#   0 — no leak observed
#   1 — config asserts failed (CoC disabled / RFCOMM enabled / etc.)
#   2 — a `bluez_card.*` card appeared during the test (LEAK)
#   3 — required tooling missing
#
# References:
#   - `.agent/memory/lessons/known-bugs.md#3` — audio-leak history
#   - `plans/l2cap-coc-transport/plan.md` — Phase 6 acceptance criterion
#   - `kernel/crates/pim-core/src/config/model.rs::BluetoothCocConfig`

set -uo pipefail

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RESET='\033[0m'
fail()    { echo -e "${RED}FAIL:${RESET} $*"; }
warn()    { echo -e "${YELLOW}WARN:${RESET} $*"; }
ok()      { echo -e "${GREEN}OK:${RESET}   $*"; }
need()    { command -v "$1" >/dev/null 2>&1 || { fail "missing required tool: $1"; exit 3; }; }
prompt()  { read -r -p "$(echo -e "${YELLOW}? ${RESET}$1 [press Enter]")" _; }

CONFIG="${1:-}"
if [[ -z "$CONFIG" ]]; then
    echo "usage: $0 /path/to/pim.toml" >&2
    exit 1
fi
if [[ ! -r "$CONFIG" ]]; then
    fail "config not readable: $CONFIG"
    exit 1
fi

need pactl
need grep
need awk
# Tolerate either `python3 -c` or `tomlq` for parsing; we keep it simple
# with grep since `enabled = ...` is uniquely scoped under each section.

# ── Step 1: config asserts ───────────────────────────────────────────
echo "── config asserts ──"
section() {
    # Print the body of a top-level TOML section by name.
    local name="$1"
    awk -v sect="[${name}]" '
        $0 == sect { in_sect = 1; next }
        /^\[/ { in_sect = 0 }
        in_sect { print }
    ' "$CONFIG"
}

coc_enabled=$(section "bluetooth_coc" | grep -E '^enabled\s*=' | head -1 | awk -F= '{print $2}' | tr -d ' "')
rfc_enabled=$(section "bluetooth_rfcomm" | grep -E '^enabled\s*=' | head -1 | awk -F= '{print $2}' | tr -d ' "')

if [[ "$coc_enabled" != "true" ]]; then
    fail "[bluetooth_coc].enabled is not 'true' (got: '${coc_enabled:-<unset>}')"
    fail "CoC must be enabled for this test to mean anything."
    exit 1
fi
ok "[bluetooth_coc].enabled = true"

if [[ "$rfc_enabled" == "true" ]]; then
    fail "[bluetooth_rfcomm].enabled = true — disable RFCOMM for the leak test."
    fail "We want to verify CoC alone does NOT leak; RFCOMM being on confounds the result."
    exit 1
fi
ok "[bluetooth_rfcomm].enabled is not 'true' (got: '${rfc_enabled:-<unset>}')"

# ── Step 2: baseline — no bluez_card before the test ─────────────────
echo
echo "── baseline ──"
baseline=$(pactl list cards short | grep -c '^.*bluez_card' || true)
if [[ "$baseline" -ne 0 ]]; then
    fail "baseline already has $baseline bluez_card(s) loaded — restart pulseaudio / pipewire and rerun."
    pactl list cards short | grep 'bluez_card' || true
    exit 1
fi
ok "baseline: 0 bluez_card.* loaded"

# ── Step 3: operator drives the test ─────────────────────────────────
echo
echo "── operator-driven test ──"
echo "1. Start pim-daemon with the config above so the CoC service is up."
echo "2. Pair the Android peer to this Linux box VIA PIM (not via OS"
echo "   Bluetooth Settings) — confirm the pair dialog on both sides."
echo "3. On the Android peer, play music or any audio for ~30 seconds."
prompt "When you've done all three, confirm to re-check the cards table"

# ── Step 4: re-check — bluez_card count must stay 0 ─────────────────
after=$(pactl list cards short | grep -c '^.*bluez_card' || true)
if [[ "$after" -ne 0 ]]; then
    fail "AUDIO LEAK: $after bluez_card.* appeared after pairing + playback"
    pactl list cards short | grep 'bluez_card'
    echo
    fail "The plan's acceptance criterion is violated. Capture this output and"
    fail "see .agent/memory/lessons/known-bugs.md#3 for the regression context."
    exit 2
fi
ok "post-test: 0 bluez_card.* loaded — no leak observed"
echo
ok "audio-leak check PASSED"
exit 0
