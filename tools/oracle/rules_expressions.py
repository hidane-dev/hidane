#!/usr/bin/env python3
"""How many expressions may a Security Rules decision evaluate, and how are they counted? (#293)

Usage (from the repository root), with the jar started under an English locale
(`java -Duser.language=en -Duser.country=US -jar …`):
    python3 -I tools/oracle/rules_expressions.py 127.0.0.1:8086 > fixture.json

The official emulator stops a decision after 1,000 evaluated expressions ("Unable to evaluate the
expression as the maximum of 1000 expressions to evaluate has been reached."), and the
documentation does not say what counts as one. Requests are made over REST without an
`Authorization` header unless a case says otherwise.

- Costs: for each kind of expression `e`, the script finds the largest N for which
  `allow get: if [e, e, …].size() > 0` (N times) still allows a read of a missing document, and
  records the ruleset and the read at N (allowed) and at N + 1 (denied). The list, `size()`, `>`
  and `0` cost 4, so N tells what `e` costs.
- Boundaries: what costs more than one is placed last in the evaluation order (a list evaluates
  its items last first) with a total of 1,000 and of 1,001.
- Order: an expression counts when its evaluation starts, so an error deep inside one that
  exceeds the limit is not reached.
- Scope: which statements, writes, verifies, documents and query disjunctions count together.
  Each scope case first deletes and writes, as the owner, the documents it uses.

Documents are written as the owner. Stdlib only.
"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import steps  # noqa: E402
from steps import OWNER, call, case, dump, rules  # noqa: E402

# The tests compare a successful answer's status only; the answers to the rulesets (their
# warnings, one for each item of some lists) are not kept.
steps.KEEP_OK = False

PROJECT = "expressions"
BASE = f"projects/{PROJECT}/databases/(default)/documents"
I = lambda n: {"integerValue": str(n)}
# An unsigned token for `user1`, as the official emulator accepts them.
USER = ("Bearer eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJzdWIiOiJ1c2VyMSIsInVzZXJfaWQiOiJ1c2VyMSIs"
        "ImVtYWlsIjoidUBleGFtcGxlLmNvbSIsImZpcmViYXNlIjp7InNpZ25faW5fcHJvdmlkZXIiOiJwYXNzd29yZCJ9fQ.")

FUNCTIONS = """function f() { return true; }
function g(a) { return a; }
function h() { let a = 1; return a; }
function h2() { let a = 1; return a + a; }
function h3() { let a = 1; let b = 2; return 0; }
function isOwner(id) { return request.auth != null && request.auth.uid == id; }
function k() { let a = (1).a; return true; }
function m() { let a = (1).a; return a; }"""


def source(statements):
    """A ruleset with FUNCTIONS and `statements` in `match /c/{id}`."""
    indent = lambda text, n: "\n".join(" " * n + line for line in text.split("\n"))
    return ("rules_version = '2';\nservice cloud.firestore {\n  match /databases/{database}/documents {\n"
            + indent(FUNCTIONS, 4) + "\n    match /c/{id} {\n" + indent(statements, 6) + "\n    }\n  }\n}\n")


def load(statements, record=True):
    content = source(statements)
    if record:
        rules(PROJECT, content)
    else:
        status, text = call("PUT", f"/emulator/v1/projects/{PROJECT}:securityRules",
                            {"rules": {"files": [{"name": "firestore.rules", "content": content}]}},
                            record=False)
        assert status == 200, text


def items(e, n):
    """`e` n times in a list, then `.size() > 0`: costs n times `e`'s cost, plus 4."""
    return "[" + ",".join([e] * n) + "].size() > 0"


def costing(total, true=True):
    """A condition that costs `total` (at least 5), true or false."""
    return "[" + ",".join(["1"] * (total - 4)) + f"].size() {'>' if true else '<'} 0"


def last(e, cost, total):
    """A condition of `total` that evaluates `e` (which costs `cost`) last."""
    return "[" + ",".join([e] + ["1"] * (total - 4 - cost)) + "].size() > 0"


GET = ("GET", f"/v1/{BASE}/c/d", None)


def allowed(condition):
    load(f"allow get: if {condition};", record=False)
    status, _ = call(*GET, record=False)
    assert status in (403, 404), status
    return status == 404


# Each kind of expression, as a list item.
COSTS = [
    # literals, names and parentheses
    "1", "1.5", "'a'", "b'abc'", "true", "null", "id", "request", "resource", "(1)", "((1))",
    # lists and maps
    "[]", "[1]", "[1, 2, 3]", "{}", "{'a': 1}",
    # members, indexes and slices
    "request.method", "request.auth", "request.time", "request.path[0]", "[1][0]", "[1, 2][0]",
    "{'a': 1}.a", "{'a': 1}['a']", "'abc'[0:1]", "[1, 2, 3][0:1]", "id[0:1]",
    # operators
    "1 == 1", "1 + 1", "'a' + 'b'", "1 in [1]", "id in {'d': 1}", "1 is int", "id is string",
    "!true", "!(true)", "-1", "-(1)", "-9223372036854775808", "request.auth == null",
    "resource == null", "request.auth != null",
    # && and ||
    "true && true", "(true && true)", "true && false", "false && true", "true || false",
    "false || true", "false || false", "true && true && true", "(1 == 1) && (1 == 1)",
    "true == true && true", "request.auth != null && request.auth.uid == id",
    # conditionals
    "true ? 1 : 2", "false ? 1 : 2", "true ? (1 + 1) : 2", "true ? 1 : (1 + 1)",
    "false ? (1 + 1) : 2", "false ? 1 : (1 + 1)", "1 > 0 ? 1 : 2", "true ? (false ? 1 : 2) : 3",
    "false ? 1 : (true ? 2 : 3)", "false ? 1 : (false ? 2 : 3)", "(true ? 1 : 2) == 1",
    "(1 ? 2 : 3) == 2 || true", "(1 ? 2 : 3) || true", "((1).a ? 2 : 3) || true",
    # calls
    "id.size()", "'a'.size()", "[1, 2].hasAny([2])", "string(1)", "math.abs(1)", "math.abs(-1)",
    "f()", "g(1)", "h()", "h2()", "h3()", "isOwner(id)",
    # errors that || absorbs
    "1 || true", "(1 && false) == false", "(true && 1) || true", "(false || 1) || true", "(1).a || true",
    "[1][5] || true", "{'a': 1}.b || true", "zzz || true", "!1 || true", "(1 + 'a') || true",
    "('a'.zz()) || true", "math.abs('a') || true", "g((1).a) || true", "k()", "m() || true",
    "exists(/a/b) || true", "get(/databases/x/documents/c/d) || true", "/a/$([1]) || true",
    # paths and lookups
    "/a", "/a/b", "/a/b/c/d/e", "/a%20b/c", "/$(id)", "/a/$(id)", "/a/$(id + 'x')",
    "/a/$(request.path)", "/databases/x/documents", "/databases/$(database)/documents/c/d",
    "exists(/databases/$(database)/documents/c/d)",
]

for e in COSTS:
    lo, hi = 1, 1000
    assert allowed(items(e, lo)), e
    while hi - lo > 1:
        mid = (lo + hi) // 2
        lo, hi = (mid, hi) if allowed(items(e, mid)) else (lo, mid)
    case(f"costs: {e} ({lo} fit in a list)")
    load(f"allow get: if {items(e, lo)};")
    call(*GET)
    load(f"allow get: if {items(e, lo + 1)};")
    call(*GET)

# What costs more than one, counted after everything else: the total decides.
BOUNDARIES = [
    ("&& evaluating its right side", lambda t: f"{costing(t - 3)} && true"),
    ("a conditional whose condition is true", lambda t: f"{costing(t - 3)} ? true : false"),
    ("a conditional whose condition is false", lambda t: f"{costing(t - 5, False)} ? false : true"),
    ("a conditional whose condition is not a bool", lambda t: f"{costing(t - 5)} && (1 ? 2 : 3)"),
    ("a conditional whose condition fails", lambda t: f"{costing(t - 6)} && ((1).a ? 2 : 3)"),
    ("is", lambda t: last("1 is int", 3, t)),
    ("a path's segments", lambda t: last("/a/b", 3, t)),
    ("a unary minus", lambda t: last("-1", 2, t)),
    ("a list", lambda t: last("[1]", 2, t)),
    ("a function", lambda t: last("f()", 2, t)),
    ("a let", lambda t: last("h()", 4, t)),
]
for name, condition in BOUNDARIES:
    case(f"boundaries: {name}, 1000 then 1001")
    for total in (1000, 1001):
        load(f"allow get: if {condition(total)};")
        call(*GET)

# An expression counts when its evaluation starts, before what is in it: a chain of members
# stops at the limit before the type error at its second member.
for n in (995, 996):
    case(f"order: a chain of {n} members on a map")
    load(f"allow get: if {{'a': 1}}{'.a' * n} == 1;")
    call(*GET)
for name, condition in [
        ("a type error after 994 others", f"{costing(994)} && (1).a == 1"),
        ("a type error, then 996 others", f"(1).a == 1 && {costing(994)}"),
        ("a type error evaluated first among 996 in a list", "[" + ",".join(["1"] * 995) + ", (1).a].size() > 0"),
        ("a type error evaluated last among 996 in a list", "[(1).a, " + ",".join(["1"] * 995) + "].size() > 0")]:
    case(f"order: {name}")
    load(f"allow get: if {condition};")
    call(*GET)

big = items("1", 1200)


def update(doc):
    return {"update": {"name": f"{BASE}/{doc}", "fields": {"n": I(1)}}}


def commit(*writes):
    return ("POST", f"/v1/{BASE}:commit", {"writes": list(writes)})


QUERY = {"structuredQuery": {"from": [{"collectionId": "c"}]}}
EQ = lambda n: {"fieldFilter": {"field": {"fieldPath": "n"}, "op": "EQUAL", "value": I(n)}}
SCOPE = [
    ("a statement's short circuit: false && …", f"allow get: if false && {big};", [GET]),
    ("a statement's short circuit: true || …", f"allow get: if true || {big};", [GET]),
    ("an error at the limit is not absorbed by ||", f"allow get: if {big} || true;", [GET]),
    ("statements count together: 600 false, 304 false, true",
     f"allow get: if {costing(600, False)};\nallow get: if {costing(304, False)};\nallow get: if true;", [GET]),
    ("statements count together: 600 false, 504 false, true",
     f"allow get: if {costing(600, False)};\nallow get: if {costing(504, False)};\nallow get: if true;", [GET]),
    ("statements count together: read 600 false, get 600 true",
     f"allow read: if {costing(600, False)};\nallow get: if {costing(600)};", [GET]),
    ("statements count together: over the limit, then true",
     f"allow get: if {big};\nallow get: if true;", [GET]),
    ("statements of separate matches count together",
     f"allow get: if {costing(600, False)};\n}}\nmatch /{{path=**}} {{\n  allow get: if {costing(600)};", [GET]),
    ("a write: 1000", f"allow read: if true;\nallow write: if {costing(1000)};", [commit(update("c/w1"))]),
    ("a write: 1001", f"allow read: if true;\nallow write: if {costing(1001)};", [commit(update("c/w1"))]),
    ("each write of a batch counts from zero: two of 600",
     f"allow read: if true;\nallow write: if {costing(600)};", [commit(update("c/w1"), update("c/w2"))]),
    ("each write of a batch counts from zero: three of 400",
     f"allow read: if true;\nallow write: if {costing(400)};",
     [commit(update("c/w1"), update("c/w2"), update("c/w3"))]),
    ("an upsert's other method counts from zero: a missing document",
     f"allow read: if true;\nallow create: if {costing(600)};\nallow update: if {costing(600)};",
     [commit(update("c/new"))]),
    ("an upsert's other method counts from zero: an existing document",
     f"allow read: if true;\nallow create: if {costing(600)};\nallow update: if {costing(600)};",
     [commit(update("c/e"))]),
    ("each verify of a commit counts from zero: a write and a verify of 600",
     f"allow get: if {costing(600)};\nallow write: if {costing(600)};",
     [commit(update("c/w1"), {"verify": f"{BASE}/c/e", "currentDocument": {"exists": True}})]),
    ("each verify of a commit counts from zero: two verifies of 600",
     f"allow get: if {costing(600)};\nallow write: if true;",
     [commit({"verify": f"{BASE}/c/e", "currentDocument": {"exists": True}},
             {"verify": f"{BASE}/c/e2", "currentDocument": {"exists": True}})]),
    ("a verify: 1001",
     f"allow get: if {costing(1001)};\nallow write: if true;",
     [commit(update("c/w1"), {"verify": f"{BASE}/c/e", "currentDocument": {"exists": True}})]),
    ("a query: 1000", f"allow list: if {costing(1000)};", [("POST", f"/v1/{BASE}:runQuery", QUERY)]),
    ("a query: 1001", f"allow list: if {costing(1001)};", [("POST", f"/v1/{BASE}:runQuery", QUERY)]),
    ("each disjunction of a query counts from zero: or",
     f"allow list: if {costing(600)};",
     [("POST", f"/v1/{BASE}:runQuery", {"structuredQuery": {
         **QUERY["structuredQuery"], "where": {"compositeFilter": {"op": "OR", "filters": [EQ(1), EQ(2)]}}}})]),
    ("each disjunction of a query counts from zero: in",
     f"allow list: if {costing(600)};",
     [("POST", f"/v1/{BASE}:runQuery", {"structuredQuery": {
         **QUERY["structuredQuery"], "where": {"fieldFilter": {
             "field": {"fieldPath": "n"}, "op": "IN", "value": {"arrayValue": {"values": [I(1), I(2)]}}}}}})]),
    ("each document of a batchGet counts from zero",
     f"allow get: if {costing(600)};", [("POST", f"/v1/{BASE}:batchGet", {"documents": [f"{BASE}/c/e", f"{BASE}/c/d"]})]),
]
for name, statements, requests in SCOPE:
    case(f"scope: {name}")
    for doc in ("c/w1", "c/w2", "c/w3", "c/new", "c/e", "c/e2"):
        call("DELETE", f"/v1/{BASE}/{doc}", auth=OWNER)
    for doc in ("e", "e2"):
        call("POST", f"/v1/{BASE}/c?documentId={doc}", {"fields": {"n": I(1)}}, auth=OWNER)
    load(statements)
    for request in requests:
        call(*request)

# Signed in: `request.auth.uid == id` is evaluated too.
SIGNED_IN = ("GET", f"/v1/{BASE}/c/user1", None)
for name, e in [("signed in: request.auth != null && request.auth.uid == id",
                 "request.auth != null && request.auth.uid == id"),
                ("signed in: isOwner(id)", "isOwner(id)")]:
    def allowed_signed_in(n):
        load(f"allow get: if {items(e, n)};", record=False)
        status, _ = call(*SIGNED_IN, auth=USER, record=False)
        return status == 404
    lo, hi = 1, 1000
    assert allowed_signed_in(lo), e
    while hi - lo > 1:
        mid = (lo + hi) // 2
        lo, hi = (mid, hi) if allowed_signed_in(mid) else (lo, mid)
    case(f"{name} ({lo} fit in a list)")
    load(f"allow get: if {items(e, lo)};")
    call(*SIGNED_IN, auth=USER)
    load(f"allow get: if {items(e, lo + 1)};")
    call(*SIGNED_IN, auth=USER)

dump()
