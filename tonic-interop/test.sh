#!/usr/bin/env bash

set -eu
set -o pipefail

set -x

echo "Running for OS: ${OSTYPE}"

case "$OSTYPE" in
  darwin*)  OS="darwin"; EXT="" ;;
  linux*)   OS="linux"; EXT="" ;;
  msys*)    OS="windows"; EXT=".exe" ;;
  *)        exit 2 ;;
esac

ARG="${1:-""}"
TLS_PROVIDER="${2:-""}"

if [[ -n "${TLS_PROVIDER}" ]] ; then
  FEATURES="--features ${TLS_PROVIDER}"
else
  FEATURES=
fi

(cd tonic-interop && cargo build --bins ${FEATURES})

SERVER="tonic-interop/bin/server_${OS}_amd64${EXT}"

# TLS_CA="tonic-interop/data/ca.pem"
TLS_CRT="tonic-interop/data/server1.pem"
TLS_KEY="tonic-interop/data/server1.key"

# run the test server
./"${SERVER}" ${ARG} --tls_cert_file $TLS_CRT --tls_key_file $TLS_KEY &
SERVER_PID=$!
echo ":; started grpc-go test server."

# trap exits to make sure we kill the server process when the script exits,
# regardless of why (errors, SIGTERM, etc).
trap 'echo ":; killing test server"; kill ${SERVER_PID};' EXIT

sleep 1

./target/debug/client \
 --test_case=empty_unary,large_unary,client_streaming,server_streaming,ping_pong,\
empty_stream,status_code_and_message,special_status_message,unimplemented_method,\
unimplemented_service,custom_metadata ${ARG}

echo ":; killing test server"; kill ${SERVER_PID};

# run the test server
./target/debug/server ${ARG} &
SERVER_PID=$!
echo ":; started tonic test server."

# trap exits to make sure we kill the server process when the script exits,
# regardless of why (errors, SIGTERM, etc).
trap 'echo ":; killing test server"; kill ${SERVER_PID};' EXIT

sleep 1

./target/debug/client \
--test_case=empty_unary,large_unary,client_streaming,server_streaming,ping_pong,\
empty_stream,status_code_and_message,special_status_message,unimplemented_method,\
unimplemented_service,custom_metadata ${ARG}
