#!/usr/bin/env bash

# Script which automates modifying source version fields, and creating a release
# commit and tag. The commit and tag are not automatically pushed, nor are the
# crates published (see publish-release.sh).

set -ex

if [ "$#" -ne 1 ]
then
  echo "Usage: $0 <version>"
  exit 1
fi

DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" >/dev/null 2>&1 && pwd )"
VERSION="$1"
MINOR="$( echo "${VERSION}" | cut -d\. -f1-2 )"

VERSION_MATCHER="([a-z0-9\\.-]+)"
TONIC_CRATE_MATCHER="(tonic|tonic-[a-z]+)"

# Update the README.md.
sed -i -E "s/${TONIC_CRATE_MATCHER} = \"${VERSION_MATCHER}\"/\1 = \"${MINOR}\"/" "$DIR/examples/helloworld-tutorial.md"
sed -i -E "s/${TONIC_CRATE_MATCHER} = \"${VERSION_MATCHER}\"/\1 = \"${MINOR}\"/" "$DIR/examples/routeguide-tutorial.md"

CRATES=( \
  "tonic" \
  "tonic-build" \
  "tonic-types" \
  "tonic-reflection" \
  "tonic-health" \
  "tonic-web" \
)

for CRATE in "${CRATES[@]}"; do
  # Update Cargo.toml version fields.
  sed -i -E "s/^version = \"${VERSION_MATCHER}\"$/version = \"${VERSION}\"/" \
    "$DIR/$CRATE/Cargo.toml"
done
