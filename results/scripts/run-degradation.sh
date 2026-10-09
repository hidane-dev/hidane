#!/bin/bash
# バッチ書き込み劣化の再現: jar を新規起動 → (任意: リスナー接続) → demo-go を NUM_RECORDS で実行 → CSV 出力
# 使い方: run-degradation.sh <run-name> <num_records> <port> [listener:none|unlimited|limit50] [timeout_sec]
set -u
RUN=$1; N=$2; PORT=$3; LISTENER=${4:-none}; TIMEOUT=${5:-300}
SCRATCH=/private/tmp/claude-501/-Users-h-nomura-projects-git-hidane/3e3b0b4b-079e-4dde-83d9-811b19117084/scratchpad/t7
JAVA=/Users/h.nomura/.local/share/mise/installs/java/openjdk-24.0.2/bin/java
NODE=/Users/h.nomura/.local/share/mise/installs/node/22.22.3/bin/node
JAR=/Users/h.nomura/projects/git/hidane/research/jar/cloud-firestore-emulator-v1.22.0.jar
OUT=$SCRATCH/logs/degradation-$RUN
mkdir -p "$OUT"
now() { perl -MTime::HiRes=time -e 'printf "%.3f\n", time'; }
echo "# run=$RUN N=$N port=$PORT listener=$LISTENER timeout=${TIMEOUT}s date=$(date -u +%Y-%m-%dT%H:%M:%SZ) load_before=$(uptime | sed 's/.*load averages: //')" | tee "$OUT/meta.txt"
"$JAVA" -jar "$JAR" --host 127.0.0.1 --port "$PORT" > "$OUT/emulator.log" 2>&1 &
JPID=$!
for i in $(seq 1 1200); do nc -z 127.0.0.1 "$PORT" >/dev/null 2>&1 && break; sleep 0.05; done
sleep 2
LPID=""
if [ "$LISTENER" != "none" ]; then
  if [ "$LISTENER" = "limit50" ]; then export LISTEN_LIMIT=50; else unset LISTEN_LIMIT; fi
  FIRESTORE_EMULATOR_HOST=127.0.0.1:$PORT "$NODE" "$SCRATCH/node-bench/listener.js" > "$OUT/listener.log" 2>&1 &
  LPID=$!
  sleep 2
fi
echo "run,batch,docs_start,docs_end,elapsed_sec,rate_per_sec,rss_kb,t_rel_sec" > "$OUT/batches.csv"
TS=$(now)
( cd "$SCRATCH/bin" && NUM_RECORDS=$N GCLOUD_PROJECT=fake-project-id FIRESTORE_EMULATOR_HOST=127.0.0.1:$PORT ./demo-go ) 2>"$OUT/demo-go.stderr" | while IFS= read -r line; do
  # done writing [0-500] 500 result(s) in 0.975259 seconds [rate: 512.684503/s]
  if [[ "$line" =~ \[([0-9]+)-([0-9]+)\]\ ([0-9]+)\ result.*in\ ([0-9.]+)\ seconds\ \[rate:\ ([0-9.]+) ]]; then
    s=${BASH_REMATCH[1]}; e=${BASH_REMATCH[2]}; el=${BASH_REMATCH[4]}; r=${BASH_REMATCH[5]}
    b=$(( e / 500 )); rss=$(/bin/ps -o rss= -p $JPID | tr -d ' '); tr=$(perl -e "printf '%.1f', $(now) - $TS")
    echo "$RUN,$b,$s,$e,$el,$r,$rss,$tr" >> "$OUT/batches.csv"
  else
    echo "$line" >> "$OUT/demo-go.other.txt"
  fi
  if [ "$(perl -e "print int($(now) - $TS)")" -ge "$TIMEOUT" ]; then echo "# TIMEOUT ${TIMEOUT}s reached, stopping writer" | tee -a "$OUT/meta.txt"; pkill -f "$SCRATCH/bin/demo-go"; break; fi
done
TE=$(now)
echo "# writer_total_sec=$(perl -e "printf '%.1f', $TE-$TS") batches=$(($(wc -l < "$OUT/batches.csv")-1)) rss_kb_end=$(/bin/ps -o rss= -p $JPID | tr -d ' ') load_after=$(uptime | sed 's/.*load averages: //')" | tee -a "$OUT/meta.txt"
[ -n "$LPID" ] && { kill $LPID 2>/dev/null; wait $LPID 2>/dev/null; tail -3 "$OUT/listener.log" | sed 's/^/# /' | tee -a "$OUT/meta.txt"; }
pkill -f "$SCRATCH/bin/demo-go" 2>/dev/null
kill $JPID; wait $JPID 2>/dev/null
for i in $(seq 1 200); do nc -z 127.0.0.1 "$PORT" >/dev/null 2>&1 || break; sleep 0.05; done
echo "# done. tail of demo-go.stderr: $(tail -c 300 "$OUT/demo-go.stderr" | tr '\n' ' ')" | tee -a "$OUT/meta.txt"
