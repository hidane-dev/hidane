#!/usr/bin/env python3
"""How does the official emulator in Datastore mode answer REST requests in protobuf? (#81)

Usage (from the repository root), with a jar freshly started with `--database-mode
datastore-mode` under an English locale:
    python3 -I tools/oracle/datastore_protobuf.py 127.0.0.1:8086 \\
        > crates/hidane/tests/fixtures/datastore_protobuf.json

The Java and Python client libraries of Datastore send REST requests over HTTP as protobuf
(`Content-Type: application/x-protobuf`). This sends `POST /v1/projects/{project}:{verb}` with
each `Content-Type` (which bodies are read as protobuf, which as JSON, which are not served),
then protobuf requests to each verb (bodies written here with a small protobuf writer, field
numbers from `google/datastore/v1/datastore.proto`): answers, and errors as a `google.rpc.Status`.
Each request and answer body is base64; `compare` is `exact` where the answer holds no time or
version, `status` (status and content type) where it does. The project is fixed
(`demo-protobuf`): a protobuf answer cannot be masked without changing its length. A fresh jar
gives the same IDs. `crates/hidane/tests/datastore.rs` replays it. Stdlib only.
"""

import base64
import http.client
import json
import sys

HOST = sys.argv[1]
PROJECT = "demo-protobuf"
PATH = f"/v1/projects/{PROJECT}"
PROTOBUF = {"Content-Type": "application/x-protobuf"}


def varint(n):
    n &= (1 << 64) - 1
    out = bytearray()
    while True:
        b = n & 0x7F
        n >>= 7
        if n:
            out.append(b | 0x80)
        else:
            out.append(b)
            return bytes(out)


def field(number, value):
    """One field: an int as a varint, bytes or str or a message (bytes) length-delimited."""
    if isinstance(value, bool) or isinstance(value, int):
        return varint(number << 3) + varint(int(value))
    if isinstance(value, str):
        value = value.encode()
    return varint(number << 3 | 2) + varint(len(value)) + value


def message(*fields):
    return b"".join(field(n, v) for n, v in fields if v is not None)


def key(*path, namespace=None):
    elements = []
    for kind, ident in path:
        if ident is None:
            elements.append((2, message((1, kind))))
        elif isinstance(ident, str):
            elements.append((2, message((1, kind), (3, ident))))
        else:
            elements.append((2, message((1, kind), (2, ident))))
    partition = (1, message((4, namespace))) if namespace else (1, None)
    return message(partition, *elements)


def entity(k, **properties):
    """An entity whose properties are integers (Value.integer_value, 2)."""
    entries = [(3, message((1, name), (2, message((2, v))))) for name, v in properties.items()]
    return message((1, k), *entries)


CASES = []


def case(name, verb, body, headers=PROTOBUF, compare="exact", method="POST", path=None):
    CASES.append({"name": name, "method": method, "path": path or f"{PATH}:{verb}", "headers": headers,
                  "body": base64.b64encode(body).decode(), "compare": compare})


# Query.kind (3) of a KindExpression named T.
KIND_T = message((3, message((1, "T"))))
JSON_RESERVE = json.dumps({"keys": [{"path": [{"kind": "T", "id": "5"}]}]}).encode()
RESERVE = message((1, key(("T", 5))))

# --- which Content-Type is read how ------------------------------------------------------------
for ct in ["application/x-protobuf", "application/json", "application/json; charset=utf-8",
           "application/jsonx", "application/json+protobuf", "text/plain", "text/plain; charset=utf-8",
           None, "", "application/x-protobuf; charset=utf-8", "APPLICATION/X-PROTOBUF",
           "application/JSON", "application/octet-stream", "application/protobuf",
           "application/x-protobuffer", "text/x-protobuf", "application/xml", "text/html",
           "application/x-www-form-urlencoded", "multipart/form-data", "*/*", "x"]:
    headers = {} if ct is None else {"Content-Type": ct}
    body = RESERVE if ct == "application/x-protobuf" else JSON_RESERVE
    case(f"reserveIds, Content-Type {ct!r}", "reserveIds", body, headers)
case("reserveIds, a JSON body as protobuf", "reserveIds", JSON_RESERVE)
case("reserveIds, a protobuf body as JSON", "reserveIds", RESERVE, {"Content-Type": "application/json"})
case("reserveIds, protobuf, Accept JSON", "reserveIds", RESERVE, {**PROTOBUF, "Accept": "application/json"})
case("reserveIds, protobuf with ?alt=json", "reserveIds", RESERVE, path=f"{PATH}:reserveIds?alt=json")
case("an unknown verb in protobuf", "unknownVerb", RESERVE)
case("GET lookup in protobuf", "lookup", b"", method="GET")
case("a path with a slash in protobuf", "lookup", b"", path=f"{PATH}/x:lookup")

# --- each verb --------------------------------------------------------------------------------
case("reserveIds", "reserveIds", RESERVE)
case("reserveIds without keys", "reserveIds", b"")
case("allocateIds, two", "allocateIds", message((1, key(("T", None))), (1, key(("T", None), namespace="ns1"))))
case("allocateIds, a complete key", "allocateIds", message((1, key(("T", 1)))))
case("lookup, empty", "lookup", b"", compare="status")
case("lookup, missing", "lookup", message((3, key(("T", "a")))), compare="status")
case("lookup, a key without a path", "lookup", message((3, b"")))
case("lookup, an ID 0", "lookup", message((3, key(("T", 0)))))
case("commit, upsert", "commit",
     message((5, 2), (6, message((6, entity(key(("T", "a")), n=1))))), compare="status")
case("lookup, found", "lookup", message((3, key(("T", "a")))), compare="status")
case("commit, insert an existing entity", "commit",
     message((5, 2), (6, message((4, entity(key(("T", "a")), n=2))))))
case("commit, a reserved kind", "commit",
     message((5, 2), (6, message((6, entity(key(("__x__", "a")), n=1))))))
case("commit, no mode", "commit", message((6, message((6, entity(key(("T", "b")), n=1))))))
case("commit, a transaction that is not ours", "commit", message((5, 1), (1, b"\x01\x02")))
case("beginTransaction", "beginTransaction", b"", compare="status")
case("rollback, a transaction that is not ours", "rollback", message((1, b"\x01\x02")))
case("rollback, no transaction", "rollback", b"")
case("runQuery, kind T", "runQuery", message((3, KIND_T)), compare="status")
case("runQuery, GQL", "runQuery",
     message((7, message((1, "SELECT * FROM T"), (2, True)))), compare="status")
case("runQuery, a GQL syntax error", "runQuery", message((7, message((1, "SELECT * FROM"), (2, True)))))
case("runQuery, neither query", "runQuery", b"")
case("runAggregationQuery, no aggregations", "runAggregationQuery", message((3, message((1, KIND_T)))))
case("runAggregationQuery, count", "runAggregationQuery",
     message((3, message((1, KIND_T), (3, message((1, b""), (7, "n")))))), compare="status")
case("a body that is not protobuf", "lookup", b"\xff\xff\xff")
case("a string that is not UTF-8", "lookup", message((3, message((2, message((1, b"\xff")))))))


def call(c):
    conn = http.client.HTTPConnection(*HOST.split(":"), timeout=60)
    body = base64.b64decode(c["body"])
    conn.putrequest(c["method"], c["path"])
    for k, v in c["headers"].items():
        conn.putheader(k, v)
    if c["method"] == "POST":
        conn.putheader("Content-Length", str(len(body)))
    conn.endheaders()
    if c["method"] == "POST":
        conn.send(body)
    r = conn.getresponse()
    return {"status": r.status, "contentType": r.getheader("content-type"),
            "body": base64.b64encode(r.read()).decode()}


if __name__ == "__main__":
    cases = [dict(c, outcome=call(c)) for c in CASES]
    for c in cases:
        if c["compare"] == "status":
            c["outcome"]["body"] = None
    json.dump({
        "oracle": "cloud-firestore-emulator v1.22.0 (sha256 9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)",
        "generated_by": "tools/oracle/datastore_protobuf.py",
        "mode": "datastore-mode",
        "cases": cases,
    }, sys.stdout, ensure_ascii=False, indent=1)
    print()
