#!/usr/bin/env python3
"""What does the official emulator do with -0.0? (#174)

Usage (from the repository root, `grpcurl` on PATH), against a freshly started jar:
    python3 -I tools/oracle/negative_zero.py 127.0.0.1:8086 > crates/hidane/tests/fixtures/negative_zero.json

Writes `-0.0` over gRPC (Commit, CreateDocument, UpdateDocument, the Write stream), REST
(PATCH, POST, `:commit`, in several spellings) and WebChannel (the browser SDK's Write stream),
at the top level, in maps and in arrays, and reads it back over each (Get, BatchGet, RunQuery,
Listen; GET, `:batchGet`, `:runQuery`; Listen). Then field transforms whose operands or stored
values are `-0.0`, queries that filter, order and page on it, aggregations over it, Security
Rules conditions that tell its sign (`string(-0.0) == '-0.0'`), and an export and import of it.

What it shows: the store keeps `-0.0`, and every read gives it back (REST and WebChannel print
`-0.0`). It is ProtoJSON input that has no negative zero: protobuf-java's `JsonFormat` reads a
double through `BigDecimal`, so `-0.0`, `-0`, `"-0"` (the web SDK's spelling) and `-0e0` are
read as `0.0`, while a literal that only rounds to zero (`-1e-400`) stays `-0.0`. That is also
why `value_order.py`, which writes over REST, stores its "double -0.0" as `0.0`. Otherwise
`-0.0` equals `0.0` and `0` in filters, orderings, cursors, array transforms and `maximum` /
`minimum` (which keep the stored value), adds as IEEE doubles in `increment`, and `sum` / `avg`
over it are `0.0`. Writing `-0.0` over `0.0` changes the document; writing NaN over NaN does
not (the stored values are compared bit for bit).

grpcurl leaves out a geo point's `-0.0` coordinate (it sends and prints only coordinates that
compare unequal to zero), so geo points are written over REST only.

Every answer is summarised the same way for each transport: a value keeps its kind, and a
double is spelled as text (`-0.0`, `0.0`, `NaN`), so the sign of zero survives the fixture. A
REST or WebChannel double is the literal the server printed; a gRPC double is the number
grpcurl printed (`-0` for -0.0). Errors keep only their code. `crates/hidane/tests/negative_zero.rs`
replays the steps against hidane and summarises its answers the same way.
"""

import http.client
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.parse

HOST = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1:8080"
PROTOS = "crates/hidane-proto/proto"
OWNER = "Bearer owner"

NZ = -0.0
i = lambda n: {"integerValue": str(n)}
d = lambda x: {"doubleValue": x}
nan = {"doubleValue": "NaN"}
arr = lambda *vs: {"arrayValue": {"values": list(vs)}}
m = lambda **f: {"mapValue": {"fields": f}}


def db(project):
    return f"projects/{project}/databases/(default)"


def docs(project):
    return f"{db(project)}/documents"


# Summaries ------------------------------------------------------------------------------------

def spell(x):
    """A double as text: the literal as printed, or a gRPC number in Python's spelling."""
    if isinstance(x, str):
        return x
    x = float(x)
    return "NaN" if x != x else repr(x)


def value(v):
    (kind, inner), = v.items()
    if kind == "doubleValue":
        return {"double": spell(inner)}
    if kind == "integerValue":
        return {"integer": str(inner)}
    if kind == "arrayValue":
        return {"array": [value(x) for x in inner.get("values", [])]}
    if kind == "mapValue":
        return {"map": fields(inner.get("fields", {}))}
    if kind == "geoPointValue":
        return {"geo": [spell(inner.get("latitude", 0.0)), spell(inner.get("longitude", 0.0))]}
    return {kind: inner}


def fields(f):
    return {name: value(v) for name, v in sorted(f.items())}


def document(doc, project):
    return {"id": doc["name"].removeprefix(docs(project) + "/"), "fields": fields(doc.get("fields", {}))}


def summary(kind, answer, project):
    """`answer`: the response messages of one call (a list), or an error code."""
    if isinstance(answer, str):
        return {"error": answer}
    if kind == "commit":
        (body,) = answer
        return {"writeResults": [{"transformResults": [value(v) for v in r.get("transformResults", [])],
                                  "changed": r.get("updateTime") == body["commitTime"]}
                                 for r in body.get("writeResults", [])]}
    if kind == "document":
        (body,) = answer
        return document(body, project)
    if kind == "batchGet":
        return {"found": [document(r["found"], project) for r in answer if "found" in r],
                "missing": [r["missing"].removeprefix(docs(project) + "/") for r in answer if "missing" in r]}
    if kind == "query":
        return {"documents": [document(r["document"], project)["id"] for r in answer if "document" in r]}
    if kind == "queryFields":
        return {"documents": [document(r["document"], project) for r in answer if "document" in r]}
    if kind == "aggregation":
        (row,) = [r for r in answer if "result" in r]
        return {"result": fields(row["result"]["aggregateFields"])}
    if kind == "listen":
        return {"documents": [document(r["documentChange"]["document"], project) for r in answer if "documentChange" in r]}
    if kind == "write":
        return {"writeResults": [{"transformResults": [value(v) for v in r.get("transformResults", [])]}
                                 for message in answer for r in message.get("writeResults", [])]}
    raise ValueError(kind)


# Transports -----------------------------------------------------------------------------------

def grpc_messages(text):
    """grpcurl's output: JSON messages, then an error. It prints -0.0 as `-0`."""
    decoder = json.JSONDecoder(parse_int=lambda s: -0.0 if s == "-0" else int(s))
    out, at = [], 0
    while True:
        while at < len(text) and text[at].isspace():
            at += 1
        if at == len(text) or text[at] != "{":
            break
        message, at = decoder.raw_decode(text, at)
        out.append(message)
    rest = text[at:]
    if rest.startswith("ERROR:"):
        code = rest.split("Code: ", 1)[1].split("\n", 1)[0]
        return re.sub(r"(?<!^)(?=[A-Z])", "_", code).upper()
    assert not rest.strip(), rest
    return out


def grpcurl(rpc, messages, database, owner=True, wait=None):
    args = ["grpcurl", "-plaintext", "-import-path", PROTOS, "-proto", "google/firestore/v1/firestore.proto",
            "-H", f"google-cloud-resource-prefix: {database}"]
    if owner:
        args += ["-H", f"authorization: {OWNER}"]
    args += ["-d", "@", HOST, f"google.firestore.v1.Firestore/{rpc}"]
    if not wait:
        out = subprocess.run(args, input=json.dumps(messages[0]), capture_output=True, text=True, timeout=60)
        return grpc_messages(out.stdout + out.stderr)
    # Streams: grpcurl sends the last message when its input closes, and Listen never ends.
    proc = subprocess.Popen(args, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    for message in messages:
        proc.stdin.write(json.dumps(message) + "\n")
        proc.stdin.flush()
        time.sleep(0.5)
    time.sleep(wait)
    proc.stdin.close()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
    return grpc_messages(proc.stdout.read())


DOUBLE_LITERAL = re.compile(r'("(?:doubleValue|latitude|longitude)": ?)(-?[0-9][0-9.eE+-]*)')


def literal_doubles(text):
    """Parses JSON with doubles as the literals printed, so `-0.0` and `0.0` stay apart."""
    return json.loads(DOUBLE_LITERAL.sub(r'\1"\2"', text))


def rest(method, path, raw=None, owner=True):
    c = http.client.HTTPConnection(*HOST.split(":"), timeout=30)
    headers = {"Content-Type": "application/json"}
    if owner:
        headers["Authorization"] = OWNER
    c.request(method, path, body=raw, headers=headers)
    r = c.getresponse()
    text = r.read().decode()
    if r.status != 200:
        return r.status, json.loads(text)["error"]["status"]
    body = literal_doubles(text)
    return r.status, body if isinstance(body, list) else [body]


WEBCHANNEL_HEADERS = "X-Goog-Api-Client:gl-js/ fire/13.0.0\r\nContent-Type:text/plain\r\nx-goog-api-key:demo\r\n"


def webchannel_frames(response, until, deadline):
    """`<length>\\n<JSON>` frames, until one contains `until` or the response ends."""
    buffer, frames = "", []
    while time.monotonic() < deadline:
        newline = buffer.find("\n")
        if newline >= 0:
            length = int(buffer[:newline])
            text, units, at = buffer[newline + 1:], 0, 0
            while at < len(text) and units < length:
                units += 2 if ord(text[at]) > 0xFFFF else 1
                at += 1
            if units == length:
                frames.append(text[:at])
                buffer = text[at:]
                if until in frames[-1]:
                    return frames
                continue
        data = response.read1(65536)
        if not data:
            break
        buffer += data.decode()
    return frames


def webchannel(rpc, database, messages):
    """Opens a session with the first message, sends the others on the forward channel, and
    returns the messages of the back channel (long polling) once it has sent `CURRENT` (Listen)
    or the answer to the last message (Write)."""
    path = f"/google.firestore.v1.Firestore/{rpc}/channel"
    query = lambda params: urllib.parse.urlencode(params, safe="()")
    c = http.client.HTTPConnection(*HOST.split(":"), timeout=30)
    c.request("POST", path + "?" + query({"VER": 8, "database": database, "RID": 1000, "CVER": 22,
                                          "X-HTTP-Session-Id": "gsessionid", "zx": "x", "t": 1}),
              body=urllib.parse.urlencode({"headers": WEBCHANNEL_HEADERS, "count": 1, "ofs": 0,
                                           "req0___data__": messages[0]}),
              headers={"Content-Type": "application/x-www-form-urlencoded"})
    (frame,) = webchannel_frames(c.getresponse(), '"c"', time.monotonic() + 10)
    sid = json.loads(frame)[0][1][1]
    for n, message in enumerate(messages[1:]):
        c = http.client.HTTPConnection(*HOST.split(":"), timeout=30)
        c.request("POST", path + "?" + query({"VER": 8, "database": database, "SID": sid, "RID": 1001 + n, "AID": 0,
                                              "zx": "x", "t": 1}),
                  body=urllib.parse.urlencode({"count": 1, "ofs": 1 + n, "req0___data__": message}),
                  headers={"Content-Type": "application/x-www-form-urlencoded"})
        c.getresponse().read()
    received, aid, deadline = [], 0, time.monotonic() + 10
    until = "CURRENT" if rpc == "Listen" else "writeResults"
    while time.monotonic() < deadline and not any(until in json.dumps(r) for r in received):
        c = http.client.HTTPConnection(*HOST.split(":"), timeout=30)
        c.request("GET", path + "?" + query({"VER": 8, "database": database, "RID": "rpc", "SID": sid, "AID": aid,
                                             "CI": 1, "TYPE": "xmlhttp", "zx": "x", "t": 1}))
        for frame in webchannel_frames(c.getresponse(), until, deadline):
            for n, (message,) in literal_doubles(frame):
                aid = n
                if message != "noop":
                    received.append(message)
    return received


steps = []


def grpc_step(name, project, rpc, request, kind, owner=True, stream=None):
    """`stream`: the messages of a Listen or Write stream, sent instead of `request`."""
    if stream is not None:
        answer = grpcurl(rpc, stream, db(project), owner, wait=2.0)
        if not isinstance(answer, str) and rpc == "Write":
            answer = answer[1:]  # the handshake
    else:
        answer = grpcurl(rpc, [request], db(project), owner)
    step = {"name": name, "project": project, "grpc": rpc}
    step.update({"stream": stream} if stream is not None else {"request": request})
    if not owner:
        step["owner"] = False
    step.update({"summary": kind, "outcome": summary(kind, answer, project)})
    steps.append(step)


def rest_step(name, project, method, path, raw, kind, owner=True):
    full = f"/v1/{docs(project)}{path}"
    status, answer = rest(method, full, raw, owner)
    step = {"name": name, "project": project, "rest": method, "path": path}
    if raw is not None:
        step["body"] = raw
    if not owner:
        step["owner"] = False
    step.update({"summary": kind, "status": status, "outcome": summary(kind, answer, project)})
    steps.append(step)


def webchannel_step(name, project, rpc, messages, kind):
    """`messages`: ProtoJSON text, as the web SDK sends it."""
    answer = webchannel(rpc, db(project), messages)
    if rpc == "Write":
        answer = answer[1:]  # the handshake
    steps.append({"name": name, "project": project, "webchannel": rpc, "messages": messages,
                  "summary": kind, "outcome": summary(kind, answer, project)})


def commit(project, *writes):
    return {"database": db(project), "writes": list(writes)}


def update(project, path, f, transforms=None, mask=None):
    w = {"update": {"name": f"{docs(project)}/{path}", "fields": f}}
    if mask is not None:
        w["updateMask"] = {"fieldPaths": mask}
    if transforms is not None:
        w["updateTransforms"] = transforms
    return w


def get(project, path):
    return {"name": f"{docs(project)}/{path}"}


def run_query(project, query):
    return {"parent": docs(project), "structuredQuery": query}


# Writes and reads -----------------------------------------------------------------------------

P = "nz-writes"
ALL = {"top": d(NZ), "pos": d(0.0), "int": i(0), "map": m(x=d(NZ), y=d(0.0)), "arr": arr(d(NZ), d(0.0), i(0)),
       "nested": m(arr=arr(d(NZ)), inner=m(z=d(NZ)))}
grpc_step("Commit -0.0 everywhere", P, "Commit", commit(P, update(P, "c/commit", ALL)), "commit")
grpc_step("GetDocument", P, "GetDocument", get(P, "c/commit"), "document")
grpc_step("Commit 0.0", P, "Commit", commit(P, update(P, "c/flip", {"v": d(0.0)})), "commit")
grpc_step("Commit 0.0 over 0.0", P, "Commit", commit(P, update(P, "c/flip", {"v": d(0.0)})), "commit")
grpc_step("Commit -0.0 over 0.0", P, "Commit", commit(P, update(P, "c/flip", {"v": d(NZ)})), "commit")
grpc_step("Commit -0.0 over -0.0", P, "Commit", commit(P, update(P, "c/flip", {"v": d(NZ)})), "commit")
grpc_step("Commit -0.0 in a map", P, "Commit", commit(P, update(P, "c/nested", {"m": m(x=d(NZ))})), "commit")
grpc_step("Commit 0.0 in a map over -0.0", P, "Commit", commit(P, update(P, "c/nested", {"m": m(x=d(0.0))})), "commit")
# A write is no change when the stored values are the same bit for bit: NaN is.
grpc_step("Commit NaN", P, "Commit", commit(P, update(P, "n/nan", {"v": nan, "arr": arr(nan)})), "commit")
grpc_step("Commit NaN over NaN", P, "Commit", commit(P, update(P, "n/nan", {"v": nan, "arr": arr(nan)})), "commit")
grpc_step("CreateDocument", P, "CreateDocument",
          {"parent": docs(P), "collectionId": "c", "documentId": "create", "document": {"fields": ALL}}, "document")
grpc_step("UpdateDocument", P, "UpdateDocument", {"document": {"name": f"{docs(P)}/c/update", "fields": ALL}}, "document")
grpc_step("UpdateDocument with a mask", P, "UpdateDocument",
          {"document": {"name": f"{docs(P)}/c/update", "fields": {"map": m(x=d(0.0), y=d(NZ))}},
           "updateMask": {"fieldPaths": ["map.y"]}}, "document")
grpc_step("Write stream", P, "Write", None, "write",
          stream=[{"database": db(P)}, {"streamToken": "", "writes": [update(P, "c/stream", ALL)]}])
grpc_step("GetDocument after the Write stream", P, "GetDocument", get(P, "c/stream"), "document")
grpc_step("BatchGetDocuments", P, "BatchGetDocuments",
          {"database": db(P), "documents": [f"{docs(P)}/c/commit", f"{docs(P)}/c/missing"]}, "batchGet")
grpc_step("RunQuery", P, "RunQuery", run_query(P, {"from": [{"collectionId": "c"}]}), "queryFields")
grpc_step("Listen", P, "Listen", None, "listen",
          stream=[{"database": db(P), "addTarget": {"documents": {"documents": [f"{docs(P)}/c/commit"]}, "targetId": 1}}])
rest_step("GET", P, "GET", "/c/commit", None, "document")
rest_step(":batchGet", P, "POST", ":batchGet", json.dumps({"documents": [f"{docs(P)}/c/commit"]}), "batchGet")
rest_step(":runQuery", P, "POST", ":runQuery",
          json.dumps({"structuredQuery": {"from": [{"collectionId": "c"}], "where": {"fieldFilter": {
              "field": {"fieldPath": "int"}, "op": "EQUAL", "value": i(0)}}}}), "queryFields")
webchannel_step("WebChannel Listen", P, "Listen", [json.dumps(
    {"database": db(P), "addTarget": {"documents": {"documents": [f"{docs(P)}/c/commit"]}, "targetId": 2}})], "listen")

# REST: how each spelling of a zero is read.
for label, literal in [("-0.0", "-0.0"), ("-0", "-0"), ("-0e0", "-0e0"), ("\"-0.0\"", "\"-0.0\""), ("\"-0\"", "\"-0\""),
                       ("0.0", "0.0"), ("-1e-400", "-1e-400"), ("\"-1e-400\"", "\"-1e-400\"")]:
    raw = ('{"fields": {"top": {"doubleValue": %s}, "map": {"mapValue": {"fields": {"x": {"doubleValue": %s}}}}, '
           '"arr": {"arrayValue": {"values": [{"doubleValue": %s}]}}, '
           '"geo": {"geoPointValue": {"latitude": %s, "longitude": 1.5}}}}') % ((literal,) * 4)
    rest_step(f"PATCH {label}", P, "PATCH", "/r/patch", raw, "document")
    grpc_step(f"GetDocument after PATCH {label}", P, "GetDocument", get(P, "r/patch"), "document")
rest_step("POST (create) -0.0", P, "POST", "/r?documentId=create", '{"fields": {"top": {"doubleValue": -0.0}}}', "document")
rest_step(":commit -0.0", P, "POST", ":commit", json.dumps({"writes": [update(P, "r/commit", {"top": d(NZ)})]}), "commit")
grpc_step("GetDocument after REST :commit", P, "GetDocument", get(P, "r/commit"), "document")

# WebChannel: the web SDK sends -0 as "-0".
webchannel_step("WebChannel Write", P, "Write", [
    json.dumps({"database": db(P)}),
    '{"streamToken": "MA==", "writes": [{"update": {"name": "%s/w/write", "fields": '
    '{"string": {"doubleValue": "-0"}, "number": {"doubleValue": -0.0}}}}]}' % docs(P)], "write")
grpc_step("GetDocument after the WebChannel Write", P, "GetDocument", get(P, "w/write"), "document")

# Field transforms (over gRPC, which keeps the operand's sign) -----------------------------------

P = "nz-transforms"
SEED = {
    "inc_nz_nz": d(NZ), "inc_nz_pz": d(NZ), "inc_pz_nz": d(0.0), "inc_iz_nz": i(0), "inc_nz_iz": d(NZ),
    "max_nz_pz": d(NZ), "max_pz_nz": d(0.0), "max_nz_iz": d(NZ), "max_iz_nz": i(0),
    "min_nz_pz": d(NZ), "min_pz_nz": d(0.0), "min_nz_iz": d(NZ), "min_iz_nz": i(0),
    "union_pz": arr(d(0.0)), "union_nz": arr(d(NZ)), "union_iz": arr(i(0)),
    "remove_pz": arr(d(NZ), d(0.0), i(0), i(1)), "remove_nz": arr(d(NZ), d(0.0), i(0), i(1)),
    "remove_iz": arr(d(NZ), d(0.0), i(0), i(1)),
}
TRANSFORMS = [
    {"fieldPath": "inc_missing", "increment": d(NZ)},
    {"fieldPath": "inc_nz_nz", "increment": d(NZ)},
    {"fieldPath": "inc_nz_pz", "increment": d(0.0)},
    {"fieldPath": "inc_pz_nz", "increment": d(NZ)},
    {"fieldPath": "inc_iz_nz", "increment": d(NZ)},
    {"fieldPath": "inc_nz_iz", "increment": i(0)},
    {"fieldPath": "max_missing", "maximum": d(NZ)},
    {"fieldPath": "max_nz_pz", "maximum": d(0.0)},
    {"fieldPath": "max_pz_nz", "maximum": d(NZ)},
    {"fieldPath": "max_nz_iz", "maximum": i(0)},
    {"fieldPath": "max_iz_nz", "maximum": d(NZ)},
    {"fieldPath": "min_missing", "minimum": d(NZ)},
    {"fieldPath": "min_nz_pz", "minimum": d(0.0)},
    {"fieldPath": "min_pz_nz", "minimum": d(NZ)},
    {"fieldPath": "min_nz_iz", "minimum": i(0)},
    {"fieldPath": "min_iz_nz", "minimum": d(NZ)},
    {"fieldPath": "union_missing", "appendMissingElements": {"values": [d(NZ), d(0.0), i(0)]}},
    {"fieldPath": "union_pz", "appendMissingElements": {"values": [d(NZ)]}},
    {"fieldPath": "union_nz", "appendMissingElements": {"values": [d(0.0)]}},
    {"fieldPath": "union_iz", "appendMissingElements": {"values": [d(NZ)]}},
    {"fieldPath": "remove_pz", "removeAllFromArray": {"values": [d(0.0)]}},
    {"fieldPath": "remove_nz", "removeAllFromArray": {"values": [d(NZ)]}},
    {"fieldPath": "remove_iz", "removeAllFromArray": {"values": [i(0)]}},
]
grpc_step("seed", P, "Commit", commit(P, update(P, "t/d", SEED)), "commit")
grpc_step("transforms", P, "Commit", commit(P, update(P, "t/d", {}, TRANSFORMS, mask=[])), "commit")
grpc_step("GetDocument after the transforms", P, "GetDocument", get(P, "t/d"), "document")
grpc_step("standalone transform: increment a missing document by -0.0", P, "Commit",
          commit(P, {"transform": {"document": f"{docs(P)}/t/standalone",
                                   "fieldTransforms": [{"fieldPath": "v", "increment": d(NZ)}]}}), "commit")
grpc_step("GetDocument after the standalone transform", P, "GetDocument", get(P, "t/standalone"), "document")
rest_step("REST :commit: increment and maximum of missing fields by -0.0", P, "POST", ":commit", json.dumps({"writes": [
    update(P, "t/rest", {}, [{"fieldPath": "v", "increment": d(NZ)}, {"fieldPath": "w", "maximum": d(NZ)}])]}), "commit")
grpc_step("GetDocument after the REST transforms", P, "GetDocument", get(P, "t/rest"), "document")

# Queries --------------------------------------------------------------------------------------

P = "nz-queries"
VALUES = [("a", d(NZ)), ("b", d(0.0)), ("c", i(0)), ("d", nan), ("e", d(-1.0)), ("f", d(1.0)), ("g", d(NZ))]
writes = [update(P, f"q/{n}", {"v": v}) for n, v in VALUES]
# The same values with the names reversed: equal values come back in name order.
writes += [update(P, f"r/{chr(ord('a') + len(VALUES) - 1 - k)}", {"v": v}) for k, (_, v) in enumerate(VALUES)]
writes += [update(P, f"arrays/{n}", {"v": arr(v)}) for n, v in VALUES[:3]]
writes += [update(P, f"maps/{n}", {"v": m(x=v)}) for n, v in VALUES[:3]]
writes += [update(P, "agg_nz/a", {"v": d(NZ)})]
writes += [update(P, f"agg_nz2/{n}", {"v": d(NZ)}) for n in "ab"]
writes += [update(P, "agg_mixed/a", {"v": d(NZ)}), update(P, "agg_mixed/b", {"v": d(0.0)})]
writes += [update(P, "agg_int/a", {"v": d(NZ)}), update(P, "agg_int/b", {"v": i(0)})]
grpc_step("seed", P, "Commit", commit(P, *writes), "commit")


def where(collection, op, v):
    return {"from": [{"collectionId": collection}],
            "where": {"fieldFilter": {"field": {"fieldPath": "v"}, "op": op, "value": v}}}


for label, op, v in [("== -0.0", "EQUAL", d(NZ)), ("== 0.0", "EQUAL", d(0.0)), ("== 0", "EQUAL", i(0)),
                     ("!= -0.0", "NOT_EQUAL", d(NZ)), ("!= 0", "NOT_EQUAL", i(0)),
                     ("< 0.0", "LESS_THAN", d(0.0)), ("<= -0.0", "LESS_THAN_OR_EQUAL", d(NZ)),
                     ("> -0.0", "GREATER_THAN", d(NZ)), (">= 0.0", "GREATER_THAN_OR_EQUAL", d(0.0)),
                     ("< 0", "LESS_THAN", i(0)),
                     ("in [-0.0]", "IN", arr(d(NZ))), ("not-in [-0.0]", "NOT_IN", arr(d(NZ))),
                     ("in [0.0]", "IN", arr(d(0.0))), ("in [-0.0, 0.0]", "IN", arr(d(NZ), d(0.0))),
                     ("not-in [-0.0, 0]", "NOT_IN", arr(d(NZ), i(0)))]:
    grpc_step(f"where v {label}", P, "RunQuery", run_query(P, where("q", op, v)), "query")
for label, op, v in [("array-contains -0.0", "ARRAY_CONTAINS", d(NZ)), ("array-contains 0.0", "ARRAY_CONTAINS", d(0.0)),
                     ("array-contains-any [-0.0]", "ARRAY_CONTAINS_ANY", arr(d(NZ))), ("== [-0.0]", "EQUAL", arr(d(NZ))),
                     ("== [0]", "EQUAL", arr(i(0)))]:
    grpc_step(f"arrays: where v {label}", P, "RunQuery", run_query(P, where("arrays", op, v)), "query")
for label, v in [("{x: -0.0}", m(x=d(NZ))), ("{x: 0}", m(x=i(0)))]:
    grpc_step(f"maps: where v == {label}", P, "RunQuery", run_query(P, where("maps", "EQUAL", v)), "query")

ASC = [{"field": {"fieldPath": "v"}, "direction": "ASCENDING"}]
DESC = [{"field": {"fieldPath": "v"}, "direction": "DESCENDING"}]
for collection in ["q", "r"]:
    grpc_step(f"{collection}: order by v", P, "RunQuery",
              run_query(P, {"from": [{"collectionId": collection}], "orderBy": ASC}), "query")
grpc_step("q: order by v descending", P, "RunQuery", run_query(P, {"from": [{"collectionId": "q"}], "orderBy": DESC}), "query")
for label, cursor in [("startAt -0.0", {"startAt": {"values": [d(NZ)], "before": True}}),
                      ("startAfter -0.0", {"startAt": {"values": [d(NZ)]}}),
                      ("startAfter 0.0", {"startAt": {"values": [d(0.0)]}}),
                      ("endBefore 0.0", {"endAt": {"values": [d(0.0)], "before": True}}),
                      ("endAt -0.0", {"endAt": {"values": [d(NZ)]}}),
                      ("startAt -0.0 and name b", {"startAt": {"values": [d(NZ), {"referenceValue": f"{docs(P)}/q/b"}], "before": True}})]:
    order = ASC + ([{"field": {"fieldPath": "__name__"}, "direction": "ASCENDING"}] if "name" in label else [])
    grpc_step(f"q: order by v, {label}", P, "RunQuery",
              run_query(P, {"from": [{"collectionId": "q"}], "orderBy": order, **cursor}), "query")

AGG = [{"alias": "sum", "sum": {"field": {"fieldPath": "v"}}}, {"alias": "avg", "avg": {"field": {"fieldPath": "v"}}},
       {"alias": "count", "count": {}}]
for collection in ["agg_nz", "agg_nz2", "agg_mixed", "agg_int"]:
    grpc_step(f"sum and avg over {collection}", P, "RunAggregationQuery",
              {"parent": docs(P), "structuredAggregationQuery": {"structuredQuery": {"from": [{"collectionId": collection}]},
                                                                 "aggregations": AGG}}, "aggregation")

# Security Rules -------------------------------------------------------------------------------

P = "nz-rules"
RULES = """rules_version = '2';
service cloud.firestore {
  match /databases/{database}/documents {
    match /c/{d} {
      allow read: if string(resource.data.v) == '-0.0';
      allow write: if string(request.resource.data.v) == '-0.0';
    }
  }
}
"""
c = http.client.HTTPConnection(*HOST.split(":"), timeout=30)
c.request("PUT", f"/emulator/v1/projects/{P}:securityRules",
          json.dumps({"rules": {"files": [{"name": "firestore.rules", "content": RULES}]}}), {"Content-Type": "application/json"})
steps.append({"name": "rules: set", "project": P, "rules": RULES, "status": c.getresponse().status})
grpc_step("rules: Commit -0.0", P, "Commit", commit(P, update(P, "c/grpc", {"v": d(NZ)})), "commit", owner=False)
grpc_step("rules: Commit 0.0", P, "Commit", commit(P, update(P, "c/pos", {"v": d(0.0)})), "commit", owner=False)
grpc_step("rules: Commit increment by -0.0", P, "Commit",
          commit(P, update(P, "c/inc", {}, [{"fieldPath": "v", "increment": d(NZ)}])), "commit", owner=False)
rest_step("rules: PATCH -0.0", P, "PATCH", "/c/rest", '{"fields": {"v": {"doubleValue": -0.0}}}', "document", owner=False)
grpc_step("rules: GetDocument of the stored -0.0", P, "GetDocument", get(P, "c/grpc"), "document", owner=False)
grpc_step("rules: Commit 0.0 as the owner", P, "Commit", commit(P, update(P, "c/owner", {"v": d(0.0)})), "commit")
grpc_step("rules: GetDocument of the stored 0.0", P, "GetDocument", get(P, "c/owner"), "document", owner=False)

# Export and import ----------------------------------------------------------------------------

P = "nz-export"
grpc_step("export: seed", P, "Commit",
          commit(P, update(P, "c/d", {"v": d(NZ), "arr": arr(d(NZ), d(0.0)), "map": m(x=d(NZ))})), "commit")
work = os.path.realpath(tempfile.mkdtemp())
try:
    c = http.client.HTTPConnection(*HOST.split(":"), timeout=30)
    c.request("POST", f"/emulator/v1/projects/{P}:export",
              json.dumps({"database": db(P), "export_directory": work, "export_name": "e"}), {"Content-Type": "application/json"})
    assert c.getresponse().status == 200
    output = open(f"{work}/e/all_namespaces/all_kinds/output-0", "rb").read()
    c = http.client.HTTPConnection(*HOST.split(":"), timeout=30)
    c.request("POST", "/emulator/v1/projects/nz-import:import",
              json.dumps({"database": db("nz-import"), "export_directory": f"{work}/e/e.overall_export_metadata"}),
              {"Content-Type": "application/json"})
    assert c.getresponse().status == 200
finally:
    shutil.rmtree(work)
# A double property is 8 bytes, little-endian: -0.0 is seven zero bytes and 0x80.
steps.append({"name": "export, then import into nz-import", "project": P, "export": "nz-import",
              "outcome": {"negative_zero_doubles": output.count(b"\x00" * 7 + b"\x80")}})
grpc_step("import: GetDocument", "nz-import", "GetDocument", get("nz-import", "c/d"), "document")

json.dump({
    "oracle": "cloud-firestore-emulator v1.22.0 (sha256 9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)",
    "generated_by": "tools/oracle/negative_zero.py",
    "steps": steps,
}, sys.stdout, ensure_ascii=False, indent=1)
print()
