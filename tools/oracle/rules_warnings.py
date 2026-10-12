#!/usr/bin/env python3
"""Which warnings does the official compiler give a Security Rules condition, and where? (#60)

Usage (from the repository root), with the jar started under an English locale
(`java -Duser.language=en -Duser.country=US -jar …`; under another locale every warning comes
back as a 500):
    python3 -I tools/oracle/rules_warnings.py 127.0.0.1:8086 > crates/hidane-rules/tests/fixtures/rules_warnings.json

Each ruleset is loaded with `PUT /emulator/v1/projects/{p}:securityRules` and the answer is
recorded: the `issues[]` of a `200`, each as `[line, column, currentOffset, endOffset, severity,
description]` (a ruleset-wide issue has no position), or the message of a `400`. Nothing is read.

The rulesets are every condition and ruleset already recorded by `rules_eval.py`
(`crates/hidane-rules/tests/fixtures/rules_eval.json`), `rules_conditions.py`
(`results/rules-conditions-official-v1.22.0.json`) and `rules_paths.py`
(`crates/hidane-rules/tests/fixtures/rules_paths.json`), whose fixtures keep only the
descriptions, then the cases below: the type checker's positions, the types of operators,
functions, methods and variables, comparisons, conditional expressions, and how the types of
function parameters and `let`s are inferred from their uses and checked at each call.
A condition goes in `RULES` (line 5, column 22); `decls :: condition` puts `decls` on line 5 of
`RULES_F` and the condition on line 6. Stdlib only.
"""

import http.client
import json
import sys

HOST = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1:8080"

RULES = """rules_version = '2';
service cloud.firestore {
  match /databases/{database}/documents {
    match /rooms/{room} {
      allow read: if %s;
    }
  }
}
"""

RULES_F = """rules_version = '2';
service cloud.firestore {
  match /databases/{database}/documents {
    match /rooms/{room} {
%s
      allow read: if %s;
    }
  }
}
"""


def paths_rules(version, nested, condition):
    """The ruleset `rules_paths.py` loads."""
    lines = [] if version == 1 else ["rules_version = '2';"]
    lines += ["service cloud.firestore {", "  match /databases/{database}/documents {"]
    indent = "    "
    for path in nested:
        lines.append(f"{indent}match {path} {{")
        indent += "  "
    lines.append(f"{indent}allow read: if {condition};")
    for _ in nested:
        indent = indent[:-2]
        lines.append(f"{indent}}}")
    lines += ["  }", "}"]
    return "\n".join(lines) + "\n"


def batch_rules(condition):
    """The ruleset of a batch in `rules_eval.py`."""
    return ("rules_version = '2';\nservice cloud.firestore {\n  match /databases/{database}/documents {\n"
            f"    match /c/{{d}} {{\n      allow read: if true;\n      allow write: if {condition};\n"
            "    }\n  }\n}\n")


def ruleset(body, service=""):
    return ("rules_version = '2';\nservice cloud.firestore {\n" + service
            + "  match /databases/{database}/documents {\n"
            + "\n".join("    " + line for line in body.split("\n")) + "\n  }\n}\n")


# Conditions: positions (each kind of node, in parentheses, as a receiver), result types.
POSITIONS = [
    "1", "(1)", "((1))", "-1", "-(1)", "!1", "!(1)", "!((1))", "1.5", "'abc'", "b'abc'", "null", "[1, 2]",
    "{'a': 1}", "/a/b", "/a/$(1)", "!/$(1)/a", "room", "!(room)", "(room).keys() == []", "!('a'.size())",
    "!(-1)", "!([1])", "!({})", "!(/a/b)", "!(null)", "!('a'[0:1])", "!(('a')[0:1])", "!request.path[1:3]",
    "!(request.path)[1:3]", "!resource.data.s[0:1]", "!(1 + 1)", "!((1 + 1))", "!(math.abs(1))", "!(int(1))",
    "!(true ? 1 : 2)", "!((true ? 1 : 2))", "!([1][0:1])", "(1).lower() == 'a'", "((1)).lower() == 'a'",
    "[1].lower() == 'a'", "'a'.lower().keys() == []", "(1).x == 1", "((1)).x == 1", "1.5.x == 1",
    "(1)[1:2] == 1", "((1))[1:2] == 1", "(null)[0] == 1", "((null))[0] == 1", "(1)[0] == 1",
    "int((true)) == 1", "'a'.matches((1))", "[1][(1.5)] == 1", "(true) + 1 == 1", "1 + (true) == 1",
    "true ? (1) : 2", "true && (1)", "-(true) == 1", "-((true)) == 1", "{(1): 1} == {}",
    "'a'[(true):2] == 'a'", "(1)[0].x == 1", "(1)[0:1].x == 1", "((1)[0:1]).x == 1", "1 in ((1))",
    "(-1).x == 1", "((true ? 1 : 2)).x == 1", "!'abc'.lower()[0:1]", "!(1 + 1)[0:1]", "!math.abs(1)[0:1]",
    "!request.path.x[0:1]", "!request.path[0][0:1]", "!f(1)[0:1]", "(1 is int) + 1 == 1",
    "(1 in [1]) + 1 == 1", "(1 < 2) + 1 == 1", "(!true) + 1 == 1", "(1 && 2) + 1 == 1", "(1 ? 2 : 3) + 1 == 1",
    "request.time.year()", "'abc'.lower()", "math.abs(1)", "int('1')", "1 + 2", "1 - 2", "1 * 2", "1 / 2",
    "1 % 2", "1 < 2", "1 in [1]", "1 is int", "[1][0]", "{'a': 1}.a", "get(/a/b)", "exists(/a/b)", "debug(1)",
    "string(1)", "float(1)", "path('a')", "duration.value(1, 's')", "timestamp.value(1)", "latlng.value(1, 1)",
    "hashing.md5('a')", "math.isNaN(1.0)", "math.pow(1, 2)", "math.round(1.5)", "math.ceil(1.5)",
    "duration.time(1, 2, 3, 4)", "timestamp.date(1, 2, 3)", "vector([1])", "request.auth.uid",
]

# Operators: the operand types each takes for each expected result.
OPERATORS = [
    "true + true", "true + true == 2", "true * 2 == 2", "-true == 1", "!'a'", "!(1 + 1)", "!!1", "-'a'",
    "('a' * 2) + 1", "(true + true).toMillis() == 1", "(true - true).toMillis() == 1",
    "duration.abs(true - true) == null", "duration.abs(true + true) == null", "math.abs(true + true) == 1",
    "math.sqrt(true + true) == 1", "math.sqrt(true * true) == 1", "(true + true).lower() == 'a'",
    "(true % true).lower() == 'a'", "-(true + true) == 1", "duration.abs(-true) == null",
    "math.sqrt(-true) == 1", "(-true).toMillis() == 1", "duration.value(-true, 's') == null",
    "duration.value(true + true, 's') == null", "duration.value(math.abs(true), 's') == null",
    "(math.abs(true)).lower() == ''", "math.sqrt('a' + 1) == 1", "(1 + 'a') * 2 == 1", "('a' + 1).size() == 1",
    "(true + 1).lower() == 'a'", "(1 - 1).lower() == 'a'", "(1 + 1).size() == 1", "(1 * 1).lower() == 'a'",
    "-(1).lower() == 'a'", "true && 1 + 1", "1 + 1 && true", "true < 1", "'a' < 1", "null < 1",
    "1 in (1)", "!(1 in 1) && true", "(1 in 1) + 1 == 1", "(1 < 'a') + 1 == 1", "(1 == 'a') + 1 == 1",
    "duration.abs(timestamp.value(1) - timestamp.value(2)) == duration.value(1, 's')",
    "(timestamp.value(1) - timestamp.value(2)).seconds() == 1", "math.abs(1 % 'a') == 1",
]

# What a failing node checks below it.
FAILING = [
    "!{1: 1}.size()", "![{1: 1}]", "!((1)[0:1])", "-{1: 1}.lower() == 1", "!(true ? {1: 1} : 2)",
    "!{'a': 'a' * 2}", "![('a' * 2)]", "!/a/$({1: 1})", "!/a/$('a' * 2)", "-('a' * 2).lower() == 1",
    "!int('a' * 2)", "!math.abs('a' * 2)", "!(1 + ('a' * 2))", "!('a' * 2)[0:1]", "!(('a' * 2)[0:1])",
    "!'a'[true:1]", "!'a'.split(1)", "!'a'.replace(1, 2)", "!{'a': 1}.get(1, 2)", "![1][true]",
    "![1].hasAll(1)", "!int(true)", "!math.abs(true)", "!duration.value(true, 1)", "!exists(1)",
    "!true.size()", "!true.split(true)", "!'a'.size(1)", "!'a'.foo(1)", "!int(1, 2)", "!math.foo(1)",
    "'abc'.size(1) == 'a'", "int(1, 2) == 'a'", "'abc'.foo() == 'a'",
]

# Indexes, slices and members.
ACCESS = [
    "'abc'[0] == 1", "!'abc'[0]", "![1][0]", "!/a/b[0]", "!{'a': 1}['a']", "!request.path[0]",
    "(1)[0:1].lower() == 'a'", "'a'[0:1].keys() == []", "'abc'[0].lower() == 'a'", "(1)[0].lower() == 'a'",
    "(1)[0] + 1 == 1", "resource.data.x[0:1] == 1", "'abc'.x.y == 1", "(1).x.y == 1",
]

# Every pair of types compared with `==`.
VALUES = {
    "int": "1", "float": "1.5", "string": "'a'", "bytes": "b'a'", "bool": "true", "null": "null",
    "list": "[1]", "map": "{'a': 1}", "path": "/a/b", "set": "[1].toSet()", "map_diff": "{}.diff({})",
    "duration": "duration.value(1, 's')", "timestamp": "timestamp.value(1)", "latlng": "latlng.value(1, 1)",
    "vector": "vector([1])", "number": "math.abs(1)", "slice": "'a'[0:1]", "sum": "(1 + 1)",
    "difference": "(1 - 1)", "negation": "-1",
}
NAMES = list(VALUES)
COMPARISONS = [f"{VALUES[a]} == {VALUES[b]}" for i, a in enumerate(NAMES) for b in NAMES[i:]] + [
    "1 != 'a'", "[1, 'a'] == [1]", "request.time == null",
]

# Conditional expressions: when do the two results have "a different type"?
TERNARIES = [
    "true ? 1 : 'a'", "(true ? 1 : 'a') == null", "(true ? 1 : 2) == 'a'", "(true ? null : 1) == null",
    "(true ? 'a' : 1) == 1", "true ? 1 : 'a' == 1", "(true ? 1 : 'a').lower() == 'a'",
    "(true ? 1 : 2).lower() == 'a'", "(true ? [1] : 'a') + 1 == 1", "(true ? 1 : 'a') + 1 == 1",
    "(true ? 1 : 1.5) * 2 == 1", "(true ? 1 : 'a').size() == 1", "(true ? [1] : 'a').size() == 1",
    "(true ? [1] : {}).size() == 1", "(true ? 1 : 'a') in [1]", "1 in (true ? [1] : {})",
    "1 in (true ? [1] : 'a')", "(true ? 1 : 'a') is int", "!(true ? true : request.x)", "(true ? 1 : 'a') < 2",
    "{'a': true ? 1 : 'a'} == {}", "{(true ? 'a' : 1): 1} == {}", "[true ? 1 : 1.5] == []",
    "[true ? 1 : request.x] == []", "[true ? 1 + 1 : 'a'] == []", "[true ? [1] : [2]] == []",
    "[true ? math.abs(1) : 1] == []", "[true ? (true ? 1 : 2) : 3] == []", "[true ? (true ? 1 : 'a') : 3] == []",
    "[true ? (true ? 1 : 'a') : (true ? 'b' : 2)] == []", "[true ? (true ? 1 : 2) : 'a'] == []",
    "[true ? null : null] == []", "[true ? room : 'a'] == []", "[true ? room : 1] == []",
    "debug(true ? 1 : 'a')", "{'a': 1}.get('a', true ? 1 : 'a') == 1", "math.sqrt(true ? 'a' : 1) == 1",
    "(true ? 1 : 'a').x == 1", "!(true ? 1 : 'a')[0:1]",
]

# Wildcards: a string, a path, or both in a path with both kinds.
WILDCARDS = [
    "room.size() == 1 && room + 1 == 1 && room * 2 == 1", "room == 1 && room == null && room.keys() == []",
    "room.keys() == [] && room.keys() == []", "room.keys() == [] && room", "!room.size()", "!room && !room",
    "database == 1", "-room == 1",
]
WILDCARD_RULES = [
    ruleset("match /{rest=**} {\n  allow read: if rest == 1 && rest.size() == 1 && rest[0] == 'a' && "
            "rest.bind({}) == rest && !rest;\n}"),
    ruleset("match /c/{d}/{rest=**} {\n  allow read: if !rest && !d && rest.lower() == 'a' && rest.size() == 1 "
            "&& rest.keys() == [] && rest + 1 == 1;\n}"),
    ruleset("match /{path=**}/e/{f} {\n  allow read: if !f && !path && path.keys() == [] && path == 1;\n}"),
    ruleset("match /{a}/{rest=**}/{b} {\n  allow read: if !a && !rest && !b;\n}"),
    ruleset("match /c/{d} {\n  match /{rest=**} {\n    allow read: if !rest && !d;\n  }\n}"),
    ruleset("match /c/{d=*}/{rest=**} {\n  allow read: if !rest && !d;\n}"),
    "rules_version = '2';\nservice cloud.firestore {\n  match /{database}/{rest=**} {\n"
    "    allow read: if !database && !rest;\n  }\n}\n",
    "service cloud.firestore {\n  match /databases/{database}/documents {\n    match /c/{d}/{rest=**} {\n"
    "      allow read: if !rest && !d;\n    }\n    match /{rest=**} {\n      allow read: if !rest;\n    }\n"
    "  }\n}\n",
    ruleset("match /rooms/{room} {\n  allow read: if true;\n  allow write: if 1;\n  allow get, list: if 'a';\n"
            "  allow delete: if room;\n}"),
]


def chain(n, cycle):
    """Functions that each call the next twice (the last one the first, if `cycle`), each called."""
    lines = []
    for i in range(n):
        call = f"f{(i + 1) % n}(x, y) && f{(i + 1) % n}(y, x) && " if cycle or i + 1 < n else ""
        lines.append(f"function f{i}(x, y) {{ let a = x; let b = a; return {call}x.size() > 0 ? x : y; }}")
    return ruleset("match /rooms/{room} {\n" + "\n".join("  " + line for line in lines)
                   + "\n  allow read: if " + " && ".join(f"f{i}('a', 'b')" for i in range(n)) + ";\n}")


# Functions, parameters and `let`s.
FUNCTIONS = [
    "function f() { return 1; } :: f()", "function f() { return 1; } :: f() && true",
    "function f() { return 1; } :: f() == 'a'", "function f() { return 1; } :: f().size() == 1",
    "function f() { return 'a'; } :: f() == 1", "function f() { return 1 + 1; } :: f() == 'a'",
    "function f() { return 1; } :: [f()] == []", "function f() { return true ? 1 : 2; } :: f() == 'a'",
    "function f() { let a = 1; return a; } :: f() == 'a'", "function f() { return true ? 1 : 'a'; } :: f()",
    "function f() { return true ? 1 : 'a'; } :: f() == 1 && f() == 2",
    "function f() { return 'a' * 2 == 1; } :: f() && f()",
    "function f(x) { return x; } :: f(1)", "function f(x) { return x; } :: f(1) && true",
    "function f(x) { return x; } :: f(1) == 'a'", "function f(x) { return x; } :: f(true) && f(1)",
    "function f(x) { return x; } :: f(1) || f('a')", "function f(x) { return x; } :: f(f(1))",
    "function f(x) { return x; } :: [f(1)] == []", "function f(x) { return x; } :: f(1) + 1 == 1",
    "function f(x) { return x; } :: f(true) + 1 == 1", "function f(x) { return x; } :: f(1).keys() == []",
    "function f(x) { return x; } :: f(1 + 1).keys() == []", "function f(x) { return x; } :: f(1 + 1).lower() == 'a'",
    "function f(x) { return x; } :: f(room)", "function f(x) { return x; } :: f(room) || f(database)",
    "function f(x) { return x; } :: f(1) == f('a')", "function f(x) { return (x); } :: f(1)",
    "function f(x) { return true ? x : x; } :: f(1)", "function f(x) { return true ? x : 1; } :: f('a') && true",
    "function f(x) { return true ? x : 1; } :: f('a') == 'a'", "function f(x) { return true ? x : 'b'; } :: f('a') == 1",
    "function f(x, y) { return y; } :: f(true, 1)", "function f(x, y) { return x + y == 1; } :: f(true, null)",
    "function f(x, y) { return x == y; } :: f(1, 'a')",
    "function f(x) { return x == 1; } :: f('a')", "function f(x) { return x == 1; } :: f(true ? 1 : 'a')",
    "function f(x) { return x.size() == 1; } :: f(1)", "function f(x) { return x.size() == 1; } :: f(1 + 1)",
    "function f(x) { return x.size() == 1; } :: f(1) && f(1)", "function f(x) { return x.size() == 1; } :: f(null)",
    "function f(x) { return x.size() == 1; } :: f(room)", "function f(x) { return x.size() == 1; } :: f({1: 1})",
    "function f(x) { return x.size() == 1; } :: f(true ? 1 : 'a')", "function f(x) { return x.size() == 1; } :: f(1, 2)",
    "function f(x) { return x.size() > 0; } :: f(request.time) && f(request.auth)",
    "function f(x) { return x * 2 == 1; } :: f(room)", "function f(x) { return x + 1 == 1; } :: f(room)",
    "function f(x) { return x.keys() == []; } :: f(room) && f(room)",
    "function f(x) { return x.lower(); } :: f(1) == 1", "function f(x) { return x.size(); } :: f('a') == 'a'",
    "function f(x) { return x.size(); } :: f(1)", "function f(x) { return x.size(); } :: f(1) == 1",
    "function f(x) { return x + 1; } :: f('a').size() == 1", "function f(x) { return x + 1; } :: f(1) && true",
    "function f(x) { return x + 1; } :: f(1) == null", "function f(x) { return -x; } :: f(true)",
    "function f(x) { return -x; } :: f(true) == 1", "function f(x) { return x.size() + 1; } :: f(1)",
    "function f(x) { return [x.size()]; } :: f(1)", "function f(x) { return true ? x.size() : 1; } :: f(1)",
    "function f(x) { return (x.size()); } :: f(1)",
    "function f(x) { return x.size() == 1 && x.lower() == 'a'; } :: f([1])",
    "function f(x) { return x.size() == 1 && x.lower() == 'a'; } :: f(true)",
    "function f(x) { return x.lower() == 'a' && x.size() == 1; } :: f(true)",
    "function f(x) { return x.hasAll([]) && x.size() == 1; } :: f('a')",
    "function f(x) { return x.size() == 1 && x.hasAll([]); } :: f('a')",
    "function f(x) { return math.sqrt(x) == 1 && math.abs(x) == 1; } :: f('a')",
    "function f(x) { return math.abs(x) == 1 && math.sqrt(x) == 1; } :: f('a')",
    "function f(x) { return x.seconds() == 1 && x.date() == x; } :: f(duration.value(1, 's'))",
    "function f(x) { return x.size() == 1 && x.size() == 2; } :: f(1)",
    "function f(x) { return x ? 1 : 2; } :: f(1) == 1", "function f(x) { return x.y; } :: f(1) == 1",
    "function f(x) { return x[0]; } :: f(true) == 1", "function f(x) { return x in [1]; } :: f(1)",
    "function f(x) { return 1 in x; } :: f(1)", "function f(x) { return x is int; } :: f(1)",
    "function f(x) { return x < 1; } :: f(true)", "function f(x) { return !x; } :: f(1)",
    "function f(x) { return -x == 1; } :: f(true)", "function f(x) { return int(x) == 1; } :: f(true)",
    "function f(x) { return math.abs(x) == 1; } :: f(true)", "function f(x) { return x.matches('a'); } :: f(1)",
    "function f(x) { return 'a'.matches(x); } :: f(1)", "function f(x) { return /a/$(x) == /a/b; } :: f([1])",
    "function f(x) { return {x: 1} == {}; } :: f(1)", "function f(x) { return [x] == []; } :: f(1)",
    "function f(x) { return x[1:2] == []; } :: f(1)", "function f(x) { return [1][x] == 1; } :: f(true)",
    "function f(x) { return 'abc'[x:2] == 'a'; } :: f(true)", "function f(x) { return get(x) == null; } :: f(1)",
    "function f(x) { return x.data == null; } :: f(1)",
    "function f(x) { return x.size() == 1 && x; } :: f(1)", "function f(x) { return x && x.size() == 1; } :: f(1)",
    "function f(x) { return x == 1 && x.size() == 1 && x; } :: f(1)",
    "function f(x) { return true && x.size() == 1 && x; } :: f(1)",
    "function f(x) { return x.size() == 1 && x.keys() == [] && x.lower() == 'a'; } :: true",
    "function f(x) { return x && x.size() == 1 && x.keys() == [] && x.lower() == ''; } :: true",
    "function f(x) { return x.size() == 1 && x.lower() == '' && x && x.keys() == []; } :: f(1)",
    "function f(x) { return x.size() == 1 && x.lower() == '' && x && x.keys() == []; } :: f([1])",
    "function g(y) { return y.size() == 1; } function f(x) { return g(x); } :: f(1)",
    "function f(x) { return g(x); } function g(y) { return y.size() == 1; } :: f(1)",
    "function f(x) { return g(x) && x.size() == 1; } function g(y) { return y.lower() == 'a'; } :: f(1)",
    "function f(x) { return g(x); } function g(y) { return y; } :: f(1)",
    "function f(x) { return x; } function g(y) { return f(y); } :: g(1)",
    "function f(x) { return g(x).size() == 1; } function g(y) { return y; } :: f(1)",
    "function f(x) { return g(x.size()); } function g(y) { return y; } :: f(1)",
    "function f(x) { return g(x.size()); } function g(y) { return y; } :: f(1) == 1",
    "function f(x) { return g(x.size()); } function g(y) { return true; } :: f(1)",
    "function f(x) { return g(x.size()) == 1; } function g(y) { return y; } :: f(1)",
    "function f(x) { return g(x.size()) && true; } function g(y) { return y; } :: f(1)",
    "function g(y) { return y; } function f(x) { return g(x.size()); } :: f(1)",
    "function f() { return g('a' * 2); } function g(y) { return y; } :: true",
    "function f(x) { return debug(x.size()); } :: f(1)", "function f(x) { return [x.size()] == []; } :: f(1)",
    "function f() { let a = 1; return a; } :: f()", "function f() { let a = 1; return a && true; } :: f()",
    "function f() { let a = 'x'; return a == 1; } :: f()",
    "function f() { let a = 1 + 1; return a == 'x' && a.size() == 1; } :: f()",
    "function f() { let a = 'x'; return a == 1 && a.size() == 1; } :: f()",
    "function f() { let a = 1; return a.size() == 1; } :: f()", "function f() { let a = 1; return a == 'x'; } :: f()",
    "function f() { let a = 1; let b = a; return b.size() == 1; } :: f()",
    "function f() { let a = [1]; return a[0] == 1; } :: f()",
    "function f() { let a = 1; return a.size() == 1 && a.keys() == []; } :: f()",
    "function f() { let a = 'a' * 2; return a == 1; } :: f()",
    "function f() { let a = 'a' * 2; return a.lower() == 'x'; } :: f()",
    "function f() { let a = 'a' * 2; return true; } :: f()", "function f() { let a = {1: 2}; return a.lower() == 'x'; } :: f()",
    "function f() { let a = true ? 1 : 'a'; return a == 1; } :: f()",
    "function f() { let a = true ? 1 : 'a'; return a.size() == 1; } :: f()",
    "function f() { let a = 1; return a + 1 == 1 && a.lower() == 'x'; } :: f()",
    "function f() { let a = 1; let b = a; return b.size() == 1 && a.keys() == []; } :: f()",
    "function f() { let a = 1; let b = a; return a.keys() == [] && b.size() == 1; } :: f()",
    "function f() { let a = room; return a.keys() == []; } :: f()",
    "function f() { let a = room; return a.keys() == [] && a.size() == 1; } :: f()",
    "function f() { let a = 1; return a.size() == 1 && a; } :: f()",
    "function f() { let a = 'x'; return a.size() == 1 && a; } :: f()",
    "function f() { let a = 'a' * 2; return a; } :: true", "function f() { let a = 'a' * 2; return a; } :: f() && f()",
    "function f(x) { let a = x; return a; } :: f(1)", "function f(x) { let a = x + 1; return a; } :: f(1)",
    "function f(x) { let a = x; return a; } :: f('a') == 1",
    "function f(x) { let a = x; return a.size() == 1 && a; } :: f(1)",
    "function f(x) { let a = x.size(); return a; } :: f(1)", "function f(x) { let a = x.size(); return a; } :: f(1) == 1",
    "function f(x) { let a = x.size(); return a; } :: f(1).size() == 1",
    "function f(x) { let a = x.size(); return a; } :: [f(1)] == []",
    "function f(x) { let a = -x; return a; } :: f(true) == 1", "function f(x) { let a = -x; return a; } :: f(true)",
    "function f(x) { let s = x.size(); return x; } :: f([1])", "function f(x) { let s = x.size(); return x; } :: f([1]) && true",
    "function f(x) { let s = x.size(); return x; } :: f('a').size() == 1",
    "function f(x) { let s = x.size(); return x; } :: f('a').keys() == []",
    "function f(x) { let s = x.size(); return x; } :: f(1).keys() == []",
    "function f(x) { let s = math.sqrt(x); return x; } :: math.abs(f('a')) == 1",
    "function f(x) { let s = math.abs(x); return x; } :: math.sqrt(f('a')) == 1",
    "function f(x) { let a = x.size(); return x && a == 1; } :: f(1)",
    "function f(x) { let a = x == 1; let b = x.size(); return x && a && b == 1; } :: f(1)",
    "function f(x) { let a = x.size(); let b = x.keys(); return x && a == 1 && b == []; } :: f(1)",
    "function f(x) { let a = x.size(); let b = x.keys(); return a == 1 && b == []; } :: f(1)",
    "function f(x) { let a = x.size(); let b = x.keys(); let c = x.lower(); return x && a == 1 && b == [] && c == ''; } :: f(1)",
    "function f(x) { let a = !x; let b = x.lower(); let c = x.keys(); return x && a && b == '' && c == []; } :: f(1)",
    "function g() { return room.keys() == []; } :: g() && g()",
    "function f() { return 1; } :: f(1)", "function f(x) { return x; } :: f()", "function f(x) { return x; } :: f(1, 2)",
]
FUNCTION_RULES = [
    "rules_version = '2';\nservice cloud.firestore {\n  function top(x) { return x.size() == 1; }\n"
    "  match /databases/{database}/documents {\n    function mid(x) { return database.keys() == [] && x; }\n"
    "    match /rooms/{room} {\n      allow read: if top(1) && mid(1);\n    }\n  }\n}\n",
    ruleset("match /rooms/{room} {\n  function h(x) {\n    let a = x.size();\n    return a == 1;\n  }\n"
            "  allow read: if h(1);\n}"),
    "service cloud.firestore {\n  match /databases/{database}/documents {\n    match /rooms/{room} {\n"
    "      allow read: if 1 + 1 && room.keys() == [] && 'a' == 1;\n    }\n  }\n}\n",
    # Recursion and cycles are errors; the warnings come with them.
    ruleset("match /rooms/{room} {\n  function f(x) { return f(x).size() == 1 && x; }\n  allow read: if f(1);\n}"),
    ruleset("match /rooms/{room} {\n  function f(x) { return g(x); }\n  function g(y) { return f(y) && y.size() == 1; }\n"
            "  allow read: if f(1) && g('a') == 1;\n}"),
    ruleset("match /rooms/{room} {\n  function f(x) { return x.size() == 1; }\n  function f(x) { return x; }\n"
            "  allow read: if f(1);\n}"),
    ruleset("match /rooms/{room} {\n  function f(x) { let x = 1; return x.size() == 1; }\n  allow read: if f(1);\n}"),
    ruleset("match /rooms/{room} {\n  function f(x, x) { return x.size() == 1; }\n  allow read: if f(1, 2);\n}"),
    ruleset("match /rooms/{room} {\n  function f(a, b, c, d, e, f, g, h) { return a; }\n  allow read: if f(1, 2, 3, 4, 5, 6, 7, 8);\n}"),
    # Functions of every scope, and wildcards they see.
    "rules_version = '2';\nfunction top(x) { return x.keys() == []; }\nservice cloud.firestore {\n"
    "  function svc(x) { return top(x) && x.size() == 1; }\n  match /databases/{database}/documents {\n"
    "    function documents(x) { return svc(x) && database.size() == 1 && x; }\n"
    "    match /rooms/{room} {\n      function rooms(x) { return documents(x) && room.keys() == []; }\n"
    "      allow read: if rooms(1) && top('a') && svc([1]);\n    }\n  }\n}\n",
    "service cloud.firestore {\n  match /databases/{database}/documents {\n"
    "    function f(x) { return x.size() == 1; }\n    match /rooms/{room} {\n"
    "      allow read: if f(1) && f(room) && room.keys() == [];\n    }\n  }\n}\n",
    ruleset("match /rooms/{room} {\n  function f(x) {\n    let a = x.size();\n    let b = a + 1;\n"
            "    let c = b.lower();\n    return c == '' && x;\n  }\n  allow read: if f(1);\n}"),
    chain(4, False), chain(4, True),
]

# Each method on a receiver of the wrong type and with arguments of the wrong type, and its result.
METHODS = {
    "get": 2, "size": 0, "matches": 1, "keys": 0, "hasAll": 1, "toSet": 0, "date": 0, "time": 0, "lower": 0,
    "upper": 0, "split": 1, "replace": 2, "trim": 0, "join": 1, "concat": 1, "toMillis": 0, "year": 0, "month": 0,
    "day": 0, "hours": 0, "minutes": 0, "seconds": 0, "nanos": 0, "dayOfWeek": 0, "dayOfYear": 0, "distance": 1,
    "latitude": 0, "longitude": 0, "bind": 1, "toBase64": 0, "toHexString": 0, "toUtf8": 0, "diff": 1,
    "addedKeys": 0, "affectedKeys": 0, "changedKeys": 0, "removedKeys": 0, "unchangedKeys": 0, "values": 0,
    "difference": 1, "intersection": 1, "union": 1, "removeAll": 1, "hasAny": 1, "hasOnly": 1,
}
SIGNATURES = [f"[true.{m}({', '.join(['true'] * n)})] == []" for m, n in METHODS.items()]
SIGNATURES += [f"!request.y.{m}({', '.join(['request.x'] * n)})" for m, n in METHODS.items()]
SIGNATURES += [f"-request.y.{m}({', '.join(['request.x'] * n)}) == 1" for m, n in METHODS.items()]
for f, n in [("int", 1), ("float", 1), ("string", 1), ("path", 1), ("get", 1), ("exists", 1), ("getAfter", 1),
             ("existsAfter", 1), ("debug", 1), ("value", 1), ("vector", 1), ("math.abs", 1), ("math.ceil", 1),
             ("math.floor", 1), ("math.isNaN", 1), ("math.pow", 2), ("math.round", 1), ("math.sqrt", 1),
             ("duration.abs", 1), ("duration.time", 4), ("duration.value", 2), ("timestamp.date", 3),
             ("timestamp.value", 1), ("latlng.value", 2), ("hashing.crc32", 1), ("hashing.crc32c", 1),
             ("hashing.md5", 1), ("hashing.sha256", 1)]:
    SIGNATURES += [f"[{f}({', '.join(['true'] * n)})] == []", f"!{f}({', '.join(['request.x'] * n)})",
                   f"-{f}({', '.join(['request.x'] * n)}) == 1"]

# Conditions as rules are written.
USUAL = [
    "request.auth.uid == room", "request.auth.token.email_verified == true",
    "resource.data.count + 1 == request.resource.data.count", "request.resource.data.size() < 10",
    "request.time < resource.data.deadline", "request.resource.data.keys().hasAll(['a'])",
    "room.size() > 3 && room.matches('^[a-z]+$')", "room in ['a', 'b']", "room in request.auth.token.rooms",
    "get(/databases/$(database)/documents/users/$(request.auth.uid)).data.admin == true",
    "exists(/databases/$(database)/documents/users/$(room))", "request.path[4] == room",
    "request.resource.data.diff(resource.data).affectedKeys().hasOnly(['a'])",
    "request.resource.data.ts == request.time", "request.resource.data.n is int && request.resource.data.n >= 0",
    "(room == 'a') == true", "room == 'a' ? true : false", "room != null", "room == null", "!(room in [])",
    "room + '' == 'a'", "room[0] == 'a'", "room[0:1] == 'a'", "room.lower() == room", "int(room) > 0",
    "path(room) == /a", "/a/$(room) == /a/b", "[room].hasAny(['a'])", "{room: 1}.size() == 1",
    "{'a': room}.a == 'x'", "(room == 'a' ? null : 1) == null", "request.time + duration.value(1, 'h') > request.time",
    "request.resource.data.tags.size() <= 5 && request.resource.data.tags is list",
    "request.auth != null && request.auth.uid in resource.data.members",
    "resource.data.visibility == 'public' || request.auth.uid == resource.data.owner",
    "request.resource.data.title is string && request.resource.data.title.size() > 0",
    "math.abs(request.resource.data.lat) <= 90", "request.resource.data.loc.distance(resource.data.loc) < 1000",
    "timestamp.date(2020, 1, 1) < request.time", "request.time.toMillis() - resource.data.created.toMillis() < 60000",
    "hashing.sha256(room).toBase64() == 'x'", "string(request.time.year()) == '2026'",
    "request.query.limit <= 10", "request.method == 'get'", "!exists(/databases/$(database)/documents/a/$(room))",
    "room.split('-')[0] == 'a'", "room.replace('a', 'b') == 'b'", "room.trim().upper() == 'A'",
    "room.toUtf8().size() < 10", "debug(room) == 'a'", "room == room", "database == room",
]
USUAL_FUNCTIONS = [
    "function isSignedIn() { return request.auth != null; } function isOwner(uid) { return isSignedIn() && request.auth.uid == uid; } :: isOwner(resource.data.owner)",
    "function isOwner(uid) { return request.auth.uid == uid; } :: isOwner(room) && isOwner(1)",
    "function valid(d) { return d.keys().hasOnly(['a', 'b']) && d.a is string && d.b > 0; } :: valid(request.resource.data)",
    "function valid(d) { return d.keys().hasOnly(['a', 'b']) && d.a.size() < 10; } :: valid({'a': 'x'}) && valid('x') && valid([1])",
    "function f(n) { return n > 0 && n < 10 && n % 2 == 0; } :: f(1) && f('a') && f(1.5)",
    "function f(t) { return t < request.time && t + duration.value(1, 'd') > request.time; } :: f(request.time) && f(1)",
    "function f(a, b) { return a.size() == b.size(); } :: f('a', [1]) && f(1, 2)",
    "function f(s) { return s.matches('a.*') && s.size() > 1; } :: f(room) && f(1)",
    "function f(m) { return m.diff({}).affectedKeys().hasOnly(['a']); } :: f(request.resource.data) && f([1])",
    "function f(p) { return exists(p) && get(p).data.x == 1; } :: f(/databases/$(database)/documents/a/b) && f('x')",
    "function f(x) { let y = x.size(); let z = y + 1; return z > 2; } :: f('a') && f(1)",
    "function f(x) { let y = x.lower(); return y == 'a' || y.size() == 0; } :: f(1)",
    "function f(x) { return x is string ? x.size() > 0 : x > 0; } :: f(1)",
    "function f(x) { return x in ['a', 'b']; } :: f(1)", "function f(x) { return x.get('a', 0) == 0; } :: f(1)",
    "function f(x) { return x[0] == 'a' && x[1:2] == 'b'; } :: f({})",
    "function f(x) { return x.toMillis() > 0; } :: f(request.time) && f(timestamp.value(1)) && f(1)",
    "function f(x) { return x == null || x.size() > 0; } :: f(null) && f(1)",
    "function f(x, y) { return x.hasAny(y); } :: f([1], 'a') && f('a', [1])",
    "function f(x) { return debug(x).size() == 0; } :: f(1)",
    "function f(x) { return x.size(); } :: f('a') > 0 && f(1) > 0",
    "function f(x) { return string(x) == 'a'; } :: f([1]) && f(true)",
    "function f(x) { return x.split(',').size() > 0; } :: f(1)",
    "function f(x) { return int(x) > 0 || float(x) > 0.0; } :: f(true)",
    "function f(x) { return math.abs(x) > 0; } :: f(duration.value(1, 's'))",
    "function f(x) { return -x < 0; } :: f(duration.value(1, 's')) && f('a')",
    "function f(x) { return x + x == x; } :: f([1]) && f('a')",
    "function f(x) { return {'a': x}.a == 1; } :: f(1)", "function f(x) { return [x, 1][0] == 1; } :: f(1)",
    "function f(x) { return x.a.b == 1; } :: f(1)", "function f(x) { return x.a.size() == 1; } :: f(1)",
    "function f(x) { return x.size() == 1; } function g(y) { return f(y) && y.keys() == []; } :: g('a')",
    "function f(x) { return x.size() == 1; } function g(y) { return f(y) && y.keys() == []; } :: g(1)",
    "function f(x) { return x; } function g(y) { return f(y).size() == 1; } :: g(1)",
    "function f(x) { return x; } function g(y) { return f(f(y)); } :: g(1)",
    "function f(x, y) { return x ? y : x; } :: f(1, 2)",
    "function f(x, y) { return x ? y : 1; } :: f(true, 'a') && f(true, 1)",
    "function f(x) { return x == x; } :: f(1)",
    "function f(x) { let a = x; let b = a; return b; } :: f(1) && f(true)",
    "function f(x) { let a = x; return a.size() == 1 ? a : a; } :: f([1]) && f('a').lower() == ''",
    "function f(x) { let a = x + 1; return a.lower(); } :: f(1) == 'a'",
    "function f(x, y) { let a = x.size() + y; return a > 0; } :: f('a', 'b') && f(1, 1)",
    "function f(x) { return g(x, x); } function g(a, b) { return a.size() == 1 && b; } :: f(1)",
    "function f(x) { return g(x) || h(x); } function g(a) { return a.size() == 1; } function h(b) { return b.keys() == []; } :: f(1)",
    "function f(x) { return x; } :: f(1) ? 1 : 2",
    "function f(x) { return x; } :: (f(1) ? 1 : 2) == 1",
    "function f(x) { return x; } :: f(room) == 1",
    "function f(x) { return x; } :: f(room).keys() == []",
    "function f(x) { return x.keys(); } :: f(room) == []",
]

CONDITIONS = {
    "positions": POSITIONS, "operators": OPERATORS, "failing": FAILING, "access": ACCESS,
    "comparisons": COMPARISONS, "ternaries": TERNARIES, "wildcards": WILDCARDS, "signatures": SIGNATURES,
    "usual": USUAL,
}


def put(content):
    c = http.client.HTTPConnection(*HOST.split(":"), timeout=30)
    body = json.dumps({"rules": {"files": [{"name": "firestore.rules", "content": content}]}})
    c.request("PUT", "/emulator/v1/projects/warnings:securityRules", body, {"Content-Type": "application/json"})
    res = c.getresponse()
    raw = res.read().decode()
    c.close()
    body = json.loads(raw)
    if res.status != 200:
        return {"error": body["error"]["message"]}
    issues = []
    for i in body.get("issues", []):
        p = i.get("sourcePosition", {})
        issues.append([p.get("line"), p.get("column"), p.get("currentOffset"), p.get("endOffset"),
                       i["severity"], i["description"]])
    return {"issues": issues} if issues else {}


def main():
    cases = []
    eval_fixture = json.load(open("crates/hidane-rules/tests/fixtures/rules_eval.json"))
    conditions_fixture = json.load(open("results/rules-conditions-official-v1.22.0.json"))
    paths_fixture = json.load(open("crates/hidane-rules/tests/fixtures/rules_paths.json"))
    assert eval_fixture["rules"] == RULES and conditions_fixture["rules"] == RULES
    for r in eval_fixture["results"]:
        if r["name"].startswith("batches/"):
            cases.append({"name": "eval/" + r["name"], "rules": batch_rules(r["condition"])})
        elif "rules" in r:
            cases.append({"name": "eval/" + r["name"], "rules": r["rules"]})
        else:
            cases.append({"name": "eval/" + r["name"], "condition": r["condition"]})
    for r in conditions_fixture["results"]:
        cases.append({"name": "conditions/" + r["name"], "condition": r["condition"]})
    for i, r in enumerate(paths_fixture["results"]):
        cases.append({"name": f"paths/{i}", "rules": paths_rules(r["version"], r["match"], r["condition"])})
    for group, conditions in CONDITIONS.items():
        for i, condition in enumerate(conditions):
            cases.append({"name": f"types/{group}/{i}", "condition": condition})
    for i, content in enumerate(WILDCARD_RULES):
        cases.append({"name": f"types/wildcard rules/{i}", "rules": content})
    for group, lines in (("functions", FUNCTIONS), ("usual functions", USUAL_FUNCTIONS)):
        for i, line in enumerate(lines):
            decls, condition = line.split(" :: ")
            cases.append({"name": f"types/{group}/{i}", "rules": RULES_F % (decls, condition)})
    for i, content in enumerate(FUNCTION_RULES):
        cases.append({"name": f"types/function rules/{i}", "rules": content})

    seen = set()
    results = []
    for case in cases:
        content = RULES % case["condition"] if "condition" in case else case["rules"]
        if content in seen:
            continue
        seen.add(content)
        results.append({**case, **put(content)})
    # One result per line, so a diff shows which cases changed.
    sys.stdout.reconfigure(encoding="utf-8")
    print("{")
    print(f' "rules": {json.dumps(RULES)},')
    print(' "results": [')
    print(",\n".join("  " + json.dumps(r, ensure_ascii=False) for r in results))
    print(" ]")
    print("}")


if __name__ == "__main__":
    main()
