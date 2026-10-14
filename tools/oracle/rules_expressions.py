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
- Order: past the limit, its error wins over an error inside what exceeds it.
- Scope: which statements, writes, verifies, documents and query disjunctions count together.
  Each scope case first deletes and writes, as the owner, the documents it uses.
- Reads, outcomes, statements, versions, writes, lists and requests: the script finds the
  largest N for which a condition that holds `cost(N)` (an expression costing N, built from
  functions so that the rulesets stay small) does not reach the limit, and records the case at
  N and at N + 1. The conditions read `resource`, `resource.data`, `request.resource.data`,
  `request.auth`, wildcards and documents (`get()`, `exists()`, `getAfter()`, `existsAfter()`,
  stored and missing), the cost placed before, after, between them, in functions, in `let`s and
  on either side of `&&`, `||` and conditionals; they come to true, false, an error, or an error
  that `&&` or `||` absorbs. The official emulator evaluates a condition that reads the stored
  document twice, the first time without it (`docs/parity-exceptions.md`); each pass counts
  from zero. Requests: gets, lists of a collection, queries (with filters on what the rules
  read, `__name__`, collection groups), aggregations, commits (creates, updates, upserts,
  deletes, field transforms, verifies), batchGets, and reads and commits in transactions, whose
  ID is the one `beginTransaction` answered; a batchGet that begins a transaction and is denied
  leaves nothing locked, and the owner then writes its document.

Documents are written as the owner. Stdlib only.
"""

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import steps  # noqa: E402
from steps import OWNER, call, case, cases, dump, rules  # noqa: E402

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

# Past the limit, its error wins over an error of what was being evaluated: a chain of members
# ends at the limit, not at the type error of its second member. (When each expression counts,
# once what is in it is evaluated, shows in what `debug()` prints: rules_expressions_report.py.)
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

# What a condition reads, where its cost sits and what it comes to, and the requests it decides.
# Each case finds the largest N for which a condition holding `cost(N)` does not reach the limit,
# and records its requests at N and N + 1, each time after writing (or deleting) the documents
# it writes, as the owner.

D = "/databases/$(database)/documents"
LIMIT = "maximum of 1000 expressions"
# Functions whose calls cost 2, 7, 17, 37, 77, 157, 317 and 637 expressions.
COSTS = [2]
for _ in range(7):
    COSTS.append(2 * COSTS[-1] + 3)
COST_FUNCTIONS = "function d0() { return true; }\n" + "\n".join(
    f"function d{k}() {{ return d{k - 1}() && d{k - 1}(); }}" for k in range(1, len(COSTS)))


def cost(n, true=True):
    """An expression that costs `n` (at least 1, or 2 when false), true or false."""
    if not true:
        return f"!({cost(n - 1)})"
    if n <= 4:
        return ["true", "!false", "!!true", "!!!false"][n - 1]
    k = max(k for k, c in enumerate(COSTS) if c + 3 <= n)
    return f"d{k}() && {cost(n - COSTS[k] - 2)}"


def ruleset(body, version="2"):
    """A ruleset with COST_FUNCTIONS and `body` in `match /databases/{database}/documents`."""
    indent = lambda text: "\n".join("    " + line if line else "" for line in text.split("\n"))
    head = f"rules_version = '{version}';\n" if version else ""
    return (head + "service cloud.firestore {\n  match /databases/{database}/documents {\n"
            + indent(COST_FUNCTIONS) + "\n" + indent(body) + "\n  }\n}\n")


def document(path, fields=None):
    """Deletes the document at `path`, then writes `fields` there unless they are None."""
    return [("DELETE", f"/v1/{BASE}/{path}", None, OWNER)] + (
        [] if fields is None else [("PATCH", f"/v1/{BASE}/{path}", {"fields": fields}, OWNER)])


def attempt(body, requests, version, record):
    """Loads `body`, then makes `requests`; whether an answer reached the limit. A request is
    `(method, path, body, auth)`, or a function of `record` that makes its own calls."""
    content = ruleset(body, version)
    if record:
        rules(PROJECT, content)
    else:
        status, text = call("PUT", f"/emulator/v1/projects/{PROJECT}:securityRules",
                            {"rules": {"files": [{"name": "firestore.rules", "content": content}]}},
                            record=False)
        assert status == 200, text
    reached = False
    for request in requests:
        if callable(request):
            reached |= request(record)
        else:
            method, path, payload, auth = request
            _, text = call(method, path, payload, auth=auth, record=record)
            reached |= LIMIT in text
    return reached


def boundary(name, body, requests, setup=(), version="2"):
    """Records `name` at the largest N for which `body(N)` does not reach the limit, and N + 1."""
    def reaches(n, record=False):
        for request in setup:
            call(*request[:3], auth=request[3], record=record)
        return attempt(body(n), requests, version, record)
    lo, hi = 5, 1000
    assert not reaches(lo) and reaches(hi), name
    while hi - lo > 1:
        mid = (lo + hi) // 2
        lo, hi = (mid, hi) if not reaches(mid) else (lo, mid)
    case(f"{name} ({lo} fits)")
    reaches(lo, True)
    reaches(lo + 1, True)


def get(path, auth=None):
    return ("GET", f"/v1/{BASE}/{path}", None, auth)


case("documents")
for request in (document("c/e", {"n": I(1)}) + document("c/f", {"n": I(1)}) + document("o/x", {"n": I(1)})
                + document("c/e/s/t", {"n": I(1)}) + document("c/zz") + document("o/zz")):
    call(*request[:3], auth=request[3])

# What a get's condition reads: the cost before and after it, and for some, elsewhere.
READS = [
    ("resource.data of a stored document", "resource.data.n == 1", "c/e", None),
    ("resource of a missing document", "resource == null", "c/zz", None),
    ("resource.data of a missing document, absorbed", "(resource.data.n == 1 || true)", "c/zz", None),
    ("resource.id", "resource.id == 'e'", "c/e", None),
    ("resource['__name__']", "resource['__name__'] != null", "c/e", None),
    ("get() of a stored document", f"get({D}/o/x).data.n == 1", "c/e", None),
    ("get() of a missing document, absorbed", f"(get({D}/o/zz).data.n == 1 || true)", "c/e", None),
    ("get() of the document read", f"get({D}/c/e).data.n == 1", "c/e", None),
    ("exists() of a stored document", f"exists({D}/o/x)", "c/e", None),
    ("exists() of a missing document", f"!exists({D}/o/zz)", "c/e", None),
    ("getAfter() of a stored document", f"getAfter({D}/o/x).data.n == 1", "c/e", None),
    ("existsAfter() of a stored document", f"existsAfter({D}/o/x)", "c/e", None),
    ("request.auth signed out", "request.auth == null", "c/e", None),
    ("request.auth signed in", "request.auth.uid == 'user1'", "c/e", USER),
    ("a wildcard", "id == 'e'", "c/e", None),
]
PLACES = [
    ("before", lambda c, r: f"{c()} && {r}", ""),
    ("after", lambda c, r: f"{r} && {c()}", ""),
    ("between two reads", lambda c, r: f"{r} && {c()} && {r}", ""),
    ("in a function, before", lambda c, r: f"fc() && {r}", "function fc() {{ return {c}; }}"),
    ("before a function that reads", lambda c, r: f"{c()} && fr()", "function fr() {{ return {r}; }}"),
    ("before a let that reads", lambda c, r: f"{c()} && fl()", "function fl() {{ let x = {r}; return x; }}"),
    ("on the right of a false ||", lambda c, r: f"{c(False)} || {r}", ""),
    ("in a conditional on the read", lambda c, r: f"{r} ? {c()} : false", ""),
    ("in a conditional's condition", lambda c, r: f"{c()} ? {r} : false", ""),
]
EVERYWHERE = ("resource.data of a stored document", "resource.data of a missing document, absorbed",
              "get() of a stored document", "get() of a missing document, absorbed")
for rname, r, path, auth in READS:
    for pname, place, function in PLACES:
        if pname not in ("before", "after") and rname not in EVERYWHERE:
            continue
        def body(n, r=r, place=place, function=function):
            c = lambda true=True: cost(n, true)
            return (function.format(c=c(), r=r) + "\n" if function else "") + (
                f"match /c/{{id}} {{\n  allow get: if {place(c, r)};\n}}")
        boundary(f"reads: {rname}, cost {pname}", body, [get(path, auth)])

# What the condition comes to: false, an error, or an error that `&&` or `||` absorbs. The right
# side of `&&` and `||` is evaluated after an error on the left, and when it reaches the limit,
# the limit's error is the condition's.
OUTCOMES = [
    ("false: the cost, then a read that is false", lambda c, f: f"{c} && resource.data.n == 2", "c/e"),
    ("false: a read, then a cost that is false", lambda c, f: f"resource.data.n == 1 && {f}", "c/e"),
    ("false: get(), then a cost that is false", lambda c, f: f"get({D}/o/x).data.n == 1 && {f}", "c/e"),
    ("false: a cost that is false || a read that is false", lambda c, f: f"{f} || resource.data.n == 2", "c/e"),
    ("an error: the cost, then a missing field", lambda c, f: f"{c} && resource.data.zz == 1", "c/e"),
    ("an error: a missing field, then the cost", lambda c, f: f"resource.data.zz == 1 && {c}", "c/e"),
    ("an error: a missing field && a cost that is false", lambda c, f: f"resource.data.zz == 1 && {f}", "c/e"),
    ("an error: a missing document's data, then the cost", lambda c, f: f"resource.data.n == 1 && {c}", "c/zz"),
    ("an error: get() of a missing document, then the cost",
     lambda c, f: f"get({D}/o/zz).data.n == 1 && {c}", "c/e"),
    ("an error: a type error, then the cost", lambda c, f: f"(1).a && {c}", "c/e"),
    ("an error: a type error || a cost that is false", lambda c, f: f"(1).a || {f}", "c/e"),
    ("an error: a cost that is false || a missing field", lambda c, f: f"{f} || resource.data.zz == 1", "c/e"),
    ("absorbed: a missing field || the cost", lambda c, f: f"resource.data.zz == 1 || {c}", "c/e"),
    ("absorbed: a missing field && a cost that is false",
     lambda c, f: f"(resource.data.zz == 1 && {f}) == false", "c/e"),
    ("absorbed: a type error && a cost that is false", lambda c, f: f"!((1).a && {f})", "c/e"),
    ("absorbed: a cost that is false && a missing field", lambda c, f: f"!({f} && resource.data.zz == 1)", "c/e"),
    ("absorbed: a function's unused let that fails, then the cost", lambda c, f: f"fu() && {c}", "c/e"),
    ("absorbed: the cost in a function with an unused let that fails", lambda c, f: "fv()", "c/e"),
]
for name, condition, path in OUTCOMES:
    def body(n, condition=condition):
        return (f"function fu() {{ let a = resource.data.zz; return true; }}\n"
                f"function fv() {{ let a = resource.data.zz; return {cost(n)}; }}\n"
                f"match /c/{{id}} {{\n  allow get: if {condition(cost(n), cost(n, False))};\n}}")
    boundary(f"outcomes: {name}", body, [get(path)])

# The statements of a decision count together, the reads of each too.
STATEMENTS = [
    ("a cost that is false, then a read", lambda n: [cost(n, False), "resource.data.n == 1"]),
    ("a read that is false, then the cost", lambda n: ["resource.data.n == 2", cost(n)]),
    ("a missing field, then the cost", lambda n: ["resource.data.zz == 1", cost(n)]),
    ("get() of a missing document, then the cost", lambda n: [f"get({D}/o/zz).data.n == 1", cost(n)]),
]
for name, conditions in STATEMENTS:
    boundary(f"statements: {name}", lambda n, conditions=conditions: "match /c/{id} {\n" + "".join(
        f"  allow get: if {c};\n" for c in conditions(n)) + "}", [get("c/e")])

# Rules versions, and recursive wildcards: in version 1 `{path=**}` matches documents only below
# the path before it.
VERSIONS = [
    ("1: the cost, then resource.data", "1", lambda n: f"match /c/{{id}} {{\n  allow get: if {cost(n)} && resource.data.n == 1;\n}}", "c/e"),
    ("1: the cost, then an error absorbed", "1", lambda n: f"match /c/{{id}} {{\n  allow get: if {cost(n)} && (resource.data.zz == 1 || true);\n}}", "c/e"),
    ("none: the cost, then get()", None, lambda n: f"match /c/{{id}} {{\n  allow get: if {cost(n)} && get({D}/o/x).data.n == 1;\n}}", "c/e"),
]
for version in ("2", "1"):
    VERSIONS += [
        (f"{version}: {{path=**}} at the root", version,
         lambda n: f"match /{{path=**}} {{\n  allow get: if {cost(n)} && path != null;\n}}", "c/e"),
        (f"{version}: {{path=**}} below a document, for a document of a subcollection", version,
         lambda n: f"match /c/{{id}}/{{path=**}} {{\n  allow get: if {cost(n)} && resource.data.n == 1;\n}}", "c/e/s/t"),
        (f"{version}: {{path=**}} and /c/{{id}}, both matching", version,
         lambda n: (f"match /c/{{id}} {{\n  allow get: if {cost(n, False)};\n}}\n"
                    f"match /{{path=**}} {{\n  allow get: if {cost(n)} && resource.data.n == 1;\n}}"), "c/e"),
    ]
for name, version, body, path in VERSIONS:
    boundary(f"rules_version {name}", body, [get(path)], version=version)


def commit(*writes):
    return ("POST", f"/v1/{BASE}:commit", {"writes": list(writes)}, None)


def write(path, fields=None, mask=None, exists=None, transforms=None):
    w = {"update": {"name": f"{BASE}/{path}", "fields": fields or {}}}
    if mask is not None:
        w["updateMask"] = {"fieldPaths": mask}
    if exists is not None:
        w["currentDocument"] = {"exists": exists}
    if transforms:
        w["updateTransforms"] = transforms
    return w


TIME = [{"fieldPath": "t", "setToServerValue": "REQUEST_TIME"}]
# A commit's condition: what it reads, the cost before or after.
WRITES = [
    ("a create, request.resource.data", "create", write("c/new", {"n": I(1)}, exists=False), "request.resource.data.n == 1"),
    ("a create, resource", "create", write("c/new", {"n": I(1)}, exists=False), "resource == null"),
    ("a create, get()", "create", write("c/new", {"n": I(1)}, exists=False), f"get({D}/o/x).data.n == 1"),
    ("a create, getAfter() of itself", "create", write("c/new", {"n": I(1)}, exists=False), f"getAfter({D}/c/$(id)).data.n == 1"),
    ("a create, existsAfter() of itself", "create", write("c/new", {"n": I(1)}, exists=False), f"existsAfter({D}/c/$(id))"),
    ("an upsert of a missing document, request.resource.data", "write", write("c/new", {"n": I(1)}), "request.resource.data.n == 1"),
    ("an update, request.resource.data", "update", write("c/e", {"n": I(1)}, exists=True), "request.resource.data.n == 1"),
    ("an update, resource.data", "update", write("c/e", {"n": I(1)}, exists=True), "resource.data.n == 1"),
    ("an update, exists() of itself", "update", write("c/e", {"n": I(1)}, exists=True), f"exists({D}/c/$(id))"),
    ("an upsert of a stored document, resource.data", "write", write("c/e", {"n": I(1)}), "resource.data.n == 1"),
    ("an update with a mask, request.resource.data", "update", write("c/e", {"n": I(1)}, mask=["n"]), "request.resource.data.n == 1"),
    ("an update with a server time, request.resource.data", "update",
     write("c/e", {"n": I(1)}, mask=["n"], transforms=TIME), "request.resource.data.t == request.time"),
    ("a create with a server time, request.resource.data", "create",
     write("c/new", {"n": I(1)}, exists=False, transforms=TIME), "request.resource.data.t == request.time"),
    ("a delete, resource.data", "delete", {"delete": f"{BASE}/c/e"}, "resource.data.n == 1"),
    ("a delete, request.resource", "delete", {"delete": f"{BASE}/c/e"}, "request.resource == null"),
    ("a delete, existsAfter() of itself", "delete", {"delete": f"{BASE}/c/e"}, f"!existsAfter({D}/c/$(id))"),
    ("a delete of a missing document, resource", "delete", {"delete": f"{BASE}/c/new"}, "resource == null"),
]
WRITTEN = document("c/e", {"n": I(1)}) + document("c/new")
for name, method, w, r in WRITES:
    for pname, place in [("before", lambda c, r: f"{c} && {r}"), ("after", lambda c, r: f"{r} && {c}")]:
        boundary(f"writes: {name}, cost {pname}",
                 lambda n, method=method, r=r, place=place: (
                     f"match /c/{{id}} {{\n  allow read: if true;\n  allow {method}: if {place(cost(n), r)};\n}}"),
                 [commit(w)], setup=WRITTEN)

QUERY = lambda where=None, group=False, **more: {"structuredQuery": {
    "from": [{"collectionId": "c", **({"allDescendants": True} if group else {})}],
    **({"where": where} if where else {}), **more}}
FIELD = lambda field, op, value: {"fieldFilter": {"field": {"fieldPath": field}, "op": op, "value": value}}
NAME = lambda path: FIELD("__name__", "EQUAL", {"referenceValue": f"{BASE}/{path}"})
RUN = lambda body: ("POST", f"/v1/{BASE}:runQuery", body, None)
AGGREGATE = lambda query: ("POST", f"/v1/{BASE}:runAggregationQuery", {"structuredAggregationQuery": {
    **query, "aggregations": [{"alias": "n", "count": {}}]}}, None)
# A list's condition: `resource.data` holds what the filter says of the documents; a field it
# does not name is undefined, and a comparison it does not decide is an error.
LISTS = [
    ("a list of a collection", ("GET", f"/v1/{BASE}/c", None, None), "/c/{id}"),
    ("a query", RUN(QUERY()), "/c/{id}"),
    ("a query of n == 1", RUN(QUERY(FIELD("n", "EQUAL", I(1)))), "/c/{id}"),
    ("a query of n > 0", RUN(QUERY(FIELD("n", "GREATER_THAN", I(0)))), "/c/{id}"),
    ("a query of a stored document's __name__", RUN(QUERY(NAME("c/e"))), "/c/{id}"),
    ("a query of a missing document's __name__", RUN(QUERY(NAME("c/zz"))), "/c/{id}"),
    ("an aggregation", AGGREGATE(QUERY()), "/c/{id}"),
    ("an aggregation of n == 1", AGGREGATE(QUERY(FIELD("n", "EQUAL", I(1)))), "/c/{id}"),
    ("a collection group query", RUN(QUERY(group=True)), "/{path=**}/c/{id}"),
    ("a collection group query of n == 1", RUN(QUERY(FIELD("n", "EQUAL", I(1)), group=True)), "/{path=**}/c/{id}"),
]
LIST_READS = [
    ("resource.data, cost before", lambda c: f"{c} && resource.data.n == 1"),
    ("resource.data, cost after", lambda c: f"resource.data.n == 1 && {c}"),
    ("resource.data absorbed, cost before", lambda c: f"{c} && (resource.data.n == 1 || true)"),
]
for qname, request, match in LISTS:
    for rname, condition in LIST_READS:
        boundary(f"lists: {qname}, {rname}",
                 lambda n, match=match, condition=condition: f"match {match} {{\n  allow list: if {condition(cost(n))};\n}}",
                 [request])
for qname, request, condition in [
        ("a query of n == 1, get()", RUN(QUERY(FIELD("n", "EQUAL", I(1)))), lambda c: f"{c} && get({D}/o/x).data.n == 1"),
        ("a query with a limit, request.query", RUN(QUERY(FIELD("n", "EQUAL", I(1)), limit=1)),
         lambda c: f"{c} && request.query.limit <= 10")]:
    boundary(f"lists: {qname}", lambda n, condition=condition: f"match /c/{{id}} {{\n  allow list: if {condition(cost(n))};\n}}",
             [request])


def transaction(*make):
    """Begins a transaction, makes each of `make` (a function of its ID returning a request),
    then rolls it back; whether an answer reached the limit."""
    def run(record):
        status, text = call("POST", f"/v1/{BASE}:beginTransaction", {}, record=record)
        assert status == 200, text
        tid = json.loads(text)["transaction"]
        if record:
            cases[-1]["steps"][-1]["response"] = {"transaction": tid}
        reached = False
        for request in make:
            method, path, payload, auth = request(tid)
            _, text = call(method, path, payload, auth=auth, record=record)
            reached |= LIMIT in text
        call("POST", f"/v1/{BASE}:rollback", {"transaction": tid}, record=record)
        return reached
    return run


def new_transaction(method, path, payload):
    """A read that begins a transaction, which is then rolled back; whether it reached the limit."""
    def run(record):
        status, text = call(method, path, payload, record=record)
        if status == 200:
            tid = json.loads(text)[0]["transaction"]
            if record:
                cases[-1]["steps"][-1]["response"] = [{"transaction": tid}]
            call("POST", f"/v1/{BASE}:rollback", {"transaction": tid}, record=record)
        return LIMIT in text
    return run


# Requests: the documents of a batchGet count from zero, and so do reads and commits in
# transactions (a batchWrite is refused to anyone but the owner, before any rule). A batchGet that begins a transaction and is denied
# leaves nothing locked: the owner then writes its document.
GET_RULES = lambda c: (f"match /c/{{id}} {{\n  allow get: if {c};\n  allow list: if {c};\n"
                       f"  allow write: if {c};\n}}")
REQUESTS = [
    ("a batchGet of two stored documents", lambda c: f"{c} && resource.data.n == 1",
     [("POST", f"/v1/{BASE}:batchGet", {"documents": [f"{BASE}/c/e", f"{BASE}/c/f"]}, None)]),
    ("a batchGet of a stored and a missing document", lambda c: f"{c} && (resource == null || resource.data.n == 1)",
     [("POST", f"/v1/{BASE}:batchGet", {"documents": [f"{BASE}/c/e", f"{BASE}/c/zz"]}, None)]),
    ("a batchGet that begins a transaction, then a write", lambda c: f"{c} && resource.data.n == 1",
     [new_transaction("POST", f"/v1/{BASE}:batchGet", {"documents": [f"{BASE}/c/e", f"{BASE}/c/f"], "newTransaction": {}}),
      ("PATCH", f"/v1/{BASE}/c/e", {"fields": {"n": I(1)}}, OWNER)]),
    ("a query that begins a transaction, then a write", lambda c: f"{c} && resource.data.n == 1",
     [new_transaction("POST", f"/v1/{BASE}:runQuery", {**QUERY(FIELD("n", "EQUAL", I(1))), "newTransaction": {}}),
      ("PATCH", f"/v1/{BASE}/c/e", {"fields": {"n": I(1)}}, OWNER)]),
    ("a batchGet of two in a transaction", lambda c: f"{c} && resource.data.n == 1",
     [transaction(lambda t: ("POST", f"/v1/{BASE}:batchGet", {"documents": [f"{BASE}/c/e", f"{BASE}/c/f"], "transaction": t}, None))]),
    ("a query in a transaction", lambda c: f"{c} && resource.data.n == 1",
     [transaction(lambda t: ("POST", f"/v1/{BASE}:runQuery", {**QUERY(FIELD("n", "EQUAL", I(1))), "transaction": t}, None))]),
    ("a commit of a write and a verify in a transaction", lambda c: f"{c} && resource.data.n == 1",
     [transaction(lambda t: ("POST", f"/v1/{BASE}:commit", {"writes": [write("c/e", {"n": I(1)}), {
         "verify": f"{BASE}/c/f", "currentDocument": {"exists": True}}], "transaction": t}, None))]),
]
for name, condition, requests in REQUESTS:
    boundary(f"requests: {name}", lambda n, condition=condition: GET_RULES(condition(cost(n))), requests,
             setup=WRITTEN + document("c/f", {"n": I(1)}))

dump()
