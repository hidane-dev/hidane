#!/usr/bin/env bash
# Starts the official Cloud Firestore emulator in the background and waits until it answers
# `GET /` ("Ok"), failing when it exits first or does not answer within TIMEOUT seconds. It runs
# under an English locale: the jar's messages for Security Rules warnings and evaluation errors
# exist in English only, and under another locale each of them becomes a `500 UNKNOWN`.
#
# Environment: JAVA, JAR, VERSION, HOST, PORT, JAVA_OPTIONS and ARGS (split on whitespace),
# TIMEOUT, LOG. Writes pid, address and startup-seconds to $GITHUB_OUTPUT, and a line to
# $GITHUB_STEP_SUMMARY, when they are set.
set -euo pipefail
set -f # JAVA_OPTIONS and ARGS are split on whitespace, never expanded as globs

# Where requests go: a wildcard address is reached on loopback, an IPv6 one in brackets.
case "$HOST" in
  0.0.0.0) probe=127.0.0.1 ;;
  :: | "[::]") probe="[::1]" ;;
  \[*) probe=$HOST ;;
  *:*) probe="[$HOST]" ;;
  *) probe=$HOST ;;
esac
address="$probe:$PORT"

mkdir -p "$(dirname "$LOG")"
started=$(date +%s%3N)
# shellcheck disable=SC2086 # JAVA_OPTIONS and ARGS are meant to be split
"$JAVA" -Duser.language=en -Duser.country=US $JAVA_OPTIONS -jar "$JAR" \
  --host "$HOST" --port "$PORT" $ARGS < /dev/null > "$LOG" 2>&1 &
pid=$!
echo "Started cloud-firestore-emulator v$VERSION (pid $pid) on $HOST:$PORT, writing to $LOG"

deadline=$((SECONDS + TIMEOUT))
until curl --fail --silent --max-time 2 --output /dev/null "http://$address/"; do
  if ! kill -0 "$pid" 2> /dev/null; then
    status=0
    wait "$pid" || status=$?
    echo "::error title=Official emulator::It exited with status $status before answering on http://$address/"
    tail -n 200 "$LOG"
    exit 1
  fi
  if ((SECONDS >= deadline)); then
    echo "::error title=Official emulator::It did not answer on http://$address/ within $TIMEOUT s"
    kill "$pid" 2> /dev/null || true
    tail -n 200 "$LOG"
    exit 1
  fi
  sleep 0.5
done
elapsed=$(($(date +%s%3N) - started))
seconds="$((elapsed / 1000)).$((elapsed % 1000 / 100))"
echo "It answered on http://$address/ $seconds s after java started"

if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
  {
    echo "pid=$pid"
    echo "address=$address"
    echo "startup-seconds=$seconds"
  } >> "$GITHUB_OUTPUT"
fi
if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
  echo "Official Cloud Firestore emulator v$VERSION answered on \`http://$address/\` $seconds s after \`java\` started." >> "$GITHUB_STEP_SUMMARY"
fi
