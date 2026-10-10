"""How does the official emulator answer what it ignores or refuses? (#26)

Usage (from the repository root, `grpcurl` on PATH):
    python3 -I tools/oracle/unsupported.py 127.0.0.1:8086 > crates/hidane/tests/fixtures/unsupported.json

- `explain_options` on RunQuery and RunAggregationQuery is ignored: the same results, no
  explain metrics.
- ExecutePipeline on a standard-edition database is `INVALID_ARGUMENT`, after the database
  name is parsed and before the `Authorization` header is read.
- PartitionQuery is `UNIMPLEMENTED`.

gRPC answers are summarised (document names, aggregation values, whether explain metrics
came back, whether the stream ended with `done`); REST bodies are recorded with server times
replaced by `<time>`. `crates/hidane/tests/unsupported.rs` replays them against hidane.
"""

import http.client
import json
import re
import subprocess
import sys

HOST = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1:8080"
DB = "projects/unsupported/databases/(default)"
DOCS = f"{DB}/documents"
QUERY = {"from": [{"collection_id": "c"}]}
EMPTY = {"from": [{"collection_id": "nothing"}]}
COUNT = {"structured_query": QUERY, "aggregations": [{"alias": "n", "count": {}}]}

GRPC = [
    # (name, rpc, request, authorization)
    ("RunQuery", "RunQuery", {"parent": DOCS, "structured_query": QUERY}, "Bearer owner"),
    ("RunQuery, explain", "RunQuery", {"parent": DOCS, "structured_query": QUERY, "explain_options": {}}, "Bearer owner"),
    ("RunQuery, explain and analyze", "RunQuery", {"parent": DOCS, "structured_query": QUERY, "explain_options": {"analyze": True}}, "Bearer owner"),
    ("RunQuery, explain, no results", "RunQuery", {"parent": DOCS, "structured_query": EMPTY, "explain_options": {"analyze": True}}, "Bearer owner"),
    ("RunAggregationQuery, explain and analyze", "RunAggregationQuery", {"parent": DOCS, "structured_aggregation_query": COUNT, "explain_options": {"analyze": True}}, "Bearer owner"),
    ("ExecutePipeline, bad database", "ExecutePipeline", {"database": "projects/unsupported"}, "Bearer owner"),
    ("ExecutePipeline", "ExecutePipeline", {"database": DB, "structured_pipeline": {"pipeline": {"stages": [{"name": "collection", "args": [{"reference_value": "/c"}]}]}}}, "Bearer owner"),
    ("ExecutePipeline, garbage token", "ExecutePipeline", {"database": DB}, "Bearer garbage"),
    ("ExecutePipeline, anonymous", "ExecutePipeline", {"database": DB}, None),
    ("PartitionQuery", "PartitionQuery", {"parent": DOCS}, "Bearer owner"),
]

REST = [
    # (name, verb, body)
    (":runQuery, explain and analyze", "runQuery", {"structuredQuery": {"from": [{"collectionId": "c"}]}, "explainOptions": {"analyze": True}}),
    (":runAggregationQuery, explain", "runAggregationQuery", {"structuredAggregationQuery": {"structuredQuery": {"from": [{"collectionId": "c"}]}, "aggregations": [{"alias": "n", "count": {}}]}, "explainOptions": {}}),
    (":executePipeline", "executePipeline", {}),
    (":partitionQuery", "partitionQuery", {}),
]


def rest(method, path, body=None):
    c = http.client.HTTPConnection(*HOST.split(":"), timeout=10)
    c.request(method, path, body=None if body is None else json.dumps(body),
              headers={"Content-Type": "application/json", "Authorization": "Bearer owner"})
    r = c.getresponse()
    return r.status, r.read().decode()


def messages(text):
    decoder, out, i = json.JSONDecoder(), [], 0
    while True:
        while i < len(text) and text[i].isspace():
            i += 1
        if i >= len(text):
            return out
        value, i = decoder.raw_decode(text, i)
        out.append(value)


def grpcurl(rpc, request, authorization):
    args = ["grpcurl", "-plaintext", "-import-path", "crates/hidane-proto/proto", "-proto", "google/firestore/v1/firestore.proto"]
    if authorization:
        args += ["-H", f"authorization: {authorization}"]
    out = subprocess.run(args + ["-d", json.dumps(request), HOST, f"google.firestore.v1.Firestore/{rpc}"],
                         capture_output=True, text=True, timeout=60)
    m = re.search(r"Code: (\w+)\n\s*Message: (.*)", out.stdout + out.stderr)
    if m:
        return {"code": re.sub(r"(?<!^)(?=[A-Z])", "_", m.group(1)).upper(), "message": m.group(2).strip()}
    responses = messages(out.stdout)
    return {
        "documents": [r["document"]["name"][len(DOCS) + 1:] for r in responses if "document" in r],
        "aggregations": [{k: v for k, v in r["result"]["aggregateFields"].items()} for r in responses if "result" in r],
        "explain_metrics": any("explainMetrics" in r for r in responses),
        "done": bool(responses) and responses[-1].get("done", False),
    }


rest("POST", "/reset")
status, _ = rest("POST", f"/v1/{DOCS}:commit", {"writes": [
    {"update": {"name": f"{DOCS}/c/a", "fields": {"n": {"integerValue": "1"}}}},
    {"update": {"name": f"{DOCS}/c/b", "fields": {"n": {"integerValue": "2"}}}},
]})
assert status == 200
fixture = {
    "oracle": "cloud-firestore-emulator v1.22.0 (sha256 9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)",
    "generated_by": "tools/oracle/unsupported.py",
    "grpc": [{"name": n, "rpc": rpc, "request": r, "authorization": a, "outcome": grpcurl(rpc, r, a)} for n, rpc, r, a in GRPC],
    "rest": [],
}
for name, verb, body in REST:
    status, text = rest("POST", f"/v1/{DOCS}:{verb}", body)
    text = re.sub(r'"(readTime|createTime|updateTime)": "[^"]+"', r'"\1": "<time>"', text)
    fixture["rest"].append({"name": name, "verb": verb, "body": body, "outcome": {"status": status, "body": text}})
json.dump(fixture, sys.stdout, ensure_ascii=False, indent=1)
print()
