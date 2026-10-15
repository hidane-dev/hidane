#!/usr/bin/env python3
"""Which project IDs does the official emulator accept, and where does it check them?

Usage (from the repository root), with a freshly started jar under an English locale:
    python3 -I tools/oracle/project_ids.py 127.0.0.1:8086 \\
        > crates/hidane/tests/fixtures/project_ids.json

Every resource name is read as `projects/{project}/databases/{database}/…`; once the database
ID is read (a project or database ID `.` or `..` is refused first, as a document's), the
project ID must be a Cloud project ID: lowercase letters, digits and hyphens, not starting
with a hyphen, at most 100 characters, after an optional domain (`example.com:`) of at most
100 lowercase letters, digits, dots and hyphens not starting with a dot or a hyphen. Other
project IDs answer `Invalid project ID(P) or database ID(D)` (`D` empty for `(default)`)
before the rest of the name is read, in a request's names (its path, the documents of its
writes and reads) and in every reference: in a document's fields, in a transform's operand,
in a filter and in a cursor, in the order the request is read. Database IDs are not checked.

The cases send each name over REST and record the answer: a GET for each project ID, every
REST binding with an invalid one in its path, writes, references and queries naming one
among other problems, and the `/emulator/v1` endpoints. The fixture has the format of
`rest.json` and its normalisation; `crates/hidane/tests/rest.rs` replays it. Stdlib only.
"""

import http.client
import json
import sys
import time
import urllib.parse
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from harness import mask  # noqa: E402

HOST = sys.argv[1]
PROJECT = f"ids-{time.strftime('%H%M%S')}"
OWNER = {"Authorization": "Bearer owner"}
P = "{project}"
DB = f"projects/{P}/databases/(default)"
DOCS = f"{DB}/documents"
RULES = ("rules_version = '2';\nservice cloud.firestore {\n  match /databases/{database}/documents {\n"
         "    match /{document=**} {\n      allow read, write: if true;\n    }\n  }\n}\n")

CASES = []


def case(name, method, path, body=None):
    c = {"name": name, "method": method, "path": path, "headers": OWNER, "compare": "exact"}
    if body is not None:
        c["body"] = body
    CASES.append(c)


def db(project, database="(default)"):
    return f"projects/{project}/databases/{database}"


def ref(name):
    return {"referenceValue": name}


def update(name, fields=None, **more):
    return {"update": {"name": name, "fields": fields or {}}, **more}


def commit(name, *writes, database=DB):
    case(name, "POST", f"/v1/{database}/documents:commit", {"writes": list(writes)})


def query(name, structured, database=DB):
    case(name, "POST", f"/v1/{database}/documents:runQuery",
         {"structuredQuery": {"from": [{"collectionId": "c"}], **structured}})


def where(field, op, value):
    return {"where": {"fieldFilter": {"field": {"fieldPath": field}, "op": op, "value": value}}}


# The grammar: a document of each project ID, read.
IDS = [
    "a", "0", "9", "ab", "1p", "p-", "a-b-c", "p--1", "demo-x", "a" * 100, "a" * 101,
    "a" * 1000, ":", ":p", "p:", "a:1", "1:a", "a.b:c", "a.:b", "a..b:c", "1.2:3", "a-:p",
    "a:p-", "google.com:p", "example.com:my-p", "a.b.c:d", "a" * 100 + ":" + "b" * 100,
    "a" * 101 + ":b", "a:" + "b" * 101, "P1", "Ab", "A.b:c", "p_1", "a_b:c", "-p", "-", "--",
    "-a:p", "a:-p", ".a:b", ".:a", "p.1", "a:b.c", "a:b:c", "p~1", "dev~p", "s~p", "~p", "~",
    "a~b", "a:b~c", "pé", ".", "..",
]
for project in IDS:
    shown = project if len(project) <= 24 else f"{project[:12]}…({len(project)})"
    case(f"project ID {shown!r}", "GET",
         f"/v1/{db(urllib.parse.quote(project, safe=':~'))}/documents/c/d")

# Where in a name: the database ID is read first; the rest of the name after the check.
case("a named database's ID in the message", "GET", f"/v1/{db('P1', 'other')}/documents/c/d")
case("an unchecked database ID in the message", "GET", f"/v1/{db('P1', 'BAD')}/documents/c/d")
case("database ID '.' before the project ID", "GET", f"/v1/{db('P1', '.')}/documents/c/d")
case("database ID '..'", "GET", f"/v1/{db('p', '..')}/documents/c/d")
case("database IDs are not checked", "GET", f"/v1/{db(P, 'A_b~c')}/documents/c/d")
case("before a reserved document ID", "GET", f"/v1/{db('P1')}/documents/c/__x__")
case("before a document ID '.'", "GET", f"/v1/{db('P1')}/documents/c/.")
case("before an odd path (a list)", "GET", f"/v1/{db('P1')}/documents/c/d/e")

# Every REST binding with an invalid project ID in its path.
bad = db("P1")
case("get", "GET", f"/v1/{bad}/documents/c/d")
case("list", "GET", f"/v1/{bad}/documents/c")
case("list of the root", "GET", f"/v1/{bad}/documents")
case("patch", "PATCH", f"/v1/{bad}/documents/c/d", {"fields": {}})
case("create", "POST", f"/v1/{bad}/documents/c?documentId=x", {"fields": {}})
case("delete", "DELETE", f"/v1/{bad}/documents/c/d")
case("commit", "POST", f"/v1/{bad}/documents:commit", {"writes": [update("x")]})
case("commit of project '.'", "POST", f"/v1/{db('.')}/documents:commit", {})
case("commit of database '.'", "POST", f"/v1/{db('P1', '.')}/documents:commit", {})
case("batchGet", "POST", f"/v1/{bad}/documents:batchGet", {"documents": [f"{bad}/documents/c/d"]})
case("runQuery", "POST", f"/v1/{bad}/documents:runQuery", {"structuredQuery": {"limit": -1}})
case("runQuery of database '.'", "POST", f"/v1/{db('P1', '.')}/documents:runQuery", {})
case("runAggregationQuery", "POST", f"/v1/{bad}/documents:runAggregationQuery",
     {"structuredAggregationQuery": {"structuredQuery": {"from": [{"collectionId": "c"}]},
                                     "aggregations": [{"count": {}}]}})
case("listCollectionIds", "POST", f"/v1/{bad}/documents:listCollectionIds", {})
case("listCollectionIds of a document", "POST", f"/v1/{bad}/documents/c/d:listCollectionIds", {})
case("listCollectionIds of project '.'", "POST", f"/v1/{db('.')}/documents:listCollectionIds", {})
case("beginTransaction", "POST", f"/v1/{bad}/documents:beginTransaction", {})
case("rollback", "POST", f"/v1/{bad}/documents:rollback", {"transaction": "eA=="})
case("batchWrite", "POST", f"/v1/{bad}/documents:batchWrite", {"writes": []})

# The documents a request names.
commit("an update's document", update(f"{bad}/documents/c/d"))
commit("a delete's document", {"delete": f"{bad}/documents/c/d"})
commit("a verify's document", {"verify": f"{bad}/documents/c/d", "currentDocument": {"exists": True}})
commit("project '.' in a write's document", update(f"{db('.')}/documents/c/d"))
commit("before the rest of a write's document: lacks /", update(f"{bad}/documents/c"))
commit("before the rest of a write's document: ID '.'", update(f"{bad}/documents/c/."))
commit("before the rest of a write's document: reserved ID", update(f"{bad}/documents/c/__x__"))
commit("a write's document, before the next write's name", update(f"{bad}/documents/c/d"), update("bad"))
commit("after the previous write's name", update("bad"), update(f"{bad}/documents/c/d"))
case("a batchGet's document", "POST", f"/v1/{DB}/documents:batchGet",
     {"documents": [f"{bad}/documents/c/d"]})
case("project '.' in a batchGet's document", "POST", f"/v1/{DB}/documents:batchGet",
     {"documents": [f"{db('.')}/documents/c/d"]})

# References in writes: read with the write's names, in the order of its fields, depth first.
R = ref(f"{bad}/documents/c/d")
NULL = {"nullValue": None}
commit("a reference", update(f"{DOCS}/c/r", {"r": R}))
commit("a reference in an array", update(f"{DOCS}/c/r", {"r": {"arrayValue": {"values": [R]}}}))
commit("a reference in a map", update(f"{DOCS}/c/r", {"m": {"mapValue": {"fields": {"r": R}}}}))
commit("a reference to a named database", update(f"{DOCS}/c/r", {"r": ref(f"{db('p_x', 'x')}/documents/c/d")}))
commit("a reference to a database", update(f"{DOCS}/c/r", {"r": ref(bad)}))
commit("a reference to project '.'", update(f"{DOCS}/c/r", {"r": ref(f"{db('.')}/documents/c/d")}))
commit("a reference in a transform's operand", update(
    f"{DOCS}/c/r", {}, updateTransforms=[{"fieldPath": "a", "appendMissingElements": {"values": [R]}}]))
commit("a reference before a reserved field name", update(f"{DOCS}/c/r", {"r": R, "__x__": NULL}))
commit("a reserved field name before a reference", update(f"{DOCS}/c/r", {"__x__": NULL, "r": R}))
commit("a reference before a reserved nested name", update(
    f"{DOCS}/c/r", {"m": {"mapValue": {"fields": {"r": R, "__x__": NULL}}}}))
commit("a reference before the write's mask", update(f"{DOCS}/c/r", {"r": R}, updateMask={"fieldPaths": ["a..b"]}))
commit("a reference before the next write's name", update(f"{DOCS}/c/r", {"r": R}), update("bad"))
commit("a reference in a transform before the next write's name", update(
    f"{DOCS}/c/r", {}, updateTransforms=[{"fieldPath": "a", "appendMissingElements": {"values": [R]}}]),
    update("bad"))
commit("a reference before the next write's mask", update(f"{DOCS}/c/r", {"r": R}),
       update(f"{DOCS}/c/s", {}, updateMask={"fieldPaths": ["a..b"]}))
commit("a reference before the next write's reserved name", update(f"{DOCS}/c/r", {"r": R}),
       update(f"{DOCS}/c/s", {"__x__": NULL}))
commit("a reference before a later verify's precondition", update(f"{DOCS}/c/r", {"r": R}), {"verify": f"{DOCS}/c/s"})
commit("a verify's precondition before a later reference", {"verify": f"{DOCS}/c/s"}, update(f"{DOCS}/c/r", {"r": R}))
commit("a reference before a second write to its document", update(f"{DOCS}/c/r", {"r": R}), update(f"{DOCS}/c/r"))
commit("a reference before the next write's geo point", update(f"{DOCS}/c/r", {"r": R}),
       update(f"{DOCS}/c/s", {"g": {"geoPointValue": {"latitude": 100, "longitude": 0}}}))
commit("a geo point before the next write's reference",
       update(f"{DOCS}/c/s", {"g": {"geoPointValue": {"latitude": 100, "longitude": 0}}}),
       update(f"{DOCS}/c/r", {"r": R}))
commit("a reference before an earlier write's long value",
       update(f"{DOCS}/c/s", {"a": {"stringValue": "x" * 1048500}}), update(f"{DOCS}/c/r", {"r": R}))

# References in queries.
query("a filter's reference", where("r", "EQUAL", R))
query("a filter's reference to project '.'", where("r", "EQUAL", ref(f"{db('.')}/documents/c/d")))
query("a filter's reference, before the rest of its name", where("r", "EQUAL", ref(f"{bad}/documents/c")))
query("a filter's reference in IN", where("r", "IN", {"arrayValue": {"values": [R]}}))
query("a filter's reference in NOT_IN", where("r", "NOT_IN", {"arrayValue": {"values": [R]}}))
query("a key filter's reference", where("__name__", "EQUAL", R))
query("a reference before a negative limit", {**where("r", "EQUAL", R), "limit": -1})
query("a reference in a filter that matches nothing", {"where": {"compositeFilter": {"op": "AND", "filters": [
    {"fieldFilter": {"field": {"fieldPath": "a"}, "op": "LESS_THAN", "value": NULL}},
    {"fieldFilter": {"field": {"fieldPath": "r"}, "op": "EQUAL", "value": R}}]}}})
query("a start cursor's reference", {"orderBy": [{"field": {"fieldPath": "r"}}], "startAt": {"values": [R]}})
query("an end cursor's reference", {"orderBy": [{"field": {"fieldPath": "r"}}], "endAt": {"values": [R]}})
query("a key cursor's reference", {"orderBy": [{"field": {"fieldPath": "__name__"}}], "startAt": {"values": [R]}})
case("an aggregation's filter's reference", "POST", f"/v1/{DB}/documents:runAggregationQuery",
     {"structuredAggregationQuery": {"structuredQuery": {"from": [{"collectionId": "c"}], **where("r", "EQUAL", R)},
                                     "aggregations": [{"count": {}}]}})

# The /emulator/v1 endpoints.
rules = {"rules": {"files": [{"name": "firestore.rules", "content": RULES}]}}
case("rules: put, project only", "PUT", "/emulator/v1/projects/P1:securityRules", rules)
case("rules: get, project only", "GET", "/emulator/v1/projects/P1:securityRules")
case("rules: put, project '.' only", "PUT", "/emulator/v1/projects/.:securityRules", rules)
case("rules: put", "PUT", f"/emulator/v1/{bad}:securityRules", rules)
case("rules: get", "GET", f"/emulator/v1/{bad}:securityRules")
case("rules: put, project '.', before the payload", "PUT", f"/emulator/v1/{db('.')}:securityRules",
     {"rules": {"files": []}})
case("rule coverage, project only", "GET", "/emulator/v1/projects/P1:ruleCoverage")
case("rule coverage", "GET", f"/emulator/v1/{bad}:ruleCoverage")
case("clear a database", "DELETE", f"/emulator/v1/{bad}/documents")
case("clear a database of project '.'", "DELETE", f"/emulator/v1/{db('.')}/documents")
case("delete a document's tree", "DELETE", f"/emulator/v1/{bad}/documents/c/d")
case("export: the body's database, before the directory", "POST", f"/emulator/v1/projects/{P}:export",
     {"database": bad, "export_directory": "/nonexistent-hidane-dir"})
case("export: project '.'", "POST", f"/emulator/v1/projects/{P}:export",
     {"database": db("."), "export_directory": "/nonexistent-hidane-dir"})
case("export: database '.'", "POST", f"/emulator/v1/projects/{P}:export",
     {"database": db("P1", "."), "export_directory": "/nonexistent-hidane-dir"})
case("import: the body's database, before the directory", "POST", f"/emulator/v1/projects/{P}:import",
     {"database": bad, "export_directory": "/nonexistent-hidane-dir"})


def norm(text):
    return mask(text, PROJECT)


def run():
    out = []
    for c in CASES:
        conn = http.client.HTTPConnection(*HOST.split(":"), timeout=30)
        path = c["path"].replace("{project}", PROJECT)
        body = json.dumps(c["body"]).replace("{project}", PROJECT) if "body" in c else None
        headers = dict(c["headers"])
        if body is not None:
            headers["Content-Type"] = "application/json"
        conn.request(c["method"], path, body=None if body is None else body.encode(), headers=headers)
        r = conn.getresponse()
        out.append(dict(c, outcome={"status": r.status, "contentType": r.getheader("content-type"),
                                    "body": norm(r.read().decode())}))
    return out


json.dump({
    "oracle": "cloud-firestore-emulator v1.22.0 (sha256 9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)",
    "generated_by": "tools/oracle/project_ids.py",
    "cases": run(),
}, sys.stdout, ensure_ascii=False, indent=1)
print()
