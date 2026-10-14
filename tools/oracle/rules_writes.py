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
documents. Batches: a batchGet or a commit stops at its first denied document or write and lists
the ones checked before as `false` (D019); a batchGet denied on a document no statement matches,
first, alone, or after one allowed, and a commit's write no statement matches after one allowed.
Stdlib only.
"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from steps import OWNER, call, case, dump, rules  # noqa: E402

PROJECT = "writes"
DB = f"projects/{PROJECT}/databases/(default)"
BASE = f"{DB}/documents"
I = lambda n: {"integerValue": str(n)}


def load(allow):
    lines = "\n".join(f"      allow {a};" for a in allow) or "      allow get: if false;"
    rules(PROJECT, "rules_version = '2';\nservice cloud.firestore {\n  match /databases/{database}/documents {\n"
          f"    match /c/{{d}} {{\n{lines}\n    }}\n  }}\n}}\n")


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

for exists in (True, False):
    for write, op in WRITES.items():
        for rules_name, allow in RULES.items():
            doc = "c/w"
            case(f"{'existing' if exists else 'missing'} document, {write}, {rules_name}",
                 document="exists" if exists else "missing", write=write, rules=rules_name)
            call("DELETE", f"/v1/{BASE}/{doc}", auth=OWNER)
            if exists:
                call("POST", f"/v1/{BASE}/c?documentId=w", {"fields": {"n": I(1)}}, auth=OWNER)
            load(allow)
            method, path, body = op(doc)
            call(method, path, body)

case("seed for the reads")
call("POST", f"/v1/{BASE}/c?documentId=a", {"fields": {"n": I(1)}}, auth=OWNER)
call("POST", f"/v1/{BASE}/c?documentId=b", {"fields": {"n": I(2)}}, auth=OWNER)
case("batchGet, second denied")
load(["get: if d == 'a'", "list: if true"])
call("POST", f"/v1/{DB}/documents:batchGet", {"documents": [f"{BASE}/c/a", f"{BASE}/c/b"]})
case("batchGet, missing document")
load(["get: if d == 'a'", "list: if true"])
call("POST", f"/v1/{DB}/documents:batchGet", {"documents": [f"{BASE}/c/a", f"{BASE}/c/zz"]})
# A batch is checked in request order and stops at its first denied document or write; its
# message lists the ones checked before as `false`, even where they were allowed (D019), and is
# "No matching allow statements" alone when no statement matched and nothing came before.
TWO_BLOCKS = ("rules_version = '2';\nservice cloud.firestore {\n  match /databases/{database}/documents {\n"
              "    match /c/{d} {\n      allow get: if d == 'a';\n      allow create, update: if d == 'a';\n    }\n"
              "    match /e/{d} {\n      allow get: if d == 'x';\n    }\n  }\n}\n")
case("batchGet, two denied under two statements")
rules(PROJECT, TWO_BLOCKS)
call("POST", f"/v1/{DB}/documents:batchGet", {"documents": [f"{BASE}/c/b", f"{BASE}/e/b"]})
case("batchGet, no statement for the first")
rules(PROJECT, TWO_BLOCKS)
call("POST", f"/v1/{DB}/documents:batchGet", {"documents": [f"{BASE}/d/x", f"{BASE}/c/b"]})
case("batchGet, no statement for the only one")
rules(PROJECT, TWO_BLOCKS)
call("POST", f"/v1/{DB}/documents:batchGet", {"documents": [f"{BASE}/d/x"]})
case("batchGet, allowed, then no statement")
rules(PROJECT, TWO_BLOCKS)
call("POST", f"/v1/{DB}/documents:batchGet", {"documents": [f"{BASE}/c/a", f"{BASE}/d/x"]})
case("commit, allowed, then no statement")
rules(PROJECT, TWO_BLOCKS)
call(*commit(update("c/a"), update("d/x")))
case("get in a transaction, denied")
load(["get: if d == 'a'", "list: if true"])
# A REST GET with ?transaction= never answers on the official emulator; batchGet does. The
# transaction ID is not replayable, so only the begin is recorded and the read is a plain one.
call("POST", f"/v1/{DB}/documents:batchGet", {"documents": [f"{BASE}/c/b"], "newTransaction": {}})
QUERY = {"structuredQuery": {"from": [{"collectionId": "c"}]}}
FILTERED = {"structuredQuery": {"from": [{"collectionId": "c"}], "where": {"fieldFilter": {
    "field": {"fieldPath": "n"}, "op": "EQUAL", "value": I(1)}}}}
for name, (method, path, body) in {
    "aggregation, denied": ("POST", f"/v1/{BASE}:runAggregationQuery", {"structuredAggregationQuery": {
        **QUERY, "aggregations": [{"alias": "n", "count": {}}]}}),
    "aggregation, allowed by its filter": ("POST", f"/v1/{BASE}:runAggregationQuery", {"structuredAggregationQuery": {
        **FILTERED, "aggregations": [{"alias": "n", "count": {}}]}}),
    "listDocuments, denied": ("GET", f"/v1/{BASE}/c", None),
    "listCollectionIds": ("POST", f"/v1/{BASE}:listCollectionIds", {}),
}.items():
    case(name)
    load(["get: if true", "list: if resource.data.n == 1"])
    call(method, path, body)

dump()
