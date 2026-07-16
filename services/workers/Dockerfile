# Build context is the repo root (see docker-compose.yml) — the worker is a
# workspace member and needs every crate it depends on plus the fixtures/
# offline base.
FROM rust:1.96-bookworm AS builder
RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake build-essential \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /build
COPY . .
RUN cargo build --release -p workers

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/workers /usr/local/bin/workers
COPY --from=builder /build/fixtures /app/fixtures
WORKDIR /app

# LES_WORKER_DATA_DIR is the worker's own DuckDB — never mount this into the
# api container (DuckDB is single-writer-per-file). LES_PUBLISH_DIR is the
# only handoff surface; docker-compose.yml mounts it into both containers.
ENV LES_FIXTURES_DIR=/app/fixtures \
    LES_WORKER_DATA_DIR=/data \
    LES_PUBLISH_DIR=/publish

ENTRYPOINT ["/usr/local/bin/workers"]
