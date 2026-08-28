#!/bin/sh
set -eu

# Run Phrona's REST server internally on 8082. Its MCP Streamable HTTP server
# remains internal on 8081. Nginx exposes both through public port 8080.
export PHRONA_ADDR="${PHRONA_ADDR_INTERNAL:-127.0.0.1:8082}"
export PHRONA_SERVER_MCP_ADDR="${PHRONA_SERVER_MCP_ADDR:-127.0.0.1:8081}"

/usr/local/bin/phrona serve &
PHRONA_PID=$!

cleanup() {
    kill -TERM "$PHRONA_PID" 2>/dev/null || true
}
trap cleanup INT TERM EXIT

exec nginx -c /etc/nginx/nginx.conf -g 'daemon off;'
