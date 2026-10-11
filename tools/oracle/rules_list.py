#!/usr/bin/env python3
"""Which queries does a `list` rule allow? (#66, #63)

Usage (from the repository root), with the jar started under an English locale
(`java -Duser.language=en -Duser.country=US -jar …`):
    python3 -I tools/oracle/rules_list.py 127.0.0.1:8086 > fixture.json

Rules are not filters: a query is allowed only if every document it could return satisfies the
condition, which the official emulator decides from the query's constraints, not from the data.
For each `allow list` condition and each query (run over REST without an `Authorization`
header), the script records whether the query was allowed. A last group probes what
`request.query.orderBy` holds. Documents are written as the owner. Stdlib only.
"""

import json
import sys
import urllib.error
import urllib.request

HOST = sys.argv[1]
PROJECT = "list"
BASE = f"projects/{PROJECT}/databases/(default)/documents"
I = lambda n: {"integerValue": str(n)}
S = lambda s: {"stringValue": s}
A = lambda *v: {"arrayValue": {"values": list(v)}}


def call(method, path, body=None, owner=False):
    headers = {"Content-Type": "application/json"}
    if owner:
        headers["Authorization"] = "Bearer owner"
    req = urllib.request.Request(f"http://{HOST}{path}", method=method,
                                 data=None if body is None else json.dumps(body).encode(), headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=30) as res:
            return res.status, res.read().decode()
    except urllib.error.HTTPError as err:
        return err.code, err.read().decode()


def field(path, op, value):
    return {"fieldFilter": {"field": {"fieldPath": path}, "op": op, "value": value}}


def query(where=None, order=None, limit=None, offset=None):
    q = {"from": [{"collectionId": "c"}]}
    if where is not None:
        q["where"] = where
    if order is not None:
        q["orderBy"] = [{"field": {"fieldPath": f}, "direction": d} for f, d in order]
    if limit is not None:
        q["limit"] = limit
    if offset is not None:
        q["offset"] = offset
    return q


def both(*filters, op="AND"):
    return {"compositeFilter": {"op": op, "filters": list(filters)}}


QUERIES = {
    "all": query(),
    "n == 1": query(field("n", "EQUAL", I(1))),
    "n == 2": query(field("n", "EQUAL", I(2))),
    "n > 0": query(field("n", "GREATER_THAN", I(0))),
    "n >= 1 and n <= 1": query(both(field("n", "GREATER_THAN_OR_EQUAL", I(1)), field("n", "LESS_THAN_OR_EQUAL", I(1)))),
    "n in [1]": query(field("n", "IN", A(I(1)))),
    "n in [1, 2]": query(field("n", "IN", A(I(1), I(2)))),
    "n != 3": query(field("n", "NOT_EQUAL", I(3))),
    "s == 'x'": query(field("s", "EQUAL", S("x"))),
    "n == 1 and s == 'x'": query(both(field("n", "EQUAL", I(1)), field("s", "EQUAL", S("x")))),
    "n == 1 or n == 2": query(both(field("n", "EQUAL", I(1)), field("n", "EQUAL", I(2)), op="OR")),
    "tags array-contains 'a'": query(field("tags", "ARRAY_CONTAINS", S("a"))),
    "tags array-contains-any ['a']": query(field("tags", "ARRAY_CONTAINS_ANY", A(S("a")))),
    "limit 5": query(limit=5),
    "n == 1 limit 5": query(field("n", "EQUAL", I(1)), limit=5),
    "limit 20": query(limit=20),
}

CONDITIONS = [
    "true", "resource.data.n == 1", "resource.data.n > 0", "resource.data.n >= 1", "resource.data.n != 3",
    "resource.data.n in [1, 2]", "resource.data.s == 'x'", "resource.data.n == 1 || resource.data.n == 2",
    "resource.data.n == 1 && resource.data.s == 'x'", "'a' in resource.data.tags",
    "resource.data.tags.hasAny(['a'])", "request.query.limit <= 5", "request.query.limit <= 5 && resource.data.n == 1",
    "resource.data.n == 1 || request.query.limit <= 5",
]

ORDER_PROBES = [
    "request.query.orderBy is map", "request.query.orderBy is list", "request.query.orderBy is string",
    "request.query.orderBy.n == 'DESC'", "request.query.orderBy['n'] == 'DESC'", "request.query.orderBy.n == 'desc'",
    "request.query.orderBy.n == 'DESCENDING'", "request.query.orderBy.size() == 1", "request.query.orderBy.keys().hasAll(['n'])",
    "request.query.orderBy == {'n': 'DESC'}",
]


def load(condition):
    content = ("rules_version = '2';\nservice cloud.firestore {\n  match /databases/{database}/documents {\n"
               f"    match /c/{{d}} {{\n      allow get: if true;\n      allow list: if {condition};\n    }}\n  }}\n}}\n")
    return call("PUT", f"/emulator/v1/projects/{PROJECT}:securityRules",
                {"rules": {"files": [{"name": "firestore.rules", "content": content}]}})


for i, (n, s, tags) in enumerate([(1, "x", ["a"]), (2, "y", ["b"]), (3, "x", [])]):
    call("POST", f"/v1/{BASE}/c?documentId=d{i + 1}",
         {"fields": {"n": I(n), "s": S(s), "tags": A(*[S(t) for t in tags])}}, owner=True)

results = []
for condition in CONDITIONS:
    status, body = load(condition)
    entry = {"condition": condition, "queries": {}}
    if status != 200:
        entry["compile"] = body
    else:
        for name, q in QUERIES.items():
            status, _ = call("POST", f"/v1/{BASE}:runQuery", {"structuredQuery": q})
            entry["queries"][name] = "allow" if status == 200 else ("deny" if status == 403 else status)
    results.append(entry)

ordered = query(order=[("n", "DESCENDING")], limit=3)
probes = []
for condition in ORDER_PROBES:
    status, body = load(condition)
    entry = {"condition": condition, "query": "orderBy n desc limit 3"}
    if status != 200:
        entry["compile"] = body
    else:
        issues = json.loads(body).get("issues", [])
        if issues:
            entry["issues"] = [i["description"] for i in issues]
        status, _ = call("POST", f"/v1/{BASE}:runQuery", {"structuredQuery": ordered})
        entry["outcome"] = "allow" if status == 200 else ("deny" if status == 403 else status)
    probes.append(entry)

json.dump({"queries": QUERIES, "results": results, "orderBy": probes}, sys.stdout, indent=1)
print()
