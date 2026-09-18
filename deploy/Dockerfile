# syntax=docker/dockerfile:1.7
FROM rust:1.94-bookworm AS builder

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY backend ./backend
RUN cargo build --locked --release -p network-atlas

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl openssh-client sqlite3 tini \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 11001 network-atlas \
    && useradd --system --uid 11001 --gid network-atlas --home /var/lib/network-atlas network-atlas

COPY --from=builder /src/target/release/network-atlas /usr/local/bin/network-atlas
COPY frontend /opt/network-atlas/frontend

RUN mkdir -p /var/lib/network-atlas /opt/network-atlas/frontend \
    && chown -R network-atlas:network-atlas /var/lib/network-atlas /opt/network-atlas

ENV NETWORK_ATLAS_BIND=0.0.0.0:8787 \
    NETWORK_ATLAS_DATA_DIR=/var/lib/network-atlas \
    NETWORK_ATLAS_DATABASE_URL=sqlite:///var/lib/network-atlas/network-atlas.db?mode=rwc \
    NETWORK_ATLAS_FRONTEND_DIR=/opt/network-atlas/frontend \
    NETWORK_ATLAS_AUTH_MODE=required \
    NETWORK_ATLAS_COOKIE_SECURE=true \
    RUST_LOG=network_atlas=info

USER network-atlas:network-atlas
EXPOSE 8787
VOLUME ["/var/lib/network-atlas"]
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/network-atlas"]
