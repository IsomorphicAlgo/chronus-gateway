# syntax=docker/dockerfile:1
# Build context: **repository root** (parent of `demo/`), so `COPY . .` is the full chronus-gateway tree.
# Ephemerust is cloned next to this crate inside the image (`../Ephemerust` matches Cargo.toml path).

FROM rust:1.89-bookworm AS builder
RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config libssl-dev git \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /workspace
# Same upstream source CI uses (see `.github/workflows/ci.yml`).
RUN git clone --depth 1 https://github.com/IsomorphicAlgo/Ephemerust.git Ephemerust

WORKDIR /workspace/chronus-gateway
COPY . .
RUN cargo build --release -p chronus-gateway -p chronus-hil-sim

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /workspace/chronus-gateway/target/release/chronus-gateway /usr/local/bin/
COPY --from=builder /workspace/chronus-gateway/target/release/chronus-hil-sim /usr/local/bin/

WORKDIR /
# Config: set `CHRONUS_GATEWAY_CONFIG` (see `demo/docker-compose.yml` and `docs/DEMO.md`).
CMD ["chronus-gateway"]
