#!/usr/bin/env bash
# Stops an official Cloud Firestore emulator the action started: reports the most resident memory
# it held (Linux keeps the peak resident set as VmHWM), asks it to shut down with `POST /shutdown`
# and waits up to 30 s for it to exit. One that is still running then gets SIGTERM, and SIGKILL
# 10 s later; that is a warning, not a failure (v1.21.0 was still running 30 s later on a
# runner).
#
# Environment: ADDRESS and PID, the action's outputs. Appends a line to $GITHUB_STEP_SUMMARY when
# it is set.
set -euo pipefail

if peak=$(awk '/^VmHWM:/ { printf "%.0f", $2 / 1024 }' "/proc/$PID/status" 2> /dev/null) && [[ -n "$peak" ]]; then
  echo "Its peak resident memory was $peak MiB." | tee -a "${GITHUB_STEP_SUMMARY:-/dev/null}"
fi
curl --fail --silent --show-error --request POST "http://$ADDRESS/shutdown" || true
if ! timeout 30 tail --pid="$PID" -f /dev/null; then
  echo "::warning title=Official emulator::It was still running 30 s after POST /shutdown; stopping it with SIGTERM"
  kill "$PID" 2> /dev/null || true
  if ! timeout 10 tail --pid="$PID" -f /dev/null; then
    kill -9 "$PID" 2> /dev/null || true
  fi
fi
