# Installation

This project supports client and relay nodes on Linux and macOS. Gateway mode remains Linux-only because the daemon still depends on Linux NAT and raw-socket behavior for internet egress.

## Requirements

Host requirements:

- Linux or macOS
- Rust toolchain when installing from source
- privileges to create the TUN interface and update routing, usually `root` or `CAP_NET_ADMIN`
- Linux gateways additionally need `/dev/net/tun`, `iproute2`, and `iptables`

Optional but strongly recommended:

- Docker Engine
- Docker Compose v2

## Install From GitHub Releases

Published releases include tarballs containing `pim` and `pim-daemon` for:

- Linux x86_64 via `x86_64-unknown-linux-musl`
- macOS Intel via `x86_64-apple-darwin`
- macOS Apple Silicon via `aarch64-apple-darwin`

Pick the archive that matches your host:

```bash
VERSION="v0.1.4"

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) ASSET="pim-${VERSION}-x86_64-unknown-linux-musl" ;;
  Darwin-x86_64) ASSET="pim-${VERSION}-x86_64-apple-darwin" ;;
  Darwin-arm64) ASSET="pim-${VERSION}-aarch64-apple-darwin" ;;
  *)
    echo "No published release artifact for $(uname -s)-$(uname -m)" >&2
    exit 1
    ;;
esac

curl -LO "https://github.com/Astervia/proximity-internet-mesh/releases/download/${VERSION}/${ASSET}.tar.gz"
curl -LO "https://github.com/Astervia/proximity-internet-mesh/releases/download/${VERSION}/${ASSET}.sha256"

if command -v sha256sum >/dev/null 2>&1; then
  sha256sum -c "${ASSET}.sha256"
else
  shasum -a 256 -c "${ASSET}.sha256"
fi

tar -xzf "${ASSET}.tar.gz"
sudo mkdir -p /usr/local/bin
sudo install -m 755 "${ASSET}/pim" /usr/local/bin/pim
sudo install -m 755 "${ASSET}/pim-daemon" /usr/local/bin/pim-daemon

if [ "$(uname -s)" = "Linux" ]; then
  sudo mkdir -p /etc/pim /var/lib/pim /run
fi
```

## Build From Source

Development build:

```bash
cargo build --workspace
```

Release build:

```bash
cargo build --workspace --release
```

This produces:

- `target/release/pim`
- `target/release/pim-daemon`

## Install The Binaries

```bash
sudo install -Dm755 target/release/pim /usr/local/bin/pim
sudo install -Dm755 target/release/pim-daemon /usr/local/bin/pim-daemon
if [ "$(uname -s)" = "Linux" ]; then
  sudo install -d /etc/pim /var/lib/pim /run
fi
```

If you prefer not to install globally, you can run the binaries directly from `target/release/`, but `pim` must still be able to find `pim-daemon` either beside it or on `PATH`.

## Prepare A Config File

Create `/etc/pim/pim.toml`. On macOS, use a `utunN` interface name such as `utun0`. Start from the examples in [configuration.md](configuration.md).

## Verify The CLI

```bash
pim --help
pim up --help
pim status --help
```

## Verify A Full Development Environment

Unit tests:

```bash
make test-unit
```

Docker image and multi-node lab:

```bash
make docker-build
make test-p1
```
