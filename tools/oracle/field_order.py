#!/usr/bin/env python3
"""Record the order in which the official emulator returns fields (#108).

Usage: python3 -I tools/oracle/field_order.py HOST:PORT > fixture.json

Each case writes documents through REST and reads them back; every call's HTTP status and body
are recorded, and the JSON keeps the order the emulator wrote the fields in. Writes, merges,
masks, transforms and nested maps decide where a field lands; read masks, query projections
and aggregation aliases decide the order of what is returned. Stdlib only.
"""

import json
import sys
from pathlib import Path
from urllib.parse import quote

sys.path.insert(0, str(Path(__file__).resolve().parent))
from harness import case, cases, commit, update  # noqa: E402

i = lambda n: {"integerValue": str(n)}
m = lambda *kv: {"mapValue": {"fields": dict(kv)}}
arr = lambda *v: {"arrayValue": {"values": list(v)}}
fields = lambda *kv: dict(kv)
get = lambda name, *mask: ("GET", "{base}/" + name + "".join(
    ("&" if k else "?") + "mask.fieldPaths=" + quote(p, safe="") for k, p in enumerate(mask)), None)


def transforms(name, mask, written, *paths_and_kinds):
    w = update(name, written, mask=mask)
    w["updateTransforms"] = [
        {"fieldPath": p, **({"setToServerValue": "REQUEST_TIME"} if k == "time" else {"increment": i(1)})}
        for p, k in paths_and_kinds
    ]
    return w


base_doc = fields(("zeta", i(1)), ("alpha", i(2)), ("mid", m(("z", i(3)), ("a", i(4)))))

case("set keeps the written order", "fo-set", [
    commit(update("c/d", fields(*base_doc.items(), ("arr", arr(m(("b", i(1)), ("a", i(2)))))))),
    get("c/d"),
])
case("set again with another order", "fo-reset", [
    commit(update("c/d", base_doc)),
    commit(update("c/d", fields(("alpha", i(5)), ("mid", m(("a", i(6)), ("z", i(7)))), ("zeta", i(8))))),
    get("c/d"),
])
case("create through POST keeps the JSON order", "fo-create", [
    ("POST", "{base}/c?documentId=d", {"fields": base_doc}),
    get("c/d"),
])
case("masked update: new fields at the end, existing ones in place", "fo-mask", [
    commit(update("c/d", base_doc)),
    commit(update("c/d", fields(("beta", i(9)), ("alpha", i(10))), mask=["beta", "alpha"])),
    get("c/d"),
])
case("masked update: a removed field comes back at the end", "fo-readd", [
    commit(update("c/d", base_doc)),
    commit(update("c/d", {}, mask=["zeta"])),
    commit(update("c/d", fields(("zeta", i(11))), mask=["zeta"])),
    get("c/d"),
])
case("masked update inside a map", "fo-nested-mask", [
    commit(update("c/d", base_doc)),
    commit(update("c/d", fields(("mid", m(("b", i(12)), ("z", i(13))))), mask=["mid.b", "mid.z"])),
    get("c/d"),
])
case("masked update replacing a whole map", "fo-replace-map", [
    commit(update("c/d", base_doc)),
    commit(update("c/d", fields(("mid", m(("y", i(14)), ("b", i(15))))), mask=["mid"])),
    get("c/d"),
])
case("transforms on new and existing fields", "fo-transforms", [
    commit(update("c/d", base_doc)),
    commit(transforms("c/d", ["omega"], fields(("omega", i(16))),
                      ("new2", "inc"), ("alpha", "inc"), ("new1", "time"), ("mid.c", "inc"))),
    get("c/d"),
])
case("transforms on a new document", "fo-transforms-new", [
    commit(transforms("c/d", None, fields(("zeta", i(1)), ("alpha", i(2))), ("new2", "inc"), ("new1", "time"))),
    get("c/d"),
])

mid_first = fields(("mid", m(("z", i(3)), ("a", i(4)))), ("zeta", i(1)), ("alpha", i(2)))
case("masked update inside a map that is not last", "fo-nested-not-last", [
    commit(update("c/d", mid_first)),
    commit(update("c/d", fields(("mid", m(("b", i(5))))), mask=["mid.b"])),
    get("c/d"),
])
case("masked update replacing a map that is not last", "fo-replace-not-last", [
    commit(update("c/d", mid_first)),
    commit(update("c/d", fields(("mid", m(("y", i(6)), ("b", i(7))))), mask=["mid"])),
    get("c/d"),
])
case("masked update deleting inside a map that is not last", "fo-delete-nested", [
    commit(update("c/d", mid_first)),
    commit(update("c/d", {}, mask=["mid.z"])),
    get("c/d"),
])
case("masked update with an unchanged value", "fo-same-value", [
    commit(update("c/d", base_doc)),
    commit(update("c/d", fields(("zeta", i(1))), mask=["zeta"])),
    get("c/d"),
])
case("masked update over several maps", "fo-tree", [
    commit(update("c/d", fields(("p", m(("a", i(1)), ("b", i(2)))), ("q", m(("a", i(3)))), ("r", i(4)), ("s", i(5))))),
    commit(update("c/d", fields(("p", m(("b", i(6)), ("a", i(7)))), ("q", m(("a", i(8)))), ("r", i(9))),
                  mask=["p.b", "q.a", "p.a", "r"])),
    get("c/d"),
])
case("masked update creating nested maps", "fo-deep", [
    commit(update("c/d", base_doc)),
    commit(update("c/d", fields(("deep", m(("x", m(("y", i(1)))))), ("alpha", i(2))), mask=["deep.x.y", "alpha"])),
    get("c/d"),
])
wide = [f"w{k}" for k in range(14)]
case("masked update of many fields", "fo-wide", [
    commit(update("c/d", fields(*[(n, i(k)) for k, n in enumerate(["keep"] + wide)]))),
    commit(update("c/d", fields(*[(n, i(k)) for k, n in enumerate(wide)]), mask=wide)),
    get("c/d"),
])
case("masked update with other names", "fo-names", [
    commit(update("c/d", fields(("zeta", i(1)), ("x.y", i(2)), ("\u65e5\u672c", i(3)), ("\u00e9t\u00e9", i(4)), ("Z", i(5))))),
    commit(update("c/d", fields(("x.y", i(6)), ("\u65e5\u672c", i(7)), ("\u00e9t\u00e9", i(8)), ("Z", i(9)), ("zeta", i(10))),
                  mask=["`x.y`", "`\u65e5\u672c`", "`\u00e9t\u00e9`", "Z", "zeta"])),
    get("c/d"),
    get("c/d", "`x.y`", "`\u65e5\u672c`", "`\u00e9t\u00e9`", "Z", "zeta"),
    get("c/d", "\u65e5\u672c"),
])
case("set again with the same values in another order", "fo-same-reordered", [
    commit(update("c/d", base_doc)),
    get("c/d"),
    commit(update("c/d", fields(("alpha", i(2)), ("mid", m(("a", i(4)), ("z", i(3)))), ("zeta", i(1))))),
    get("c/d"),
])
case("set again with some values changed", "fo-set-partial", [
    commit(update("c/d", fields(("zeta", i(1)), ("alpha", i(2)), ("mid", m(("z", i(3)), ("a", i(4)))), ("beta", i(5))))),
    commit(update("c/d", fields(("alpha", i(2)), ("mid", m(("a", i(4)), ("z", i(9)))), ("beta", i(5)), ("zeta", i(9))))),
    get("c/d"),
])
case("masked update with an int that equals a double", "fo-int-double", [
    commit(update("c/d", fields(("zeta", i(1)), ("alpha", i(2))))),
    commit(update("c/d", fields(("zeta", {"doubleValue": 1.0})), mask=["zeta"])),
    get("c/d"),
])
case("transforms inside a map that is not last", "fo-transform-nested", [
    commit(update("c/d", mid_first)),
    commit(transforms("c/d", [], {}, ("mid.z", "inc"))),
    get("c/d"),
    commit(transforms("c/d", [], {}, ("mid.c", "inc"))),
    get("c/d"),
])
def array_transform(name, path, kind, *values):
    w = update(name, {}, mask=[])
    w["updateTransforms"] = [{"fieldPath": path, kind: {"values": list(values)} if kind in ("appendMissingElements", "removeAllFromArray") else values[0]}]
    return w
case("array and maximum transforms on existing fields", "fo-array-transforms", [
    commit(update("c/d", fields(("tags", arr(i(1))), ("zeta", i(1)), ("top", i(5)), ("alpha", i(2))))),
    commit(array_transform("c/d", "tags", "appendMissingElements", i(2))),
    get("c/d"),
    commit(array_transform("c/d", "top", "maximum", i(9))),
    get("c/d"),
    commit(array_transform("c/d", "zeta", "minimum", i(9))),
    get("c/d"),
])
case("document transform write", "fo-transform-write", [
    commit(update("c/d", base_doc)),
    ("POST", "{base}:commit", {"writes": [{"transform": {"document": "{base}/c/d", "fieldTransforms": [
        {"fieldPath": "alpha", "increment": i(1)}, {"fieldPath": "new", "increment": i(1)}]}}]}),
    get("c/d"),
])

# Read masks: many names, so the order can be compared with a hash order. Written in one order.
names = ["str", "int", "dbl", "zeta", "alpha", "a", "b", "c", "mid", "x1", "x2", "x3", "x4", "x5",
         "longer_name", "Upper", "_under", "n10", "n2"]
many = fields(*[(n, m(("z", i(1)), ("a", i(2)))) if n == "mid" else (n, i(k)) for k, n in enumerate(names)])
masks = [
    ["str", "int"], ["int", "str"], ["dbl", "int"], ["int", "dbl"],
    ["zeta", "alpha", "a"], ["a", "alpha", "zeta"],
    ["mid.z", "mid.a"], ["mid.a", "mid.z", "str"],
    names[:12], names[:13], list(reversed(names)),
]
case("get with a mask", "fo-get-mask", [commit(update("c/d", many))] + [get("c/d", *mk) for mk in masks])
case("batchGet with a mask", "fo-batch-mask", [
    commit(update("c/d", many)),
    *[("POST", "{db}/documents:batchGet", {"documents": ["{base}/c/d"], "mask": {"fieldPaths": mk}})
      for mk in masks],
])
case("list with a mask", "fo-list-mask", [
    commit(update("c/d", many)),
    *[("GET", "{base}/c?" + "&".join("mask.fieldPaths=" + quote(p, safe="") for p in mk), None) for mk in masks[:4]],
])
case("query projection", "fo-select", [
    commit(update("c/d", many)),
    *[("POST", "{base}:runQuery", {"structuredQuery": {
        "from": [{"collectionId": "c"}],
        "select": {"fields": [{"fieldPath": p} for p in mk]}}}) for mk in masks],
])
case("aggregation aliases", "fo-aggregations", [
    commit(update("c/d", many)),
    *[("POST", "{base}:runAggregationQuery", {"structuredAggregationQuery": {
        "structuredQuery": {"from": [{"collectionId": "c"}]},
        "aggregations": [{"alias": a, "count": {}} for a in aliases]}})
      for aliases in (["zeta", "alpha", "mid"], ["mid", "alpha", "zeta"], ["str", "int", "dbl"], ["dbl", "int", "str"])],
])

# Two names in one bucket (`a`, `q`), and masks naming fields that are missing or unchanged: the
# names are first gathered in a hash map of the mask, and what is returned or changed is put
# into a second one in that order, so the two maps' sizes both matter.
present = ["str", "int", "dbl", "zeta", "alpha", "a", "b", "c", "x1", "x2", "x3"]
case("one bucket, two names", "fo-bucket", [
    commit(update("c/d", fields(("a", i(1)), ("q", i(2)), ("zeta", i(3))))),
    get("c/d", "q", "a"),
    get("c/d", "a", "q"),
    commit(update("c/e", fields(("a", i(1)), ("q", i(2)), ("zeta", i(3))))),
    commit(update("c/e", fields(("a", i(9)), ("q", i(9))), mask=["q", "a"])),
    get("c/e"),
    commit(update("c/f", fields(("a", i(1)), ("q", i(2)), ("zeta", i(3))))),
    commit(update("c/f", fields(("a", i(9)), ("q", i(9))), mask=["a", "q"])),
    get("c/f"),
])
case("read mask naming missing fields", "fo-mask-missing", [
    commit(update("c/d", fields(*[(k, i(n)) for n, k in enumerate(present)]))),
    get("c/d", *present, "gone1", "gone2"),
])
case("masked update deleting missing fields", "fo-delete-missing", [
    commit(update("c/d", fields(("keep", i(0)), *[(k, i(n)) for n, k in enumerate(present)]))),
    commit(update("c/d", fields(*[(k, i(n + 100)) for n, k in enumerate(present)]), mask=present + ["gone1", "gone2"])),
    get("c/d"),
])
case("masked update deleting existing fields", "fo-delete-existing", [
    commit(update("c/d", fields(("keep", i(0)), *[(k, i(n)) for n, k in enumerate(present)], ("gone1", i(1)), ("gone2", i(2))))),
    commit(update("c/d", fields(*[(k, i(n + 100)) for n, k in enumerate(present)]), mask=present + ["gone1", "gone2"])),
    get("c/d"),
])
case("masked update with unchanged values in the input", "fo-unchanged-input", [
    commit(update("c/d", fields(("keep", i(0)), *[(k, i(n)) for n, k in enumerate(present)], ("u1", i(1)), ("u2", i(2))))),
    commit(update("c/d", fields(*[(k, i(n + 100)) for n, k in enumerate(present)], ("u1", i(1)), ("u2", i(2))),
                  mask=present + ["u1", "u2"])),
    get("c/d"),
])
case("masked update deleting inside one map and changing another field", "fo-delete-and-put", [
    commit(update("c/d", fields(("p", m(("x", i(1)), ("y", i(2)))), ("alpha", i(1)), ("zeta", i(2)), ("mid", i(3))))),
    commit(update("c/d", fields(("alpha", i(9))), mask=["p.y", "alpha"])),
    get("c/d"),
])

json.dump(cases, sys.stdout, indent=1)
print()
