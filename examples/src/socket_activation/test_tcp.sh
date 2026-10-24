#!/usr/bin/env bash
# Smoke test for the tonic systemd socket-activation example over TCP.

set -u

ADDR="[::1]:50051"
UNIT_DIR="${XDG_CONFIG_HOME:-${HOME}/.config}/systemd/user"
SOCKET_UNIT="socketact-tcp.socket"
SERVICE_UNIT="socketact-tcp.service"

clean() {
  echo "Cleaning..."
  systemctl --user stop "${SOCKET_UNIT}" 2>/dev/null
  systemctl --user stop "${SERVICE_UNIT}" 2>/dev/null
  rm -f "${UNIT_DIR}/${SOCKET_UNIT}" "${UNIT_DIR}/${SERVICE_UNIT}"
  systemctl --user daemon-reload 2>/dev/null
}

fail() {
  clean
  echo "FAIL: $*" >&2
  exit 1
}

pass() {
  echo "SUCCESS: $1"
}

command -v systemctl >/dev/null 2>&1 \
  || fail "systemctl not found (install systemd)"
systemctl --user show-environment >/dev/null 2>&1 \
  || fail "no systemd user instance available (needs a user session bus)"

echo "Building example binaries..."
cargo build --bin socket-activation-tcp-server --bin socket-activation-tcp-client \
  || fail "Failed to build example binaries"

SERVER_BIN="$(cargo metadata --format-version 1 --no-deps \
  | grep -o '"target_directory":"[^"]*"' | head -n1 | cut -d'"' -f4)/debug/socket-activation-tcp-server"
[[ -x "${SERVER_BIN}" ]] || fail "Server binary not found at ${SERVER_BIN}"

# Install user units. systemd owns the listening socket; the service inherits it
# as fd 3 when it is activated on the first client connection.
echo "Installing user units into ${UNIT_DIR}..."
mkdir -p "${UNIT_DIR}"

cat > "${UNIT_DIR}/${SOCKET_UNIT}" <<EOF
[Unit]
Description=tonic socket-activation TCP example socket

[Socket]
ListenStream=${ADDR}
ReusePort=true

[Install]
WantedBy=sockets.target
EOF

cat > "${UNIT_DIR}/${SERVICE_UNIT}" <<EOF
[Unit]
Description=tonic socket-activation TCP example service

[Service]
ExecStart=${SERVER_BIN}
EOF

systemctl --user daemon-reload || fail "daemon-reload failed"
systemctl --user start "${SOCKET_UNIT}" || fail "Failed to start ${SOCKET_UNIT}"

# The socket is now listening but the server has NOT started yet: on-demand
# activation means the service stays inactive until the first connection.
[[ "$(systemctl --user is-active "${SOCKET_UNIT}")" == "active" ]] \
  || fail "${SOCKET_UNIT} did not become active"
[[ "$(systemctl --user is-active "${SERVICE_UNIT}")" == "inactive" ]] \
  || fail "${SERVICE_UNIT} should be inactive before the first connection"
echo "Socket active, service still inactive."

echo "Running client (this triggers activation)..."
OUTPUT="$(cargo run --bin socket-activation-tcp-client 2>/dev/null)" \
  || fail "Client failed to run"

echo "${OUTPUT}" | grep -q "Hello" || fail "Response not received (got: ${OUTPUT})"

# The connection should have activated the service.
[[ "$(systemctl --user is-active "${SERVICE_UNIT}")" == "active" ]] \
  || fail "${SERVICE_UNIT} should be active after the client connected"
echo "Service activated on demand by the client connection."

pass "Response received: ${OUTPUT}"
clean
