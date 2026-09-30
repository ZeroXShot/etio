# syntax=docker/dockerfile:1.7
#
# Etio server image: `docker build -t etio .`
#
# Three stages: the web UI (Node), the server (Rust, statically linked
# SQLite, rustls: no OpenSSL), and a distroless runtime running as non-root.

ARG RUST_VERSION=1.98
ARG NODE_VERSION=24

FROM --platform=$BUILDPLATFORM node:${NODE_VERSION}-bookworm-slim AS ui
WORKDIR /ui
COPY ui/package.json ui/package-lock.json ./
RUN --mount=type=cache,target=/root/.npm npm ci --no-audit --no-fund
COPY ui/ ./
RUN npm run build

FROM rust:${RUST_VERSION}-bookworm AS build
# Parallel compile jobs (lower it on small machines).
ARG CARGO_BUILD_JOBS=4
ENV CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS} CARGO_TERM_COLOR=never
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/src/target,sharing=locked \
    cargo build --release --locked -p etio-server \
    && install -D target/release/etio /out/etio \
    && mkdir -p /out/data

FROM gcr.io/distroless/cc-debian12:nonroot
LABEL org.opencontainers.image.title="etio" \
      org.opencontainers.image.description="OpenTelemetry-native streaming root-cause analysis" \
      org.opencontainers.image.licenses="Apache-2.0" \
      org.opencontainers.image.source="https://github.com/ZeroXShot/etio"
COPY --from=build /out/etio /usr/local/bin/etio
COPY --from=build --chown=65532:65532 /out/data /var/lib/etio
COPY --from=ui /ui/dist /usr/share/etio/ui
ENV ETIO__UI__DIR=/usr/share/etio/ui \
    ETIO__STORAGE__DIR=/var/lib/etio \
    ETIO__LOG__FORMAT=json
VOLUME ["/var/lib/etio"]
# OTLP/gRPC, OTLP/HTTP, API + UI, edge-to-core summaries.
EXPOSE 4317 4318 7070 7071
USER 65532:65532
HEALTHCHECK --interval=15s --timeout=5s --start-period=10s CMD ["/usr/local/bin/etio", "health"]
ENTRYPOINT ["/usr/local/bin/etio"]
CMD ["serve"]
