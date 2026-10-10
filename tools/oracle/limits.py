"""Which document limits does the official emulator enforce, with which messages? (#121)

Usage:
    python3 -I tools/oracle/limits.py 127.0.0.1:8086 > crates/hidane/tests/fixtures/limits.json

Commits one document per case over REST and records the status and the error message. The
limits are Firestore's: 1,048,487 bytes for a string or bytes value, 1,500 bytes for a field
name (nested names count their dotted path from the top-level field, an array as `array`)
and for a collection or document ID, 20 levels of maps and arrays, 1 MiB for a document.
A problem inside a map is only "Property … contains an invalid nested entity."

The document-size boundary is the official emulator's own (its Datastore entity encoding);
hidane uses Firestore's storage size rules, a few tens of bytes apart, so the size cases stay
clear of the boundary. `crates/hidane/tests/limits.rs` replays every case against hidane.
"""

import base64
import http.client
import json
import sys

HOST = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1:8080"
DB = "projects/limits/databases/(default)"
LIMIT = 1_048_487

# Values are written as small specs, expanded the same way by the oracle and the test, so the
# fixture stays small: ["string", n, char?], ["bytes", n], ["int"], ["map", {key: spec}],
# ["array", [spec]], ["nest", levels, mixed]; a key or ID spec is ["repeat", char, n] or text.
S = lambda n, c="x": ["string", n, c]
B = lambda n: ["bytes", n]
M = lambda **f: ["map", f]
A = lambda *v: ["array", list(v)]
ONE = ["int"]
R = lambda c, n: ["repeat", c, n]


def keyed(n):
    return {"k" * n: ONE}


CASES = [
    # (name, document path segments below documents/, {field name or ["repeat", c, n]: spec})
    ("a string at the limit", ["c", "x"], {"s": S(LIMIT)}),
    ("a string over the limit", ["c", "x"], {"s": S(LIMIT + 1)}),
    ("bytes over the limit", ["c", "x"], {"b": B(LIMIT + 1)}),
    ("a multibyte string over the limit", ["c", "x"], {"s": S(LIMIT // 3 + 1, "€")}),
    ("a multibyte string at the limit", ["c", "x"], {"s": S(LIMIT // 3, "€")}),
    ("a long string in an array", ["c", "x"], {"a": A(S(LIMIT + 1))}),
    ("a long string in a map", ["c", "x"], {"m": M(s=S(LIMIT + 1))}),
    ("a string at the limit in a map", ["c", "x"], {"m": M(s=S(LIMIT))}),
    ("a long string in a map in an array", ["c", "x"], {"a": A(M(s=S(LIMIT + 1)))}),
    ("a long string in an array in a map", ["c", "x"], {"m": M(a=A(S(LIMIT + 1)))}),
    ("a field name of 1500 bytes", ["c", "x"], {"n" * 1500: ONE}),
    ("a field name of 1501 bytes", ["c", "x"], {"n" * 1501: ONE}),
    ("a nested name of 1500 bytes with its path", ["c", "x"], {"m": M(**keyed(1498))}),
    ("a nested name of 1501 bytes with its path", ["c", "x"], {"m": M(**keyed(1499))}),
    ("a deeper nested name of 1500 bytes", ["c", "x"], {"m": M(n=M(**keyed(1496)))}),
    ("a deeper nested name of 1501 bytes", ["c", "x"], {"m": M(n=M(**keyed(1497)))}),
    ("a name in an array of 1500 bytes", ["c", "x"], {"long": A(M(**keyed(1494)))}),
    ("a name in an array of 1501 bytes", ["c", "x"], {"long": A(M(**keyed(1495)))}),
    ("a name in an array in a map of 1500 bytes", ["c", "x"], {"m": M(a=A(M(**keyed(1492))))}),
    ("a name in an array in a map of 1501 bytes", ["c", "x"], {"m": M(a=A(M(**keyed(1493))))}),
    ("20 levels of maps", ["c", "x"], {"f": ["nest", 20, False]}),
    ("21 levels of maps", ["c", "x"], {"f": ["nest", 21, False]}),
    ("20 levels of maps and arrays", ["c", "x"], {"f": ["nest", 20, True]}),
    ("21 levels of maps and arrays", ["c", "x"], {"f": ["nest", 21, True]}),
    ("22 levels of maps and arrays", ["c", "x"], {"f": ["nest", 22, True]}),
    ("a collection ID of 1500 bytes", [R("c", 1500), "x"], {"a": ONE}),
    ("a collection ID of 1501 bytes", [R("c", 1501), "x"], {"a": ONE}),
    ("a document ID of 1500 bytes", ["c", R("d", 1500)], {"a": ONE}),
    ("a document ID of 1501 bytes", ["c", R("d", 1501)], {"a": ONE}),
    ("a long subcollection ID", ["c", "x", R("s", 1501), "y"], {"a": ONE}),
    ("a long name before a long value", ["c", "x"], {"n" * 1501: S(LIMIT + 1)}),
    ("a long ID before a long name", ["c", R("d", 1501)], {"n" * 1501: ONE}),
    ("a reserved nested name before a long value", ["c", "x"], {"m": M(__x__=S(LIMIT + 1))}),
    ("a document well under 1 MiB", ["c", "x"], {"a": S(500_000), "b": S(500_000)}),
    ("a document well over 1 MiB", ["c", "x"], {"a": S(600_000), "b": S(600_000)}),
    ("a document over 1 MiB in a map", ["c", "x"], {"m": M(a=S(600_000), b=S(600_000))}),
    ("a document over 1 MiB in an array", ["c", "x"], {"a": A(S(600_000), S(600_000))}),
]


def text(spec):
    return spec if isinstance(spec, str) else spec[1] * spec[2]


def value(spec):
    kind = spec[0]
    if kind == "string":
        return {"stringValue": spec[2] * spec[1]}
    if kind == "bytes":
        return {"bytesValue": base64.b64encode(b"x" * spec[1]).decode()}
    if kind == "int":
        return {"integerValue": "1"}
    if kind == "map":
        return {"mapValue": {"fields": {k: value(v) for k, v in spec[1].items()}}}
    if kind == "array":
        return {"arrayValue": {"values": [value(v) for v in spec[1]]}}
    if kind == "nest":
        v = {"integerValue": "1"}
        for i in range(spec[1]):
            v = {"arrayValue": {"values": [v]}} if spec[2] and i % 2 == 0 else {"mapValue": {"fields": {"m": v}}}
        return v
    raise ValueError(kind)


def commit(path, fields):
    c = http.client.HTTPConnection(*HOST.split(":"), timeout=300)
    name = f"{DB}/documents/" + "/".join(text(s) for s in path)
    body = {"writes": [{"update": {"name": name, "fields": {k: value(v) for k, v in fields.items()}}}]}
    c.request("POST", f"/v1/{DB}/documents:commit", body=json.dumps(body),
              headers={"Content-Type": "application/json", "Authorization": "Bearer owner"})
    r = c.getresponse()
    raw = r.read().decode()
    return {"status": r.status, "message": json.loads(raw)["error"]["message"] if r.status != 200 else ""}


fixture = {
    "oracle": "cloud-firestore-emulator v1.22.0 (sha256 9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)",
    "generated_by": "tools/oracle/limits.py",
    "cases": [{"name": n, "path": p, "fields": f, "outcome": commit(p, f)} for n, p, f in CASES],
}
json.dump(fixture, sys.stdout, ensure_ascii=False, indent=1)
print()
