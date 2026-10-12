#!/usr/bin/env python3
"""What do `request` and `resource` hold for each operation? (#63, #64, #166)

Usage (from the repository root), with the jar started under an English locale
(`java -Duser.language=en -Duser.country=US -jar …`):
    python3 -I tools/oracle/rules_request.py 127.0.0.1:8086 > fixture.json

For each operation (a get, a get of a missing document, lists, queries under a document and of
collection groups, a create with and without a document ID, updates with and without a mask, a
set over an existing document, transforms, a delete), the script loads rules where every method
but the one under test is allowed and the one under test has a probe condition, runs the
operation over REST without an `Authorization` header and records whether it was allowed. Each
probe is one fact (`request.method == 'get'`, `resource.data.n == 1`, `request.query.limit == 2`,
`request.writeFields == ['z', 'm', 'n']`, …); reading a key `request` does not have denies.
`rules_request_fields.py` shows the whole of `request`. Documents are written as the owner.
Stdlib only.
"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from steps import OWNER, call, case, dump, rules  # noqa: E402

PROJECT = "request"
DB = f"projects/{PROJECT}/databases/(default)"
BASE = f"{DB}/documents"
I = lambda n: {"integerValue": str(n)}


def load(method, condition):
    others = [m for m in ("get", "list", "create", "update", "delete") if m != method]
    rules(PROJECT, "rules_version = '2';\nservice cloud.firestore {\n  match /databases/{database}/documents {\n"
          "    match /{document=**} {\n"
          f"      allow {', '.join(others)}: if true;\n"
          f"      allow {method}: if {condition};\n"
          "    }\n  }\n}\n")


def seed(doc, fields):
    parent, id_ = doc.rsplit("/", 1)
    call("DELETE", f"/v1/{BASE}/{doc}", auth=OWNER)
    status, _ = call("POST", f"/v1/{BASE}/{parent}?documentId={id_}", {"fields": fields}, auth=OWNER)
    assert status == 200, (doc, status)


def commit(*writes):
    return lambda: call("POST", f"/v1/{BASE}:commit", {"writes": list(writes)})


# (name, method, setup, run, probes)
OPERATIONS = [
    ("get", "get", lambda: seed("c/d", {"n": I(1)}), lambda: call("GET", f"/v1/{BASE}/c/d"), [
        "request.path == /databases/$(database)/documents/c/d", "resource.data.n == 1", "resource.id == 'd'",
        "resource.__name__ == /databases/$(database)/documents/c/d", "request.resource == null",
        "request.query == null", "request.time is timestamp", "database == '(default)'",
        "request.inTransaction == false", "request.fields == null", "request.writeFields == null",
        "request.autoId == false", "'resource' in request",
        "request.keys().hasOnly(['auth', 'fields', 'inTransaction', 'method', 'path', 'time'])"]),
    ("get missing", "get", lambda: call("DELETE", f"/v1/{BASE}/c/missing", auth=OWNER),
     lambda: call("GET", f"/v1/{BASE}/c/missing"), ["resource == null", "request.path == /databases/$(database)/documents/c/missing"]),
    ("list", "list", lambda: seed("c/d", {"n": I(1)}), lambda: call("GET", f"/v1/{BASE}/c?pageSize=2"), [
        "request.path == /databases/$(database)/documents/c", "resource.data.n == 1", "resource == null",
        "request.query.limit == 2", "request.query.limit == null", "request.query.offset == 0",
        "request.query.orderBy == null", "request.time is timestamp", "request.query.orderBy == {}",
        "request.inTransaction == false", "request.fields == null", "request.query.kind == 'c'",
        "request.query.parent == null", "request.query.allDescendants == false",
        "request.query.distinct == false", "request.query.selectOnlyKeys == false", "request.query.groupBy == {}",
        "request.query.ancestor == null",
        "request.query.keys().hasOnly(['allDescendants', 'distinct', 'groupBy', 'kind', 'limit', 'offset', "
        "'orderBy', 'parent', 'selectOnlyKeys'])"]),
    ("query", "list", lambda: seed("c/d", {"n": I(1)}),
     lambda: call("POST", f"/v1/{BASE}:runQuery", {"structuredQuery": {
         "from": [{"collectionId": "c"}], "orderBy": [{"field": {"fieldPath": "n"}, "direction": "DESCENDING"}],
         "limit": 3, "offset": 1, "where": {"fieldFilter": {"field": {"fieldPath": "n"}, "op": "EQUAL", "value": I(1)}}}}), [
        "request.path == /databases/$(database)/documents/c", "request.query.limit == 3", "request.query.offset == 1",
        "request.query.orderBy == 'n'", "request.query.orderBy == ['n']", "request.query.orderBy[0] == 'n'",
        "resource.data.n == 1", "request.query.orderBy == {'n': 'DESC'}", "request.query.orderBy.n == 'DESC'"]),
    ("query with a projection under a document", "list", lambda: seed("c/d/s/e", {"n": I(1)}),
     lambda: call("POST", f"/v1/{BASE}/c/d:runQuery", {"structuredQuery": {
         "from": [{"collectionId": "s"}], "select": {"fields": [{"fieldPath": "n"}, {"fieldPath": "m.a"},
                                                               {"fieldPath": "m"}, {"fieldPath": "b"}]},
         "orderBy": [{"field": {"fieldPath": "z"}}, {"field": {"fieldPath": "b"}}]}}), [
        "request.fields == ['b', 'm', 'n']", "request.fields == ['n', 'm', 'b']", "request.fields.size() == 3",
        "request.query.kind == 's'", "request.query.parent == /databases/$(database)/documents/c/d",
        "request.path == /databases/$(database)/documents/c/d/s", "request.query.orderBy == {'z': 'ASC', 'b': 'ASC'}",
        "request.query.orderBy.keys() == ['b', 'z']"]),
    ("collection group", "list", lambda: seed("c/d", {"n": I(1)}),
     lambda: call("POST", f"/v1/{BASE}/c/d:runQuery", {"structuredQuery": {
         "from": [{"collectionId": "s", "allDescendants": True}]}}), [
        "request.query.kind == 's'", "request.query.ancestor == /databases/$(database)/documents/c/d",
        "request.query.allDescendants == true", "request.query.parent == null", "request.path == null",
        "request.query.keys().hasAll(['ancestor'])"]),
    ("every collection at any depth", "list", lambda: seed("c/d", {"n": I(1)}),
     lambda: call("POST", f"/v1/{BASE}:runQuery", {"structuredQuery": {"from": [{"allDescendants": True}]}}), [
        "request.query.kind == null", "request.query.ancestor == null", "request.query.allDescendants == true"]),
    ("every collection directly below a document", "list", lambda: seed("c/d", {"n": I(1)}),
     lambda: call("POST", f"/v1/{BASE}/c/d:runQuery", {"structuredQuery": {"from": [{}]}}), [
        "request.query.kind == null", "request.query.parent == /databases/$(database)/documents/c/d",
        "request.query.allDescendants == false", "request.query.ancestor == null"]),
    ("list with a mask and an order", "list", lambda: seed("c/d", {"n": I(1)}),
     lambda: call("GET", f"/v1/{BASE}/c?mask.fieldPaths=n&mask.fieldPaths=b&orderBy=__name__"), [
        "request.fields == ['b', 'n']", "request.query.orderBy == {'__name__': 'ASC'}", "request.query.limit == 30"]),
    ("create", "create", lambda: call("DELETE", f"/v1/{BASE}/c/new", auth=OWNER),
     lambda: call("POST", f"/v1/{BASE}/c?documentId=new", {"fields": {"n": I(2), "s": {"stringValue": "x"}}}), [
        "resource == null", "request.resource.data.n == 2", "request.resource.data.keys().hasOnly(['n', 's'])",
        "request.resource.id == 'new'", "request.resource.__name__ == /databases/$(database)/documents/c/new",
        "request.path == /databases/$(database)/documents/c/new", "request.autoId == false",
        "request.inTransaction == true", "request.writeFields == null", "request.transforms == null",
        "request.readFields == null", "request.fields == null",
        "request.keys().hasOnly(['auth', 'autoId', 'inTransaction', 'method', 'path', 'readFields', 'resource', "
        "'time', 'transforms', 'writeFields'])"]),
    ("create without a document ID", "create", lambda: None,
     lambda: call("POST", f"/v1/{BASE}/c", {"fields": {"n": I(2)}}), [
        "request.autoId == true", "request.path == /databases/$(database)/documents/c", "resource == null",
        "request.resource.data.n == 2", "request.resource.keys().hasOnly(['data'])", "request.resource.id == null",
        "request.resource.__name__ == null", "document == null", "document.size() == 2",
        "exists(request.path)", "getAfter(request.path) == null"]),
    ("update with a mask", "update", lambda: seed("c/u", {"n": I(1), "m": I(5)}),
     lambda: call("PATCH", f"/v1/{BASE}/c/u?updateMask.fieldPaths=n", {"fields": {"n": I(2)}}), [
        "resource.data.n == 1", "request.resource.data.n == 2", "request.resource.data.m == 5",
        "request.resource.data.diff(resource.data).affectedKeys().hasOnly(['n'])",
        "request.resource.id == 'u'", "request.writeFields == ['n']", "request.transforms == null",
        "request.inTransaction == true", "request.autoId == false", "request.readFields == null"]),
    ("update with a mask in another order", "update", lambda: seed("c/o", {"n": I(1)}),
     lambda: call("PATCH", f"/v1/{BASE}/c/o?updateMask.fieldPaths=n&updateMask.fieldPaths=m.b"
                          "&updateMask.fieldPaths=z&updateMask.fieldPaths=m", {"fields": {}}), [
        "request.writeFields == ['z', 'm', 'n']", "request.writeFields == ['n', 'm.b', 'z', 'm']",
        "request.writeFields.hasOnly(['n', 'm', 'z'])", "request.writeFields[0] == 'z'"]),
    ("set over an existing document", "update", lambda: seed("c/s", {"n": I(1), "m": I(5)}),
     commit({"update": {"name": f"{BASE}/c/s", "fields": {"n": I(3)}}}), [
        "resource.data.n == 1", "request.resource.data.n == 3", "!('m' in request.resource.data)",
        "request.resource.data.keys().hasOnly(['n'])", "request.writeFields == null", "'autoId' in request",
        "request.method == 'update'"]),
    ("transform", "update", lambda: seed("c/t", {"n": I(1)}),
     commit({"transform": {"document": f"{BASE}/c/t", "fieldTransforms": [
         {"fieldPath": "n", "increment": I(1)}, {"fieldPath": "ts", "setToServerValue": "REQUEST_TIME"}]}}), [
        "request.resource.data.n == 2", "request.resource.data.ts == request.time", "request.resource.data.ts is timestamp",
        "request.transforms == ['n', 'ts']", "request.writeFields == ['n', 'ts']"]),
    ("update with a server timestamp", "update", lambda: seed("c/st", {"n": I(1)}),
     commit({"update": {"name": f"{BASE}/c/st", "fields": {"n": I(2)}}, "updateMask": {"fieldPaths": ["n"]},
             "updateTransforms": [{"fieldPath": "ts", "setToServerValue": "REQUEST_TIME"}]}), [
        "request.resource.data.ts == request.time", "request.resource.data.n == 2",
        "request.transforms == ['ts']", "request.writeFields == ['ts', 'n']"]),
    ("delete", "delete", lambda: seed("c/del", {"n": I(1)}), commit({"delete": f"{BASE}/c/del"}), [
        "resource.data.n == 1", "request.resource == null", "request.path == /databases/$(database)/documents/c/del",
        "request.writeFields == null", "request.transforms == null", "request.inTransaction == true",
        "request.autoId == false"]),
    ("delete missing", "delete", lambda: call("DELETE", f"/v1/{BASE}/c/gone", auth=OWNER), commit({"delete": f"{BASE}/c/gone"}), [
        "resource == null"]),
]

COMMON = ["true", "false", "request.method == 'get'", "request.method == 'list'", "request.method == 'create'",
          "request.method == 'update'", "request.method == 'delete'", "request.auth == null"]

for name, method, setup, run, probes in OPERATIONS:
    for probe in COMMON + probes:
        case(f"{name}: {probe}", operation=name, method=method, condition=probe)
        setup()
        load(method, probe)
        run()

dump()
