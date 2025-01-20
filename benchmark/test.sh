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

(cd benchmark && cargo build --bins)

WORKER_PORT=50056

# run the worker.
./target/debug/worker --driver_port="${WORKER_PORT}" &
WORKER_PID=$!
echo ":; started worker on port ${WORKER_PORT}."

# trap exits to make sure we kill the worker process when the script exits,
# regardless of why (errors, SIGTERM, etc).
trap 'echo ":; killing  worker"; kill ${WORKER_PID};' EXIT

sleep 1

# run the tester.
echo ":; starting tester."
./target/debug/tester --worker_port="${WORKER_PORT}"
