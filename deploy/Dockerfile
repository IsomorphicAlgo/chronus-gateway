# syntax=docker/dockerfile:1
# Production image for the gateway alone (Tranche 4, Methodology D-053). Build from the repository root:
#
#   docker build -f deploy/Dockerfile -t chronus-gateway .
#   docker build -f deploy/Dockerfile --build-arg FEATURES=refresh -t chronus-gateway:refresh .
#
# Run (the API needs a token because the container listens on all interfaces; see docs/DEPLOYMENT.md):
#
#   docker run -d --name chronus -p 127.0.0.1:8080:8080 -p 7301:7301/udp \
#     -e CHRONUS_API_TOKENS="$(chronus-gateway --generate-token)" \
#     -v chronus-data:/var/lib/chronus chronus-gateway
#
# The demo image with the HIL simulator is demo/Dockerfile; this one carries only the gateway.

ARG RUST_VERSION=1.89

FROM rust:${RUST_VERSION}-bookworm AS builder
WORKDIR /src
COPY . .
# Optional cargo features, e.g. FEATURES=refresh (fetch TLE / Earth orientation data itself).
ARG FEATURES=""
RUN cargo build --release --locked -p chronus-gateway ${FEATURES:+--features "$FEATURES"} \
    && mkdir -p /out/var/lib/chronus \
    && cp target/release/chronus-gateway /out/chronus-gateway

# Distroless: glibc and CA certificates, no shell or package manager, runs as uid 65532.
FROM gcr.io/distroless/cc-debian12:nonroot AS runtime
COPY --from=builder /out/chronus-gateway /usr/local/bin/chronus-gateway
COPY --from=builder --chown=65532:65532 /out/var/lib/chronus /var/lib/chronus
COPY deploy/gateway.container.toml /etc/chronus/gateway.toml
ENV CHRONUS_GATEWAY_CONFIG=/etc/chronus/gateway.toml \
    CHRONUS_LOG_FORMAT=json \
    RUST_LOG=info
USER 65532:65532
WORKDIR /var/lib/chronus
VOLUME ["/var/lib/chronus"]
EXPOSE 7301/udp 8080/tcp
# No HEALTHCHECK: the image has no shell or curl. Point your orchestrator's HTTP probe at /health.
ENTRYPOINT ["/usr/local/bin/chronus-gateway"]
