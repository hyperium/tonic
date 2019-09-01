#!/usr/bin/env bash

set -eu
set -o pipefail

SERVER="tonic-interop/bin/darwin/server"

# run the test server
./"${SERVER}" &
SERVER_PID=$!
echo ":; started grpc-go test server."

# trap exits to make sure we kill the server process when the script exits,
# regardless of why (errors, SIGTERM, etc).
trap 'echo ":; killing test server"; kill ${SERVER_PID};' EXIT

 cargo run -p tonic-interop --bin client -- \
 --test_case=empty_unary,large_unary,client_streaming,server_streaming,ping_pong,\
empty_stream,status_code_and_message,special_status_message,unimplemented_method,\
unimplemented_service,custom_metadata
