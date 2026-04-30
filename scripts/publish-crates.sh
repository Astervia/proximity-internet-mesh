#!/usr/bin/env bash
# Publishes all workspace crates to crates.io in dependency order.
# Run prepare-release.sh first to bump versions, then commit and tag before publishing.
#
# Usage:
#   scripts/publish-crates.sh                        # publish all crates
#   START_FROM=pim-bluetooth scripts/publish-crates.sh  # resume after a rate-limit interruption
#   DRY_RUN=1 scripts/publish-crates.sh              # dry-run (no network writes)
#
# crates.io allows ~1 new crate per 10 minutes. PUBLISH_DELAY defaults to 600s
# for first-time publishes. For subsequent version bumps of already-registered
# crates you can lower it: PUBLISH_DELAY=30 scripts/publish-crates.sh
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

DRY_RUN="${DRY_RUN:-}"
START_FROM="${START_FROM:-}"
PUBLISH_DELAY="${PUBLISH_DELAY:-600}"  # 10 min default: crates.io rate-limits new crate registrations

die() { echo "error: $*" >&2; exit 1; }
require_tool() { command -v "$1" >/dev/null 2>&1 || die "required tool not found: $1"; }

require_tool cargo
require_tool git

# Refuse to publish with uncommitted changes
if ! git diff --quiet || ! git diff --cached --quiet; then
    die "working tree has uncommitted changes — commit everything before publishing"
fi

# Pre-publish CI checks (same toolchain as CI)
echo "==> Running pre-publish checks (Rust 1.94.0)..."
cargo +1.94.0 fmt --all -- --check
cargo +1.94.0 clippy --workspace --all-targets --locked -- -D warnings
cargo +1.94.0 test --workspace --all-targets --locked
echo ""

# Topological publish order (each crate after all its internal dependencies)
CRATES=(
    pim-core
    pim-crypto
    pim-protocol
    pim-gateway
    pim-tun
    pim-bluetooth
    pim-transport
    pim-routing
    pim-wifidirect
    pim-discovery
    pim-cli
    pim-daemon
)

publish_flags="--locked"
if [[ -n "$DRY_RUN" ]]; then
    publish_flags="$publish_flags --dry-run"
    echo "==> DRY RUN — no crates will actually be published"
    echo ""
fi

# Validate START_FROM if given
if [[ -n "$START_FROM" ]]; then
    found=0
    for c in "${CRATES[@]}"; do [[ "$c" == "$START_FROM" ]] && found=1 && break; done
    [[ $found -eq 1 ]] || die "START_FROM crate '$START_FROM' not found in publish list"
    echo "==> Resuming from $START_FROM (skipping earlier crates)"
    echo ""
fi

skipping=true
[[ -z "$START_FROM" ]] && skipping=false

for crate in "${CRATES[@]}"; do
    if $skipping; then
        [[ "$crate" == "$START_FROM" ]] && skipping=false || continue
    fi

    echo "==> Publishing $crate..."
    # shellcheck disable=SC2086
    cargo publish -p "$crate" $publish_flags
    if [[ -z "$DRY_RUN" ]]; then
        echo "    Waiting ${PUBLISH_DELAY}s for crates.io to index $crate..."
        sleep "$PUBLISH_DELAY"
    fi
done

echo ""
echo "All crates published successfully."
