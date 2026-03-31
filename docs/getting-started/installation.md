# Installation

This project is currently easiest to run on Linux because the daemon creates a TUN interface, manipulates routes, and may install NAT rules.

## Requirements

Host requirements:

- Linux with `/dev/net/tun`
- Rust toolchain when installing from source
- `iproute2`
- `iptables`
- privileges to create `pim0` and update routing, usually `root` or `CAP_NET_ADMIN`

Optional but strongly recommended:

- Docker Engine
- Docker Compose v2

## Install From GitHub Releases

Published releases include a prebuilt Linux x86_64 tarball containing `pim` and `pim-daemon`.

```bash
VERSION="v0.1.1"
ASSET="pim-${VERSION}-x86_64-unknown-linux-gnu"
curl -LO "https://github.com/Astervia/proximity-internet-mesh/releases/download/${VERSION}/${ASSET}.tar.gz"
curl -LO "https://github.com/Astervia/proximity-internet-mesh/releases/download/${VERSION}/${ASSET}.sha256"
sed -i 's#dist/##' "${ASSET}.sha256"
sha256sum -c "${ASSET}.sha256"
tar -xzf "${ASSET}.tar.gz"
sudo install -Dm755 "${ASSET}/pim" /usr/local/bin/pim
sudo install -Dm755 "${ASSET}/pim-daemon" /usr/local/bin/pim-daemon
sudo install -d /etc/pim /var/lib/pim /run
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
sudo install -d /etc/pim /var/lib/pim /run
```

If you prefer not to install globally, you can run the binaries directly from `target/release/`, but `pim` must still be able to find `pim-daemon` either beside it or on `PATH`.

## Prepare A Config File

Create `/etc/pim/pim.toml`. Start from the examples in [configuration.md](configuration.md).

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
