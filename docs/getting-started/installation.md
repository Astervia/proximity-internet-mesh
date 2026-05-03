# Installation

This project supports client, relay, and gateway nodes on Linux and macOS. The gateway dataplane uses platform-specific host integration on each OS: `iptables` on Linux and `pfctl` on macOS.

## Supported Scope By Platform

| Capability | Linux | macOS |
| --- | --- | --- |
| Client node | Supported | Supported |
| Relay node | Supported | Supported |
| Gateway node | Supported | Supported |
| Wi-Fi Direct | Supported | Supported |
| Bluetooth PAN | Supported | Supported |
| Bluetooth RFCOMM | Supported | Sidecar |
| Docker lab flows | Supported | Not supported |

On macOS, plan on a `utunN` interface name for the mesh TUN and use a real uplink such as `en0` for `gateway.nat_interface`. Wi-Fi Direct uses Bonjour peer-to-peer discovery on macOS, Bluetooth PAN uses the host stack, and the Docker lab workflows still remain Linux-only.

## Requirements

Host requirements:

- Linux or macOS
- Rust toolchain when installing from source
- privileges to create the TUN interface and update routing, usually `root` or `CAP_NET_ADMIN`
- Linux gateways additionally need `/dev/net/tun`, `iproute2`, and `iptables`
- macOS gateways additionally need `pfctl` and privileges to enable `net.inet.ip.forwarding`

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
VERSION="$(curl -fsSLI -o /dev/null -w '%{url_effective}' \
  https://github.com/Astervia/proximity-internet-mesh/releases/latest \
  | sed 's:.*/::')"

if [ -z "${VERSION}" ]; then
  echo "Failed to determine the latest GitHub release version" >&2
  exit 1
fi

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

Create `/etc/pim/pim.toml`. On macOS, use a `utunN` interface name such as `utun0`, set `[gateway].nat_interface` to the real uplink such as `en0`, and install `blueutil` if you want Bluetooth PAN radio discovery and pairing automation. Wi-Fi Direct can also be enabled there; the backend uses Bonjour peer-to-peer discovery rather than Linux `wpa_cli` controls. Start from the examples in [configuration.md](configuration.md).

## Verify The CLI

```bash
pim --help
pim up --help
pim status --help
```

## Verify A Full Development Environment

These checks are Linux-oriented. The Docker-based development environment is not part of the supported macOS path.

Unit tests:

```bash
make test-unit
```

Docker image and multi-node lab:

```bash
make docker-build
make test-single-hop
```
