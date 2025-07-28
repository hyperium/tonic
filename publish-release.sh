#!/usr/bin/env bash

# Script which automates publishing a crates.io release of the prost crates.

set -ex

if [ "$#" -ne 0 ]
then
  echo "Usage: $0"
  exit 1
fi

DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" >/dev/null 2>&1 && pwd )"

CRATES=( \
  "tonic" \
  "tonic-build" \
  "tonic-types" \
  "tonic-reflection" \
  "tonic-health" \
  "tonic-web" \
  "tonic-prost" \
  "tonic-prost-build" \
)

for CRATE in "${CRATES[@]}"; do
  pushd "$DIR/$CRATE"

  echo "Publishing $CRATE"

  cargo publish

  popd
done
