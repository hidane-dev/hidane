#!/usr/bin/env bash
# Runs the end-to-end tests against the emulators.
set -euo pipefail

npx firebase emulators:exec --only firestore,auth \
  "npx playwright test"
