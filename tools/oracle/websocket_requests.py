#!/usr/bin/env python3
"""What does the WebSocket feed on `--websocket_port` send, the one the Emulator UI's Firestore
"Requests" tab reads? (#69)

Usage (from the repository root), with a freshly started jar under an English locale, given a
WebSocket port, and `grpcurl` on `PATH`:
    (cd /tmp && java -Duser.language=en -Duser.country=US -jar …/cloud-firestore-emulator-v1.22.0.jar \\
        --host 127.0.0.1 --port 8086 --websocket_port 8087) &
    python3 -I tools/oracle/websocket_requests.py 127.0.0.1:8086 8087 \\
        > crates/hidane/tests/fixtures/websocket_requests.json

The feed is a WebSocket server at `ws://<host>:<websocket_port>/requests`. A client that
connects gets the backlog (the last records, oldest first) as one JSON-array text frame, then
one text frame per Security Rules evaluation. A monitor client connects first; each REST call
of a case records what it then received (`records`), and steps marked `ws` record handshakes
and frames of other clients. The cases:

- the handshake, and answers to requests that are not one;
- reads, lists, queries, writes, batches, transactions and Listen targets, allowed, denied and
  failing, with and without a token, by the owner (not recorded), and in a database without
  rules (the open rules are recorded);
- values of every type in `request.resource`, ruleset files and release keys;
- the backlog of a client that connects late (the last 50 records), after `PUT
  :securityRules` and `POST /reset` (neither clears it), with several clients at once;
- what a client may send: text (ignored), a ping (answered with a pong), a close (echoed);
- a client that never reads: the writes checked against rules wait for it (`stall`, last).

The official emulator evaluates a request in passes: first without the stored document
(`resource` is undefined), then, if that does not decide, with it, and each pass is a record
(firebase-tools #6252). Steps marked `differs` record such second passes, where hidane, which
evaluates once, records one decision (`docs/parity-exceptions.md`). Stdlib only, apart from
`grpcurl` for Listen.
"""

import base64
import json
import os
import socket
import struct
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import steps  # noqa: E402
from steps import OWNER, call, case, cases, dump  # noqa: E402

HOST = sys.argv[1]
WS_PORT = int(sys.argv[2])
WS_HOST = HOST.rsplit(":", 1)[0]
PROTOS = str(Path(__file__).resolve().parents[2] / "crates/hidane-proto/proto")
I = lambda n: {"integerValue": str(n)}
S = lambda s: {"stringValue": s}


def b64(obj):
    return base64.urlsafe_b64encode(json.dumps(obj, separators=(",", ":")).encode()).decode().rstrip("=")


# An unsigned token, its claims in an order no map would keep.
TOKEN = "Bearer " + b64({"alg": "none"}) + "." + b64({"zeta": 1, "sub": "u1", "alpha": True, "email": "e@x"}) + "."


class Client:
    """A WebSocket client: the handshake, then frames, unmasked from the server."""

    def __init__(self, path="/requests"):
        self.sock = socket.create_connection((WS_HOST, WS_PORT), timeout=10)
        key = base64.b64encode(os.urandom(16)).decode()
        self.sock.sendall((f"GET {path} HTTP/1.1\r\nHost: {WS_HOST}:{WS_PORT}\r\nUpgrade: websocket\r\n"
                           f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n").encode())
        self.buf = b""
        while b"\r\n\r\n" not in self.buf:
            chunk = self.sock.recv(65536)
            assert chunk, "closed during the handshake"
            self.buf += chunk
        head, _, self.buf = self.buf.partition(b"\r\n\r\n")
        lines = head.decode().split("\r\n")
        self.status = lines[0]
        self.headers = [line.split(": ", 1)[0] for line in lines[1:]]

    def _read(self, n):
        while len(self.buf) < n:
            chunk = self.sock.recv(1 << 20)
            if not chunk:
                raise EOFError
            self.buf += chunk
        out, self.buf = self.buf[:n], self.buf[n:]
        return out

    def _frame(self):
        b0, b1 = self._read(2)
        n = b1 & 127
        if n == 126:
            n = struct.unpack(">H", self._read(2))[0]
        elif n == 127:
            n = struct.unpack(">Q", self._read(8))[0]
        assert not b1 & 128, "a masked frame from the server"
        return {"fin": b0 >> 7, "opcode": b0 & 15, "data": self._read(n)}

    def frames(self, wait=0.6):
        """The frames received until none came for `wait` seconds: text as JSON, others as
        opcode and payload, and `"EOF"` when the server closed."""
        out = []
        self.sock.settimeout(wait)
        try:
            while True:
                f = self._frame()
                assert f["fin"], "a fragmented message"
                if f["opcode"] == 1:
                    out.append(json.loads(f["data"]))
                else:
                    out.append({"opcode": f["opcode"], "payload": f["data"].decode("latin-1")})
        except (socket.timeout, TimeoutError):
            pass
        except EOFError:
            out.append("EOF")
        return out

    def send(self, opcode, payload):
        data = payload.encode("latin-1")
        mask = os.urandom(4)
        assert len(data) < 126
        self.sock.sendall(bytes([0x80 | opcode, 0x80 | len(data)]) + mask
                          + bytes(b ^ mask[i % 4] for i, b in enumerate(data)))


MONITOR = None


def second_pass(record):
    """Whether `record` is of a pass that read the stored document: one with a `resource`, but for
    a list of a collection (whose `resource` holds what the filter says of its documents)."""
    context = record["rulesContext"]
    listed = context["method"] == "list" and context["path"].endswith("*")
    return not listed and context["resource"] != {"undefined": {}}


def rest(method, path, body=None, auth=None, wait=0.6, **step):
    """A REST call, with the records the monitor received after it."""
    call(method, path, body, auth=auth)
    recorded = cases[-1]["steps"][-1]
    recorded["records"] = MONITOR.frames(wait)
    if any(second_pass(r) for r in recorded["records"]):
        step.setdefault("differs", "second pass")
    recorded.update(step)
    return recorded


def ws(**step):
    cases[-1]["steps"].append({"ws": step.pop("action"), **step})


def ruleset(body):
    return ("rules_version = '2';\nservice cloud.firestore {\n  match /databases/{database}/documents {\n"
            + "\n".join("    " + line if line else "" for line in body.split("\n")) + "\n  }\n}\n")


def put(project, body, database=None, files=None, name="firestore.rules"):
    target = project if database is None else f"{project}/databases/{database}"
    files = files or [{"name": name, "content": ruleset(body)}]
    rest("PUT", f"/emulator/v1/projects/{target}:securityRules", {"rules": {"files": files}})


def docs(project, database="(default)"):
    return f"/v1/projects/{project}/databases/{database}/documents"


def name(project, path, database="(default)"):
    return f"projects/{project}/databases/{database}/documents/{path}"


def raw_handshake(request):
    """What the WebSocket port answers to `request` within a second (`null` for nothing)."""
    s = socket.create_connection((WS_HOST, WS_PORT), timeout=1)
    s.sendall(request.encode())
    out = b""
    try:
        while True:
            chunk = s.recv(65536)
            if not chunk:
                break
            out += chunk
    except (socket.timeout, TimeoutError):
        pass
    s.close()
    if not out:
        return None
    head, _, body = out.partition(b"\r\n\r\n")
    return {"status": head.decode().split("\r\n")[0], "body": body.decode()}


def handshake_cases():
    global MONITOR
    case("handshake")
    MONITOR = Client()
    ws(action="connect", client="monitor", status=MONITOR.status, headers=MONITOR.headers,
       frames=MONITOR.frames())
    upgrade = "Upgrade: websocket\r\nConnection: Upgrade\r\n"
    key = "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n"
    for label, request in [
        ("no upgrade", "GET /requests HTTP/1.1\r\nHost: x\r\n\r\n"),
        ("POST", f"POST /requests HTTP/1.1\r\nHost: x\r\n{upgrade}{key}Content-Length: 0\r\n\r\n"),
        ("no key", f"GET /requests HTTP/1.1\r\nHost: x\r\n{upgrade}Sec-WebSocket-Version: 13\r\n\r\n"),
        ("no Connection: Upgrade", f"GET /requests HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\n{key}\r\n"),
        ("another path", f"GET / HTTP/1.1\r\nHost: x\r\n{upgrade}{key}\r\n"),
        ("a query string", f"GET /requests?x=1 HTTP/1.1\r\nHost: x\r\n{upgrade}{key}\r\n"),
    ]:
        answer = raw_handshake(request)
        step = {"differs": "no answer"} if answer is None else {}
        ws(action="raw", label=label, request=request, answer=answer, **step)


def read_cases():
    p = "ws-reads"
    case("reads", project=p)
    put(p, """match /a/{d} {
  allow read: if d == 'one';
  allow get: if d == 'two';
  allow read: if
    d == 'three';
}
match /{path=**} {
  allow get: if path == 'x';
}
match /b/{d} {
  allow get;
}
match /e/{d} {
  allow get: if d == 'zero' && request.auth.uid == 'u1';
  allow get: if debug(d) == 'q' || debug(request.auth.uid) == 'u1';
}
match /n/{d} {
  allow list: if false;
}""")
    for d in ("one", "two", "three", "four"):
        rest("GET", f"{docs(p)}/a/{d}")
    rest("GET", f"{docs(p)}/b/x")
    rest("GET", f"{docs(p)}/e/q", auth=TOKEN)
    rest("GET", f"{docs(p)}/e/q")
    rest("GET", f"{docs(p)}/n/x")
    rest("GET", f"{docs(p)}/z/x")
    rest("PATCH", f"{docs(p)}/b/x", {"fields": {"n": I(1)}}, auth=OWNER)
    rest("GET", f"{docs(p)}/b/x")
    rest("GET", f"{docs(p)}/a/one", auth=OWNER)
    rest("POST", f"{docs(p)}:batchGet", {"documents": [name(p, "b/x"), name(p, "a/one"), name(p, "b/y")]})
    case("reads that read the document", project=p)
    put(p, """match /f/{d} {
  allow get: if resource.data.n == 1;
}
match /g/{d} {
  allow get: if false;
  allow get: if resource == null;
}""")
    rest("PATCH", f"{docs(p)}/f/x", {"fields": {"n": I(1)}}, auth=OWNER)
    rest("GET", f"{docs(p)}/f/x")
    rest("GET", f"{docs(p)}/f/missing")
    rest("GET", f"{docs(p)}/g/missing")


def list_cases():
    p = "ws-lists"
    case("lists", project=p)
    put(p, """match /k/{d} {
  allow list: if request.query.limit <= 5 || resource.data.n == 1;
}
match /e/{d} {
  allow list: if d == 'x';
}
match /{path=**}/g/{d} {
  allow list: if true;
}""")
    q = lambda **extra: {"structuredQuery": {"from": [{"collectionId": "k"}], **extra}}
    field = lambda f, op, v: {"fieldFilter": {"field": {"fieldPath": f}, "op": op, "value": v}}
    rest("POST", f"{docs(p)}:runQuery", q(limit=3))
    rest("POST", f"{docs(p)}:runQuery", q())
    rest("POST", f"{docs(p)}:runQuery", q(where=field("n", "EQUAL", I(1)),
                                          orderBy=[{"field": {"fieldPath": "n"}, "direction": "DESCENDING"}], offset=1))
    rest("POST", f"{docs(p)}:runQuery", q(where={"compositeFilter": {"op": "AND", "filters": [
        field("a.b", "EQUAL", S("x")), field("n", "GREATER_THAN", I(1)), field("t", "IN", {"arrayValue": {"values": [I(1)]}}),
        field("z", "ARRAY_CONTAINS", I(1)), {"unaryFilter": {"field": {"fieldPath": "u"}, "op": "IS_NULL"}}]}}, limit=2))
    rest("POST", f"{docs(p)}:runQuery", q(where={"compositeFilter": {"op": "OR", "filters": [
        field("n", "EQUAL", I(1)), field("n", "EQUAL", I(2))]}}))
    rest("POST", f"{docs(p)}:runQuery", q(where=field("n", "IN", {"arrayValue": {"values": [I(1), I(2)]}})))
    rest("POST", f"{docs(p)}:runQuery", q(where=field("__name__", "EQUAL", {"referenceValue": name(p, "k/a")})))
    rest("POST", f"{docs(p)}:runQuery", {"structuredQuery": {"from": [{"collectionId": "e"}]}})
    rest("POST", f"{docs(p)}:runQuery", {"structuredQuery": {"from": [{"collectionId": "g", "allDescendants": True}],
                                                              "select": {"fields": [{"fieldPath": "n"}]}}})
    rest("POST", f"{docs(p)}/k/a:runQuery", {"structuredQuery": {"from": [{"collectionId": "g"}]}})
    rest("POST", f"{docs(p)}/k/a:runQuery", {"structuredQuery": {"from": [{"collectionId": "g", "allDescendants": True}]}},
         differs="resource.__name__ below a document")
    rest("POST", f"{docs(p)}:runAggregationQuery", {"structuredAggregationQuery": {
        "structuredQuery": {"from": [{"collectionId": "k"}], "limit": 4}, "aggregations": [{"count": {}}]}})
    rest("GET", f"{docs(p)}/k?pageSize=2")
    rest("GET", f"{docs(p)}/k")
    rest("POST", f"{docs(p)}/k/a:listCollectionIds", {})


def write_cases():
    p = "ws-writes"
    base = name(p, "")[:-1]
    case("writes", project=p)
    put(p, """match /w/{d} {
  allow create: if request.resource.data.n == 1;
  allow update: if request.resource.data.n == 2;
  allow delete: if d != 'keep';
}
match /v/{d} {
  allow create: if true;
}""")
    upd = lambda d, n, **extra: {"update": {"name": f"{base}/{d}", "fields": {"n": I(n)}}, **extra}
    commit = lambda *writes, **step: rest("POST", f"{docs(p)}:commit", {"writes": list(writes)}, auth=TOKEN, **step)
    rest("POST", f"{docs(p)}/w?documentId=a", {"fields": {"n": I(1)}})
    rest("POST", f"{docs(p)}/w?documentId=b", {"fields": {"n": I(3)}}, auth=TOKEN)
    commit(upd("w/a", 2, currentDocument={"exists": True}))
    commit(upd("w/a", 1, currentDocument={"exists": True}))
    commit(upd("w/c", 1))
    commit(upd("w/c", 2))
    commit({"delete": f"{base}/w/c"})
    commit({"delete": f"{base}/w/keep"})
    commit(upd("w/d", 1, currentDocument={"exists": False}), upd("w/e", 1), {"delete": f"{base}/w/f"})
    commit(upd("w/g", 1, currentDocument={"exists": False}), upd("w/keep", 5, currentDocument={"exists": False}))
    rest("PATCH", f"{docs(p)}/w/d?updateMask.fieldPaths=n", {"fields": {"n": I(2)}})
    rest("DELETE", f"{docs(p)}/w/d")
    commit({"transform": {"document": f"{base}/w/e", "fieldTransforms": [{"fieldPath": "n", "increment": I(1)}]}})
    case("values", project=p)
    rest("PATCH", f"{docs(p)}/v/ref", {"fields": {}}, auth=OWNER)
    rest("POST", f"{docs(p)}/v?documentId=all", {"fields": {
        "null": {"nullValue": None}, "true": {"booleanValue": True}, "int": I(-12), "big": I(2**63 - 1),
        "double": {"doubleValue": 1.5}, "whole": {"doubleValue": 3.0}, "large": {"doubleValue": 1e100},
        "small": {"doubleValue": 1e-7}, "nan": {"doubleValue": "NaN"}, "inf": {"doubleValue": "-Infinity"},
        "time": {"timestampValue": "2024-01-02T03:04:05.123456789Z"}, "second": {"timestampValue": "2024-01-02T03:04:05Z"},
        "string": S("é\u0001\t<>&='\"\\/ 😀"), "empty": S(""), "bytes": {"bytesValue": "AP8="},
        "ref": {"referenceValue": name(p, "v/ref")}, "geo": {"geoPointValue": {"latitude": 1.5, "longitude": 0}},
        "array": {"arrayValue": {"values": [I(1), S("a"), {"mapValue": {}}]}}, "noarray": {"arrayValue": {}},
        "map": {"mapValue": {"fields": {"z": I(1), "a": {"mapValue": {}}}}}, "nomap": {"mapValue": {}},
    }})
    rest("POST", f"{docs(p)}/v", {"fields": {"n": I(1)}})


def transaction_cases():
    p = "ws-transactions"
    case("transactions", project=p)
    put(p, """match /t/{d} {
  allow read, write: if d != 'no';
}""")
    begin = call("POST", f"{docs(p)}:beginTransaction", {}, auth=TOKEN)
    cases[-1]["steps"][-1]["records"] = MONITOR.frames()
    tx = json.loads(begin[1])["transaction"]
    rest("POST", f"{docs(p)}:batchGet", {"transaction": tx, "documents": [name(p, "t/a"), name(p, "t/b")]}, auth=TOKEN,
         transaction=True)
    rest("POST", f"{docs(p)}:runQuery", {"transaction": tx, "structuredQuery": {"from": [{"collectionId": "t"}]}},
         auth=TOKEN, transaction=True)
    rest("POST", f"{docs(p)}:commit", {"transaction": tx, "writes": [
        {"update": {"name": name(p, "t/a"), "fields": {"n": I(1)}}, "currentDocument": {"exists": False}}]}, auth=TOKEN,
         transaction=True)


def listen(database, add, wait=1.5):
    """Opens a Listen stream (grpcurl) adding each target of `add` in turn; returns the stream."""
    proc = subprocess.Popen(
        ["grpcurl", "-plaintext", "-import-path", PROTOS, "-proto", "google/firestore/v1/firestore.proto",
         "-H", f"google-cloud-resource-prefix: {database}", "-d", "@", HOST, "google.firestore.v1.Firestore/Listen"],
        stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, text=True)
    for target in add:
        proc.stdin.write(json.dumps({"database": database, "addTarget": target}) + "\n")
        proc.stdin.flush()
        time.sleep(wait)
    return proc


def listen_cases():
    p = "ws-listen"
    database = f"projects/{p}/databases/(default)"
    case("listen", project=p)
    put(p, """match /l/{d} {
  allow get: if d != 'no';
  allow list: if true;
}""")
    targets = [
        {"documents": {"documents": [name(p, "l/a"), name(p, "l/b")]}, "targetId": 1},
        {"query": {"parent": f"{database}/documents", "structuredQuery": {"from": [{"collectionId": "l"}]}}, "targetId": 2},
    ]
    proc = listen(database, targets)
    cases[-1]["steps"].append({"listen": database, "targets": targets, "records": MONITOR.frames(1.0)})
    rest("PATCH", f"{docs(p)}/l/a", {"fields": {"n": I(1)}}, auth=OWNER, wait=1.5, differs="query targets checked again")
    rest("PATCH", f"{docs(p)}/m/a", {"fields": {"n": I(1)}}, auth=OWNER, wait=1.5)
    proc.stdin.close()
    proc.kill()
    proc.wait()


def database_cases():
    case("rulesets")
    rest("GET", f"{docs('ws-open')}/x/y")
    rest("POST", f"{docs('ws-open')}:runQuery", {"structuredQuery": {"from": [{"collectionId": "x"}]}})
    put("ws-rules", "match /x/{d} {\n  allow read: if true;\n}")
    rest("GET", f"{docs('ws-rules')}/x/y")
    put("ws-rules", "match /x/{d} {\n  allow read: if true;\n}", database="other")
    rest("GET", f"{docs('ws-rules', 'other')}/x/y")
    put("ws-rules", None, files=[
        {"name": "storage.rules", "content": "service firebase.storage {\n  match /b/{bucket}/o {\n    allow read;\n  }\n}\n"},
        {"name": "firestore.rules", "content": ruleset("match /x/{d} {\n  allow read: if d == 'y';\n}")},
    ])
    rest("GET", f"{docs('ws-rules')}/x/y")


def backlog_cases():
    p = "ws-backlog"
    case("backlog", project=p)
    put(p, "match /x/{d} {\n  allow get: if true;\n}")
    for i in range(55):
        rest("GET", f"{docs(p)}/x/{i}", wait=0.05)
    MONITOR.frames(0.5)
    late = Client()
    ws(action="connect", client="late", status=late.status, headers=late.headers, frames=late.frames())
    second = Client()
    ws(action="connect", client="second", status=second.status, headers=second.headers, frames=second.frames())
    rest("GET", f"{docs(p)}/x/after")
    ws(action="receive", client="late", frames=late.frames())
    ws(action="receive", client="second", frames=second.frames())
    put(p, "match /x/{d} {\n  allow get: if false;\n}")
    rest("POST", "/reset")
    third = Client()
    ws(action="connect", client="third", status=third.status, headers=third.headers, frames=third.frames())
    for c in (late, second, third):
        c.sock.close()


def client_cases():
    p = "ws-client"
    case("what a client sends", project=p)
    put(p, "match /x/{d} {\n  allow get: if true;\n}")
    MONITOR.send(1, "hello")
    ws(action="send", client="monitor", opcode=1, payload="hello", frames=MONITOR.frames())
    MONITOR.send(9, "ping")
    ws(action="send", client="monitor", opcode=9, payload="ping", frames=MONITOR.frames())
    rest("GET", f"{docs(p)}/x/y")
    MONITOR.send(8, "\x03\xe8bye")
    ws(action="send", client="monitor", opcode=8, payload="\x03\xe8bye", frames=MONITOR.frames(1.0))


def stall_cases():
    """A client that connects (with a small backlog) and never reads: do writes checked against
    rules wait for it? Each write is 256 KiB, sent until one has not answered after 5 s; then the
    client goes away. Last, because its records fill the backlog."""
    p = "ws-stall"
    case("a client that does not read", project=p)
    steps.rules(p, ruleset("match /x/{d} {\n  allow write: if true;\n}"))
    stalled = socket.socket()
    stalled.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 4096)
    stalled.connect((WS_HOST, WS_PORT))
    stalled.sendall(b"GET /requests HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
                    b"Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n")
    time.sleep(0.5)
    body = json.dumps({"fields": {"s": S("x" * 256 * 1024)}}).encode()
    answered, waited = 0, None
    for i in range(40):
        req = urllib.request.Request(f"http://{HOST}{docs(p)}/x/{i % 10}", data=body, method="PATCH",
                                     headers={"Content-Type": "application/json"})
        start = time.time()
        try:
            urllib.request.urlopen(req, timeout=5).read()
            answered += 1
        except (socket.timeout, TimeoutError, urllib.error.URLError):
            waited = round(time.time() - start, 1)
            break
    stalled.close()
    ws(action="stall", answered=answered, waited=waited, differs="holds up requests")


handshake_cases()
read_cases()
list_cases()
write_cases()
transaction_cases()
listen_cases()
database_cases()
backlog_cases()
client_cases()
stall_cases()
dump()
