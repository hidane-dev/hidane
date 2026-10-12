#!/bin/sh
# Runs firebase-js-sdk's Firestore browser integration tests (`test:browser:emulator`, Vitest in
# Playwright's Chromium) against an emulator on localhost:<port>, and writes Vitest's JSON report.
#
#   tools/js-sdk-browser/run.sh <firebase-js-sdk checkout> <port> <report.json>
#
# The checkout must be installed and its dependencies built (see README.md). The emulator must
# be running; the tests use the project `test-emulator` and the databases `(default)`, `db1` and
# `db2`. Failing tests do not fail this script: compare.mjs decides. A missing report does.
set -eu

sdk=$(cd "$1" && pwd)
port=$2
report=$(cd "$(dirname "$3")" && pwd)/$(basename "$3")
here=$(cd "$(dirname "$0")" && pwd)

# The package's Vitest configuration, with cleanup.ts as one more setup file.
cp "$here/vitest.config.mjs" "$sdk/packages/firestore/vitest.hidane.config.mjs"
cp "$here/cleanup.ts" "$sdk/packages/firestore/test/hidane_cleanup.ts"
# The tests import a project configuration, which they do not use against an emulator; upstream
# CI copies this one.
[ -f "$sdk/config/project.json" ] || cp "$sdk/config/ci.config.json" "$sdk/config/project.json"

rm -f "$report"
cd "$sdk/packages/firestore"
# `test:browser:emulator` is `FIRESTORE_TARGET_BACKEND=emulator vitest run --project=browser`;
# test/integration leaves out the unit tests it also runs, which never reach an emulator.
FIRESTORE_TARGET_BACKEND=emulator FIRESTORE_EMULATOR_PORT=$port \
  npx vitest run --project=browser --config vitest.hidane.config.mjs \
  --reporter=default --reporter=json --outputFile.json="$report" test/integration || true

if [ ! -s "$report" ]; then
  echo "run.sh: Vitest wrote no report" >&2
  exit 1
fi
