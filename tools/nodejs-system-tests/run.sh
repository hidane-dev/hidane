#!/bin/sh
# Runs the system tests of nodejs-firestore (google-cloud-node's handwritten/firestore, as
# `system-test:emulator:grpc` and `system-test:emulator:rest` do) against an emulator on
# 127.0.0.1:<port>, over gRPC (the default) or REST, and writes Mocha's JSON report.
#
#   tools/nodejs-system-tests/run.sh <google-cloud-node checkout> <port> <report.json> [grpc|rest]
#
# The checkout must be set up (setup.sh) and the emulator running; the tests use the project
# `demo-system` and the `(default)` database. Over REST, metadata.py stands in for the metadata
# server, so that the SDK finds credentials (the token `owner`) without reaching Google. Failing
# tests do not fail this script: compare.mjs decides. A missing report does.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
package=$(cd "$1/handwritten/firestore" && pwd)
port=$2
report=$(cd "$(dirname "$3")" && pwd)/$(basename "$3")
transport=${4:-grpc}

rm -f "$report"
case $transport in
  grpc) ;;
  rest)
    ports=$(mktemp)
    python3 -I "$here/metadata.py" > "$ports" &
    metadata=$!
    trap 'kill "$metadata" 2> /dev/null; wait "$metadata" 2> /dev/null || :; rm -f "$ports"' EXIT
    tries=0
    until [ -s "$ports" ]; do
      tries=$((tries + 1))
      if [ "$tries" -gt 100 ]; then
        echo "run.sh: the metadata server did not start" >&2
        exit 1
      fi
      sleep 0.1
    done
    export FIRESTORE_PREFER_REST=true METADATA_SERVER_DETECTION=assume-present
    GCE_METADATA_HOST=127.0.0.1:$(head -n 1 "$ports")
    export GCE_METADATA_HOST
    ;;
  *)
    echo "run.sh: the transport is grpc or rest, not $transport" >&2
    exit 2
    ;;
esac

cd "$package"
# `system-test:emulator:grpc` runs `../../bin/run-test.cjs build/system-test --timeout 1200000`
# with FIRESTORE_EMULATOR_HOST=localhost:8080: Mocha under c8. Mocha alone, with a timeout per
# test of a minute, gives the same outcomes and a report.
FIRESTORE_EMULATOR_HOST=127.0.0.1:$port GCLOUD_PROJECT=demo-system \
  MOCHA_REPORTER=json MOCHA_REPORTER_OUTPUT="$report" \
  node_modules/.bin/mocha build/system-test --timeout 60000 > /dev/null || true

if [ ! -s "$report" ]; then
  echo "run.sh: Mocha wrote no report" >&2
  exit 1
fi
