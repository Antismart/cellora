# syntax=docker/dockerfile:1
#
# Multi-stage build producing one slim image that ships both Cellora binaries:
#   * cellora-api      — REST + GraphQL gateway
#   * cellora-indexer  — CKB block poller
#
# Pick which to run via the container command:
#   docker run --rm cellora cellora-api
#   docker run --rm cellora cellora-indexer
#
# The build is fully offline: SQLx compile-time query checking reads the
# committed .sqlx/ cache, so no database is needed to build the image.

# ---- Stage 1: build ----------------------------------------------------------
FROM rust:1.82-bookworm AS builder

WORKDIR /build

# Read cached SQLx query metadata instead of connecting to a database.
ENV SQLX_OFFLINE=true

# Copy the whole workspace. .dockerignore keeps the context small (no target/,
# no .git/). migrations/ must be present: the indexer embeds it via
# sqlx::migrate!, and the .sqlx cache is required for offline query checking.
COPY . .

# Build both release binaries. BuildKit cache mounts persist the cargo registry
# and the target directory across builds for fast incremental rebuilds; the
# finished binaries are copied out before the target cache mount is unmounted.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build --release --locked \
        --bin cellora-api --bin cellora-indexer && \
    mkdir -p /out && \
    cp target/release/cellora-api target/release/cellora-indexer /out/

# ---- Stage 2: runtime --------------------------------------------------------
FROM debian:bookworm-slim AS runtime

# ca-certificates: outbound TLS to the CKB node, GitHub OAuth, and webhook
# endpoints. No OpenSSL runtime is needed — every TLS client is rustls.
RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*

# Run as an unprivileged system user.
RUN useradd --system --uid 10001 --user-group --no-create-home cellora

COPY --from=builder /out/cellora-api /out/cellora-indexer /usr/local/bin/

USER cellora

# REST + GraphQL gateway. The indexer exposes its metrics server on a
# separately configured port; publish whatever you map at run time.
EXPOSE 8080

# Default to the API; override the command with `cellora-indexer` to poll.
CMD ["cellora-api"]
