# Build context is the repo root (see docker-compose.yml) — the api is a
# workspace member and needs every crate it depends on.
FROM rust:1.96-bookworm AS builder
RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake build-essential \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /build
COPY . .
RUN cargo build --release -p api

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/api /usr/local/bin/api

# Never a .duckdb path here — LES_PUBLISH_DIR is Parquet only, mounted
# read-only in docker-compose.yml (DuckDB is single-writer-per-file).
ENV LES_API_BIND=0.0.0.0:8080
EXPOSE 8080

ENTRYPOINT ["/usr/local/bin/api"]
