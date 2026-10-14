#!/usr/bin/env python3
"""Is the limit of 1,000 expressions per Security Rules decision the same over gRPC? (#293)

Usage (from the repository root), with the jar started under an English locale
(`java -Duser.language=en -Duser.country=US -jar …`) and `grpcurl` on `PATH`:
    python3 -I tools/oracle/rules_expressions_grpc.py 127.0.0.1:8086 > fixture.json

For each call, the script finds the largest N for which a condition holding `cost(N)` (as in
`rules_expressions.py`) does not reach the limit, and records the call at N and at N + 1: a
GetDocument, a BatchGetDocuments of two documents (each counts from zero), a RunQuery and a
RunAggregationQuery (each disjunction counts from zero), a Commit of two writes (each counts
from zero), Listen targets of two documents, of a missing one and of queries, and two writes on
the Write stream. Each call runs through grpcurl with the repository's protos without
credentials, in the format of `rules_streams.py`; the messages it prints are recorded.
Documents are written as the owner over REST. Stdlib only, apart from `grpcurl`.
"""

import json
import subprocess
import sys
import time
import urllib.request

HOST = sys.argv[1]
PROTOS = "crates/hidane-proto/proto"
PROJECT = "expressions-grpc"
DATABASE = f"projects/{PROJECT}/databases/(default)"
BASE = f"{DATABASE}/documents"
LIMIT = "maximum of 1000 expressions"
# Functions whose calls cost 2, 7, 17, 37, 77, 157, 317 and 637 expressions.
COSTS = [2]
for _ in range(7):
    COSTS.append(2 * COSTS[-1] + 3)
COST_FUNCTIONS = "function d0() { return true; }\n" + "\n".join(
    f"function d{k}() {{ return d{k - 1}() && d{k - 1}(); }}" for k in range(1, len(COSTS)))


def cost(n):
    """An expression that costs `n` (at least 1) and is true."""
    if n <= 4:
        return ["true", "!false", "!!true", "!!!false"][n - 1]
    k = max(k for k, c in enumerate(COSTS) if c + 3 <= n)
    return f"d{k}() && {cost(n - COSTS[k] - 2)}"


def ruleset(condition):
    """`condition` for every method on the documents of `c`."""
    body = COST_FUNCTIONS + f"\nmatch /c/{{id}} {{\n  allow read, write: if {condition};\n}}"
    return ("rules_version = '2';\nservice cloud.firestore {\n  match /databases/{database}/documents {\n"
            + "\n".join("    " + line for line in body.split("\n")) + "\n  }\n}\n")


def rest(method, path, body=None, owner=False):
    headers = {"Content-Type": "application/json"}
    if owner:
        headers["Authorization"] = "Bearer owner"
    req = urllib.request.Request(f"http://{HOST}{path}", method=method,
                                 data=None if body is None else json.dumps(body).encode(), headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=30) as res:
            return res.status
    except urllib.error.HTTPError as err:
        return err.code


def parse(output):
    """grpcurl prints each message as JSON, and a status that is not OK as
    `ERROR:\n  Code: PermissionDenied\n  Message: …`."""
    decoder = json.JSONDecoder()
    received, at = [], 0
    while True:
        while at < len(output) and output[at].isspace():
            at += 1
        if at == len(output) or output[at] != "{":
            break
        message, at = decoder.raw_decode(output, at)
        received.append(message)
    rest_ = output[at:]
    status = None
    if rest_.startswith("ERROR:"):
        code = rest_.split("Code: ", 1)[1].split("\n", 1)[0]
        status = {"code": code, "message": rest_.split("Message: ", 1)[1].rstrip("\n") if "Message: " in rest_ else ""}
    else:
        assert not rest_.strip(), rest_
    return received, status


def run(rpc, messages, stream):
    """Sends `messages`: a stream's one at a time, then waits; returns what grpcurl printed and
    the status the call ended with (`None` for OK)."""
    args = ["grpcurl", "-plaintext", "-import-path", PROTOS, "-proto", "google/firestore/v1/firestore.proto",
            "-H", f"google-cloud-resource-prefix: {DATABASE}", "-d", "@", HOST, f"google.firestore.v1.Firestore/{rpc}"]
    if not stream:
        done = subprocess.run(args, input="\n".join(json.dumps(m) for m in messages),
                              capture_output=True, text=True, timeout=60)
        return parse(done.stdout + done.stderr)
    proc = subprocess.Popen(args, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    for message in messages:
        proc.stdin.write(json.dumps(message) + "\n")
        proc.stdin.flush()
        time.sleep(0.5)
    time.sleep(1.5)
    proc.stdin.close()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
    return parse(proc.stdout.read())


DOCUMENTS = [("c/e", 1), ("c/f", 1)]
DELETED = ["c/new", "c/zz"]


def attempt(condition, rpc, messages, stream):
    """Loads the rules, deletes the documents of DELETED and writes those of DOCUMENTS as the
    owner, then makes the call."""
    content = ruleset(condition)
    status = rest("PUT", f"/emulator/v1/projects/{PROJECT}:securityRules",
                  {"rules": {"files": [{"name": "firestore.rules", "content": content}]}})
    assert status == 200
    for doc in DELETED:
        rest("DELETE", f"/v1/{BASE}/{doc}", owner=True)
    for doc, n in DOCUMENTS:
        rest("DELETE", f"/v1/{BASE}/{doc}", owner=True)
        rest("PATCH", f"/v1/{BASE}/{doc}", {"fields": {"n": {"integerValue": str(n)}}}, owner=True)
    received, status = run(rpc, messages, stream)
    return content, received, status


def reached(received, status):
    causes = [m["targetChange"]["cause"].get("message", "") for m in received
              if "cause" in m.get("targetChange", {})]
    return any(LIMIT in text for text in causes + [(status or {}).get("message", "")])


results = []
upd = lambda doc: {"update": {"name": f"{BASE}/{doc}", "fields": {"n": {"integerValue": "1"}}}}
EQ = {"fieldFilter": {"field": {"fieldPath": "n"}, "op": "EQUAL", "value": {"integerValue": "1"}}}
QUERY = {"from": [{"collectionId": "c"}], "where": EQ}
OR = {"from": [{"collectionId": "c"}], "where": {"compositeFilter": {"op": "OR", "filters": [
    EQ, {"fieldFilter": {"field": {"fieldPath": "n"}, "op": "EQUAL", "value": {"integerValue": "2"}}}]}}}
listen = lambda target: [{"database": DATABASE, "addTarget": {**target, "targetId": 1}}]
CALLS = [
    ("GetDocument", "resource.data.n == 1", "GetDocument", [{"name": f"{BASE}/c/e"}], False),
    ("BatchGetDocuments of two documents", "resource.data.n == 1", "BatchGetDocuments",
     [{"database": DATABASE, "documents": [f"{BASE}/c/e", f"{BASE}/c/f"]}], False),
    ("RunQuery", "resource.data.n == 1", "RunQuery", [{"parent": BASE, "structuredQuery": QUERY}], False),
    ("RunQuery of two disjunctions", "resource.data.n > 0", "RunQuery", [{"parent": BASE, "structuredQuery": OR}], False),
    ("RunAggregationQuery", "resource.data.n == 1", "RunAggregationQuery",
     [{"parent": BASE, "structuredAggregationQuery": {"structuredQuery": QUERY, "aggregations": [{"alias": "n", "count": {}}]}}],
     False),
    ("Commit of two writes", "request.resource.data.n == 1", "Commit",
     [{"database": DATABASE, "writes": [upd("c/e"), upd("c/new")]}], False),
    ("Listen to two documents", "resource.data.n == 1", "Listen",
     listen({"documents": {"documents": [f"{BASE}/c/e", f"{BASE}/c/f"]}}), True),
    ("Listen to a missing document", "resource == null", "Listen",
     listen({"documents": {"documents": [f"{BASE}/c/zz"]}}), True),
    ("Listen to a query", "resource.data.n == 1", "Listen",
     listen({"query": {"parent": BASE, "structuredQuery": QUERY}}), True),
    ("Listen to a query of two disjunctions", "resource.data.n > 0", "Listen",
     listen({"query": {"parent": BASE, "structuredQuery": OR}}), True),
    ("two writes on the Write stream", "request.resource.data.n == 1", "Write",
     [{"database": DATABASE}, {"streamToken": "", "writes": [upd("c/e"), upd("c/new")]}], True),
]
for name, read, rpc, messages, stream in CALLS:
    condition = lambda n, read=read: f"{cost(n)} && {read}"
    hits = lambda n: reached(*attempt(condition(n), rpc, messages, stream)[1:])
    lo, hi = 5, 1000
    assert not hits(lo) and hits(hi), name
    while hi - lo > 1:
        mid = (lo + hi) // 2
        lo, hi = (mid, hi) if not hits(mid) else (lo, mid)
    for n in (lo, lo + 1):
        content, received, status = attempt(condition(n), rpc, messages, stream)
        results.append({"name": f"{name}: {n} ({lo} fits)", "rules": content, "database": DATABASE,
                        "documents": [{"path": d, "n": v} for d, v in DOCUMENTS], "deleted": DELETED, "rpc": rpc,
                        "requests": messages, "during": None, "responses": received, "status": status})

json.dump(results, sys.stdout, indent=1)
print()
