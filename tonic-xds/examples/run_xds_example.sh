#!/usr/bin/env bash
# Run the tonic-xds example: xDS server + greeter backend + channel client.
set -euo pipefail

prefix() {
    local tag="$1"
    sed -u "s/^/[$tag] /"
}

cleanup() {
    echo "Shutting down..."
    kill "$GREETER_PID" "$XDS_PID" 2>/dev/null || true
}
trap cleanup EXIT

# 1. Start greeter backend
PORT=50051 cargo run -p tonic-xds --example greeter_server --features testutil 2>&1 | prefix "greeter" &
GREETER_PID=$!

# 2. Start xDS control plane
cargo run -p tonic-xds --example xds_server 2>&1 | prefix "xds" &
XDS_PID=$!

# Wait for servers to be ready
sleep 2

# 3. Run xDS-aware client
GRPC_XDS_BOOTSTRAP_CONFIG='{"xds_servers":[{"server_uri":"http://localhost:18000"}],"node":{"id":"test"}}' \
    cargo run -p tonic-xds --example channel --features testutil 2>&1 | prefix "client"
