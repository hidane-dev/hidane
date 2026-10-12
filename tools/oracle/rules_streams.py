#!/usr/bin/env python3
"""How do the Listen and Write streams answer under rules? (#63, #67)

Usage (from the repository root), with the jar started under an English locale
(`java -Duser.language=en -Duser.country=US -jar …`) and `grpcurl` on `PATH`:
    python3 -I tools/oracle/rules_streams.py 127.0.0.1:8086 > fixture.json

Listen: targets for an allowed document, a denied one, both in one target, a denied query, and
an allowed document that a later write (as the owner) makes denied. Write: a denied write on
the stream, and verifies (#173), allowed, denied, and denied after an allowed write. Each stream runs through grpcurl with the repository's protos (the official
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
    """Sends `messages` (then waits), runs `during` meanwhile; returns the messages grpcurl
    printed, in order, and the status the stream ended with (`None` for OK). grpcurl sends the
    last message only when its input closes, so a write's answer comes after the wait."""
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
    return parse(proc.stdout.read())


def parse(output):
    """grpcurl prints each message as JSON, and a status that is not OK as
    `ERROR:\n  Code: PermissionDenied\n  Message: …`."""
    decoder = json.JSONDecoder()
    received, at = [], 0
    while True:
        while at < len(output) and output[at].isspace():
            at += 1
        if at == len(output) or output[at] != "{":
            break
        message, at = decoder.raw_decode(output, at)
        received.append(message)
    rest = output[at:]
    status = None
    if rest.startswith("ERROR:"):
        code = rest.split("Code: ", 1)[1].split("\n", 1)[0]
        status = {"code": code, "message": rest.split("Message: ", 1)[1].rstrip("\n") if "Message: " in rest else ""}
    else:
        assert not rest.strip(), rest
    return received, status


def project(name, rules, documents):
    database = f"projects/{name}/databases/(default)"
    content = ("rules_version = '2';\nservice cloud.firestore {\n  match /databases/{database}/documents {\n"
               + "\n".join("    " + line for line in rules.split("\n")) + "\n  }\n}\n")
    status = rest("PUT", f"/emulator/v1/projects/{name}:securityRules",
                  {"rules": {"files": [{"name": "firestore.rules", "content": content}]}})
    assert status == 200
    for doc, n in documents:
        parent, id_ = doc.rsplit("/", 1)
        rest("DELETE", f"/v1/{database}/documents/{doc}", owner=True)
        rest("POST", f"/v1/{database}/documents/{parent}?documentId={id_}", {"fields": {"n": {"integerValue": str(n)}}}, owner=True)
    return database, content, documents


results = []


def record(name, setup, rpc, messages, during=None, **kwargs):
    """`setup`: what `project` returned. `during`: a REST call (method, path, body) made as the
    owner while the stream is open."""
    database, rules, documents = setup
    received, status = stream(rpc, database, messages,
                              during=during and (lambda: rest(*during, owner=True)), **kwargs)
    results.append({"name": name, "rules": rules, "database": database,
                    "documents": [{"path": d, "n": n} for d, n in documents], "rpc": rpc, "requests": messages,
                    "during": during and {"method": during[0], "path": during[1], "body": during[2]},
                    "responses": received, "status": status})


setup = project("streams-a", "match /c/{d} {\n  allow get: if d == 'a';\n  allow list: if false;\n  allow write: if d == 'a';\n}",
                [("c/a", 1), ("c/b", 1)])
db = setup[0]
doc = lambda d: f"{db}/documents/{d}"
for name, target in {
    "listen: allowed document": {"documents": {"documents": [doc("c/a")]}, "targetId": 1},
    "listen: denied document": {"documents": {"documents": [doc("c/b")]}, "targetId": 2},
    "listen: one denied of two": {"documents": {"documents": [doc("c/a"), doc("c/b")]}, "targetId": 3},
    "listen: denied query": {"query": {"parent": f"{db}/documents", "structuredQuery": {"from": [{"collectionId": "c"}]}}, "targetId": 4},
}.items():
    record(name, setup, "Listen", [{"database": db, "addTarget": target}])
record("write: denied", setup, "Write", [
    {"database": db}, {"streamToken": "", "writes": [{"update": {"name": doc("c/x"), "fields": {"n": {"integerValue": "2"}}}}]}])
verify = lambda d: {"verify": doc(d), "currentDocument": {"exists": True}}
record("write: an allowed verify", setup, "Write", [{"database": db}, {"streamToken": "", "writes": [verify("c/a")]}])
record("write: a denied verify", setup, "Write", [{"database": db}, {"streamToken": "", "writes": [verify("c/b")]}])
record("write: an allowed write and a denied verify", setup, "Write", [
    {"database": db}, {"streamToken": "", "writes": [
        {"update": {"name": doc("c/a"), "fields": {"n": {"integerValue": "1"}}}}, verify("c/b")]}])

setup = project("streams-b", "match /c/{d} {\n  allow get: if resource.data.n == 1;\n}", [("c/a", 1)])
db = setup[0]
record("listen: a write makes the document denied", setup, "Listen",
       [{"database": db, "addTarget": {"documents": {"documents": [f"{db}/documents/c/a"]}, "targetId": 1}}],
       during=("PATCH", f"/v1/{db}/documents/c/a", {"fields": {"n": {"integerValue": "2"}}}), wait=3)

json.dump(results, sys.stdout, indent=1)
print()
