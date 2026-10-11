#!/usr/bin/env python3
"""What does a denial say, and what does `:ruleCoverage` report? (#67)

Usage (from the repository root), with the jar started under an English locale
(`java -Duser.language=en -Duser.country=US -jar …`):
    python3 -I tools/oracle/rules_denials.py 127.0.0.1:8086 > fixture.json

Each case loads a ruleset and runs one operation over REST without an `Authorization` header,
recording the status and body of the answer: conditions that are false or fail, no matching
`allow`, several `allow` statements and matches, each method, queries and writes in a batch.
At the end the coverage report of the last project is fetched (`GET :ruleCoverage`). Documents
are written as the owner. Stdlib only.
"""

import json
import sys
import urllib.error
import urllib.request

HOST = sys.argv[1]
I = lambda n: {"integerValue": str(n)}


def call(method, path, body=None, owner=False):
    headers = {"Content-Type": "application/json"}
    if owner:
        headers["Authorization"] = "Bearer owner"
    req = urllib.request.Request(f"http://{HOST}{path}", method=method,
                                 data=None if body is None else json.dumps(body).encode(), headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=30) as res:
            return res.status, res.headers.get("content-type"), res.read().decode()
    except urllib.error.HTTPError as err:
        return err.code, err.headers.get("content-type"), err.read().decode()


def ruleset(body):
    return ("rules_version = '2';\nservice cloud.firestore {\n  match /databases/{database}/documents {\n"
            + "\n".join("    " + line for line in body.split("\n")) + "\n  }\n}\n")


def doc_base(project):
    return f"/v1/projects/{project}/databases/(default)/documents"


GET = lambda b: ("GET", f"{b}/c/d", None)
LIST = lambda b: ("GET", f"{b}/c", None)
QUERY = lambda b: ("POST", f"{b}:runQuery", {"structuredQuery": {"from": [{"collectionId": "c"}]}})
CREATE = lambda b: ("POST", f"{b}/c?documentId=new", {"fields": {"n": I(1)}})
UPDATE = lambda b: ("PATCH", f"{b}/c/d?updateMask.fieldPaths=n", {"fields": {"n": I(2)}})
DELETE = lambda b: ("DELETE", f"{b}/c/d", None)
BATCH = lambda b: ("POST", f"{b}:commit", {"writes": [
    {"update": {"name": f"{b[4:]}/c/a", "fields": {"n": I(1)}}},
    {"update": {"name": f"{b[4:]}/c/b", "fields": {"n": I(1)}}}]})

CASES = [
    ("false", "match /c/{d} {\n  allow read: if false;\n}", GET),
    ("false, two lines", "match /c/{d} {\n  allow read: if\n    false;\n}", GET),
    ("error", "match /c/{d} {\n  allow read: if int('a') == 1;\n}", GET),
    ("error in a function", "function f() {\n  return int('a') == 1;\n}\nmatch /c/{d} {\n  allow read: if f();\n}", GET),
    ("missing field", "match /c/{d} {\n  allow read: if resource.data.missing == 1;\n}", GET),
    ("no allow for the method", "match /c/{d} {\n  allow write: if true;\n}", GET),
    ("no matching path", "match /x/{d} {\n  allow read: if true;\n}", GET),
    ("two allows, both false", "match /c/{d} {\n  allow read: if false;\n  allow get: if 1 == 2;\n}", GET),
    ("two matches, both false", "match /c/{d} {\n  allow read: if false;\n}\nmatch /{p=**} {\n  allow read: if false;\n}", GET),
    ("allow without condition elsewhere", "match /c/{d} {\n  allow read: if false;\n}\nmatch /x/{d} {\n  allow read;\n}", GET),
    ("list denied", "match /c/{d} {\n  allow list: if false;\n}", LIST),
    ("query denied", "match /c/{d} {\n  allow list: if resource.data.n == 1;\n}", QUERY),
    ("create denied", "match /c/{d} {\n  allow create: if false;\n}", CREATE),
    ("update denied", "match /c/{d} {\n  allow update: if false;\n}", UPDATE),
    ("delete denied", "match /c/{d} {\n  allow delete: if false;\n}", DELETE),
    ("write denied", "match /c/{d} {\n  allow write: if false;\n}", UPDATE),
    ("batch, second write denied", "match /c/{d} {\n  allow write: if d == 'a';\n}", BATCH),
]

results = []
for i, (name, body, op) in enumerate(CASES):
    project = f"denials-{i}"
    base = doc_base(project)
    call("POST", f"{base}/c?documentId=d", {"fields": {"n": I(1)}}, owner=True)
    content = ruleset(body)
    status, _, text = call("PUT", f"/emulator/v1/projects/{project}:securityRules",
                           {"rules": {"files": [{"name": "firestore.rules", "content": content}]}})
    assert status == 200, text
    method, path, payload = op(base)
    status, ctype, text = call(method, path, payload)
    results.append({"name": name, "rules": content, "request": {"method": method, "path": path, "body": payload},
                    "status": status, "contentType": ctype, "response": text})

coverage = {}
for suffix in (":ruleCoverage", ":ruleCoverage/html"):
    status, ctype, text = call("GET", f"/emulator/v1/projects/denials-0{suffix}")
    coverage[suffix] = {"status": status, "contentType": ctype, "response": text[:4000]}

json.dump({"results": results, "coverage": coverage}, sys.stdout, indent=1, ensure_ascii=False)
print()
