#!/usr/bin/env python3
"""How do `get`, `exists`, `getAfter` and `existsAfter` behave, and how many calls are allowed? (#65)

Usage (from the repository root), with the jar started under an English locale
(`java -Duser.language=en -Duser.country=US -jar …`):
    python3 -I tools/oracle/rules_lookups.py 127.0.0.1:8086 > fixture.json

Each case loads one ruleset and runs one operation over REST without an `Authorization`
header (single reads, queries, single writes, batches of several writes), recording the status
and the error message. Chains of `exists()` over documents that do not exist show the call
limits (and whether repeated calls to one document count once); `getAfter` / `existsAfter` show
which state a write sees. Documents are written as the owner. Stdlib only.
"""

import json
import sys
import urllib.error
import urllib.request

HOST = sys.argv[1]
PROJECT = "lookups"
BASE = f"projects/{PROJECT}/databases/(default)/documents"
I = lambda n: {"integerValue": str(n)}
P = "/databases/$(database)/documents"


def call(method, path, body=None, owner=False):
    headers = {"Content-Type": "application/json"}
    if owner:
        headers["Authorization"] = "Bearer owner"
    req = urllib.request.Request(f"http://{HOST}{path}", method=method,
                                 data=None if body is None else json.dumps(body).encode(), headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=30) as res:
            return res.status, res.read().decode()
    except urllib.error.HTTPError as err:
        return err.code, err.read().decode()


def load(body):
    content = ("rules_version = '2';\nservice cloud.firestore {\n  match /databases/{database}/documents {\n"
               + "\n".join("    " + line for line in body.split("\n")) + "\n  }\n}\n")
    status, text = call("PUT", f"/emulator/v1/projects/{PROJECT}:securityRules",
                        {"rules": {"files": [{"name": "firestore.rules", "content": content}]}})
    assert status == 200, text
    return content


def none_exist(n, distinct=True):
    return " && ".join(f"!exists({P}/missing/m{i if distinct else 0})" for i in range(n))


def update(doc, fields):
    return {"update": {"name": f"{BASE}/{doc}", "fields": fields}}


def commit(*writes):
    return ("POST", f"/v1/{BASE}:commit", {"writes": list(writes)})


GET = ("GET", f"/v1/{BASE}/c/d", None)
QUERY = ("POST", f"/v1/{BASE}:runQuery", {"structuredQuery": {"from": [{"collectionId": "c"}]}})

CASES = []
for n in (10, 11, 20, 21):
    CASES.append((f"get: {n} distinct exists", f"match /c/{{d}} {{\n  allow read: if {none_exist(n)};\n}}", GET))
CASES.append(("get: 15 exists of one document", f"match /c/{{d}} {{\n  allow read: if {none_exist(15, distinct=False)};\n}}", GET))
for n in (10, 11):
    CASES.append((f"query: {n} distinct exists", f"match /c/{{d}} {{\n  allow read: if {none_exist(n)};\n}}", QUERY))
for n in (10, 11, 20, 21):
    CASES.append((f"single write: {n} distinct exists",
                  f"match /c/{{d}} {{\n  allow read: if true;\n  allow write: if {none_exist(n)};\n}}",
                  commit(update("c/w1", {"n": I(1)}))))
for n in (10, 11, 20, 21):
    CASES.append((f"batch of two writes: {n} distinct exists in each",
                  f"match /c/{{d}} {{\n  allow read: if true;\n  allow write: if {none_exist(n)};\n}}",
                  commit(update("c/w1", {"n": I(1)}), update("c/w2", {"n": I(1)}))))
# Each write of a batch checks its own documents (`$(d)` in the path), so the batch's total grows.
per_write = lambda n: " && ".join(f"!exists({P}/missing/$(d + '_{i}'))" for i in range(n))
for n in (6, 10, 11):
    CASES.append((f"batch of two writes: {n} exists of the write's own documents in each",
                  f"match /c/{{d}} {{\n  allow read: if true;\n  allow write: if {per_write(n)};\n}}",
                  commit(update("c/w1", {"n": I(1)}), update("c/w2", {"n": I(1)}))))
for n in (4, 5, 6, 7):
    CASES.append((f"batch of four writes: {n} exists of the write's own documents in each",
                  f"match /c/{{d}} {{\n  allow read: if true;\n  allow write: if {per_write(n)};\n}}",
                  commit(*(update(f"c/w{k}", {"n": I(1)}) for k in range(4)))))
for n in (11, 21):
    CASES.append((f"query: {n} distinct exists, with a filter", f"match /c/{{d}} {{\n  allow read: if {none_exist(n)};\n}}",
                  ("POST", f"/v1/{BASE}:runQuery", {"structuredQuery": {"from": [{"collectionId": "c"}], "where": {
                      "fieldFilter": {"field": {"fieldPath": "n"}, "op": "EQUAL", "value": I(1)}}}})))
CASES.append(("query: 21 distinct exists", f"match /c/{{d}} {{\n  allow read: if {none_exist(21)};\n}}", QUERY))
CASES += [
    ("get of an existing document", f"match /c/{{d}} {{\n  allow read: if get({P}/other/o).data.n == 1;\n}}", GET),
    ("get of a missing document", f"match /c/{{d}} {{\n  allow read: if get({P}/missing/x).data.n == 1;\n}}", GET),
    ("get of a missing document, absorbed", f"match /c/{{d}} {{\n  allow read: if get({P}/missing/x).data.n == 1 || true;\n}}", GET),
    ("exists of an existing document", f"match /c/{{d}} {{\n  allow read: if exists({P}/other/o);\n}}", GET),
    ("get with a variable", f"match /c/{{d}} {{\n  allow read: if get({P}/other/$(d)).data.n == 1;\n}}", ("GET", f"/v1/{BASE}/c/o", None)),
    ("get in a function", f"function n(id) {{\n  return get({P}/other/$(id)).data.n;\n}}\nmatch /c/{{d}} {{\n  allow read: if n('o') == 1;\n}}", GET),
    ("getAfter sees the write", f"match /c/{{d}} {{\n  allow read: if true;\n  allow write: if getAfter({P}/c/$(d)).data.n == 2;\n}}",
     commit(update("c/after", {"n": I(2)}))),
    ("get sees the state before", f"match /c/{{d}} {{\n  allow read: if true;\n  allow write: if get({P}/c/$(d)).data.n == 1;\n}}",
     commit(update("c/after", {"n": I(3)}))),
    ("existsAfter of another write in the batch", f"match /c/{{d}} {{\n  allow read: if true;\n  allow write: if d != 'a' || existsAfter({P}/c/b);\n}}",
     commit(update("c/a", {"n": I(1)}), update("c/b", {"n": I(1)}))),
    ("existsAfter of a document the batch deletes",
     f"match /c/{{d}} {{\n  allow read: if true;\n  allow write: if d != 'a' || !existsAfter({P}/other/o);\n}}\nmatch /other/{{o}} {{\n  allow write: if true;\n}}",
     commit(update("c/a", {"n": I(1)}), {"delete": f"{BASE}/other/o"})),
    ("get of a document the batch deletes",
     f"match /c/{{d}} {{\n  allow read: if true;\n  allow write: if d != 'a' || get({P}/other/o).data.n == 1;\n}}\nmatch /other/{{o}} {{\n  allow write: if true;\n}}",
     commit(update("c/a", {"n": I(1)}), {"delete": f"{BASE}/other/o"})),
    ("getAfter in a transaction-less single write of another document",
     f"match /c/{{d}} {{\n  allow read: if true;\n  allow write: if getAfter({P}/other/o).data.n == 1;\n}}",
     commit(update("c/x", {"n": I(1)}))),
    ("getAfter on a read", f"match /c/{{d}} {{\n  allow read: if getAfter({P}/other/o).data.n == 1;\n}}", GET),
]


def reset():
    for doc in ("c/d", "other/o", "c/after"):
        call("DELETE", f"/v1/{BASE}/{doc}", owner=True)
    call("POST", f"/v1/{BASE}/c?documentId=d", {"fields": {"n": I(1)}}, owner=True)
    call("POST", f"/v1/{BASE}/other?documentId=o", {"fields": {"n": I(1)}}, owner=True)
    call("POST", f"/v1/{BASE}/c?documentId=after", {"fields": {"n": I(1)}}, owner=True)


results = []
for name, body, (method, path, payload) in CASES:
    reset()
    content = load(body)
    status, text = call(method, path, payload)
    entry = {"name": name, "rules": content, "status": status}
    if status != 200:
        try:
            entry["message"] = json.loads(text)["error"]["message"]
        except (ValueError, KeyError):
            entry["message"] = text[:300]
    results.append(entry)

json.dump(results, sys.stdout, indent=1)
print()
