#!/bin/bash
# firebase emulators:start --only firestore 経由の起動時間: コマンド起動から TCP 8091 が accept するまで (ms)
set -u
export PATH=/Users/h.nomura/.local/share/mise/installs/java/openjdk-24.0.2/bin:/Users/h.nomura/.local/share/mise/installs/node/22.22.3/bin:/usr/bin:/bin:/usr/sbin:/sbin
export JAVA_HOME=/Users/h.nomura/.local/share/mise/installs/java/openjdk-24.0.2
FIREBASE=/Users/h.nomura/.local/share/mise/installs/node/22.22.3/bin/firebase
CFG=/private/tmp/claude-501/-Users-h-nomura-projects-git-hidane/3e3b0b4b-079e-4dde-83d9-811b19117084/scratchpad/t7/firebase-cfg
PORT=8091
LOG=${FB_LOG:-/dev/null}
now() { perl -MTime::HiRes=time -e 'printf "%.6f\n", time'; }
cd "$CFG"
T0=$(now)
"$FIREBASE" emulators:start --only firestore --project demo-hidane --config "$CFG/firebase.json" >"$LOG" 2>&1 &
PID=$!
for i in $(seq 1 2400); do
  if nc -z 127.0.0.1 "$PORT" >/dev/null 2>&1; then T1=$(now); break; fi
  if ! kill -0 "$PID" 2>/dev/null; then echo "ERROR: firebase exited early" >&2; exit 1; fi
  sleep 0.05
done
if [ -z "${T1:-}" ]; then echo "ERROR: timeout" >&2; kill -INT "$PID"; exit 1; fi
# java プロセスのコマンドライン(JVM フラグ確認用)を 1 回だけ記録
if [ -n "${FB_PSLOG:-}" ]; then /bin/ps -o pid,rss,command -p "$(pgrep -f 'java.*cloud-firestore-emulator' | head -1)" > "$FB_PSLOG" 2>&1; fi
kill -INT "$PID" 2>/dev/null
for i in $(seq 1 600); do kill -0 "$PID" 2>/dev/null || break; sleep 0.05; done
# java 残留があれば kill
for jp in $(pgrep -f 'java.*cloud-firestore-emulator'); do kill "$jp" 2>/dev/null; done
for i in $(seq 1 200); do nc -z 127.0.0.1 "$PORT" >/dev/null 2>&1 || break; sleep 0.05; done
perl -e "printf \"%.1f\n\", ($T1 - $T0) * 1000"
