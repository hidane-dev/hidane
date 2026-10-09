#!/bin/bash
# 簡易スループット: REST PATCH / GET / gRPC Commit / GetDocument と各クライアントの基準値を hyperfine で計測(エミュレータは port 8093 で起動済みとする)
set -u
S=/private/tmp/claude-501/-Users-h-nomura-projects-git-hidane/3e3b0b4b-079e-4dde-83d9-811b19117084/scratchpad/t7/scripts
echo "# date: $(date -u +%Y-%m-%dT%H:%M:%SZ) load avg before: $(uptime | sed 's/.*load averages: //')"
curl -s -o /dev/null -w "pre-create PATCH http=%{http_code}\n" -X PATCH "http://127.0.0.1:8093/v1/projects/demo-hidane/databases/(default)/documents/bench/doc1" -H 'Content-Type: application/json' -d '{"fields":{"v":{"integerValue":"0"},"s":{"stringValue":"hello"}}}'
hyperfine --warmup 3 --runs 20 -N --export-json "/Users/h.nomura/projects/git/hidane/results/latency-official-v1.22.0.json" \
  --command-name "REST PATCH 1doc" "$S/lat-rest-patch.sh" \
  --command-name "REST GET 1doc" "$S/lat-rest-get.sh" \
  --command-name "gRPC Commit 1write" "$S/lat-grpc-commit.sh" \
  --command-name "gRPC GetDocument" "$S/lat-grpc-get.sh" \
  --command-name "baseline: curl GET /" "$S/lat-rest-baseline.sh" \
  --command-name "baseline: grpcurl list (reflection)" "$S/lat-grpc-baseline.sh" \
  --command-name "baseline: curl --version (spawn only)" "$S/lat-curl-spawn.sh" \
  --command-name "baseline: grpcurl -version (spawn only)" "$S/lat-grpcurl-spawn.sh" 2>&1
echo "# load avg after: $(uptime | sed 's/.*load averages: //')"
