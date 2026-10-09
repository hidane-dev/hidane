#!/bin/bash
# 常駐メモリ計測: 起動→アイドル 10 秒後 RSS → REST :commit で 1000 ドキュメント書き込み → RSS。jcmd で JVM フラグ/ヒープも記録。
set -u
JAVA_HOME=/Users/h.nomura/.local/share/mise/installs/java/openjdk-24.0.2
JAVA=$JAVA_HOME/bin/java; JCMD=$JAVA_HOME/bin/jcmd
JAR=/Users/h.nomura/projects/git/hidane/research/jar/cloud-firestore-emulator-v1.22.0.jar
PORT=${1:-8092}; PROJECT=demo-hidane
BASE="http://127.0.0.1:$PORT/v1/projects/$PROJECT/databases/(default)/documents"
TMP=${TMPDIR_BENCH:-/tmp}
now() { perl -MTime::HiRes=time -e 'printf "%.3f\n", time'; }
rss() { /bin/ps -o rss= -p "$1" | tr -d ' '; }   # KB
echo "# date: $(date -u +%Y-%m-%dT%H:%M:%SZ)  load avg before: $(uptime | sed 's/.*load averages: //')"
echo "# JVM default flags on this machine (java -XX:+PrintFlagsFinal -version):"
"$JAVA" -XX:+PrintFlagsFinal -version 2>/dev/null | grep -E ' (MaxHeapSize|InitialHeapSize|MaxRAMPercentage|InitialRAMPercentage|UseG1GC|UseSerialGC|UseParallelGC|UseZGC|UseCompressedOops|ActiveProcessorCount|SoftMaxHeapSize) ' | sed 's/^/#   /'
T0=$(now)
"$JAVA" -jar "$JAR" --host 127.0.0.1 --port "$PORT" > "$TMP/mem-emulator.log" 2>&1 &
PID=$!
for i in $(seq 1 1200); do nc -z 127.0.0.1 "$PORT" >/dev/null 2>&1 && break; sleep 0.05; done
T1=$(now)
echo "pid=$PID port_open_ms=$(perl -e "printf '%.0f', ($T1-$T0)*1000")"
echo "rss_kb_at_port_open=$(rss $PID)"
sleep 1; echo "rss_kb_idle_1s=$(rss $PID)"
sleep 4; echo "rss_kb_idle_5s=$(rss $PID)"
sleep 5; echo "rss_kb_idle_10s=$(rss $PID)"
echo "## jcmd VM.flags (idle 10s)"; "$JCMD" "$PID" VM.flags 2>&1 | sed 's/^/#   /'
echo "## jcmd GC.heap_info (idle 10s)"; "$JCMD" "$PID" GC.heap_info 2>&1 | sed 's/^/#   /'
echo "## jcmd VM.info (first lines: vm/gc)"; "$JCMD" "$PID" VM.version 2>&1 | sed 's/^/#   /'
echo "## threads"; echo "thread_count_idle=$(/bin/ps -M -p $PID | tail -n +2 | wc -l | tr -d ' ')"
# 1000 ドキュメント書き込み: :commit 500 writes × 2
for b in 0 1; do
  python3 -I -c "
import json,sys
b=int(sys.argv[1]); base=sys.argv[2]
writes=[{'update':{'name':f'{base}/users/{i}','fields':{'mykey':{'stringValue':'my data'},'myid':{'integerValue':str(i)}}}} for i in range(b*500,(b+1)*500)]
print(json.dumps({'writes':writes}))" "$b" "projects/$PROJECT/databases/(default)/documents" > "$TMP/commit-$b.json"
  TS=$(now)
  CODE=$(curl -s -o "$TMP/commit-$b.out" -w '%{http_code}' -X POST "http://127.0.0.1:$PORT/v1/projects/$PROJECT/databases/(default)/documents:commit" -H 'Content-Type: application/json' --data-binary @"$TMP/commit-$b.json")
  TE=$(now)
  echo "commit_batch_$b http=$CODE ms=$(perl -e "printf '%.0f', ($TE-$TS)*1000") writeResults=$(jq '.writeResults|length' "$TMP/commit-$b.out" 2>/dev/null)"
done
echo "rss_kb_after_1000_writes=$(rss $PID)"
sleep 5; echo "rss_kb_after_1000_writes_idle_5s=$(rss $PID)"
echo "## jcmd GC.heap_info (after writes)"; "$JCMD" "$PID" GC.heap_info 2>&1 | sed 's/^/#   /'
# 確認: ドキュメント数
echo "doc_count_via_runAggregationQuery=$(curl -s -X POST "$BASE:runAggregationQuery" -H 'Content-Type: application/json' -d '{"structuredAggregationQuery":{"structuredQuery":{"from":[{"collectionId":"users"}]},"aggregations":[{"count":{},"alias":"n"}]}}' | jq -r '.[0].result.aggregateFields.n.integerValue' 2>/dev/null)"
kill "$PID"; wait "$PID" 2>/dev/null
echo "# load avg after: $(uptime | sed 's/.*load averages: //')"
