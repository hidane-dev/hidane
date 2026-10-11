#!/usr/bin/env python3
"""How does the official emulator compile Security Rules? (#59)

Usage (from the repository root), with the jar started under an English locale
(`java -Duser.language=en -Duser.country=US -jar …`; under another locale every warning comes
back as a 500):
    python3 -I tools/oracle/rules_compile.py 127.0.0.1:8086 corpus.json > fixture.json

Each case is loaded with `PUT /emulator/v1/projects/{p}:securityRules`, as firebase-tools and
rules-unit-testing do, and the status and body are recorded: `200` with `issues[]` (warnings)
or nothing, `400` with "Error compiling rules:" and one line per error. `corpus.json` is a list
of `{"name", "content"}` (optionally `"source"` and `"fileName"`), such as the samples of the
Firebase documentation and firebase/quickstart-testing; the cases below are added to it.
Stdlib only.
"""

import json
import sys
import urllib.error
import urllib.request

HOST = sys.argv[1]
CORPUS = json.load(open(sys.argv[2])) if len(sys.argv) > 2 else []


def put(body, project="compile", raw=None):
    data = raw if raw is not None else json.dumps(body).encode()
    req = urllib.request.Request(
        f"http://{HOST}/emulator/v1/projects/{project}:securityRules",
        method="PUT", data=data, headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=30) as res:
            status, text = res.status, res.read().decode()
    except urllib.error.HTTPError as err:
        status, text = err.code, err.read().decode()
    try:
        return status, json.loads(text)
    except ValueError:
        return status, text


def files(content, name="firestore.rules"):
    return {"rules": {"files": [{"name": name, "content": content}]}}


V2 = "rules_version = '2';\n"


def doc(body, version=V2, path="/databases/{database}/documents"):
    """`body` inside the documents match, indented."""
    inner = "\n".join("    " + line if line else line for line in body.split("\n"))
    return f"{version}service cloud.firestore {{\n  match {path} {{\n{inner}\n  }}\n}}\n"


def allow(condition, version=V2, match="/{document=**}", methods="read"):
    return doc(f"match {match} {{\n  allow {methods}: if {condition};\n}}", version)


CASES = [
    # Versions and services.
    ("version 2", allow("true")),
    ("no version", allow("true", version="")),
    ("version 1", allow("true", version="rules_version = '1';\n")),
    ("version 3", allow("true", version="rules_version = '3';\n")),
    ("version in double quotes", allow("true", version='rules_version = "2";\n')),
    ("version as a number", allow("true", version="rules_version = 2;\n")),
    ("version without semicolon", allow("true", version="rules_version = '2'\n")),
    ("storage service", V2 + "service firebase.storage {\n  match /b/{bucket}/o {\n    allow read: if true;\n  }\n}\n"),
    ("unknown service", V2 + "service foo.bar {\n  match /x {\n    allow read: if true;\n  }\n}\n"),
    ("two services", allow("true") + "service cloud.firestore {\n  match /x/{y} {\n    allow read;\n  }\n}\n"),
    ("empty file", ""),
    ("only a version", V2),
    ("empty service", V2 + "service cloud.firestore {\n}\n"),
    ("empty documents match", doc("")),
    ("trailing text", allow("true") + "garbage\n"),
    # Statements.
    ("missing semicolon", allow("true").replace("if true;", "if true")),
    ("missing closing brace", allow("true")[:-2] + "\n"),
    ("extra closing brace", allow("true") + "}\n"),
    ("allow without condition", doc("match /{document=**} {\n  allow read;\n}")),
    ("allow without if", doc("match /{document=**} {\n  allow read: true;\n}")),
    ("allow with empty condition", doc("match /{document=**} {\n  allow read: if ;\n}")),
    ("allow without colon", doc("match /{document=**} {\n  allow read if true;\n}")),
    ("all methods", doc("match /{document=**} {\n  allow get, list, create, update, delete: if true;\n  allow read, write: if false;\n}")),
    ("unknown method", doc("match /{document=**} {\n  allow foo: if true;\n}")),
    ("repeated method", doc("match /{document=**} {\n  allow read, read: if true;\n}")),
    ("allow outside a match", V2 + "service cloud.firestore {\n  allow read: if true;\n}\n"),
    ("two errors", allow("true &&").replace("allow read", "allow foo")),
    # Paths.
    ("match without slash", doc("match {document=**} {\n  allow read;\n}")),
    ("path keyword missing", doc("/foo/{bar} {\n  allow read;\n}")),
    ("recursive wildcard in the middle v2", allow("true", match="/a/{rest=**}/b")),
    ("recursive wildcard in the middle v1", allow("true", version="", match="/a/{rest=**}/b")),
    ("two recursive wildcards", allow("true", match="/{a=**}/{b=**}")),
    ("single wildcard with equals", allow("true", match="/{a=*}")),
    ("duplicate wildcard names", allow("true", match="/{a}/{a}")),
    ("wildcard shadowing", doc("match /a/{x} {\n  match /b/{x} {\n    allow read: if x != null;\n  }\n}")),
    ("literal segments", allow("true", match="/users/alice/posts/{post}")),
    ("segment with dash and dot", allow("true", match="/a-b/c.d/{e}")),
    ("empty segment", allow("true", match="/a//b")),
    ("trailing slash", allow("true", match="/a/")),
    # Functions.
    ("function", doc("function isOwner(uid) {\n  return request.auth.uid == uid;\n}\nmatch /users/{uid} {\n  allow read: if isOwner(uid);\n}")),
    ("function with let", doc("function f() {\n  let x = 1;\n  let y = x + 1;\n  return y == 2;\n}\nmatch /{d=**} {\n  allow read: if f();\n}")),
    ("let in version 1", doc("function f() {\n  let x = 1;\n  return x == 1;\n}\nmatch /{d=**} {\n  allow read: if f();\n}", version="")),
    ("let after return", doc("function f() {\n  return true;\n  let x = 1;\n}\nmatch /{d=**} {\n  allow read: if f();\n}")),
    ("function without return", doc("function f() {\n  true;\n}\nmatch /{d=**} {\n  allow read: if f();\n}")),
    ("function with two returns", doc("function f() {\n  return true;\n  return false;\n}\nmatch /{d=**} {\n  allow read: if f();\n}")),
    ("unused function", doc("function f() {\n  return true;\n}\nmatch /{d=**} {\n  allow read: if true;\n}")),
    ("unknown function", allow("foo()")),
    ("unknown function with arguments", allow("foo(1, 'a') == true")),
    ("unknown variable", allow("foo == 1")),
    ("unknown member", allow("request.foo == 1")),
    ("wrong number of arguments", doc("function f(a) {\n  return a;\n}\nmatch /{d=**} {\n  allow read: if f();\n}")),
    ("duplicate function", doc("function f() {\n  return true;\n}\nfunction f() {\n  return false;\n}\nmatch /{d=**} {\n  allow read: if f();\n}")),
    ("recursive function", doc("function f(n) {\n  return n == 0 || f(n - 1);\n}\nmatch /{d=**} {\n  allow read: if f(3);\n}")),
    ("function named like a builtin", doc("function get(x) {\n  return true;\n}\nmatch /{d=**} {\n  allow read: if get(1);\n}")),
    ("function at service level", V2 + "service cloud.firestore {\n  function f() {\n    return true;\n  }\n  match /databases/{database}/documents {\n    match /{d=**} {\n      allow read: if f();\n    }\n  }\n}\n"),
    ("function in a nested match", doc("match /a/{b} {\n  function g() {\n    return b == 'x';\n  }\n  allow read: if g();\n}")),
    ("function using a wildcard of an enclosing match", doc("function f() {\n  return database != null;\n}\nmatch /{d=**} {\n  allow read: if f();\n}")),
    # Conditions.
    ("invalid regex", allow("'a'.matches('(')")),
    ("regex not literal", allow("'a'.matches('(' + '')")),
    ("block comment", doc("/* a\n   block */\nmatch /{d=**} {\n  allow read: if true; // trailing\n}")),
    ("comment at the end without newline", allow("true") + "// end"),
    ("crlf line endings", allow("true").replace("\n", "\r\n")),
    ("tabs", allow("true").replace("    ", "\t")),
    ("unicode in a string", allow("'日本語' != ''")),
    ("unicode before an error", allow("'日本語' &&")),
    ("unicode identifier", allow("日本 == 1")),
    ("decrement token", allow("--1 == -1")),
    ("chained equality", allow("1 == 1 == true")),
    ("is with types", allow("1 is int && 1.0 is float && 'a' is string && true is bool && [] is list && {} is map && null == null")),
    ("is with an unknown type", allow("1 is foo")),
    ("path literal", allow("exists(/databases/$(database)/documents/users/$(request.auth.uid))")),
    ("get with data", allow("get(/databases/$(database)/documents/users/x).data.admin == true")),
    ("namespaced functions", allow("math.abs(-1) == 1 && duration.value(1, 's') > duration.value(0, 's') && timestamp.date(2020, 1, 1) < request.time && latlng.value(1, 2) != null && hashing.sha256('a') != null")),
    ("diff and affectedKeys", allow("request.resource.data.diff(resource.data).affectedKeys().hasOnly(['a'])", methods="update")),
    ("ternary", allow("request.auth == null ? false : true")),
    ("map literal", allow("{'a': 1}['a'] == 1")),
    ("long condition", allow(" || ".join(f"resource.data.f{i} == {i}" for i in range(60)))),
    ("deeply nested parentheses", allow("(" * 40 + "true" + ")" * 40)),
    ("warning and error", doc("function f() {\n  return true;\n}\nmatch /{d=**} {\n  allow read: if true &&;\n}")),
    # Functions, further.
    ("indirect recursion", doc("function f() {\n  return g();\n}\nfunction g() {\n  return f();\n}\nmatch /{d=**} {\n  allow read: if f();\n}")),
    ("call cycle of three, unused", V2 + "service cloud.firestore {\n  function h() { return f(); }\n  function f() { return g(); }\n  function g() { return h(); }\n  match /a {\n    allow read;\n  }\n}\n"),
    ("unused recursive function", doc("function f() {\n  return f();\n}\nmatch /{d=**} {\n  allow read;\n}")),
    ("duplicate parameter", doc("function f(a, a) {\n  return a == 1;\n}\nmatch /{d=**} {\n  allow read: if f(1, 2);\n}")),
    ("duplicate let", doc("function f() {\n  let a = 1;\n  let a = 2;\n  return a == 2;\n}\nmatch /{d=**} {\n  allow read: if f();\n}")),
    ("let shadowing a parameter", doc("function f(a) {\n  let a = 2;\n  return a == 2;\n}\nmatch /{d=**} {\n  allow read: if f(1);\n}")),
    ("unused let", doc("function f() {\n  let a = 2;\n  return true;\n}\nmatch /{d=**} {\n  allow read: if f();\n}")),
    ("let before its use", doc("function f() {\n  let a = b;\n  let b = 1;\n  return a == 1;\n}\nmatch /{d=**} {\n  allow read: if f();\n}")),
    ("function in a sibling match", doc("match /a/{x} {\n  function g() { return x == 'a'; }\n  allow read: if g();\n}\nmatch /b/{y} {\n  allow read: if g();\n}")),
    ("same function name nested", V2 + "service cloud.firestore {\n  function g() { return true; }\n  match /databases/{database}/documents {\n    function g() { return false; }\n    match /b/{y} {\n      allow read: if g();\n    }\n  }\n}\n"),
    ("service function using a wildcard", V2 + "service cloud.firestore {\n  function f() { return database == 'a'; }\n  match /databases/{database}/documents {\n    match /b/{y} {\n      allow read: if f();\n    }\n  }\n}\n"),
    ("top-level function", V2 + "function f() { return true; }\nservice cloud.firestore {\n  match /a {\n    allow read: if f();\n  }\n}\n"),
    ("function without a body", V2 + "service cloud.firestore {\n  function f() {\n  }\n  match /a { allow read: if f(); }\n}\n"),
    ("function without a name", V2 + "service cloud.firestore {\n  function () {\n    return true;\n  }\n  match /a { allow read; }\n}\n"),
    ("function without parameters", V2 + "service cloud.firestore {\n  function f {\n    return true;\n  }\n  match /a { allow read: if f(); }\n}\n"),
    ("function with only a let", V2 + "service cloud.firestore {\n  function f() {\n    let x = 1;\n  }\n  match /a { allow read: if f(); }\n}\n"),
    ("let without a semicolon", doc("function f() {\n  let x = 1\n  return x == 1;\n}\nmatch /{d=**} {\n  allow read: if f();\n}")),
    ("arities", allow("get() == null && exists(/a, /b) && int(1, 2) == 1 && 'a'.matches() && math.abs() == 1")),
    ("namespaced unknown function", allow("math.foo(1) == 1 && hashing.nope('a') == null")),
    ("package as a variable", allow("math == 1 && duration.x == 1")),
    ("reserved words", allow("{'if': 1}.if == 1 && request.while == 1 && request.function.size() == 1")),
    ("reserved word as a method", allow("request.x.while() == 1")),
    ("reserved word as a variable", allow("var == 1")),
    # Statements and paths, further.
    ("allow without methods", doc("match /a {\n  allow : if true;\n}")),
    ("allow with a trailing comma", doc("match /a {\n  allow read, : if true;\n}")),
    ("allow without a comma", doc("match /a {\n  allow read write: if true;\n}")),
    ("allow with only a colon", doc("match /a {\n  allow read: ;\n}")),
    ("path without match keyword", doc("/a {}\nmatch /b {\n  allow read;\n}")),
    ("match path then semicolon", doc("match /a ;")),
    ("match path then brace", doc("match /a }")),
    ("match with an unexpected body", doc("match /a {\n  write: if true;\n}")),
    ("match with one stray word", doc("match /a {\n  foo\n}")),
    ("empty nested match", doc("match /a {\n  match /b {\n  }\n}")),
    ("v1 glob with children", "service cloud.firestore {\n  match /databases/{database}/documents {\n    match /{a=**} {\n      match /b {\n        allow read;\n      }\n    }\n  }\n}\n"),
    ("v2 glob in parent and child", doc("match /x/{a=**} {\n  match /y/{b=**} {\n    allow read;\n  }\n}")),
    ("v1 two globs", allow("true", version="", match="/{a=**}/{b=**}")),
    ("wildcard repeated in a child", doc("match /x/{a} {\n  match /y/{a}/{a} {\n    allow read;\n  }\n}")),
    ("errors of several stages", doc("match /{a}/{a} {\n  allow read: true;\n}")),
    ("version 0", allow("true", version="rules_version = '0';\n")),
    ("version without equals", allow("true", version="rules_version \"2\";\n")),
    ("import", V2 + "import \"x.rules\";\nservice cloud.firestore {\n  match /a {\n    allow read;\n  }\n}\n"),
    ("unknown service name", V2 + "service cloud {\n  match /a {\n    allow read, foo;\n  }\n}\n"),
    ("storage methods", V2 + "service firebase.storage {\n  match /b/{bucket}/o {\n    allow read, write, get, list, create, update, delete, foo: if true;\n  }\n}\n"),
    ("service after service", V2 + "service firebase.storage {\n  match /b/{bucket}/o {\n    allow read: if foo();\n  }\n}\nservice cloud.firestore {\n  match /a {\n    allow read: if bar();\n  }\n}\n"),
    # Lexical rules, further.
    ("non-breaking space", allow("true").replace("    match", "    match")),
    ("non-breaking space after an error", allow("true &&") + " \n"),
    ("characters that start no token", allow("true # ~")),
    ("at sign", allow("@")),
    ("identifiers with dollars", allow("$b == 1 && a$ == 1")),
    ("float with a trailing dot", allow("1. == 1.0")),
    ("exponent", allow("1e3 == 1000")),
    ("division without spaces", allow("4/2 == 2")),
    ("path segment characters", allow("/a+b/c:d/e@f/(g)/h%20i/-j/.k == /a")),
    ("unclosed string", allow("'abc")),
    ("unclosed string in parentheses", allow("('abc")),
    ("unclosed parenthesis", allow("(true")),
    ("missing operand in a call", allow("f(,)")),
    ("missing map value", allow("{'a': }")),
    ("missing ternary colon", allow("true ? 'a' 'b'")),
    ("decrement twice", allow("----1 == 1")),
    ("emoji before a warning", allow("'😀' != '' && foo()")),
    ("regular expressions", allow("'a'.split('(') == [] && 'a'.replace('(', 'b') == 'a' && 'a'.matches('a**') && 'a'.matches('a*?') && 'a'.matches('(?P<n>a)')")),
    # Limits.
    ("parentheses 98 deep", allow("(" * 98 + "true" + ")" * 98)),
    ("parentheses 99 deep", allow("(" * 99 + "true" + ")" * 99)),
    ("negations 99 deep", allow("!" * 99 + "true")),
    ("lists 99 deep", allow("[" * 99 + "]" * 99 + " == []")),
    ("maps 98 deep", allow("{'a': " * 98 + "1" + "}" * 98 + " != null")),
    ("operators 99 long", allow(" && ".join(["true"] * 100))),
    ("minus 99 deep", allow("-(" * 99 + "1" + ")" * 99 + " == 1")),
    ("conditionals 99 deep", allow("true ? " * 99 + "true" + " : false" * 99)),
    ("members 600 long", allow("request" + ".a" * 600 + " == 1")),
    ("too complex with a warning", allow("(" * 99 + "true" + ")" * 99 + " && foo()")),
    ("matches 12 deep", V2 + "service cloud.firestore {\n" + "match /a {\n" * 12 + "allow read: if foo();\n" + "}\n" * 12 + "}\n"),
    ("matches 64 deep", V2 + "service cloud.firestore {\n" + "match /a {\n" * 64 + "allow read: if math == 1;\n" + "}\n" * 64 + "}\n"),
    ("path 101 long",V2 + "service cloud.firestore {\nmatch " + "/a" * 101 + " {\nallow read;\n}\n}\n"),
    ("21 path variables", V2 + "service cloud.firestore {\nmatch " + "".join(f"/{{a{i}}}" for i in range(15)) + " {\nmatch " + "".join(f"/{{b{i}}}" for i in range(6)) + " {\nallow read;\n}\n}\n}\n"),
    ("8 parameters", doc("function f(a, b, c, d, e, f, g, h) {\n  return true;\n}\nmatch /x {\n  allow read: if f(1, 2, 3, 4, 5, 6, 7, 8) && foo();\n}")),
    ("12 lets", doc("function f() {\n" + "".join(f"  let x{i} = 1;\n" for i in range(12)) + "  return x0 == x1;\n}\nmatch /x {\n  allow read: if f();\n}")),
    ("calls 22 deep", doc("".join(f"function f{i}() {{ return f{i + 1}(); }}\n" for i in range(22)) + "function f22() { return true; }\nmatch /a {\n  allow read: if f0();\n}")),
    # Types, as far as the comparison warning needs them.
    ("comparisons with null", allow("hashing.crc32('a') != null && request.path.bind({}) != null && math.floor(1.5) != null && vector([1.0]) != null && get(/a/b) != null")),
    ("recursive wildcard alone", allow("w == 'a' && w == /a/b && w == null", match="/{w=**}")),
    ("recursive wildcard after a wildcard", allow("w == 'a' && w == /a/b && w == null && v == 'a'", match="/{v}/{w=**}")),
    ("recursive wildcard in version 1", allow("w == 'a' && w == /a/b", version="", match="/{w=**}")),
]

REQUESTS = [
    ("two files", {"rules": {"files": [{"name": "a.rules", "content": allow("true")}, {"name": "b.rules", "content": allow("true")}]}}),
    ("file without a name", {"rules": {"files": [{"content": allow("foo()")}]}}),
    ("other file name", files(allow("foo()"), name="/abs/path/security.rules")),
    ("no files", {"rules": {"files": []}}),
    ("no rules", {}),
    ("unknown field", {"rules": {"files": [{"name": "a", "content": allow("true")}]}, "x": 1}),
    ("second file with a syntax error", {"rules": {"files": [{"name": "a.rules", "content": allow("true")}, {"name": "b.rules", "content": allow("true &&")}]}}),
    ("syntax errors in two files", {"rules": {"files": [{"name": "a.rules", "content": allow("false ||")}, {"name": "b.rules", "content": allow("true &&")}]}}),
    ("warnings in two files", {"rules": {"files": [{"name": "a.rules", "content": allow("foo()")}, {"name": "b.rules", "content": allow("bar()", version="")}]}}),
    ("second file with a glob error", {"rules": {"files": [{"name": "a.rules", "content": allow("foo()")}, {"name": "b.rules", "content": allow("true", match="/{a=**}/{b=**}")}]}}),
    ("second file empty", {"rules": {"files": [{"name": "a.rules", "content": allow("true")}, {"name": "b.rules", "content": ""}]}}),
    ("file without content", {"rules": {"files": [{"name": "a.rules"}]}}),
    ("file with a fingerprint", {"rules": {"files": [{"name": "a.rules", "content": allow("true"), "fingerprint": "AAAA"}]}}),
    ("file with an unknown field", {"rules": {"files": [{"name": "a.rules", "content": allow("true"), "x": 1}]}}),
    ("file name as a number", {"rules": {"files": [{"name": 1, "content": allow("foo()")}]}}),
    ("file name with special characters", files(allow("foo()", version=""), name="a<b>&='é /\u0001\"\\")),
]

results = []
for case in CORPUS:
    status, body = put(files(case["content"], case.get("fileName", "firestore.rules")))
    results.append({**case, "status": status, "response": body})
for name, content in CASES:
    status, body = put(files(content))
    results.append({"name": name, "content": content, "status": status, "response": body})
for name, request in REQUESTS:
    status, body = put(request)
    results.append({"name": name, "request": request, "status": status, "response": body})
status, body = put(None, raw=b"not json")
results.append({"name": "body that is not JSON", "raw": "not json", "status": status, "response": body})

json.dump(results, sys.stdout, indent=1, ensure_ascii=False)
print()
