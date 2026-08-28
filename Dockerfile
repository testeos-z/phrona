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
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl nginx && \
    useradd --create-home --shell /usr/sbin/nologin phrona && \
    mkdir -p /tmp/client_body /tmp/proxy /tmp/fastcgi /tmp/uwsgi /tmp/scgi && \
    chown -R phrona:phrona /tmp/client_body /tmp/proxy /tmp/fastcgi /tmp/uwsgi /tmp/scgi && \
    rm -rf /var/lib/apt/lists/*
COPY --from=builder /phrona /usr/local/bin/phrona
COPY --from=builder /build/crates/phrona-api/assets /usr/share/phrona/frontend
COPY docker/nginx.conf /etc/nginx/nginx.conf
COPY docker/entrypoint.sh /usr/local/bin/phrona-entrypoint
RUN chmod +x /usr/local/bin/phrona-entrypoint

# Nginx is the only public listener. It proxies Web/REST to Phrona on 8082
# and MCP Streamable HTTP /mcp to Phrona on 8081.
ENV PHRONA_ADDR_INTERNAL=127.0.0.1:8082
ENV PHRONA_SERVER_MCP_ADDR=127.0.0.1:8081
ENV PHRONA_FRONTEND_DIR=/usr/share/phrona/frontend
ENV HOME=/home/phrona
EXPOSE 8080

USER phrona
WORKDIR /home/phrona
HEALTHCHECK --interval=30s --timeout=3s --retries=3 CMD curl -f http://localhost:8080/health || exit 1
ENTRYPOINT ["/usr/local/bin/phrona-entrypoint"]
