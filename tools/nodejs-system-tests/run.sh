#!/bin/sh
# Runs the system tests of nodejs-firestore (google-cloud-node's handwritten/firestore, as
# `system-test:emulator:grpc` does) against an emulator on 127.0.0.1:<port>, over gRPC, and
# writes Mocha's JSON report.
#
#   tools/nodejs-system-tests/run.sh <google-cloud-node checkout> <port> <report.json>
#
# The checkout must be set up (setup.sh) and the emulator running; the tests use the project
# `demo-system` and the `(default)` database. Failing tests do not fail this script: compare.mjs
# decides. A missing report does.
set -eu

package=$(cd "$1/handwritten/firestore" && pwd)
port=$2
report=$(cd "$(dirname "$3")" && pwd)/$(basename "$3")

rm -f "$report"
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
