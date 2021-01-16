set -e

# This script updates server and client go binaries for interop tests.
# It clones grpc-go, compiles interop clients and servers for linux, windows
# and macos and finally deletes the cloned repo.
#
# It is not meant to be executed on every test run or CI and should run from
# inside tonic/interop.

command -v go >/dev/null 2>&1 || {
  echo >&2 "go executable is not available"
  exit 1
}

if [ ! -d "./grpc-go" ]; then
  git clone https://github.com/grpc/grpc-go.git
fi

cd grpc-go

PLATFORMS="darwin linux windows"
ROLES="client server"
ARCH=amd64

for ROLE in $ROLES; do
  for OS in $PLATFORMS; do
    FILENAME="${ROLE}_${OS}_${ARCH}"
    if [[ "${OS}" == "windows" ]]; then FILENAME="${FILENAME}.exe"; fi
    GOOS=$OS GOARCH=$ARCH go build -o "../bin/$FILENAME" "./interop/$ROLE"
  done
done

rm -rf ../grpc-go