#!/bin/bash
# 起動時間計測: java -jar を起動し、TCP ポートが accept するまでの時間(ms)を出力して kill する。
# 使い方: measure-startup.sh <port> [extra java args...]
set -u
JAVA=/Users/h.nomura/.local/share/mise/installs/java/openjdk-24.0.2/bin/java
JAR=/Users/h.nomura/projects/git/hidane/research/jar/cloud-firestore-emulator-v1.22.0.jar
PORT=${1:-8090}; shift || true
LOG=${STARTUP_LOG:-/dev/null}
now() { perl -MTime::HiRes=time -e 'printf "%.6f\n", time'; }
T0=$(now)
"$JAVA" -jar "$JAR" --host 127.0.0.1 --port "$PORT" "$@" >"$LOG" 2>&1 &
PID=$!
# 50ms 間隔でポーリング(最大 60 秒)
for i in $(seq 1 1200); do
  if nc -z 127.0.0.1 "$PORT" >/dev/null 2>&1; then
    T1=$(now)
    break
  fi
  if ! kill -0 "$PID" 2>/dev/null; then echo "ERROR: process exited early" >&2; exit 1; fi
  sleep 0.05
done
if [ -z "${T1:-}" ]; then echo "ERROR: timeout" >&2; kill "$PID" 2>/dev/null; exit 1; fi
kill "$PID" 2>/dev/null
wait "$PID" 2>/dev/null
# 終了後、ポートが閉じるまで待つ(次の計測でポート競合しないように)
for i in $(seq 1 200); do nc -z 127.0.0.1 "$PORT" >/dev/null 2>&1 || break; sleep 0.05; done
perl -e "printf \"%.1f\n\", ($T1 - $T0) * 1000"
