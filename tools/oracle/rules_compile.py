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
]

REQUESTS = [
    ("two files", {"rules": {"files": [{"name": "a.rules", "content": allow("true")}, {"name": "b.rules", "content": allow("true")}]}}),
    ("file without a name", {"rules": {"files": [{"content": allow("foo()")}]}}),
    ("other file name", files(allow("foo()"), name="/abs/path/security.rules")),
    ("no files", {"rules": {"files": []}}),
    ("no rules", {}),
    ("unknown field", {"rules": {"files": [{"name": "a", "content": allow("true")}]}, "x": 1}),
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
