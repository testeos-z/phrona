# Multi-stage build: Rust release binary + frontend assets.
FROM rust:1.97-bookworm AS builder

# btls-sys builds BoringSSL from source and requires a native C/C++ toolchain
# plus CMake. Keep these dependencies in the builder stage only so the final
# runtime image remains small.
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential \
    clang \
    cmake \
    pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY . .
# The MCP HTTP feature adds transitive dependencies that are not yet present
# in the fork's Cargo.lock. Allow Cargo to refresh the lock during the image
# build so the native Streamable HTTP transport can be validated/deployed.
RUN cargo build --release -p phrona-cli && \
    cp target/release/phrona /phrona

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl && \
    useradd --create-home --shell /usr/sbin/nologin phrona && \
    rm -rf /var/lib/apt/lists/*
COPY --from=builder /phrona /usr/local/bin/phrona
COPY --from=builder /build/crates/phrona-api/assets /usr/share/phrona/frontend
ENV PHRONA_ADDR=0.0.0.0:8080
ENV PHRONA_FRONTEND_DIR=/usr/share/phrona/frontend
ENV HOME=/home/phrona
EXPOSE 8080 8081
# Run as an unprivileged user: the API never writes to the filesystem, so
# the least-privileged identity is the safest default.
USER phrona
WORKDIR /home/phrona
HEALTHCHECK --interval=30s --timeout=3s --retries=3 CMD curl -f http://localhost:8080/health || exit 1
ENTRYPOINT ["/usr/local/bin/phrona", "serve"]
