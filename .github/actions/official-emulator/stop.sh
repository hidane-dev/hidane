#!/usr/bin/env bash
# Stops an official Cloud Firestore emulator the action started: reports the most resident memory
# it held (Linux keeps the peak resident set as VmHWM), asks it to shut down with `POST /shutdown`
# (or kills it), and waits up to 30 s for it to exit.
#
# Environment: ADDRESS and PID, the action's outputs. Appends a line to $GITHUB_STEP_SUMMARY when
# it is set.
set -euo pipefail

if peak=$(awk '/^VmHWM:/ { printf "%.0f", $2 / 1024 }' "/proc/$PID/status" 2> /dev/null) && [[ -n "$peak" ]]; then
  echo "Its peak resident memory was $peak MiB." | tee -a "${GITHUB_STEP_SUMMARY:-/dev/null}"
fi
curl --fail --silent --show-error --request POST "http://$ADDRESS/shutdown" || kill "$PID" || true
timeout 30 tail --pid="$PID" -f /dev/null
