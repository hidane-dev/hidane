#!/usr/bin/env python3
"""Which documents does a `match` path cover, and what do its wildcards hold? (#59)

Usage (from the repository root), with the jar started under an English locale
(`java -Duser.language=en -Duser.country=US -jar …`):
    python3 -I tools/oracle/rules_paths.py 127.0.0.1:8086 > fixture.json

For each rules version (1, 2) and `match` path (under `/databases/{database}/documents`), the
script loads `allow read: if <condition>;` for several conditions, then reads a set of
documents over REST without an `Authorization` header and records whether each read was
allowed. The documents are written once, as the owner. With the condition `true` this shows
which documents a path covers; the other conditions show what its wildcards hold (`is string`,
`is path`, sizes and segments). Stdlib only.
"""

import json
import sys
import urllib.error
import urllib.request

HOST = sys.argv[1]
PROJECT = "paths"
BASE = f"projects/{PROJECT}/databases/(default)/documents"
DOCS = ["c/d", "c/x", "x/y", "c/d/e/f", "c/d/e/g", "c/d/x/f", "c/d/e/f/g/h", "a/b/e/f", "e/f"]


def request(method, path, body=None, owner=False):
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


def rules(version, nested, condition):
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


# (match paths, outermost first; conditions besides `true`)
CASES = [
    (["/{document=**}"], ["document is path", "document is string", "document.size() == 2", "document[0] == 'c'"]),
    (["/c/{rest=**}"], ["rest is path", "rest is string", "rest.size() == 1", "rest[0] == 'd'"]),
    (["/c/{d}"], ["d is string", "d == 'd'"]),
    (["/{c}/{d}"], ["c == 'c'"]),
    (["/c/{d}/{rest=**}"], ["rest.size() == 2", "rest[0] == 'e'"]),
    (["/c/d/{rest=**}"], ["rest.size() == 0"]),
    (["/{path=**}/e/{f}"], ["path.size() == 2", "path.size() == 0", "path[0] == 'c'"]),
    (["/c/{rest=**}/f"], ["rest.size() == 3"]),
    (["/c/{d}", "/{rest=**}"], ["rest.size() == 0", "rest.size() == 2"]),
    (["/c/{d}", "/e/{f}"], ["d == 'd' && f == 'f'"]),
    (["/{a}/{b}", "/{rest=**}"], ["rest.size() == 2"]),
]

for doc in DOCS:
    status, _ = request("POST", f"/v1/{BASE}/{doc.rsplit('/', 1)[0]}?documentId={doc.rsplit('/', 1)[1]}",
                        {"fields": {"n": {"integerValue": "1"}}}, owner=True)
    assert status in (200, 409), (doc, status)

results = []
for version in (1, 2):
    for nested, conditions in CASES:
        for condition in ["true"] + conditions:
            content = rules(version, nested, condition)
            status, body = request("PUT", f"/emulator/v1/projects/{PROJECT}:securityRules",
                                   {"rules": {"files": [{"name": "firestore.rules", "content": content}]}})
            entry = {"version": version, "match": nested, "condition": condition}
            if status != 200:
                entry["compile"] = {"status": status, "message": json.loads(body).get("error", {}).get("message")}
                results.append(entry)
                continue
            issues = json.loads(body).get("issues", [])
            if issues:
                entry["issues"] = [i["description"] for i in issues]
            entry["reads"] = {}
            for doc in DOCS:
                status, body = request("GET", f"/v1/{BASE}/{doc}")
                entry["reads"][doc] = "allow" if status == 200 else ("deny" if status == 403 else status)
            results.append(entry)

json.dump({"documents": DOCS, "results": results}, sys.stdout, indent=1)
print()
