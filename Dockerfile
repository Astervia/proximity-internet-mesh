# ── Stage 1: Build ─────────────────────────────────────────────────────────────
FROM rust:1.94-bookworm AS builder

WORKDIR /build

# Copy manifests first so dependency fetching is cached independently of source.
COPY Cargo.toml Cargo.lock ./
COPY crates/pim-bluetooth/Cargo.toml  crates/pim-bluetooth/
COPY crates/pim-core/Cargo.toml       crates/pim-core/
COPY crates/pim-crypto/Cargo.toml     crates/pim-crypto/
COPY crates/pim-protocol/Cargo.toml   crates/pim-protocol/
COPY crates/pim-transport/Cargo.toml  crates/pim-transport/
COPY crates/pim-tun/Cargo.toml        crates/pim-tun/
COPY crates/pim-gateway/Cargo.toml    crates/pim-gateway/
COPY crates/pim-routing/Cargo.toml    crates/pim-routing/
COPY crates/pim-discovery/Cargo.toml  crates/pim-discovery/
COPY crates/pim-wifidirect/Cargo.toml crates/pim-wifidirect/
COPY crates/pim-daemon/Cargo.toml     crates/pim-daemon/
COPY crates/pim-cli/Cargo.toml        crates/pim-cli/

# Stub sources so `cargo fetch` and the dependency compile step can run.
RUN for crate in pim-bluetooth pim-core pim-crypto pim-protocol pim-transport pim-tun \
        pim-gateway pim-routing pim-discovery pim-wifidirect; do \
        mkdir -p crates/$crate/src && \
        printf 'pub fn _stub() {}' > crates/$crate/src/lib.rs; \
    done && \
    mkdir -p crates/pim-daemon/src && printf 'fn main() {}' > crates/pim-daemon/src/main.rs && \
    mkdir -p crates/pim-daemon/src && \
        printf 'pub mod rate_limiter; pub mod reputation; pub mod send_buffer;\n' \
        >> crates/pim-daemon/src/main.rs && \
    touch crates/pim-daemon/src/rate_limiter.rs \
          crates/pim-daemon/src/reputation.rs \
          crates/pim-daemon/src/send_buffer.rs && \
    mkdir -p crates/pim-cli/src && printf 'fn main() {}' > crates/pim-cli/src/main.rs

RUN cargo build --release 2>/dev/null; true

# Copy real sources and rebuild (only changed crates are recompiled).
COPY crates/ crates/
RUN touch crates/*/src/*.rs && cargo build --release

# ── Stage 2: Runtime ───────────────────────────────────────────────────────────
FROM debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
        iptables \
        iproute2 \
        iputils-ping \
        curl \
        dnsutils \
        tcpdump \
        netcat-openbsd \
        ca-certificates \
        procps \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/pim-daemon /usr/local/bin/pim-daemon
COPY --from=builder /build/target/release/pim        /usr/local/bin/pim

COPY docker/entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

RUN mkdir -p /etc/pim /var/lib/pim /run

EXPOSE 9100/tcp

ENTRYPOINT ["/entrypoint.sh"]
