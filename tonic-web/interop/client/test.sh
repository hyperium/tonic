#!/bin/bash

set -e

HOST=${1:-"localhost"}

npm test -- --host="$HOST"
npm test -- --mode=binary --host="$HOST"