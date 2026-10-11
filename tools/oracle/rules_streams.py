#!/usr/bin/env python3
"""How do the Listen and Write streams answer under rules? (#63, #67)

Usage (from the repository root), with the jar started under an English locale
(`java -Duser.language=en -Duser.country=US -jar …`) and `grpcurl` on `PATH`:
    python3 -I tools/oracle/rules_streams.py 127.0.0.1:8086 > fixture.json

Listen: targets for an allowed document, a denied one, both in one target, a denied query, and
an allowed document that a later write (as the owner) makes denied. Write: a denied write on
the stream. Each stream runs through grpcurl with the repository's protos (the official
emulator's reflection cannot describe the service); the messages it prints are recorded.
"""

import json
import subprocess
import sys
import threading
import time
import urllib.request

HOST = sys.argv[1]
PROTOS = "crates/hidane-proto/proto"


def rest(method, path, body=None, owner=False):
    headers = {"Content-Type": "application/json"}
    if owner:
        headers["Authorization"] = "Bearer owner"
    req = urllib.request.Request(f"http://{HOST}{path}", method=method,
                                 data=None if body is None else json.dumps(body).encode(), headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=30) as res:
            return res.status
    except urllib.error.HTTPError as err:
        return err.code


def stream(rpc, database, messages, wait=2.0, during=None):
    """Sends `messages` (then waits), runs `during` meanwhile, returns what grpcurl printed."""
    proc = subprocess.Popen(
        ["grpcurl", "-plaintext", "-import-path", PROTOS, "-proto", "google/firestore/v1/firestore.proto",
         "-H", f"google-cloud-resource-prefix: {database}", "-d", "@", HOST, f"google.firestore.v1.Firestore/{rpc}"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    for message in messages:
        proc.stdin.write(json.dumps(message) + "\n")
        proc.stdin.flush()
        time.sleep(0.5)
    if during:
        threading.Timer(0.5, during).start()
    time.sleep(wait)
    proc.stdin.close()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
    return proc.stdout.read()


def project(name, rules, documents):
    database = f"projects/{name}/databases/(default)"
    content = ("rules_version = '2';\nservice cloud.firestore {\n  match /databases/{database}/documents {\n"
               + "\n".join("    " + line for line in rules.split("\n")) + "\n  }\n}\n")
    status = rest("PUT", f"/emulator/v1/projects/{name}:securityRules",
                  {"rules": {"files": [{"name": "firestore.rules", "content": content}]}})
    assert status == 200
    for doc, n in documents:
        parent, id_ = doc.rsplit("/", 1)
        rest("POST", f"/v1/{database}/documents/{parent}?documentId={id_}", {"fields": {"n": {"integerValue": str(n)}}}, owner=True)
    return database, content


results = []
db, rules = project("streams-a", "match /c/{d} {\n  allow get: if d == 'a';\n  allow list: if false;\n  allow write: if d == 'a';\n}",
                    [("c/a", 1), ("c/b", 1)])
doc = lambda d: f"{db}/documents/{d}"
for name, target in {
    "listen: allowed document": {"documents": {"documents": [doc("c/a")]}, "targetId": 1},
    "listen: denied document": {"documents": {"documents": [doc("c/b")]}, "targetId": 2},
    "listen: one denied of two": {"documents": {"documents": [doc("c/a"), doc("c/b")]}, "targetId": 3},
    "listen: denied query": {"query": {"parent": f"{db}/documents", "structuredQuery": {"from": [{"collectionId": "c"}]}}, "targetId": 4},
}.items():
    results.append({"name": name, "rules": rules, "output": stream("Listen", db, [{"database": db, "addTarget": target}])})
results.append({"name": "write: denied", "rules": rules, "output": stream("Write", db, [
    {"database": db}, {"streamToken": "", "writes": [{"update": {"name": doc("c/x"), "fields": {"n": {"integerValue": "2"}}}}]}])})

db, rules = project("streams-b", "match /c/{d} {\n  allow get: if resource.data.n == 1;\n}", [("c/a", 1)])
results.append({"name": "listen: a write makes the document denied", "rules": rules, "output": stream(
    "Listen", db, [{"database": db, "addTarget": {"documents": {"documents": [f"{db}/documents/c/a"]}, "targetId": 1}}],
    wait=3, during=lambda: rest("PATCH", f"/v1/{db}/documents/c/a", {"fields": {"n": {"integerValue": "2"}}}, owner=True))})

json.dump(results, sys.stdout, indent=1)
print()
