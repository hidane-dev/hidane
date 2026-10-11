#!/usr/bin/env python3
"""Which queries does a `list` rule allow, and what does a denial say? (#66, #63)

Usage (from the repository root), with a freshly started jar under an English locale
(`java -Duser.language=en -Duser.country=US -jar …`):
    python3 -I tools/oracle/rules_list.py 127.0.0.1:8086 > fixture.json

Rules are not filters: a query is allowed only if every document it could return satisfies the
condition, which the official emulator decides from the query's constraints, not from the data.
Each case of the table loads one `allow list` condition and runs every query of `QUERIES` with a
token for `u1` (an unsigned JWT): queries, aggregations and listings. The pairs after it run one
query under one condition each. Documents are written as the owner. Stdlib only.
"""

import base64
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import steps  # noqa: E402
from steps import OWNER, call, case, cases, dump, rules  # noqa: E402

steps.KEEP_OK = False
PROJECT = "list"
DOCS = f"projects/{PROJECT}/databases/(default)/documents"
BASE = f"/v1/{DOCS}"
I = lambda n: {"integerValue": str(n)}
D = lambda f: {"doubleValue": f}
S = lambda s: {"stringValue": s}
A = lambda *v: {"arrayValue": {"values": list(v)}}
REF = lambda d: {"referenceValue": f"{DOCS}/{d}"}


def b64(obj):
    return base64.urlsafe_b64encode(json.dumps(obj, separators=(",", ":")).encode()).decode().rstrip("=")


# Nothing in a token is verified; `sub` is all `request.auth.uid` needs.
TOKEN = "Bearer " + b64({"alg": "none"}) + "." + b64({"sub": "u1"}) + "."


def field(path, op, value):
    return {"fieldFilter": {"field": {"fieldPath": path}, "op": op, "value": value}}


def unary(path, op):
    return {"unaryFilter": {"field": {"fieldPath": path}, "op": op}}


def both(*filters, op="AND"):
    return {"compositeFilter": {"op": op, "filters": list(filters)}}


def query(where=None, order=None, limit=None, offset=None, start=None, group=False):
    q = {"from": [{"collectionId": "c", **({"allDescendants": True} if group else {})}]}
    if where is not None:
        q["where"] = where
    if order is not None:
        q["orderBy"] = [{"field": {"fieldPath": f}, "direction": d} for f, d in order]
    if start is not None:
        q["startAt"] = {"values": start, "before": True}
    if limit is not None:
        q["limit"] = limit
    if offset is not None:
        q["offset"] = offset
    return q


RUN = lambda q: ("POST", f"{BASE}:runQuery", {"structuredQuery": q})
COUNT = lambda q: ("POST", f"{BASE}:runAggregationQuery",
                   {"structuredAggregationQuery": {"structuredQuery": q, "aggregations": [{"alias": "n", "count": {}}]}})

QUERIES = {
    "all": RUN(query()),
    "n == 1": RUN(query(field("n", "EQUAL", I(1)))),
    "n == 2": RUN(query(field("n", "EQUAL", I(2)))),
    "n == 1.0": RUN(query(field("n", "EQUAL", D(1.0)))),
    "n > 0": RUN(query(field("n", "GREATER_THAN", I(0)))),
    "n >= 1": RUN(query(field("n", "GREATER_THAN_OR_EQUAL", I(1)))),
    "n < 5": RUN(query(field("n", "LESS_THAN", I(5)))),
    "n > 0 and n < 10": RUN(query(both(field("n", "GREATER_THAN", I(0)), field("n", "LESS_THAN", I(10))))),
    "n >= 1 and n <= 1": RUN(query(both(field("n", "GREATER_THAN_OR_EQUAL", I(1)), field("n", "LESS_THAN_OR_EQUAL", I(1))))),
    "n in [1]": RUN(query(field("n", "IN", A(I(1))))),
    "n in [1, 2]": RUN(query(field("n", "IN", A(I(1), I(2))))),
    "n not-in [3]": RUN(query(field("n", "NOT_IN", A(I(3))))),
    "n != 3": RUN(query(field("n", "NOT_EQUAL", I(3)))),
    "n == null": RUN(query(unary("n", "IS_NULL"))),
    "n != null": RUN(query(unary("n", "IS_NOT_NULL"))),
    "n == 1 or n == 2": RUN(query(both(field("n", "EQUAL", I(1)), field("n", "EQUAL", I(2)), op="OR"))),
    "n == 1 or s == 'x'": RUN(query(both(field("n", "EQUAL", I(1)), field("s", "EQUAL", S("x")), op="OR"))),
    "n == 1 and s == 'x'": RUN(query(both(field("n", "EQUAL", I(1)), field("s", "EQUAL", S("x"))))),
    "s == 'x'": RUN(query(field("s", "EQUAL", S("x")))),
    "s >= 'a'": RUN(query(field("s", "GREATER_THAN_OR_EQUAL", S("a")))),
    "tags array-contains 'a'": RUN(query(field("tags", "ARRAY_CONTAINS", S("a")))),
    "tags array-contains-any ['a']": RUN(query(field("tags", "ARRAY_CONTAINS_ANY", A(S("a"))))),
    "tags array-contains-any ['a', 'b']": RUN(query(field("tags", "ARRAY_CONTAINS_ANY", A(S("a"), S("b"))))),
    "a.b == 1": RUN(query(field("a.b", "EQUAL", I(1)))),
    "owner == 'u1'": RUN(query(field("owner", "EQUAL", S("u1")))),
    "owner == 'u2'": RUN(query(field("owner", "EQUAL", S("u2")))),
    "__name__ == c/d1": RUN(query(field("__name__", "EQUAL", REF("c/d1")))),
    "__name__ in [c/d1, c/d2]": RUN(query(field("__name__", "IN", A(REF("c/d1"), REF("c/d2"))))),
    "__name__ > c/d1": RUN(query(field("__name__", "GREATER_THAN", REF("c/d1")))),
    "orderBy n": RUN(query(order=[("n", "ASCENDING")])),
    "orderBy n desc limit 5": RUN(query(order=[("n", "DESCENDING")], limit=5)),
    "orderBy n startAt 1": RUN(query(order=[("n", "ASCENDING")], start=[I(1)])),
    "limit 5": RUN(query(limit=5)),
    "n == 1 limit 5": RUN(query(field("n", "EQUAL", I(1)), limit=5)),
    "limit 20": RUN(query(limit=20)),
    "offset 2": RUN(query(offset=2)),
    "group: n == 1": RUN(query(field("n", "EQUAL", I(1)), group=True)),
    "count all": COUNT(query()),
    "count n == 1": COUNT(query(field("n", "EQUAL", I(1)))),
    "listDocuments": ("GET", f"{BASE}/c", None),
}

R = "resource.data"
CONDITIONS = [
    "true", "false", "resource == null", "resource != null", f"{R} == null",
    f"{R}.n == 1", f"{R}.n == 1.0", f"{R}.n == 2", f"{R}.n > 0", f"{R}.n >= 1", f"{R}.n < 5", f"{R}.n <= 1",
    f"{R}.n > 0 && {R}.n < 10", f"{R}.n != 3", f"{R}.n != 1", f"{R}.n in [1, 2]", f"{R}.n in [1]",
    f"{R}.n is int", f"{R}.n is number", f"{R}.n == null", f"{R}.n != null",
    f"!({R}.n == 2)", f"!({R}.n != 1)", f"{R}.n == 1 ? true : false",
    f"{R}.n == 1 || {R}.n == 2", f"{R}.n == 1 && {R}.s == 'x'", f"{R}.n == 1 || {R}.s == 'x'",
    f"{R}.s == 'x'", f"{R}.s > 'a'", f"'a' in {R}.tags", f"{R}.tags.hasAny(['a'])", f"{R}.tags.hasAll(['a'])",
    f"{R}.tags.size() > 0", f"{R}.a.b == 1", f"{R}['n'] == 1", f"{R}.get('n', 0) == 1",
    f"{R}.keys().hasAll(['n'])", f"{R}.size() > 0", f"{R}.n + 1 == 2", f"int({R}.n) == 1",
    "resource.id == 'd1'", "resource['__name__'] == /databases/$(database)/documents/c/d1",
    f"{R}.owner == request.auth.uid", f"request.auth.uid == {R}.owner",
    "f()", "g(resource.data.n)",
    "request.query.limit <= 5", "request.query.limit == null", "request.query.offset == 0",
    f"request.query.limit <= 5 && {R}.n == 1", f"{R}.n == 1 || request.query.limit <= 5",
    f"{R}.n == 1 && exists(/databases/$(database)/documents/c/d1)",
]
FUNCTIONS = "function f() {\n  return resource.data.n == 1;\n}\nfunction g(v) {\n  return v == 1;\n}\n"

# Pairs measured one by one: queries outside the table above, each with the conditions that
# tell how a constraint compares, which documents and wildcards a list sees, and its limit.
MORE = {
    "n > 5": RUN(query(field("n", "GREATER_THAN", I(5)))),
    "n >= 1.5": RUN(query(field("n", "GREATER_THAN_OR_EQUAL", D(1.5)))),
    "n == 1 and n > 0": RUN(query(both(field("n", "EQUAL", I(1)), field("n", "GREATER_THAN", I(0))))),
    "n > 0 and n > 2": RUN(query(both(field("n", "GREATER_THAN", I(0)), field("n", "GREATER_THAN", I(2))))),
    "n not-in [1, 2]": RUN(query(field("n", "NOT_IN", A(I(1), I(2))))),
    "n is nan": RUN(query(unary("n", "IS_NAN"))),
    "n is not nan": RUN(query(unary("n", "IS_NOT_NAN"))),
    "s > 'a'": RUN(query(field("s", "GREATER_THAN", S("a")))),
    "n > 0 or n < -5": RUN(query(both(field("n", "GREATER_THAN", I(0)), field("n", "LESS_THAN", I(-5)), op="OR"))),
    "n in [1, 2] and s == 'x'": RUN(query(both(field("n", "IN", A(I(1), I(2))), field("s", "EQUAL", S("x"))))),
    "a.b > 0": RUN(query(field("a.b", "GREATER_THAN", I(0)))),
    "n > 0 or s == 'x'": RUN(query(both(field("n", "GREATER_THAN", I(0)), field("s", "EQUAL", S("x")), op="OR"))),
    "__name__ == c/d1 and n == 2": RUN(query(both(field("__name__", "EQUAL", REF("c/d1")), field("n", "EQUAL", I(2))))),
    "__name__ == c/missing": RUN(query(field("__name__", "EQUAL", REF("c/missing")))),
    "__name__ in [c/d1, c/missing]": RUN(query(field("__name__", "IN", A(REF("c/d1"), REF("c/missing"))))),
    "listDocuments pageSize 2": ("GET", f"{BASE}/c?pageSize=2", None),
}
DOC = "/{path=**}/c/{d}"
PAIRS = [
    (DOC, "n > 5", [f"{R}.n {c}" for c in ["< 3", "== 1", "!= 1", "!= 'a'", "in [1, 2]", "> 6", "> 4", "> 4.0", "> 4.5",
                                         ">= 5", ">= 5.0", "> 5.0", "!= 5", "!= 5.0", "!= 1.0", "== null", "is constraint"]]
     + [f"!({R}.n < 3)", f"int({R}.n) == 1", f"string({R}.n) == 'x'", f"{R}.n.x == 1", f"[{R}.n] == [1]",
        f"{R}.n in {{'a': 1}}"]),
    (DOC, "n >= 1.5", [f"{R}.n {c}" for c in ["> 1", ">= 1.0", "> 1.4", ">= 1.5", "> 1.5", "!= 1", "!= 1.0", "!= 1.5"]]),
    (DOC, "n == 1 and n > 0", [f"{R}.n {c}" for c in ["== 1", "== 1.0", "> 0", "> 0.5", "!= 2", "< 2", ">= 1", "<= 1", "is int"]]),
    (DOC, "n > 0 and n > 2", [f"{R}.n > 0", f"{R}.n > 1", f"{R}.n > 2"]),
    (DOC, "n not-in [1, 2]", [f"{R}.n != 1", f"{R}.n != 2", f"{R}.n != 1.0", f"{R}.n != 3", f"!({R}.n in [1, 2])"]),
    (DOC, "n is nan", [f"math.isNaN({R}.n)", f"{R}.n != {R}.n", f"{R}.n is float"]),
    (DOC, "n is not nan", [f"{R}.n != null", f"{R}.n != 1", f"{R}.n == {R}.n"]),
    (DOC, "s > 'a'", [f"{R}.s {c}" for c in [">= 'a'", "!= 'a'", "> 'a'", "> 'A'", "!= 1", "> 1"]]),
    (DOC, "n != 3", [f"{R}.n < 3 || {R}.n > 3", f"{R}.n != 3.0", f"{R}.n != 3 && {R}.n != null"]),
    (DOC, "n < 5", [f"{R}.n < 5.0", f"5 > {R}.n", f"{R}.n <= 5", f"{R}.n < 6", f"{R}.n < 4"]),
    (DOC, "tags array-contains 'a'", [f"{R}.tags.hasOnly(['a'])", f"{R}.tags != null", f"'b' in {R}.tags",
                                      f"!('b' in {R}.tags)", f"{R}.tags.hasAny(['b'])", f"{R}.tags.hasAny(['a', 'b'])",
                                      f"{R}.tags.hasAny([])", f"{R}.tags.hasAny('a')", f"{R}.tags == ['a']", f"1 in {R}.tags"]),
    (DOC, "n > 0 or n < -5", [f"{R}.n != 0", f"{R}.n > 0 || {R}.n < -5"]),
    (DOC, "n in [1, 2] and s == 'x'", [f"{R}.n == 1 || {R}.n == 2", f"{R}.s == 'x'"]),
    (DOC, "n > 0 or s == 'x'", [f"{R}.n > 0 || {R}.s == 'x'", f"{R}.size() == 1"]),
    (DOC, "a.b > 0", [f"{R}.a.b > 0", f"{R}.a is map", f"{R}.a.size() == 1"]),
    (DOC, "n > 0", [f"{R}.n > 0 ? true : false", "request.resource == null", f"{R}.n > 0 && {R}.n < 3",
                    f"{R}.n == 1 || {R}.n > 0", "debug(resource.data.n) != null"]),
    (DOC, "n == 1", ["resource.keys().hasAll(['data'])", "resource.size() == 1"]),
    (DOC, "__name__ == c/d1 and n == 2", [f"{R}.n == 1", f"{R}.n == 2"]),
    (DOC, "__name__ == c/missing", ["resource == null", f"{R}.n == 1", "true"]),
    (DOC, "__name__ in [c/d1, c/missing]", ["resource == null", "resource != null", f"{R}.n == 1"]),
    (DOC, "listDocuments pageSize 2", ["request.query.limit == 2", "request.query.limit == null"]),
    (DOC, "listDocuments", ["request.query.limit == 30", "request.query.limit == null", "request.query.offset == 0",
                            "request.query.orderBy.size() == 0"]),
    ("/c/{d}", "n == 1", ["d == 'x'", "d != null", "request.path == /databases/$(database)/documents/c",
                          "request.path[4] == 'c'"]),
    ("/c/{d}", "__name__ == c/d1", ["d == 'd1'", "request.path == /databases/$(database)/documents/c/d1",
                                    "resource.id == 'd1'"]),
    ("/{rest=**}", "n == 1", ["rest == /c", "debug(rest) != null", "rest[0] == 'c'", "rest[0:1] == /c"]),
    ("/{coll}/{d}", "n == 1", ["coll == 'c'", "d != null"]),
    ("/c/{d}", "group: n == 1", ["true"]),
    ("/{path=**}/c/{d}", "group: n == 1", ["debug(path) != null", "d != null", "true", "debug(request.path) != null",
                                           "request.path != null", "request['path'] != null"]),
]


case("documents")
for i, (n, s, tags, b, owner) in enumerate([(1, "x", ["a"], 1, "u1"), (2, "y", ["b"], 2, "u2"), (3, "x", [], 3, "u1")]):
    call("PATCH", f"{BASE}/c/d{i + 1}",
         {"fields": {"n": I(n), "s": S(s), "tags": A(*[S(t) for t in tags]), "a": {"mapValue": {"fields": {"b": I(b)}}},
                     "owner": S(owner)}}, auth=OWNER)

def load(condition, match=DOC):
    rules(PROJECT, "rules_version = '2';\nservice cloud.firestore {\n  match /databases/{database}/documents {\n"
          + "\n".join("    " + line for line in FUNCTIONS.split("\n")) + "\n"
          f"    match {match} {{\n      allow get: if true;\n      allow list: if {condition};\n    }}\n  }}\n}}\n")


def run(name):
    method, path, body = {**QUERIES, **MORE}[name]
    call(method, path, body, auth=TOKEN)
    cases[-1]["steps"][-1]["query"] = name


for condition in CONDITIONS:
    case(condition)
    load(condition)
    for name in QUERIES:
        run(name)

for match, name, conditions in PAIRS:
    for condition in conditions:
        case(f"{name}: {condition}" + ("" if match == DOC else f" (match {match})"))
        load(condition, match)
        run(name)

dump()
