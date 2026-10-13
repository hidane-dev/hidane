"""How does the official emulator deliver Firestore events to the Functions emulator? (#70)

Usage (from the repository root, `grpcurl` on `PATH`):
    python3 -I tools/oracle/function_events.py <cloud-firestore-emulator.jar> [java] [port] [functions port] \
        > crates/hidane/tests/fixtures/function_events.json

`firebase emulators:start` passes `--functions_emulator <host:port>` to the Firestore emulator,
and the Functions emulator registers each Firestore trigger with it
(`tools/oracle/function_triggers.py`). The Firestore emulator then sends an event for each
document a commit changes to every trigger that matches it, as `POST
/functions/projects/{p}/triggers/{key}` on the Functions emulator:

- v1 triggers (`providers/cloud.firestore/eventTypes/document.*`): a JSON body
  `{"data": {value, oldValue, updateMask}, "context": {eventId, timestamp, eventType, resource}}`;
- v2 triggers (`google.cloud.firestore.document.v1.*`): a binary-mode CloudEvent, its attributes
  in `ce-*` headers and its data a `google.events.cloud.firestore.v1.DocumentEventData`
  (googleapis/google-cloudevents), sent as `application/protobuf` in base64, which firebase-tools
  decodes.

The script starts the jar three times on `port` (default 8200), from a temporary directory and
under an English locale: with `--functions_emulator` pointing at a server of its own on
`functions port` (default 8201) that records every request it receives and answers `200` (a
trigger whose key holds `fail` gets a `500`, `missing` a `404`, `slow` its answer after a
second); without the flag; and with the flag pointing at a port nothing listens on
(`functions port` + 2). Against each, each case registers triggers in a project of its own as
firebase-tools does, then writes over REST, gRPC (Commit) and the Write stream, and records
for each step the answer, the requests the Functions emulator received (in order, with every
header and the body as sent; a v2 body also decoded, as ProtoJSON) and the `WARNING` lines the
jar printed. The host and port of the recording server read `{functions}`, the temporary
directory `{work}`.

`crates/hidane/tests/function_events.rs` replays the cases against hidane.
"""

import base64
import http.client
import http.server
import json
import os
import re
import struct
import subprocess
import sys
import tempfile
import threading
import time
from datetime import datetime, timezone

JAR = sys.argv[1]
JAVA = sys.argv[2] if len(sys.argv) > 2 else "java"
PORT = int(sys.argv[3]) if len(sys.argv) > 3 else 8200
FUNCTIONS_PORT = int(sys.argv[4]) if len(sys.argv) > 4 else 8201
HOST = f"127.0.0.1:{PORT}"
FUNCTIONS = f"127.0.0.1:{FUNCTIONS_PORT}"
NOWHERE = f"127.0.0.1:{FUNCTIONS_PORT + 2}"
PROTOS = "crates/hidane-proto/proto"
OWNER = {"Authorization": "Bearer owner"}

V1 = "providers/cloud.firestore/eventTypes/document."
V2 = "google.cloud.firestore.document.v1."


# --- The Functions emulator's side: record every request -----------------------------------------

class Recorder:
    def __init__(self):
        self.requests = []
        self.busy = 0
        self.last = time.monotonic()
        self.lock = threading.Lock()

    def settle(self, quiet=0.5, least=0.6):
        """Waits until no request has been in flight or arrived for `quiet` seconds."""
        start = time.monotonic()
        while True:
            time.sleep(0.05)
            with self.lock:
                idle = self.busy == 0 and time.monotonic() - self.last >= quiet
            if idle and time.monotonic() - start >= least:
                return

    def take(self):
        with self.lock:
            taken, self.requests = self.requests, []
        return taken


RECORDER = Recorder()


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_POST(self):
        with RECORDER.lock:
            RECORDER.busy += 1
            RECORDER.last = time.monotonic()
        try:
            length = int(self.headers.get("Content-Length") or 0)
            body = self.rfile.read(length)
            with RECORDER.lock:
                RECORDER.requests.append({"method": self.command, "path": self.path, "version": self.request_version,
                                          "headers": [[k, v] for k, v in self.headers.items()], "body": body})
            key = self.path.rsplit("/", 1)[-1]
            status, answer = 200, b"{}"
            if "slow" in key:
                time.sleep(1)
            if "fail" in key:
                status, answer = 500, b'{"error":"the function failed"}'
            if "missing" in key:
                status, answer = 404, b"Function missing does not exist"
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(answer)))
            self.end_headers()
            self.wfile.write(answer)
        finally:
            with RECORDER.lock:
                RECORDER.busy -= 1
                RECORDER.last = time.monotonic()

    do_PUT = do_GET = do_DELETE = do_PATCH = do_POST

    def log_message(self, *args):
        pass


# --- google.events.cloud.firestore.v1.DocumentEventData, as ProtoJSON ----------------------------

def varint(buf, i):
    shift = result = 0
    while True:
        b = buf[i]
        i += 1
        result |= (b & 0x7F) << shift
        if b < 0x80:
            return result, i
        shift += 7


def wire(buf):
    i, out = 0, []
    while i < len(buf):
        key, i = varint(buf, i)
        number, kind = key >> 3, key & 7
        if kind == 0:
            value, i = varint(buf, i)
        elif kind == 1:
            value, i = buf[i:i + 8], i + 8
        elif kind == 2:
            n, i = varint(buf, i)
            value, i = buf[i:i + n], i + n
        elif kind == 5:
            value, i = buf[i:i + 4], i + 4
        else:
            raise ValueError(f"wire type {kind}")
        out.append((number, value))
    return out


def signed(n):
    return n - (1 << 64) if n >= 1 << 63 else n


def timestamp(buf):
    seconds = nanos = 0
    for number, value in wire(buf):
        if number == 1:
            seconds = signed(value)
        elif number == 2:
            nanos = value
    text = datetime.fromtimestamp(seconds, timezone.utc).strftime("%Y-%m-%dT%H:%M:%S")
    if nanos:
        digits = f"{nanos:09d}"
        text += "." + (digits[:3] if digits.endswith("000000") else digits[:6] if digits.endswith("000") else digits)
    return text + "Z"


def double(raw):
    d = struct.unpack("<d", raw)[0]
    if d != d:
        return "NaN"
    if d in (float("inf"), float("-inf")):
        return "Infinity" if d > 0 else "-Infinity"
    return d


def value(buf):
    out = {}
    for number, raw in wire(buf):
        if number == 11:
            out = {"nullValue": None}
        elif number == 1:
            out = {"booleanValue": bool(raw)}
        elif number == 2:
            out = {"integerValue": str(signed(raw))}
        elif number == 3:
            out = {"doubleValue": double(raw)}
        elif number == 10:
            out = {"timestampValue": timestamp(raw)}
        elif number == 17:
            out = {"stringValue": raw.decode()}
        elif number == 18:
            out = {"bytesValue": base64.b64encode(raw).decode()}
        elif number == 5:
            out = {"referenceValue": raw.decode()}
        elif number == 8:
            point = {}
            for n, r in wire(raw):
                point["latitude" if n == 1 else "longitude"] = double(r)
            out = {"geoPointValue": point}
        elif number == 9:
            out = {"arrayValue": {"values": [value(r) for n, r in wire(raw) if n == 1]} if raw else {}}
        elif number == 6:
            entries = [entry(r) for n, r in wire(raw) if n == 1]
            out = {"mapValue": {"fields": dict(entries)} if entries else {}}
        else:
            raise ValueError(f"Value field {number}")
    return out


def entry(buf):
    key, val = "", {}
    for number, raw in wire(buf):
        if number == 1:
            key = raw.decode()
        elif number == 2:
            val = value(raw)
    return key, val


def document(buf):
    out, fields = {}, []
    for number, raw in wire(buf):
        if number == 1:
            out["name"] = raw.decode()
        elif number == 2:
            fields.append(entry(raw))
        elif number == 3:
            out["createTime"] = timestamp(raw)
        elif number == 4:
            out["updateTime"] = timestamp(raw)
    if fields:
        out = {"name": out.pop("name", ""), "fields": dict(fields), **out}
    return out


def event_data(buf):
    out = {}
    for number, raw in wire(buf):
        if number == 1:
            out["value"] = document(raw)
        elif number == 2:
            out["oldValue"] = document(raw)
        elif number == 3:
            out["updateMask"] = {"fieldPaths": [r.decode() for n, r in wire(raw) if n == 1]} if raw else {}
    return out


def delivery(request):
    body = request["body"]
    headers = [[k, v.replace(FUNCTIONS, "{functions}")] for k, v in request["headers"]]
    out = {"method": request["method"], "path": request["path"], "headers": headers, "body": body.decode()}
    content_type = dict((k.lower(), v) for k, v in headers).get("content-type", "")
    if content_type.startswith("application/protobuf"):
        out["event"] = event_data(base64.b64decode(body))
    elif content_type.startswith("application/json"):
        out["event"] = json.loads(body)
    return out


# --- The emulator's side ----------------------------------------------------------------------

def rest(method, path, body=None, headers=None):
    conn = http.client.HTTPConnection(HOST, timeout=15)
    conn.request(method, path, body=None if body is None else json.dumps(body).encode(),
                 headers={"Content-Type": "application/json", **(headers or {})})
    res = conn.getresponse()
    text = res.read().decode()
    try:
        answer = json.loads(text)
    except ValueError:
        answer = text
    return res.status, answer


def grpc_output(text):
    """grpcurl's messages, then `ERROR:\n  Code: …` for a status that is not OK."""
    decoder, out, at = json.JSONDecoder(), [], 0
    while True:
        while at < len(text) and text[at].isspace():
            at += 1
        if at == len(text) or text[at] != "{":
            break
        message, at = decoder.raw_decode(text, at)
        out.append(message)
    rest_ = text[at:]
    code = rest_.split("Code: ", 1)[1].split("\n", 1)[0] if rest_.startswith("ERROR:") else "OK"
    return out, code


def grpcurl(rpc, database, messages, headers=None):
    """`headers` replace the owner's credential (`{}`: none)."""
    sent = []
    for name, value in (OWNER if headers is None else headers).items():
        sent += ["-H", f"{name.lower()}: {value}"]
    args = ["grpcurl", "-plaintext", "-import-path", PROTOS, "-proto", "google/firestore/v1/firestore.proto",
            "-H", f"google-cloud-resource-prefix: {database}", *sent,
            "-d", "@", HOST, f"google.firestore.v1.Firestore/{rpc}"]
    proc = subprocess.run(args, input="\n".join(json.dumps(m) for m in messages), capture_output=True, text=True,
                          timeout=60)
    return grpc_output(proc.stdout + proc.stderr)


class Log:
    """What the jar's event publisher (`com.google.cloud.datastore.emulator.impl.events`) printed
    since `mark`: each record's `WARNING: ` line without that prefix, with the lines that
    continue it. Records of other loggers (such as a client closing a stream) are left out."""

    STAMP = re.compile(r"^[A-Z][a-z]{2} \d{1,2}, \d{4} \d{1,2}:\d{2}:\d{2} [AP]M (\S+)")
    EVENTS = "com.google.cloud.datastore.emulator.impl.events."

    def __init__(self, path):
        self.path = path
        self.at = 0

    def mark(self):
        self.at = os.path.getsize(self.path)

    def read(self):
        with open(self.path, "rb") as f:
            f.seek(self.at)
            printed = f.read().decode(errors="replace")
        self.at += len(printed.encode())
        lines, events, keep = [], False, False
        for line in printed.splitlines():
            stamp = self.STAMP.match(line)
            if stamp:
                events, keep = stamp.group(1).startswith(self.EVENTS), False
            elif line.startswith("WARNING: ") and events:
                keep = True
                lines.append(line[len("WARNING: "):])
            elif line.startswith(("INFO: ", "WARNING: ", "SEVERE: ")):
                keep = False
            elif keep and line:
                lines.append(line)
        return [line.replace(FUNCTIONS, "{functions}").replace(NOWHERE, "{functions}") for line in lines]


# --- Cases ------------------------------------------------------------------------------------

def base(project, database="(default)"):
    return f"/v1/projects/{project}/databases/{database}/documents"


def v1(project, key, event, resource, database="(default)"):
    """A v1 registration as firebase-tools sends it."""
    body = {"eventTrigger": {"eventType": V1 + event,
                             "resource": f"projects/{project}/databases/{database}/documents/{resource}",
                             "service": "firestore.googleapis.com"}}
    return ("register", "PUT", f"/emulator/v1/projects/{project}/triggers/{key}", body)


def v1_resource(project, key, event, resource):
    body = {"eventTrigger": {"eventType": V1 + event, "resource": resource, "service": "firestore.googleapis.com"}}
    return ("register", "PUT", f"/emulator/v1/projects/{project}/triggers/{key}", body)


def v2(project, key, event, document, match="PATH_PATTERN", database="(default)"):
    body = {"eventType": V2 + event, "database": database, "namespace": "(default)",
            "document": {"value": document, "matchType": match}}
    if database is None:
        del body["database"]
    return ("register", "POST", f"/emulator/v1/projects/{project}/eventarcTrigger?eventarcTriggerId={key}", body)


def patch(project, path, fields, query="", database="(default)", headers=None):
    return ("rest", "PATCH", f"{base(project, database)}/{path}{query}", {"fields": fields}, headers)


def delete(project, path, database="(default)"):
    return ("rest", "DELETE", f"{base(project, database)}/{path}", None, None)


def commit(project, *writes, transaction=False):
    body = {"writes": list(writes)}
    if transaction:
        body["transaction"] = "{transaction}"
    return ("rest", "POST", f"{base(project)}:commit", body, None)


def update(project, path, fields, mask=None, exists=None, transforms=None):
    w = {"update": {"name": f"projects/{project}/databases/(default)/documents/{path}", "fields": fields}}
    if mask is not None:
        w["updateMask"] = {"fieldPaths": mask}
    if exists is not None:
        w["currentDocument"] = {"exists": exists}
    if transforms:
        w["updateTransforms"] = transforms
    return w


def remove(project, path, exists=None):
    w = {"delete": f"projects/{project}/databases/(default)/documents/{path}"}
    if exists is not None:
        w["currentDocument"] = {"exists": exists}
    return w


def I(n):
    return {"integerValue": str(n)}


def S(s):
    return {"stringValue": s}


EVERY_VALUE = {
    "zeta": S("last letter first"),
    "null": {"nullValue": None},
    "true": {"booleanValue": True},
    "false": {"booleanValue": False},
    "int": I(42),
    "negative": I(-7),
    "big": I(9007199254740993),
    "min": I(-9223372036854775808),
    "double": {"doubleValue": 1.5},
    "whole": {"doubleValue": 2.0},
    "zero": {"doubleValue": 0.0},
    "tiny": {"doubleValue": 5e-324},
    "huge": {"doubleValue": 1e300},
    "nan": {"doubleValue": "NaN"},
    "inf": {"doubleValue": "Infinity"},
    "-inf": {"doubleValue": "-Infinity"},
    "time": {"timestampValue": "2024-02-29T12:34:56.789Z"},
    "time0": {"timestampValue": "1970-01-01T00:00:00Z"},
    "timeNanos": {"timestampValue": "2001-02-03T04:05:06.123456Z"},
    "string": S("héllo \"wörld\" 😀 <&>\n\t\u0001"),
    "empty": S(""),
    "bytes": {"bytesValue": "AAEC/w=="},
    "ref": {"referenceValue": "projects/demo-values/databases/(default)/documents/all/other"},
    "geo": {"geoPointValue": {"latitude": 35.68, "longitude": -139.76}},
    "geo0": {"geoPointValue": {}},
    "array": {"arrayValue": {"values": [I(1), S("two"), {"nullValue": None}, {"mapValue": {}}]}},
    "emptyArray": {"arrayValue": {}},
    "map": {"mapValue": {"fields": {"b": I(1), "a": {"mapValue": {"fields": {"deep": {"booleanValue": True}}}}}}},
    "emptyMap": {"mapValue": {}},
    "a.b": S("a dotted name"),
    "`q`": S("a quoted name"),
    "vector": {"mapValue": {"fields": {"__type__": S("__vector__"),
                                       "value": {"arrayValue": {"values": [{"doubleValue": 1.0}, {"doubleValue": -2.5}]}}}}},
}


def cases():
    P = "demo-types"
    keys = [("v2", "written"), ("v1", "create"), ("v2", "created.withAuthContext"), ("v1", "write"),
            ("v2", "created"), ("v1", "update"), ("v2", "updated"), ("v1", "delete"), ("v2", "deleted"),
            ("v2", "written.withAuthContext"), ("v2", "updated.withAuthContext"), ("v2", "deleted.withAuthContext")]
    registrations = []
    for version, event in keys:
        key = f"us-central1-{version}{event.replace('.withAuthContext', 'Auth')}"
        registrations.append(v1(P, key, event, "items/{id}") if version == "v1" else v2(P, key, event, "items/{id}"))
    yield "event types, v1 and v2", registrations + [
        patch(P, "items/a", {"n": I(1), "s": S("x")}),
        patch(P, "items/a", {"n": I(2), "s": S("x")}),
        patch(P, "items/a", {"n": I(2), "s": S("x")}),
        patch(P, "items/a", {"m": I(3)}, "?updateMask.fieldPaths=m&updateMask.fieldPaths=s"),
        delete(P, "items/a"),
        delete(P, "items/a"),
        ("rest", "POST", f"{base(P)}/items?documentId=b", {"fields": {"n": I(1)}}, None),
        patch(P, "other/a", {"n": I(1)}),
        patch(P, "items/a/sub/b", {"n": I(1)}),
        patch(P, "items/b", {}),
    ]

    P = "demo-values"
    yield "every value", [
        v1(P, "us-central1-v1", "write", "all/{id}"),
        v2(P, "us-central1-v2", "written", "all/{id}"),
        patch(P, "all/a", EVERY_VALUE),
        patch(P, "all/a", {"only": I(1)}),
        patch(P, "all/b", {"c": I(1), "b": I(2), "a": I(3)}),
        patch(P, "all/x%3Dy%27z%3C%26%3E", {"s": S("=' <&> \u2028 / \u007f")}),
        commit(P, update(P, "all/b", {"z": I(4), "a": I(5)}, mask=["z", "a", "b"])),
        commit(P, update(P, "all/b", {}, mask=[], transforms=[
            {"fieldPath": "t", "setToServerValue": "REQUEST_TIME"}, {"fieldPath": "c", "increment": I(10)},
            {"fieldPath": "arr", "appendMissingElements": {"values": [I(1)]}}])),
        delete(P, "all/a"),
    ]

    P = "demo-patterns"
    yield "paths and patterns", [
        v1(P, "v1-items", "write", "items/{id}"),
        v1(P, "v1-sub", "write", "items/{id}/sub/{sid}"),
        v1(P, "v1-any", "write", "{coll}/{id}"),
        v1(P, "v1-exact", "write", "items/a"),
        v1(P, "v1-deep", "write", "items/{id=**}"),
        v2(P, "v2-items", "written", "items/{id}"),
        v2(P, "v2-sub", "written", "items/{id}/sub/{sid}"),
        v2(P, "v2-any", "written", "{coll}/{id}"),
        v2(P, "v2-exact", "written", "items/a", match="EXACT"),
        v2(P, "v2-exact-pattern", "written", "items/{id}", match="EXACT"),
        v2(P, "v2-deep", "written", "items/{id=**}"),
        v2(P, "v2-glob", "written", "items/*"),
        v2(P, "v2-globs", "written", "items/**"),
        v2(P, "v2-leading-slash", "written", "/items/{id}"),
        patch(P, "items/a", {"n": I(1)}),
        patch(P, "items/b", {"n": I(1)}),
        patch(P, "items/a/sub/x", {"n": I(1)}),
        patch(P, "items/a/sub/x/deeper/y", {"n": I(1)}),
        patch(P, "other/a", {"n": I(1)}),
        patch(P, "items/%7Bid%7D", {"n": I(1)}),
    ]

    P = "demo-no-database"
    yield "a v2 trigger without a database", [
        v1(P, "v1", "write", "items/{id}"),
        v2(P, "v2-no-database", "written", "items/{id}", database=None),
        patch(P, "items/a", {"n": I(1)}),
        ("rest", "GET", f"{base(P)}/items/a", None, None),
    ]

    P = "demo-databases"
    yield "databases and projects", [
        v1(P, "v1-default", "write", "items/{id}"),
        v1(P, "v1-other", "write", "items/{id}", database="other-db"),
        v1_resource(P, "v1-elsewhere", "write", "projects/demo-elsewhere/databases/(default)/documents/items/{id}"),
        v2(P, "v2-default", "written", "items/{id}"),
        v2(P, "v2-other", "written", "items/{id}", database="other-db"),
        patch(P, "items/a", {"n": I(1)}),
        patch(P, "items/a", {"n": I(1)}, database="other-db"),
        patch("demo-elsewhere", "items/a", {"n": I(1)}),
    ]

    P = "demo-commits"
    c = lambda path: f"c/{path}"
    yield "batches, transactions and transports", [
        v1(P, "v1", "write", "c/{id}"),
        v2(P, "v2", "written", "c/{id}"),
        commit(P, update(P, c("c"), {"n": I(1)}), update(P, c("a"), {"n": I(1)}), remove(P, c("missing")),
               update(P, c("b"), {"n": I(1)})),
        commit(P, update(P, c("a"), {"n": I(2)}), remove(P, c("b")), update(P, c("d"), {"n": I(1)}, exists=False)),
        commit(P, update(P, c("a"), {"n": I(3)}), update(P, c("e"), {"n": I(1)}, exists=True)),
        commit(P, update(P, c("a"), {"n": I(4)}), update(P, c("a"), {"n": I(5)}, mask=["m"]),
               update(P, c("a"), {}, mask=[], transforms=[{"fieldPath": "k", "increment": I(1)}])),
        ("rest", "POST", f"{base(P)}:beginTransaction", {}, None),
        commit(P, update(P, c("t"), {"n": I(1)}), remove(P, c("c")), transaction=True),
        ("rest", "POST", f"{base(P)}:batchWrite",
         {"writes": [update(P, c("w1"), {"n": I(1)}), update(P, c("w2"), {"n": I(1)}, exists=True)]}, OWNER),
        ("grpc", "Commit", f"projects/{P}/databases/(default)",
         [{"database": f"projects/{P}/databases/(default)", "writes": [update(P, c("g"), {"n": I(1)}), remove(P, c("t"))]}]),
        ("grpc", "Write", f"projects/{P}/databases/(default)",
         [{"database": f"projects/{P}/databases/(default)"},
          {"streamToken": "", "writes": [update(P, c("s"), {"n": I(1)}), update(P, c("g"), {"n": I(2)})]}]),
        ("rest", "DELETE", f"/emulator/v1/projects/{P}/databases/(default)/documents/c", None, None),
        commit(P, update(P, c("x"), {"n": I(1)}), update(P, c("y"), {"n": I(1)})),
        ("rest", "DELETE", f"/emulator/v1/projects/{P}/databases/(default)/documents", None, None),
        commit(P, update(P, c("x"), {"n": I(1)}), update(P, c("nop"), {"n": I(1)})),
        commit(P, update(P, c("nop"), {"n": I(1)}), update(P, c("q"), {"n": I(1)}), remove(P, c("gone"))),
        ("rest", "POST", f"/emulator/v1/projects/{P}:export",
         {"database": f"projects/{P}/databases/(default)", "export_directory": "{work}/export", "export_name": "e"}, None),
        delete(P, c("x")),
        patch(P, c("q"), {"n": I(2)}),
        ("rest", "POST", f"/emulator/v1/projects/{P}:import",
         {"database": f"projects/{P}/databases/(default)", "export_directory": "{work}/export/e/e.overall_export_metadata"},
         None),
    ]

    P = "demo-auth"
    user = {"Authorization": "Bearer eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJzdWIiOiJ1c2VyMSIsInVzZXJfaWQiOiJ1c2VyMSIs"
                             "ImVtYWlsIjoidUBleGFtcGxlLmNvbSIsImZpcmViYXNlIjp7InNpZ25faW5fcHJvdmlkZXIiOiJwYXNzd29yZCJ9fQ."}
    rules = ("rules_version = '2';\nservice cloud.firestore {\n  match /databases/{database}/documents {\n"
             "    match /{document=**} {\n      allow read, write: if true;\n    }\n  }\n}\n")
    yield "callers", [
        ("rest", "PUT", f"/emulator/v1/projects/{P}:securityRules",
         {"rules": {"files": [{"name": "firestore.rules", "content": rules}]}}, None),
        v2(P, "v2-auth", "written.withAuthContext", "c/{id}"),
        v1(P, "v1", "write", "c/{id}"),
        patch(P, "c/owner", {"n": I(1)}, headers=OWNER),
        patch(P, "c/user", {"n": I(1)}, headers=user),
        patch(P, "c/anonymous", {"n": I(1)}),
        ("grpc", "Write", f"projects/{P}/databases/(default)",
         [{"database": f"projects/{P}/databases/(default)"},
          {"streamToken": "", "writes": [update(P, "c/stream", {"n": I(1)})]}]),
    ]

    # Rules that deny writes to `closed`: a denied write commits nothing and sends no event, over
    # each transport, in a batch with an allowed write and in a transaction; the owner's
    # credential skips the rules.
    P = "demo-denied"
    rules = ("rules_version = '2';\nservice cloud.firestore {\n  match /databases/{database}/documents {\n"
             "    match /open/{id} {\n      allow read, write: if true;\n    }\n"
             "    match /closed/{id} {\n      allow read: if true;\n      allow write: if false;\n    }\n  }\n}\n")
    database = f"projects/{P}/databases/(default)"
    yield "writes the rules deny", [
        ("rest", "PUT", f"/emulator/v1/projects/{P}:securityRules",
         {"rules": {"files": [{"name": "firestore.rules", "content": rules}]}}, None),
        v1(P, "v1", "write", "{coll}/{id}"),
        v2(P, "v2", "written", "{coll}/{id}"),
        v2(P, "v2-auth", "written.withAuthContext", "{coll}/{id}"),
        patch(P, "open/a", {"n": I(1)}),
        patch(P, "closed/a", {"n": I(1)}),
        patch(P, "closed/a", {"n": I(1)}, headers=user),
        commit(P, update(P, "open/b", {"n": I(1)}), update(P, "closed/b", {"n": I(1)})),
        ("rest", "GET", f"{base(P)}/open/b", None, None),
        ("grpc", "Commit", database,
         [{"database": database, "writes": [update(P, "closed/c", {"n": I(1)})]}], {}),
        ("grpc", "Commit", database,
         [{"database": database, "writes": [update(P, "open/c", {"n": I(1)}), update(P, "closed/c", {"n": I(1)})]}],
         {}),
        ("grpc", "Write", database,
         [{"database": database}, {"streamToken": "", "writes": [update(P, "closed/d", {"n": I(1)})]}], {}),
        ("rest", "POST", f"{base(P)}:beginTransaction", {}, None),
        commit(P, update(P, "open/e", {"n": I(1)}), update(P, "closed/e", {"n": I(1)}), transaction=True),
        ("rest", "GET", f"{base(P)}/open/e", None, None),
        patch(P, "closed/a", {"n": I(1)}, headers=OWNER),
        ("grpc", "Commit", database, [{"database": database, "writes": [update(P, "closed/c", {"n": I(1)})]}]),
        delete(P, "closed/a"),
        ("rest", "DELETE", f"{base(P)}/closed/a", None, OWNER),
    ]

    P = "demo-answers"
    yield "the Functions emulator's answers", [
        v1(P, "a-slow-v1", "write", "c/{id}"),
        v2(P, "b-fail-v2", "written", "c/{id}"),
        v1(P, "c-fail-v1", "write", "c/{id}"),
        v2(P, "d-missing-v2", "written", "c/{id}"),
        v1(P, "e-ok-v1", "write", "c/{id}"),
        patch(P, "c/a", {"n": I(1)}),
        commit(P, update(P, "c/b", {"n": I(1)}), update(P, "c/c", {"n": I(1)})),
    ]

    P = "demo-replaced"
    yield "registrations replaced", [
        v1(P, "v1", "create", "c/{id}"),
        v2(P, "v2", "created", "c/{id}"),
        patch(P, "c/a", {"n": I(1)}),
        v1(P, "v1", "delete", "d/{id}"),
        v2(P, "v2", "deleted", "d/{id}"),
        patch(P, "c/b", {"n": I(1)}),
        patch(P, "d/a", {"n": I(1)}),
        delete(P, "c/a"),
        delete(P, "d/a"),
    ]

    # `t0` and `t40` share a bucket of the map the official emulator orders deliveries by.
    P = "demo-tie"
    yield "order of triggers", [
        v1(P, "t0", "write", "c/{id}"),
        v1(P, "t40", "write", "c/{id}"),
        v1(P, "t1", "write", "c/{id}"),
        patch(P, "c/a", {"n": I(1)}),
        v1(P, "t0", "write", "c/{id}"),
        patch(P, "c/a", {"n": I(2)}),
        v1(P, "t2", "write", "c/{id}"),
        patch(P, "c/a", {"n": I(3)}),
    ]

    P = "demo-reset"
    yield "reset", [
        v1(P, "v1", "write", "c/{id}"),
        v2(P, "v2", "written", "c/{id}"),
        patch(P, "c/a", {"n": I(1)}),
        ("rest", "POST", "/reset", None, None),
        patch(P, "c/a", {"n": I(2)}),
    ]


def unconfigured_cases():
    P = "demo-unconfigured"
    yield "no Functions emulator", [
        v1(P, "us-central1-v1", "write", "c/{id}"),
        v2(P, "us-central1-v2", "written", "c/{id}"),
        v2(P, "us-central1-v2auth", "created.withAuthContext", "c/{id}"),
        patch(P, "c/a", {"n": I(1)}),
        patch(P, "c/a", {"n": I(2)}),
        patch(P, "d/a", {"n": I(1)}),
        delete(P, "c/a"),
    ]


# --- Running ----------------------------------------------------------------------------------

def run_step(step, work, log, state):
    kind = step[0]
    log.mark()
    started = time.monotonic()
    if kind in ("register", "rest"):
        _, method, path, body, *rest_ = step
        headers = OWNER if kind == "register" else (rest_[0] if rest_ else None)
        body = json.loads(json.dumps(body).replace("{work}", work).replace("{transaction}", state.get("transaction", "")))
        status, answer = rest(method, path, body, headers)
        if isinstance(answer, dict) and "transaction" in answer:
            state["transaction"] = answer["transaction"]
        recorded = {"kind": "rest", "method": method, "path": path, "headers": headers or {},
                    "body": json.loads(json.dumps(body).replace(work, "{work}")) if body is not None else None,
                    "status": status}
        if status != 200:
            recorded["response"] = json.loads(json.dumps(answer).replace(work, "{work}"))
    else:
        _, rpc, database, messages, *rest_ = step
        headers = rest_[0] if rest_ else None
        answers, code = grpcurl(rpc, database, messages, headers)
        recorded = {"kind": "grpc", "rpc": rpc, "database": database, "messages": messages, "code": code}
        if headers is not None:
            recorded["headers"] = headers
        if code != "OK":
            recorded["answers"] = answers
    took = time.monotonic() - started
    RECORDER.settle()
    recorded["deliveries"] = [delivery(r) for r in RECORDER.take()]
    printed = log.read()
    if printed:
        recorded["log"] = printed
    recorded["answered_within_a_second"] = took < 1
    return recorded


def startup(log_path):
    """What the jar printed until it was ready, without the JVM's and Netty's notices."""
    with open(log_path, "rb") as f:
        printed = f.read().decode(errors="replace")
    lines = []
    for line in printed.split("Dev App Server is now running.")[0].splitlines():
        if line.startswith("WARNING: ") and ("sun.misc.Unsafe" in line or "Netty" in line or "netty" in line):
            continue
        if Log.STAMP.match(line) or line.startswith("INFO: Started WebSocket server"):
            continue
        lines.append(line.replace(f":{PORT}", ":{port}"))
    return lines + ["Dev App Server is now running."]


def run(name, flags, case_list, work):
    log_path = os.path.join(work, f"{name}.out")
    with open(log_path, "wb") as out:
        proc = subprocess.Popen(
            [JAVA, "-Duser.language=en", "-Duser.country=US", "-jar", JAR, "--host", "127.0.0.1",
             "--port", str(PORT), *flags], cwd=work, stdout=out, stderr=subprocess.STDOUT)
    try:
        for _ in range(240):
            try:
                if rest("GET", "/")[0] == 200:
                    break
            except OSError:
                time.sleep(0.25)
        else:
            raise SystemExit("the emulator did not start")
        log = Log(log_path)
        os.makedirs(os.path.join(work, "export"), exist_ok=True)
        recorded = []
        for case_name, steps in case_list:
            state = {}
            recorded.append({"name": case_name, "steps": [run_step(s, work, log, state) for s in steps]})
        shown = [f.replace(FUNCTIONS, "{functions}").replace(NOWHERE, "{functions}") for f in flags]
        return {"name": name, "flags": shown, "startup": startup(log_path), "cases": recorded}
    finally:
        try:
            rest("POST", "/shutdown")
        except OSError:
            pass
        try:
            proc.wait(timeout=30)
        except subprocess.TimeoutExpired:
            proc.kill()


def main():
    server = http.server.ThreadingHTTPServer(("127.0.0.1", FUNCTIONS_PORT), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        with tempfile.TemporaryDirectory() as work:
            work = os.path.realpath(work)
            runs = [
                run("functions", ["--functions_emulator", FUNCTIONS], list(cases()), work),
                run("unconfigured", [], list(unconfigured_cases()), work),
                run("unreachable", ["--functions_emulator", NOWHERE], list(unconfigured_cases()), work),
            ]
    finally:
        server.shutdown()
    fixture = {
        "oracle": "cloud-firestore-emulator v1.22.0 (sha256 9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)",
        "generated_by": "tools/oracle/function_events.py",
        "runs": runs,
    }
    compact = lambda v: json.dumps(v, ensure_ascii=False, separators=(",", ":"))
    print("{")
    for key in ("oracle", "generated_by"):
        print(f" {compact(key)}: {compact(fixture[key])},")
    print(' "runs": [')
    for i, r in enumerate(runs):
        print(f'  {{"name": {compact(r["name"])}, "flags": {compact(r["flags"])}, "startup": {compact(r["startup"])}, "cases": [')
        for j, c in enumerate(r["cases"]):
            print(f'   {{"name": {compact(c["name"])}, "steps": [')
            print(",\n".join("    " + compact(s) for s in c["steps"]))
            print("   ]}" + ("," if j + 1 < len(r["cases"]) else ""))
        print("  ]}" + ("," if i + 1 < len(runs) else ""))
    print(" ]")
    print("}")


main()
