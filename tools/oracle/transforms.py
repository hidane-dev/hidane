#!/usr/bin/env python3
"""Record how the official emulator applies field transforms (#23).

Usage: python3 -I tools/oracle/transforms.py HOST:PORT > fixture.json

Every case writes through REST `:commit` with `updateTransforms` (or a standalone transform
write) and reads the document back, so the fixture holds both the transform results and the
stored document. Stdlib only.
"""

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from harness import case, cases, commit, update  # noqa: E402

i = lambda n: {"integerValue": str(n)}
d = lambda x: {"doubleValue": x}
s = lambda v: {"stringValue": v}
null = {"nullValue": None}
nan = {"doubleValue": "NaN"}
arr = lambda *vs: {"arrayValue": {"values": list(vs)}}
m = lambda **f: {"mapValue": {"fields": f}}

server_time = lambda p: {"fieldPath": p, "setToServerValue": "REQUEST_TIME"}
inc = lambda p, v: {"fieldPath": p, "increment": v}
maximum = lambda p, v: {"fieldPath": p, "maximum": v}
minimum = lambda p, v: {"fieldPath": p, "minimum": v}
union = lambda p, *vs: {"fieldPath": p, "appendMissingElements": {"values": list(vs)}}
remove = lambda p, *vs: {"fieldPath": p, "removeAllFromArray": {"values": list(vs)}}


def write(name, fields=None, transforms=(), mask=None, exists=None):
    w = update(name, fields or {}, mask=mask, exists=exists)
    w["updateTransforms"] = list(transforms)
    return w


def transform_only(name, *transforms):
    return {"transform": {"document": "{base}/" + name, "fieldTransforms": list(transforms)}}


get = lambda name: ("GET", "{base}/" + name, None)
seed = lambda name, **fields: commit(update(name, fields))

case("server time", "t-server-time", [
    commit(write("c/d", {"a": i(1)}, [server_time("b"), server_time("m.at")]),
           write("c/e", {}, [server_time("b")])),
    get("c/d"),
])
case("increment", "t-increment", [
    seed("c/d", i5=i(5), dbl=d(1.5), text=s("x"), big=i(9223372036854775807), small=i(-9223372036854775808)),
    commit(write("c/d", {}, [
        inc("missing", i(5)), inc("i5", i(3)), inc("dbl", i(2)), inc("text", i(7)),
        inc("big", i(1)), inc("small", i(-1)), inc("mixed", d(0.5)),
    ], mask=[])),
    get("c/d"),
])
case("increment int by double", "t-increment-mixed", [
    seed("c/d", n=i(5)),
    commit(write("c/d", {}, [inc("n", d(2.5))], mask=[])),
    get("c/d"),
])
case("maximum and minimum", "t-minmax", [
    seed("c/d", a=i(3), b=i(3), c=i(3), e=s("x"), z=d(-0.0), n=i(1), lo=i(3), lo2=i(3)),
    commit(write("c/d", {}, [
        maximum("a", i(5)), maximum("b", d(3.0)), maximum("c", d(2.5)), maximum("missing", i(7)),
        maximum("e", i(1)), maximum("z", d(0.0)), maximum("n", nan),
        minimum("lo", d(2.5)), minimum("lo2", d(3.0)),
    ], mask=[])),
    get("c/d"),
])
case("array union", "t-union", [
    seed("c/d", a=arr(i(1), s("a")), notarray=i(5)),
    commit(write("c/d", {}, [
        union("a", d(1.0), s("b"), s("b"), null),
        union("missing", i(1), i(1), nan, nan),
        union("notarray", s("x")),
    ], mask=[])),
    get("c/d"),
])
case("array remove", "t-remove", [
    seed("c/d", a=arr(i(1), d(1.0), i(2), nan, null, s("x"), m(k=i(1))), notarray=i(5)),
    commit(write("c/d", {}, [
        remove("a", i(1), nan, null, m(k=d(1.0))),
        remove("missing", i(1)),
        remove("notarray", i(5)),
    ], mask=[])),
    get("c/d"),
])
case("update then transform on the same field", "t-order", [
    commit(write("c/d", {"a": i(1)}, [inc("a", i(1))])),
    get("c/d"),
])
case("transform on a masked field", "t-mask-overlap", [
    seed("c/d", a=i(1)),
    commit(write("c/d", {"a": i(10)}, [inc("a", i(1))], mask=["a"])),
    get("c/d"),
])
case("two transforms on one field", "t-two", [
    commit(write("c/d", {}, [inc("a", i(1)), inc("a", i(2))])),
])
case("transform through a non-map parent", "t-parent", [
    seed("c/d", a=i(1)),
    commit(write("c/d", {}, [inc("a.b", i(1))], mask=[])),
    get("c/d"),
])
case("standalone transform write", "t-standalone", [
    seed("c/d", n=i(1)),
    commit(transform_only("c/d", inc("n", i(1)))),
    commit(transform_only("c/missing", inc("n", i(1)))),
    get("c/d"),
    get("c/missing"),
])
case("transform that changes nothing", "t-noop", [
    seed("c/d", a=arr(i(1))),
    commit(write("c/d", {}, [union("a", i(1))], mask=[])),
    get("c/d"),
])
case("increment by a string", "t-bad-operand", [
    commit(write("c/d", {}, [inc("a", s("x"))])),
])
case("server time with a precondition on a missing document", "t-precondition", [
    commit(write("c/d", {"a": i(1)}, [server_time("t")], exists=False)),
    get("c/d"),
])

json.dump({
    "oracle": "cloud-firestore-emulator v1.22.0 (sha256 9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)",
    "generated_by": "tools/oracle/transforms.py",
    "cases": cases,
}, sys.stdout, ensure_ascii=False, indent=1)
print()
