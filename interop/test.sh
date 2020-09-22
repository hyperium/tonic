#!/usr/bin/env bash

set -eu
set -o pipefail

# the go client does not support passing an argument with multiple test cases
# so we loop over this array calling the binary each time around
TEST_CASES=(
  "empty_unary"
  "large_unary"
  "client_streaming"
  "server_streaming"
  "ping_pong"
  "empty_stream"
  "status_code_and_message"
  "special_status_message"
  "custom_metadata"
  "unimplemented_method"
  "unimplemented_service"
)

# join all test cases in one comma separated string (dropping the first one)
# so we can call the rust client only once, reducing the noise
JOINED_TEST_CASES=$(printf ",%s" "${TEST_CASES[@]}")
JOINED_TEST_CASES="${JOINED_TEST_CASES:1}"

set -x

echo "Running for OS: ${OSTYPE}"

case "$OSTYPE" in
  darwin*)  OS="darwin"; EXT="" ;;
  linux*)   OS="linux"; EXT="" ;;
  msys*)    OS="windows"; EXT=".exe" ;;
  *)        exit 2 ;;
esac

ARG="${1:-""}"


(cd interop && cargo build --bins)

SERVER="interop/bin/server_${OS}_amd64${EXT}"

TLS_CA="interop/data/ca.pem"
TLS_CRT="interop/data/server1.pem"
TLS_KEY="interop/data/server1.key"

# run the test server
./"${SERVER}" ${ARG} --tls_cert_file $TLS_CRT --tls_key_file $TLS_KEY &
SERVER_PID=$!
echo ":; started grpc-go test server."

# trap exits to make sure we kill the server process when the script exits,
# regardless of why (errors, SIGTERM, etc).
trap 'echo ":; killing test server"; kill ${SERVER_PID};' EXIT

sleep 1

./target/debug/client --test_case="${JOINED_TEST_CASES}" ${ARG}

echo ":; killing test server"; kill ${SERVER_PID};

# run the test server
./target/debug/server ${ARG} &
SERVER_PID=$!
echo ":; started tonic test server."

# trap exits to make sure we kill the server process when the script exits,
# regardless of why (errors, SIGTERM, etc).
trap 'echo ":; killing test server"; kill ${SERVER_PID};' EXIT

sleep 1

./target/debug/client --test_case="${JOINED_TEST_CASES}" ${ARG}

TLS_ARGS=""

if [ -n "${ARG}" ]; then
  TLS_ARGS="--use_tls --use_test_ca --server_host_override=foo.test.google.fr --ca_file=${TLS_CA}"
fi

for CASE in "${TEST_CASES[@]}"; do
  interop/bin/client_${OS}_amd64${EXT} --test_case="${CASE}" ${TLS_ARGS}
done
