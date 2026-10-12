#!/usr/bin/env python3
"""What does `request` hold, key by key, for each kind of read and write? (#166)

Usage (from the repository root), with a freshly started jar under an English locale whose
standard output goes to a file, as `rules_coverage.py` takes it:
    (cd /tmp && java -Duser.language=en -Duser.country=US -jar …/cloud-firestore-emulator-v1.22.0.jar \\
        --host 127.0.0.1 --port 8086 > emulator.out) &
    python3 -I tools/oracle/rules_request_fields.py 127.0.0.1:8086 /tmp/emulator.out \\
        > crates/hidane/tests/fixtures/rules_request_fields.json

Each case loads rules into a project of its own that allow everything once `debug()` has printed
`request`, makes its calls (documents are written as the owner) and reads the coverage report:
the fixture keeps what each call printed (`log`) and the report (`raw`), which lists the values
of `request` with their keys in the official emulator's order. The cases:

- reads: a get (of a missing document, with a mask), a batchGet (with a mask, in a transaction,
  beginning one), lists (a page size, a mask, an order), queries (projections, orderings and
  implicit ones, cursors, offsets and limits, `find_nearest`, under a document, collection
  groups, every collection directly below a document or at any depth, in a transaction or
  beginning one) and an aggregation;
- writes: a create with and without a document ID (`CreateDocument`) or by an `exists: false`
  precondition, sets and patches of documents that exist and that do not (upserts, whose
  `create` and `update` statements both print), an update by an `exists: true` precondition,
  deletes, a batch, and a commit in a transaction;
- writes with an update mask or transforms, in many forms: there `request.resource` is computed
  only when a condition reads it, and `debug(request)` shows it as `undefined` (see
  `docs/parity-exceptions.md`), so these cases print `request`'s other keys one by one and
  read no report.

A transaction's ID is the one `beginTransaction` answered before. Stdlib only.
"""

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import steps  # noqa: E402
from steps import OWNER, call, case, dump, rules  # noqa: E402

steps.LOG = sys.argv[2]
I = lambda n: {"integerValue": str(n)}
S = lambda s: {"stringValue": s}
WHOLE = "debug(request) != null"
# `request` but its `resource`, and its keys.
PARTS = ("debug([request.method, request.path, request.inTransaction, request.writeFields, "
         "request.transforms, request.readFields, request.keys()]) != null")
count = [0]


def start(name, condition=WHOLE):
    """A case of its own project, its rules allowing everything once `condition` has run;
    returns the project and its documents' path."""
    count[0] += 1
    project = f"fields-{count[0]}"
    case(name, project=project)
    rules(project, "rules_version = '2';\nservice cloud.firestore {\n  match /databases/{database}/documents {\n"
          "    match /{document=**} {\n"
          f"      allow read, write: if {condition};\n"
          "    }\n  }\n}\n")
    return project, f"projects/{project}/databases/(default)/documents"


def seed(docs, path, fields):
    parent, id_ = path.rsplit("/", 1)
    status, _ = call("POST", f"/v1/{docs}/{parent}?documentId={id_}", {"fields": fields}, auth=OWNER)
    assert status == 200, (path, status)


def report(project):
    call("GET", f"/emulator/v1/projects/{project}:ruleCoverage", raw=True)


def database(docs):
    return docs.rsplit("/documents", 1)[0]


def begin(docs):
    status, text = call("POST", f"/v1/{database(docs)}/documents:beginTransaction", {})
    assert status == 200, text
    return json.loads(text)["transaction"]


def run_query(docs, query, parent="", **extra):
    return call("POST", f"/v1/{docs}{parent}:runQuery", {"structuredQuery": query, **extra})


def commit(docs, *writes, **extra):
    return call("POST", f"/v1/{database(docs)}/documents:commit", {"writes": list(writes), **extra})


def mask_query(paths, key="updateMask.fieldPaths"):
    return "&".join(f"{key}={p.replace('`', '%60').replace(' ', '%20').replace(chr(92), '%5C')}" for p in paths)


SEED = {"n": I(1), "m": {"mapValue": {"fields": {"a": I(1)}}}, "v": {"mapValue": {"fields": {
    "__type__": S("__vector__"), "value": {"arrayValue": {"values": [{"doubleValue": 1.0}]}}}}}}

# Reads.
for name, run in [
    ("get", lambda d: call("GET", f"/v1/{d}/c/d")),
    ("get of a missing document", lambda d: call("GET", f"/v1/{d}/c/missing")),
    ("get with a mask", lambda d: call("GET", f"/v1/{d}/c/d?mask.fieldPaths=n")),
    ("batchGet with a mask", lambda d: call("POST", f"/v1/{database(d)}/documents:batchGet", {
        "documents": [f"{d}/c/d", f"{d}/c/missing"], "mask": {"fieldPaths": ["n", "m.a"]}})),
    ("batchGet beginning a transaction", lambda d: call("POST", f"/v1/{database(d)}/documents:batchGet", {
        "documents": [f"{d}/c/d"], "newTransaction": {}})),
    ("list", lambda d: call("GET", f"/v1/{d}/c")),
    ("list with a page size, a mask and an order", lambda d: call(
        "GET", f"/v1/{d}/c?pageSize=2&{mask_query(['n', 'm.a', 'b'], 'mask.fieldPaths')}&orderBy=__name__")),
    ("list of a subcollection", lambda d: call("GET", f"/v1/{d}/c/d/s")),
    ("query", lambda d: run_query(d, {"from": [{"collectionId": "c"}]})),
    ("query with everything", lambda d: run_query(d, {
        "select": {"fields": [{"fieldPath": "n"}, {"fieldPath": "m.a"}, {"fieldPath": "__name__"}]},
        "from": [{"collectionId": "c"}],
        "where": {"compositeFilter": {"op": "AND", "filters": [
            {"fieldFilter": {"field": {"fieldPath": "n"}, "op": "GREATER_THAN", "value": I(0)}},
            {"fieldFilter": {"field": {"fieldPath": "m.a"}, "op": "EQUAL", "value": I(1)}}]}},
        "orderBy": [{"field": {"fieldPath": "n"}, "direction": "DESCENDING"}],
        "startAt": {"values": [I(5)], "before": True}, "endAt": {"values": [I(0)]},
        "offset": 1, "limit": 3})),
    ("query ordered by several fields", lambda d: run_query(d, {"from": [{"collectionId": "c"}], "orderBy": [
        {"field": {"fieldPath": f}, "direction": "DESCENDING" if f == "m" else "ASCENDING"}
        for f in ("z", "m", "b", "`a.b`", "__name__")]})),
    ("query with an inequality, ordered implicitly", lambda d: run_query(d, {"from": [{"collectionId": "c"}], "where": {
        "fieldFilter": {"field": {"fieldPath": "n"}, "op": "NOT_EQUAL", "value": I(0)}}})),
    ("query selecting names only", lambda d: run_query(d, {"from": [{"collectionId": "c"}],
                                                           "select": {"fields": [{"fieldPath": "__name__"}]}})),
    ("query selecting nothing", lambda d: run_query(d, {"from": [{"collectionId": "c"}], "select": {}})),
    ("query selecting nested and quoted fields", lambda d: run_query(d, {"from": [{"collectionId": "c"}], "select": {
        "fields": [{"fieldPath": p} for p in ("x.y", "x", "b", "`a.b`.c", "`q r`", "`a\\\\b`", "n", "n")]}})),
    ("query with a limit of 0", lambda d: run_query(d, {"from": [{"collectionId": "c"}], "limit": 0})),
    ("query with find_nearest", lambda d: run_query(d, {"from": [{"collectionId": "c"}], "findNearest": {
        "vectorField": {"fieldPath": "v"}, "queryVector": SEED["v"], "distanceMeasure": "EUCLIDEAN", "limit": 3}})),
    ("query under a document", lambda d: run_query(d, {"from": [{"collectionId": "s"}]}, "/c/d")),
    ("collection group", lambda d: run_query(d, {"from": [{"collectionId": "c", "allDescendants": True}]})),
    ("collection group under a document", lambda d: run_query(d, {"from": [{"collectionId": "s", "allDescendants": True}]}, "/c/d")),
    ("every collection at any depth", lambda d: run_query(d, {"from": [{"allDescendants": True}]})),
    ("every collection at any depth under a document", lambda d: run_query(d, {"from": [{"allDescendants": True}]}, "/c/d")),
    ("every collection directly below the root", lambda d: run_query(d, {})),
    ("every collection directly below a document", lambda d: run_query(d, {"from": [{}]}, "/c/d")),
    ("query beginning a transaction", lambda d: run_query(d, {"from": [{"collectionId": "c"}]}, newTransaction={})),
    ("aggregation", lambda d: call("POST", f"/v1/{d}:runAggregationQuery", {"structuredAggregationQuery": {
        "structuredQuery": {"from": [{"collectionId": "c"}], "select": {"fields": [{"fieldPath": "n"}]}, "limit": 5},
        "aggregations": [{"alias": "k", "count": {}}]}})),
]:
    project, docs = start(name)
    seed(docs, "c/d", SEED)
    run(docs)
    report(project)

project, docs = start("reads and a write in a transaction")
seed(docs, "c/d", SEED)
transaction = begin(docs)
call("POST", f"/v1/{database(docs)}/documents:batchGet", {"documents": [f"{docs}/c/d"], "transaction": transaction})
run_query(docs, {"from": [{"collectionId": "c"}]}, transaction=transaction)
call("POST", f"/v1/{docs}:runAggregationQuery", {"structuredAggregationQuery": {
    "structuredQuery": {"from": [{"collectionId": "c"}]}, "aggregations": [{"alias": "k", "count": {}}]},
    "transaction": transaction})
commit(docs, {"update": {"name": f"{docs}/c/d", "fields": {"n": I(2)}}}, transaction=transaction)
report(project)

# Writes whose `request.resource` is computed at once.
for name, run in [
    ("create", lambda d: call("POST", f"/v1/{d}/c?documentId=new", {"fields": {"n": I(2), "s": S("x")}})),
    ("create without a document ID", lambda d: call("POST", f"/v1/{d}/c", {"fields": {"n": I(2)}})),
    ("create without a document ID in a subcollection", lambda d: call("POST", f"/v1/{d}/c/d/s", {"fields": {}})),
    ("create by a precondition", lambda d: commit(d, {"update": {"name": f"{d}/c/new", "fields": {"n": I(3)}},
                                                      "currentDocument": {"exists": False}})),
    ("update by a precondition", lambda d: commit(d, {"update": {"name": f"{d}/c/d", "fields": {"n": I(3)}},
                                                      "currentDocument": {"exists": True}})),
    ("set of a document that exists", lambda d: call("PATCH", f"/v1/{d}/c/d", {"fields": {"n": I(3)}})),
    ("set of a document that does not exist", lambda d: commit(d, {"update": {"name": f"{d}/c/new", "fields": {}}})),
    ("delete", lambda d: call("DELETE", f"/v1/{d}/c/d")),
    ("delete by a precondition", lambda d: commit(d, {"delete": f"{d}/c/d", "currentDocument": {"exists": True}})),
    ("batch", lambda d: commit(d, {"update": {"name": f"{d}/c/e", "fields": {"n": I(1)}}},
                               {"delete": f"{d}/c/d"})),
]:
    project, docs = start(name)
    seed(docs, "c/d", {"n": I(1)})
    run(docs)
    report(project)

# Writes with an update mask or transforms: `request` but its `resource`.
for name, run in [
    ("update mask", lambda d: call("PATCH", f"/v1/{d}/c/d?{mask_query(['n', 'm.b', 'z'])}", {"fields": {"n": I(5)}})),
    ("update mask, its order", lambda d: call("PATCH", f"/v1/{d}/c/d?{mask_query(['c', 'b', 'a', 'b'])}", {"fields": {}})),
    ("update mask, nested and quoted fields", lambda d: call(
        "PATCH", f"/v1/{d}/c/d?{mask_query(['x.y', 'a', '`q r`', 'b.c.d', '`a.b`.`c.d`', '`a\\\\b`', '`a\\`b`'])}", {"fields": {}})),
    ("update mask, paths below others", lambda d: call(
        "PATCH", f"/v1/{d}/c/d?{mask_query(['m.b', 'm', 'k.b.c', 'k.a', 'z'])}", {"fields": {}})),
    ("update mask of a document that does not exist", lambda d: call(
        "PATCH", f"/v1/{d}/c/new?{mask_query(['n'])}", {"fields": {"n": I(5)}})),
    ("empty update mask", lambda d: commit(d, {"update": {"name": f"{d}/c/d", "fields": {}}, "updateMask": {"fieldPaths": []}})),
    ("update mask and transforms", lambda d: commit(d, {
        "update": {"name": f"{d}/c/d", "fields": {}}, "updateMask": {"fieldPaths": ["b", "m.c", "a"]},
        "updateTransforms": [{"fieldPath": "z", "increment": I(1)}, {"fieldPath": "y", "setToServerValue": "REQUEST_TIME"},
                             {"fieldPath": "a", "maximum": I(1)}]})),
    ("set with transforms", lambda d: commit(d, {
        "update": {"name": f"{d}/c/d", "fields": {"n": I(1)}},
        "updateTransforms": [{"fieldPath": "z", "increment": I(1)}]})),
    ("transform", lambda d: commit(d, {"transform": {"document": f"{d}/c/d", "fieldTransforms": [
        {"fieldPath": "`q r`", "increment": I(1)}, {"fieldPath": "`a.b`.c", "increment": I(1)},
        {"fieldPath": "`a\\\\b`", "increment": I(1)}, {"fieldPath": "k", "increment": I(1)},
        {"fieldPath": "k", "minimum": I(1)}, {"fieldPath": "m.a", "appendMissingElements": {"values": [I(1)]}}]}})),
    ("transform of a document that does not exist", lambda d: commit(d, {"transform": {
        "document": f"{d}/c/new", "fieldTransforms": [{"fieldPath": "n", "increment": I(1)}]}})),
]:
    project, docs = start(name, PARTS)
    seed(docs, "c/d", {"n": I(1)})
    run(docs)

dump()
