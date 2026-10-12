#!/usr/bin/env python3
"""What do `GET :securityRules` and `GET :ruleCoverage` answer, and what does `debug()` print? (#67, #155)

Usage (from the repository root), with a freshly started jar under an English locale whose
standard output goes to a file, as firebase-tools sends it to `firestore-debug.log`:
    (cd /tmp && java -Duser.language=en -Duser.country=US -jar …/cloud-firestore-emulator-v1.22.0.jar \\
        --host 127.0.0.1 --port 8086 > emulator.out) &
    python3 -I tools/oracle/rules_coverage.py 127.0.0.1:8086 /tmp/emulator.out \\
        > crates/hidane/tests/fixtures/rules_coverage.json
and with `--rules`, from a jar started in a directory holding `tools/oracle/rules_coverage.rules`
with `--rules rules_coverage.rules`:
    python3 -I tools/oracle/rules_coverage.py 127.0.0.1:8086 /tmp/emulator.out --rules \\
        > crates/hidane/tests/fixtures/rules_coverage_flag.json

Each step keeps what the emulator printed while it ran (`log`), and the steps that read the
rules or the coverage report keep their body as sent (`raw`). The cases:

- the rules of a database without any, after a `PUT` that compiled, after one that did not,
  of several files, with fingerprints and names of other types, of another database, and after
  `POST /reset`; the methods, paths, content types and query strings each endpoint answers;
- coverage reports: which expressions they hold and where (functions, `let`s, nested matches,
  parentheses, literals), what each evaluated to (every type of value, as `debug()` prints it
  too), in which order and how many times, expressions not evaluated (`&&`, `||`, `?:`), errors
  and where they are placed, statements counted once more for each one evaluated after them,
  upserts (whose `create` and `update` statements are both evaluated), lists (constraints, and
  the path segments a list does not know) and offsets in code points. The conditions end in a
  value, not an error, and read no document: the official emulator evaluates a condition again
  after an error, or when it reads one, and its report counts both passes;
- cases marked `differs`, where hidane does not do what the official emulator does
  (`docs/parity-exceptions.md`): the second pass, and a branch of `?:` not taken.

With `--rules`, the cases show the file's rules in each database, a coverage report of its own
for each, and `PUT` and `POST /reset` over it. Documents are written as the owner. Stdlib only.
"""

import base64
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import steps  # noqa: E402
from steps import OWNER, call, case, dump, rules  # noqa: E402

steps.LOG = sys.argv[2]
FLAG = "--rules" in sys.argv[3:]
I = lambda n: {"integerValue": str(n)}
E = "/emulator/v1/projects"


def b64(obj):
    return base64.urlsafe_b64encode(json.dumps(obj, separators=(",", ":")).encode()).decode().rstrip("=")


# An unsigned token whose claims have every type the errors need.
TOKEN = "Bearer " + b64({"alg": "none"}) + "." + b64(
    {"sub": "u", "n": 1, "s": "a", "t": True, "f": False, "nul": None, "l": [1], "m": {"a": 1}, "pat": "["}) + "."
# The token, through a function: its claims are the values, and `request` shows once per call.
T = "t()"


def ruleset(body):
    return ("rules_version = '2';\nservice cloud.firestore {\n  match /databases/{database}/documents {\n"
            + "\n".join("    " + line for line in body.split("\n")) + "\n  }\n}\n")


def put(path, files):
    return call("PUT", path, {"rules": {"files": files}})


def read(path, **kw):
    return call("GET", path, raw=True, **kw)


def docs(project, database="(default)"):
    return f"/v1/projects/{project}/databases/{database}/documents"


def coverage_case(name, project, body, calls, **extra):
    """Loads `body` (a ruleset, or the inside of `match /databases/…/documents`) into `project`,
    makes `calls` (each `(method, path below documents, body, auth)`) and reads the report."""
    case(name, **extra)
    rules(project, body if body.startswith("rules_version") else ruleset(body))
    for method, path, payload, auth in calls:
        call(method, f"{docs(project)}{path}", payload, auth=auth)
    read(f"{E}/{project}:ruleCoverage")


def get(path, auth=None):
    return ("GET", path, None, auth)


def flag_cases():
    case("--rules")
    for path in ("cov-a:securityRules", "cov-a/databases/other:securityRules", "cov-a:ruleCoverage"):
        read(f"{E}/{path}")
    call("GET", f"{docs('cov-a')}/c/x")
    call("GET", f"{docs('cov-a')}/c/y")
    call("GET", f"{docs('cov-b')}/c/x")
    call("GET", f"{docs('cov-a', 'other')}/c/x")
    for path in ("cov-a:ruleCoverage", "cov-b:ruleCoverage", "cov-a/databases/other:ruleCoverage"):
        read(f"{E}/{path}")
    rules("cov-a", ruleset("match /c/{d} {\n  allow read: if d == 'y';\n}"))
    read(f"{E}/cov-a:securityRules")
    call("GET", f"{docs('cov-a')}/c/x")
    call("POST", "/reset")
    read(f"{E}/cov-a:securityRules")
    call("GET", f"{docs('cov-a')}/c/x")
    read(f"{E}/cov-a:ruleCoverage")


def main_cases():
    open_rules = ruleset("match /c/{d} {\n  allow read: if d == 'x';\n}")

    case("no rules")
    for path in ("rules-a:securityRules", "rules-a:ruleCoverage", "rules-a/databases/other:securityRules"):
        read(f"{E}/{path}")
    read(f"{E}/rules-a:ruleCoverage.html", content_type=None)

    case("loaded rules")
    put(f"{E}/rules-b:securityRules", [{"name": "a.rules", "content": open_rules, "fingerprint": "AA"}])
    read(f"{E}/rules-b:securityRules")
    read(f"{E}/rules-b:ruleCoverage")
    put(f"{E}/rules-b:securityRules", [{"name": "b.rules", "content": "service cloud.firestore { oops }"}])
    read(f"{E}/rules-b:securityRules")
    put(f"{E}/rules-b:securityRules", [{"name": "one.rules", "content": open_rules},
                                       {"content": ruleset("match /c/{d} {\n  allow read: if d == 'y';\n}"),
                                        "fingerprint": "-_8"}])
    read(f"{E}/rules-b:securityRules")
    read(f"{E}/rules-b:ruleCoverage")
    put(f"{E}/rules-b:securityRules", [{"name": 12, "content": open_rules, "fingerprint": True}])
    read(f"{E}/rules-b:securityRules")
    put(f"{E}/rules-b:securityRules", [{"name": "", "content": open_rules, "fingerprint": ""}])
    read(f"{E}/rules-b:securityRules")
    for fingerprint in ("!!!", 5):
        put(f"{E}/rules-b:securityRules", [{"name": "x", "content": open_rules, "fingerprint": fingerprint}])
    put(f"{E}/rules-b:securityRules", [{"name": "n<&>=' é", "content": open_rules}])
    read(f"{E}/rules-b:securityRules")
    read(f"{E}/rules-b:ruleCoverage.html", content_type=None)

    case("databases")
    put(f"{E}/rules-c/databases/(default):securityRules", [{"name": "default", "content": open_rules}])
    read(f"{E}/rules-c:securityRules")
    put(f"{E}/rules-c/databases/other:securityRules",
        [{"name": "other", "content": ruleset("match /c/{d} {\n  allow read: if false;\n}")}])
    read(f"{E}/rules-c/databases/other:securityRules")
    read(f"{E}/rules-c/databases/(default):securityRules")
    read(f"{E}/rules-c/databases/third:securityRules")
    call("GET", f"{docs('rules-c', 'other')}/c/x")
    call("GET", f"{docs('rules-c')}/c/x")
    call("GET", f"{docs('rules-c', 'third')}/c/y")
    read(f"{E}/rules-c/databases/other:ruleCoverage")
    read(f"{E}/rules-c:ruleCoverage")

    case("methods and paths")
    for method in ("POST", "DELETE", "PATCH", "HEAD"):
        call(method, f"{E}/rules-c:securityRules")
    for method in ("POST", "PUT", "DELETE", "PATCH", "HEAD"):
        call(method, f"{E}/rules-c:ruleCoverage")
        call(method, f"{E}/rules-c:ruleCoverage.html", content_type=None)
    for path in ("rules-c:ruleCoverage/html", "rules-c:ruleCoverage/", "rules-c:securityRules/",
                 "rules-c:ruleCoverage.json", "rules-c:rulecoverage", "rules-c/x/y:ruleCoverage",
                 "rules-c/databases/(default)/documents:ruleCoverage"):
        call("GET", f"{E}/{path}")
    read(f"{E}/rules-c:ruleCoverage?x=1")
    read(f"{E}/rules-c:securityRules?x=1")
    read(f"{E}/rules-c:securityRules", content_type="application/x-www-form-urlencoded")
    read(f"{E}/rules-c:ruleCoverage", content_type=None)
    for content_type in ("application/json", "text/plain"):
        call("GET", f"{E}/rules-c:ruleCoverage.html", content_type=content_type)
    call("GET", f"{E}/rules-c:ruleCoverage.html?x=1", content_type=None)
    read(f"{E}/rules-c/databases/(default):ruleCoverage.html", content_type=None)
    for path in (":securityRules", ":ruleCoverage", "/databases/x:ruleCoverage", "rules-c/databases/:ruleCoverage",
                 "rules-c/databases/:securityRules", "rules-c/databases/UPPER:ruleCoverage"):
        call("GET", f"{E}/{path}")
    call("GET", f"{E}/:ruleCoverage.html", content_type=None)
    call("DELETE", f"{E}/:securityRules")
    put(f"{E}/:securityRules", [{"name": "x", "content": open_rules}])
    put(f"{E}/rules-c/databases/:securityRules", [{"name": "x", "content": open_rules}])

    coverage_case("structure", "cov-structure", """rules_version = '2';
// A comment: 😀 é
function top(a) {
  return a == 1;
}
function literal() {
  return true;
}
function uncalled(a) {
  return a > 1;
}
service cloud.firestore {
  function lets(x) {
    let unused = x + 1;
    let used = x * 2;
    return used == 2 && top(x);
  }
  match /databases/{database}/documents {
    match /c/{d} {
      function inner(y) {
        return y is string;
      }
      allow read: if lets(1) && literal() && inner(d) && (d == 'x') && !(d == 'y') && [1, d][1] == d
        && 'abc'[1:2] == 'b' && (true ? d : 'z') == d && d in [d] && -(1) < 0 && math.abs(-1) == 1
        && /databases/$(database)/documents/c/$(d) == /databases/(default)/documents/c/x
        && d.size() > 0 && {'a': d}.a == d && {}.size() == 0 && [].size() == 0 && later();
      allow write: if true;
      allow create: if (1) == 1;
      allow update;
      match /n/{e} {
        allow read: if e == d;
      }
    }
  }
  function later() {
    let a = 1;
    return a == 1;
  }
}
""", [get("/c/x"), get("/c/x/n/x")])

    values = [
        "null", "true", "2", "-9223372036854775807 - 1", "1.5", "100000000.0", "-0.0", "0.001", "0.0001",
        "float('NaN')", "float('Infinity')", "-float('Infinity')", "'a\"b\\'c\\nd\\te'", "'é😀\\u0001'", "''",
        "b'\\x00\\xffa'", "'é'.toUtf8()", "[1, 'a', [2]]", "[]", "{'b': 1, 'a': {'c': true}, 'é': 2, 'B': 3}", "{}",
        "[1, 2, 1.0].toSet()", "[].toSet()", "{'c': 1, 'a': 2, 'b': 3}.diff({'b': 1, 'a': 2, 'd': 4})",
        "timestamp.value(0)", "timestamp.value(1500)", "timestamp.date(2020, 1, 2)", "duration.value(90, 's')",
        "duration.value(-1500, 'ms')", "duration.time(1, 2, 3, 4)", "duration.value(0, 's')",
        "latlng.value(1.5, -2.25)", "latlng.value(0, 0)", "latlng.value(-0.0, 0.00001)", "path('a/b%20c')",
        "/databases/$(database)/documents/v/$(d)", "d", "int(d + '1')", "float('1.0E10')",
    ]
    body = "\n".join(f"match /v{i}/{{d}} {{\n  allow read: if debug({v}) != 0 || true;\n}}" for i, v in enumerate(values))
    coverage_case("values", "cov-values", body, [get(f"/v{i}/x") for i in range(len(values))])

    coverage_case("order and counts", "cov-order", "match /c/{d} {\n  allow read: if d.size() > 0;\n}",
                  [get(f"/c/{d}") for d in ("m", "b", "b", "zz", "a", "a", "a")])

    coverage_case("statements", "cov-statements", "\n".join([
        "function f(x) {",
        "  return x == 'y';",
        "}",
        "match /c/{d} {",
        "  allow get: if d == 'x';",
        "  allow read: if f(d);",
        "  allow get: if d.size() == 1;",
        "}",
        "match /{path=**} {",
        "  allow get: if path[1] == 'z';",
        "}",
    ]), [get("/c/a"), get("/c/b"), get("/c/zz")])

    coverage_case("not evaluated", "cov-skipped", "\n".join([
        "match /c/{d} {",
        "  allow read: if d == 'x' && debug(d) == 'y' || debug(d.size()) == 1;",
        "  allow read: if d == 'never';",
        "}",
        "match /t/{d} {",
        "  allow read: if (d == 'x' ? debug(d + '1') : debug(d + '2')) != '' && (d == 'x' || d.size() == 9);",
        "}",
    ]), [get("/c/a"), get("/c/x"), get("/t/x")])

    errors = [
        f"{T}.n + {T}.s == 2", f"{T}.l[5] == 1", "request.x == 1", f"{T}.n.size() == 1", f"-{T}.s == 1",
        f"!{T}.n", f"{T}.m['b'] == 1", f"{T}.m.b == 1", f"{T}.s[1:10] == 'b'", "f2()",
        "(true ? int('x') : 1) == 1", f"1 in {T}.n", f"{T}.missing.x == 1", f"({T}.m).b == 1",
        f"((({T}.m))).b == 1", f"{T}['zz'] == 1", f"({T}.n + {T}.s) == 2", f"{T}.n / 0 == 1",
        f"math.abs({T}.s) == 1", f"f3({T}.n)", f"int({T}.m) == 1", f"[1, 2].hasAll({T}.n)", f"{T}.n < {T}.s",
        f"{T}.s.matches({T}.pat)", f"{T}.n.x == 1", f"{T}.s in {T}.n", f"{T}.l[{T}.s] == 1",
        f"{{'a': {T}.n + {T}.s}} == {{}}", f"{{{T}.n: 1}} == {{}}", f"{T}.n && true", f"true && {T}.n",
        f"{T}.n || false", f"{T}.f || {T}.n", f"({T}.n ? 1 : 2) == 1", f"/databases/$({T}.m)/documents == /a/b",
        f"{T}.nul.size() == 1", f"{T}.s.size(1) == 1", f"{T}.m.get('x') == 1", f"{T}.s.split(1) == 1",
        "int('a') == 1", "debug(int('a')) == 1",
    ]
    fns = ("function t() {\n  return request.auth.token;\n}\nfunction f2() {\n  let z = int('q');\n  return z == 1;\n}\n"
           "function f3(x) {\n  return x.size() == 1;\n}\n")
    body = fns + "\n".join(f"match /e{i}/{{d}} {{\n  allow read: if ({e}) || true;\n}}" for i, e in enumerate(errors))
    coverage_case("errors", "cov-errors", body, [get(f"/e{i}/x", TOKEN) for i in range(len(errors))])

    coverage_case("functions and lets", "cov-lets", "\n".join([
        "function f(x) {",
        "  let unused = debug('unused ' + x);",
        "  let used = debug(x + '!');",
        "  return used == 'a!';",
        "}",
        "match /c/{d} {",
        "  allow read: if f(d) || f(d + d);",
        "}",
    ]), [get("/c/a"), get("/c/b")])

    upserts = "\n".join([
        "match /c/{d} {",
        "  allow create: if debug('c1') == 'x';",
        "  allow update: if debug('u1') == 'u1';",
        "  allow update: if debug('u2') == 'x';",
        "  allow create: if debug('c2') == 'c2';",
        "  allow delete: if debug('d1') == 'd1';",
        "  allow read: if debug('r1') == 'r1';",
        "}",
    ])
    p = "cov-upserts"
    batch = {"writes": [{"update": {"name": f"projects/{p}/databases/(default)/documents/c/b", "fields": {"n": I(5)}}},
                        {"update": {"name": f"projects/{p}/databases/(default)/documents/c/y", "fields": {"n": I(5)}}}]}
    coverage_case("upserts", p, upserts, [
        ("POST", "/c?documentId=a", {"fields": {"n": I(1)}}, OWNER),
        ("PATCH", "/c/a", {"fields": {"n": I(2)}}, None),
        ("PATCH", "/c/b", {"fields": {"n": I(2)}}, None),
        ("PATCH", "/c/a?currentDocument.exists=true", {"fields": {"n": I(3)}}, None),
        ("POST", "/c?documentId=z", {"fields": {"n": I(1)}}, None),
        ("DELETE", "/c/a", None, None),
        ("POST", ":commit", batch, None),
        ("GET", "/c/z", None, None),
    ])

    def query(where=None):
        q = {"from": [{"collectionId": "q"}]}
        if where:
            q["where"] = where
        return ("POST", ":runQuery", {"structuredQuery": q}, None)

    field = lambda op, value: {"fieldFilter": {"field": {"fieldPath": "n"}, "op": op, "value": value}}
    coverage_case("lists", "cov-lists", "match /q/{d} {\n  allow list: if debug(resource.data.n) > 0 || true;\n}", [
        query(field("GREATER_THAN", I(1))), query(field("EQUAL", I(2))),
        query({"compositeFilter": {"op": "AND", "filters": [field("GREATER_THAN", I(1)), field("NOT_EQUAL", I(3))]}}),
        query(field("IN", {"arrayValue": {"values": [I(1), I(2)]}})), query(field("ARRAY_CONTAINS", I(1))),
        query(field("LESS_THAN_OR_EQUAL", I(1))), query(field("NOT_IN", {"arrayValue": {"values": [I(1), I(2)]}})),
        query(),
    ])

    def group(parent=""):
        q = {"from": [{"collectionId": "q", "allDescendants": True}]}
        return ("POST", f"{parent}:runQuery", {"structuredQuery": q}, None)

    coverage_case("unknown segments", "cov-unknown", "match /{rest=**} {\n  allow list: if debug(rest) != null || true;\n}", [
        query(), ("POST", "/x/y:runQuery", {"structuredQuery": {"from": [{"collectionId": "q"}]}}, None),
        group(), group("/x/y"),
    ])

    case("two passes", differs="the official emulator evaluates a condition that reads `resource` twice")
    rules("cov-twice", ruleset("match /c/{d} {\n  allow read: if resource.data.n == 1 && debug(d) == d;\n}"))
    call("POST", f"{docs('cov-twice')}/c?documentId=x", {"fields": {"n": I(1)}}, auth=OWNER)
    call("GET", f"{docs('cov-twice')}/c/x")
    read(f"{E}/cov-twice:ruleCoverage")

    case("a branch not taken", differs="the official emulator gives a `?:`'s value to its first branch when not taken")
    rules("cov-branch", ruleset("match /c/{d} {\n  allow read: if (d == 'y' ? debug(d + '1') : d + '2') != '';\n}"))
    call("GET", f"{docs('cov-branch')}/c/x")
    read(f"{E}/cov-branch:ruleCoverage")

    case("reset")
    rules("rules-d", open_rules)
    call("GET", f"{docs('rules-d')}/c/y")
    read(f"{E}/rules-d:securityRules")
    call("POST", "/reset")
    read(f"{E}/rules-d:securityRules")
    read(f"{E}/rules-d:ruleCoverage")
    call("GET", f"{docs('rules-d')}/c/y")


if FLAG:
    flag_cases()
else:
    main_cases()
dump()
