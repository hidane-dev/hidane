#!/usr/bin/env python3
"""Which `allow` methods decide a write, and how do reads of several documents, transactions and
aggregations answer under rules? (#63, #67)

Usage (from the repository root), with the jar started under an English locale
(`java -Duser.language=en -Duser.country=US -jar …`):
    python3 -I tools/oracle/rules_writes.py 127.0.0.1:8086 > fixture.json

Writes: for a document that exists and one that does not, each kind of write (a commit's
update with and without a mask or an `exists` precondition, a transform, a delete, REST
create, patch and delete) runs under rules that allow only `create`, only `update`, only
`delete`, or none, without an `Authorization` header; the status and message are recorded.
Reads: a batchGet with one document denied, a transaction's get, an aggregation and a list of
documents. Stdlib only.
"""

import json
import sys
import urllib.error
import urllib.request

HOST = sys.argv[1]
PROJECT = "writes"
DB = f"projects/{PROJECT}/databases/(default)"
BASE = f"{DB}/documents"
I = lambda n: {"integerValue": str(n)}


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


def load(allow):
    lines = "\n".join(f"      allow {a};" for a in allow) or "      allow get: if false;"
    content = ("rules_version = '2';\nservice cloud.firestore {\n  match /databases/{database}/documents {\n"
               f"    match /c/{{d}} {{\n{lines}\n    }}\n  }}\n}}\n")
    status, body = call("PUT", f"/emulator/v1/projects/{PROJECT}:securityRules",
                        {"rules": {"files": [{"name": "firestore.rules", "content": content}]}})
    assert status == 200, body


def message(body):
    try:
        return json.loads(body)["error"]["message"]
    except (ValueError, KeyError, TypeError):
        return None


def commit(*writes):
    return ("POST", f"/v1/{BASE}:commit", {"writes": list(writes)})


def update(doc, mask=None, exists=None):
    w = {"update": {"name": f"{BASE}/{doc}", "fields": {"n": I(2)}}}
    if mask is not None:
        w["updateMask"] = {"fieldPaths": mask}
    if exists is not None:
        w["currentDocument"] = {"exists": exists}
    return w


WRITES = {
    "set": lambda d: commit(update(d)),
    "set with a mask": lambda d: commit(update(d, mask=["n"])),
    "set with exists=true": lambda d: commit(update(d, exists=True)),
    "set with exists=false": lambda d: commit(update(d, exists=False)),
    "transform": lambda d: commit({"transform": {"document": f"{BASE}/{d}", "fieldTransforms": [{"fieldPath": "n", "increment": I(1)}]}}),
    "delete": lambda d: commit({"delete": f"{BASE}/{d}"}),
    "REST create": lambda d: ("POST", f"/v1/{BASE}/c?documentId={d.split('/')[1]}", {"fields": {"n": I(2)}}),
    "REST patch": lambda d: ("PATCH", f"/v1/{BASE}/{d}", {"fields": {"n": I(2)}}),
    "REST patch with a mask": lambda d: ("PATCH", f"/v1/{BASE}/{d}?updateMask.fieldPaths=n", {"fields": {"n": I(2)}}),
    "REST delete": lambda d: ("DELETE", f"/v1/{BASE}/{d}", None),
}
RULES = {
    "create only": ["create: if true"],
    "update only": ["update: if true"],
    "delete only": ["delete: if true"],
    "write": ["write: if true"],
    "none": [],
}

results = []
for exists in (True, False):
    for write, op in WRITES.items():
        for rules, allow in RULES.items():
            doc = "c/w"
            call("DELETE", f"/v1/{BASE}/{doc}", owner=True)
            if exists:
                call("POST", f"/v1/{BASE}/c?documentId=w", {"fields": {"n": I(1)}}, owner=True)
            load(allow)
            method, path, body = op(doc)
            status, text = call(method, path, body)
            results.append({"document": "exists" if exists else "missing", "write": write, "rules": rules,
                            "status": status, "message": message(text)})

# Reads.
call("POST", f"/v1/{BASE}/c?documentId=a", {"fields": {"n": I(1)}}, owner=True)
call("POST", f"/v1/{BASE}/c?documentId=b", {"fields": {"n": I(2)}}, owner=True)
reads = []
load(["get: if d == 'a'", "list: if true"])
for name, (method, path, body) in {
    "batchGet, second denied": ("POST", f"/v1/{DB}/documents:batchGet", {"documents": [f"{BASE}/c/a", f"{BASE}/c/b"]}),
    "batchGet, missing document allowed": ("POST", f"/v1/{DB}/documents:batchGet", {"documents": [f"{BASE}/c/a", f"{BASE}/c/zz"]}),
    "get in a transaction, denied": ("POST", f"/v1/{DB}/documents:beginTransaction", {}),
}.items():
    status, text = call(method, path, body)
    entry = {"name": name, "status": status, "response": text[:1500]}
    if name.startswith("get in a transaction"):
        tx = json.loads(text)["transaction"]
        # A REST GET with ?transaction= never answers on the official emulator; batchGet does.
        status, text = call("POST", f"/v1/{DB}/documents:batchGet", {"documents": [f"{BASE}/c/b"], "transaction": tx})
        entry.update(status=status, response=text[:1500])
    reads.append(entry)
load(["get: if true", "list: if resource.data.n == 1"])
for name, (method, path, body) in {
    "aggregation, denied": ("POST", f"/v1/{BASE}:runAggregationQuery", {"structuredAggregationQuery": {
        "structuredQuery": {"from": [{"collectionId": "c"}]}, "aggregations": [{"alias": "n", "count": {}}]}}),
    "aggregation, allowed by its filter": ("POST", f"/v1/{BASE}:runAggregationQuery", {"structuredAggregationQuery": {
        "structuredQuery": {"from": [{"collectionId": "c"}], "where": {"fieldFilter": {"field": {"fieldPath": "n"}, "op": "EQUAL", "value": I(1)}}},
        "aggregations": [{"alias": "n", "count": {}}]}}),
    "listDocuments, denied": ("GET", f"/v1/{BASE}/c", None),
    "listCollectionIds": ("POST", f"/v1/{BASE}:listCollectionIds", {}),
}.items():
    status, text = call(method, path, body)
    reads.append({"name": name, "status": status, "response": text[:1500]})

json.dump({"writes": results, "reads": reads}, sys.stdout, indent=1)
print()
