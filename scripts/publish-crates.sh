#!/usr/bin/env bash
# Publishes all workspace crates to crates.io in dependency order.
# Run prepare-release.sh first to bump versions, then commit and tag before publishing.
#
# Usage:
#   scripts/publish-crates.sh          # publish all crates
#   DRY_RUN=1 scripts/publish-crates.sh  # dry-run (no network writes)
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

DRY_RUN="${DRY_RUN:-}"
PUBLISH_DELAY="${PUBLISH_DELAY:-30}"  # seconds to wait between publishes for crates.io indexing

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

for crate in "${CRATES[@]}"; do
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
