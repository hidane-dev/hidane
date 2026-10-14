"""How does the official emulator answer RunQuery? (#21)

Usage:
    python3 -I tools/oracle/queries.py 127.0.0.1:8086 > crates/hidane/tests/fixtures/queries.json

Seeds one project with two datasets, runs every case through REST `:runQuery`, and records the
outcome. The fixture keeps the dataset and the queries next to the outcomes, so
`crates/hidane/tests/queries.rs` can seed hidane the same way and compare over gRPC.

- `t/*`: one document per kind of value in field `v` (every type, numbers that compare equal
  across int and double, NaN, infinities, UTF-8 vs UTF-16 string order, nested values).
- `p/*`: documents with several fields (`a`, `b`, `tags`, `m.x`, `opt`) for composite filters,
  implicit ordering, cursors and projections, plus nested `p` collections for collection groups.

`{documents}` in the dataset and the queries stands for `projects/<project>/databases/(default)/
documents`. Outcomes record, per response, the document (relative path), `skipped`
(skippedResults) and `done`; the fields of returned documents when the query has a projection;
or the error status and message (with the project replaced by `{project}`).
"""

import json
import sys
import time
import urllib.error
import urllib.request

HOST = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1:8080"
PROJECT = f"queries-{time.strftime('%H%M%S')}"
DOCUMENTS = f"projects/{PROJECT}/databases/(default)/documents"


def i(n): return {"integerValue": str(n)}
def d(x): return {"doubleValue": x}
def s(x): return {"stringValue": x}
def b(x): return {"booleanValue": x}
def ts(x): return {"timestampValue": x}
def ref(path): return {"referenceValue": "{documents}/" + path}
def arr(*xs): return {"arrayValue": {"values": list(xs)} if xs else {}}
def mp(**kv): return {"mapValue": {"fields": kv} if kv else {}}
def vec(*xs): return mp(__type__=s("__vector__"), value=arr(*[d(x) for x in xs]))
NULL = {"nullValue": None}
NAN = d("NaN")

TYPES = {
    "null": NULL, "false": b(False), "true": b(True), "nan": NAN, "ninf": d("-Infinity"),
    "neg": i(-1), "negzero": d(-0.0), "zero": i(0), "half": d(0.5), "one": i(1), "onef": d(1.0),
    "two": i(2), "big": i(9007199254740993), "bigf": d(9007199254740992.0), "inf": d("Infinity"),
    "ts": ts("2020-01-01T00:00:00Z"), "tsus": ts("2020-01-01T00:00:00.000001Z"),
    "sempty": s(""), "sa": s("a"), "saa": s("aa"), "sb": s("b"), "sffff": s("￿"), "semoji": s("\U0001F600"),
    "bytes": {"bytesValue": "AQ=="}, "bytes2": {"bytesValue": "AQI="},
    "ref": ref("t/sa"), "ref2": ref("t/sa/x/y"),
    "geo": {"geoPointValue": {"latitude": 1, "longitude": 2}},
    "arr0": arr(), "arr1": arr(i(1)), "arr1f": arr(d(1.0)), "arr12": arr(i(1), i(2)), "arr2": arr(i(2)),
    "arrnull": arr(NULL), "arrnan": arr(NAN), "arrs": arr(s("a")), "arrmix": arr(s("a"), i(1), NULL),
    "arrmap": arr(mp(a=i(1))),
    "map0": mp(), "mapa": mp(a=i(1)), "mapa1f": mp(a=d(1.0)), "mapb": mp(b=i(0)),
    "vec": vec(1, 2), "vec3": vec(0, 0, 0),
}
DATASET = [{"name": f"t/{k}", "fields": {"v": v}} for k, v in TYPES.items()]
DATASET.append({"name": "t/nov", "fields": {"u": i(1)}})
DATASET += [
    {"name": "p/p1", "fields": {"a": i(1), "b": s("x"), "tags": arr(s("red"), s("blue")), "m": mp(x=i(1)), "a b": i(1)}},
    {"name": "p/p2", "fields": {"a": i(2), "b": s("y"), "tags": arr(s("red")), "m": mp(x=i(2)), "opt": b(True)}},
    {"name": "p/p3", "fields": {"a": i(2), "b": s("x"), "tags": arr(), "m": mp(x=i(3), y=i(1))}},
    {"name": "p/p4", "fields": {"a": i(3), "b": s("z"), "tags": arr(s("blue"), s("green")), "opt": NULL}},
    {"name": "p/p5", "fields": {"a": i(3), "b": s("y"), "m": mp(), "opt": b(False)}},
    {"name": "p/p6", "fields": {"a": i(-1), "b": s("x"), "tags": arr(s("green")), "m": mp(x=s("s"))}},
    {"name": "p/p7", "fields": {"b": s("w")}},
    {"name": "p/p8", "fields": {"a": d(4.5), "b": s("x")}},
    {"name": "p/p1/p/sub1", "fields": {"a": i(10), "b": s("x")}},
    {"name": "p/p2/p/sub2", "fields": {"a": i(11), "b": s("y")}},
    {"name": "x/y/p/deep", "fields": {"a": i(12), "b": s("x")}},
    {"name": "p/p1/other/o1", "fields": {"a": i(13), "b": s("x")}},
]


def f(path): return {"fieldPath": path}
def ff(path, op, value): return {"fieldFilter": {"field": f(path), "op": op, "value": value}}
def uf(path, op): return {"unaryFilter": {"field": f(path), "op": op}}
def AND(*fs): return {"compositeFilter": {"op": "AND", "filters": list(fs)}}
def OR(*fs): return {"compositeFilter": {"op": "OR", "filters": list(fs)}}
def asc(path): return {"field": f(path), "direction": "ASCENDING"}
def desc(path): return {"field": f(path), "direction": "DESCENDING"}
def cur(*values, before): return {"values": list(values), "before": before}
def q(coll, all_descendants=False, **kw):
    out = {"from": [{"collectionId": coll, "allDescendants": all_descendants} if all_descendants else {"collectionId": coll}]}
    for k, v in kw.items():
        out[{"where_": "where", "order": "orderBy", "start": "startAt", "end": "endAt"}.get(k, k)] = v
    return out


CASES = []
def case(name, query, parent=""):
    CASES.append({"name": name, "parent": parent, "query": query})


# --- every value type ----------------------------------------------------------------------
case("all of t, by name", q("t"))
case("order by v ascending (type order; documents without v left out)", q("t", order=[asc("v")]))
case("order by v descending", q("t", order=[desc("v")]))
for label, value in [("null", NULL), ("NaN", NAN), ("int 1", i(1)), ("double 1.0", d(1.0)), ("-0.0", d(-0.0)),
                     ("int 2^53+1", i(9007199254740993)), ("double 2^53", d(9007199254740992.0)), ("string a", s("a")),
                     ("timestamp", ts("2020-01-01T00:00:00Z")), ("bytes", {"bytesValue": "AQ=="}), ("reference", ref("t/sa")),
                     ("geo point", {"geoPointValue": {"latitude": 1, "longitude": 2}}), ("array [1]", arr(i(1))),
                     ("empty array", arr()), ("map {a: 1}", mp(a=i(1))), ("empty map", mp()), ("vector", vec(1, 2)),
                     ("true", b(True))]:
    case(f"v == {label}", q("t", where_=ff("v", "EQUAL", value)))
for label, value in [("int 1", i(1)), ("null", NULL), ("NaN", NAN), ("string a", s("a"))]:
    case(f"v != {label}", q("t", where_=ff("v", "NOT_EQUAL", value)))
for op in ["LESS_THAN", "LESS_THAN_OR_EQUAL", "GREATER_THAN", "GREATER_THAN_OR_EQUAL"]:
    for label, value in [("int 1", i(1)), ("string b", s("b")), ("timestamp", ts("2020-01-01T00:00:00Z")),
                         ("array [2]", arr(i(2))), ("empty map", mp()), ("vector of 2", vec(1, 2)), ("false", b(False)),
                         ("-Infinity", d("-Infinity")), ("bytes", {"bytesValue": "AQ=="}), ("reference", ref("t/sa")),
                         ("geo point", {"geoPointValue": {"latitude": 1, "longitude": 2}})]:
        case(f"v {op} {label}", q("t", where_=ff("v", op, value)))
    case(f"v {op} NaN", q("t", where_=ff("v", op, NAN)))
    case(f"v {op} null", q("t", where_=ff("v", op, NULL)))
for label, value in [("int 1", i(1)), ("null", NULL), ("NaN", NAN), ("string a", s("a")), ("map {a: 1}", mp(a=i(1)))]:
    case(f"v array-contains {label}", q("t", where_=ff("v", "ARRAY_CONTAINS", value)))
case("v in [1, a]", q("t", where_=ff("v", "IN", arr(i(1), s("a")))))
case("v in [null]", q("t", where_=ff("v", "IN", arr(NULL))))
case("v in [NaN]", q("t", where_=ff("v", "IN", arr(NAN))))
case("v in [[1]]", q("t", where_=ff("v", "IN", arr(arr(i(1))))))
case("v in [1, 1]", q("t", where_=ff("v", "IN", arr(i(1), i(1)))))
case("v not-in [1, a]", q("t", where_=ff("v", "NOT_IN", arr(i(1), s("a")))))
case("v not-in [null]", q("t", where_=ff("v", "NOT_IN", arr(NULL))))
case("v not-in [NaN]", q("t", where_=ff("v", "NOT_IN", arr(NAN))))
case("v array-contains-any [1, a]", q("t", where_=ff("v", "ARRAY_CONTAINS_ANY", arr(i(1), s("a")))))
case("v array-contains-any [null]", q("t", where_=ff("v", "ARRAY_CONTAINS_ANY", arr(NULL))))
for op in ["IS_NULL", "IS_NOT_NULL", "IS_NAN", "IS_NOT_NAN"]:
    case(f"v {op}", q("t", where_=uf("v", op)))

# --- several fields --------------------------------------------------------------------------
case("a == 2", q("p", where_=ff("a", "EQUAL", i(2))))
case("a > 1 (implicit order by a)", q("p", where_=ff("a", "GREATER_THAN", i(1))))
case("a > 1 order by b", q("p", where_=ff("a", "GREATER_THAN", i(1)), order=[asc("b")]))
case("a > 1 order by a desc", q("p", where_=ff("a", "GREATER_THAN", i(1)), order=[desc("a")]))
case("a > 1 and b < z (two inequalities)", q("p", where_=AND(ff("a", "GREATER_THAN", i(1)), ff("b", "LESS_THAN", s("z")))))
case("b < z and a > 1 order by a desc", q("p", where_=AND(ff("b", "LESS_THAN", s("z")), ff("a", "GREATER_THAN", i(1))), order=[desc("a")]))
case("a != 2", q("p", where_=ff("a", "NOT_EQUAL", i(2))))
case("a != 2 order by b", q("p", where_=ff("a", "NOT_EQUAL", i(2)), order=[asc("b")]))
case("order by a", q("p", order=[asc("a")]))
case("order by a desc", q("p", order=[desc("a")]))
case("order by a desc, b", q("p", order=[desc("a"), asc("b")]))
case("order by b, a desc", q("p", order=[asc("b"), desc("a")]))
case("order by m.x", q("p", order=[asc("m.x")]))
case("order by __name__ desc", q("p", order=[desc("__name__")]))
case("order by a, __name__ desc", q("p", order=[asc("a"), desc("__name__")]))
case("b == x order by a", q("p", where_=ff("b", "EQUAL", s("x")), order=[asc("a")]))
case("a >= 2 and b == y", q("p", where_=AND(ff("a", "GREATER_THAN_OR_EQUAL", i(2)), ff("b", "EQUAL", s("y")))))
case("a == 1 or b == z", q("p", where_=OR(ff("a", "EQUAL", i(1)), ff("b", "EQUAL", s("z")))))
case("(a == 2 and b == x) or tags contains green", q("p", where_=OR(AND(ff("a", "EQUAL", i(2)), ff("b", "EQUAL", s("x"))), ff("tags", "ARRAY_CONTAINS", s("green")))))
case("a < 2 or a > 3", q("p", where_=OR(ff("a", "LESS_THAN", i(2)), ff("a", "GREATER_THAN", i(3)))))
case("a < 2 or b == z", q("p", where_=OR(ff("a", "LESS_THAN", i(2)), ff("b", "EQUAL", s("z")))))
case("or of one filter", q("p", where_=OR(ff("a", "EQUAL", i(3)))))
case("and of one filter", q("p", where_=AND(ff("a", "EQUAL", i(3)))))
case("nested and inside and", q("p", where_=AND(AND(ff("a", "EQUAL", i(2))), ff("b", "EQUAL", s("x")))))
case("tags array-contains red", q("p", where_=ff("tags", "ARRAY_CONTAINS", s("red"))))
case("tags array-contains-any [blue, green]", q("p", where_=ff("tags", "ARRAY_CONTAINS_ANY", arr(s("blue"), s("green")))))
case("a in [1, 3] and tags array-contains blue", q("p", where_=AND(ff("a", "IN", arr(i(1), i(3))), ff("tags", "ARRAY_CONTAINS", s("blue")))))
case("m.x == 1", q("p", where_=ff("m.x", "EQUAL", i(1))))
case("m.x > 1", q("p", where_=ff("m.x", "GREATER_THAN", i(1))))
case("m == {x: 1}", q("p", where_=ff("m", "EQUAL", mp(x=i(1)))))
case("opt == null", q("p", where_=ff("opt", "EQUAL", NULL)))
case("opt != false", q("p", where_=ff("opt", "NOT_EQUAL", b(False))))
case("opt not-in [true]", q("p", where_=ff("opt", "NOT_IN", arr(b(True)))))
case("opt is not null", q("p", where_=uf("opt", "IS_NOT_NULL")))
case("quoted field `a b` == 1", q("p", where_=ff("`a b`", "EQUAL", i(1))))
case("__name__ == p3", q("p", where_=ff("__name__", "EQUAL", ref("p/p3"))))
case("__name__ > p4", q("p", where_=ff("__name__", "GREATER_THAN", ref("p/p4"))))
case("__name__ in [p2, p5]", q("p", where_=ff("__name__", "IN", arr(ref("p/p2"), ref("p/p5")))))
case("__name__ not-in [p2, p5]", q("p", where_=ff("__name__", "NOT_IN", arr(ref("p/p2"), ref("p/p5")))))
case("__name__ != p2", q("p", where_=ff("__name__", "NOT_EQUAL", ref("p/p2"))))

# --- projection ------------------------------------------------------------------------------
case("select a, m.x", q("p", select={"fields": [f("a"), f("m.x")]}, order=[asc("a")]))
case("select nothing", q("p", select={"fields": []}, limit=2))
case("select __name__", q("p", select={"fields": [f("__name__")]}, limit=2))
case("select a missing field", q("p", select={"fields": [f("nope")]}, limit=2))
case("select m", q("p", select={"fields": [f("m")]}, where_=ff("a", "EQUAL", i(2))))

# --- cursors, offset and limit ---------------------------------------------------------------
case("order by a, start at 2", q("p", order=[asc("a")], start=cur(i(2), before=True)))
case("order by a, start after 2", q("p", order=[asc("a")], start=cur(i(2), before=False)))
case("order by a, end at 3", q("p", order=[asc("a")], end=cur(i(3), before=False)))
case("order by a, end before 3", q("p", order=[asc("a")], end=cur(i(3), before=True)))
case("order by a, start at 2, end before 3", q("p", order=[asc("a")], start=cur(i(2), before=True), end=cur(i(3), before=True)))
case("order by a, start after (2, p2)", q("p", order=[asc("a")], start=cur(i(2), ref("p/p2"), before=False)))
case("order by a desc, start at 2", q("p", order=[desc("a")], start=cur(i(2), before=True)))
case("order by a desc, start after (2, p3)", q("p", order=[desc("a")], start=cur(i(2), ref("p/p3"), before=False)))
case("a > 1, start after (2, p2) on the implicit order", q("p", where_=ff("a", "GREATER_THAN", i(1)), start=cur(i(2), ref("p/p2"), before=False)))
case("no order, start after p3", q("p", start=cur(ref("p/p3"), before=False)))
case("no order, end at p3", q("p", end=cur(ref("p/p3"), before=False)))
case("order by a, start at 2.5 (between values)", q("p", order=[asc("a")], start=cur(d(2.5), before=True)))
case("order by a, start at string (another type)", q("p", order=[asc("a")], start=cur(s("x"), before=True)))
case("order by a, offset 2 limit 2", q("p", order=[asc("a")], offset=2, limit=2))
case("offset 3", q("p", offset=3))
case("offset beyond the results", q("p", offset=50))
case("limit 0", q("p", limit=0))
case("limit 3", q("p", limit=3))
case("start after p2, offset 1, limit 2", q("p", start=cur(ref("p/p2"), before=False), offset=1, limit=2))

# --- collection groups and parents -----------------------------------------------------------
case("collection group p", q("p", all_descendants=True))
case("collection group p, a > 9", q("p", all_descendants=True, where_=ff("a", "GREATER_THAN", i(9))))
case("collection group p order by __name__ desc", q("p", all_descendants=True, order=[desc("__name__")]))
case("collection group p under p/p1", q("p", all_descendants=True), parent="p/p1")
case("collection group p under x", q("p", all_descendants=True), parent="x/y")
case("collection p under p/p1", q("p"), parent="p/p1")
case("collection other under p/p1", q("other"), parent="p/p1")
case("collection group p, __name__ >= p/p2", q("p", all_descendants=True, where_=ff("__name__", "GREATER_THAN_OR_EQUAL", ref("p/p2"))))
case("collection group p, start after p/p8", q("p", all_descendants=True, start=cur(ref("p/p8"), before=False)))
case("empty collection", q("nothing"))
case("collection group with no documents", q("nothing", all_descendants=True))

# --- ordering by __name__, cursors on explicit orderings ---------------------------------------
case("order by __name__ asc", q("p", order=[asc("__name__")]))
case("a == 2 order by __name__ desc", q("p", where_=ff("a", "EQUAL", i(2)), order=[desc("__name__")]))
case("array-contains + __name__ desc", q("p", where_=ff("tags", "ARRAY_CONTAINS", s("red")), order=[desc("__name__")]))
case("opt is null + __name__ desc", q("p", where_=uf("opt", "IS_NULL"), order=[desc("__name__")]))
case("or + __name__ desc", q("p", where_=OR(ff("a", "EQUAL", i(1)), ff("a", "EQUAL", i(3))), order=[desc("__name__")]))
case("in + __name__ desc", q("p", where_=ff("a", "IN", arr(i(1), i(3))), order=[desc("__name__")]))
case("a == 2 and __name__ > p1, __name__ desc", q("p", where_=AND(ff("a", "EQUAL", i(2)), ff("__name__", "GREATER_THAN", ref("p/p1"))), order=[desc("__name__")]))
case("group p, a == 11, __name__ desc", q("p", all_descendants=True, where_=ff("a", "EQUAL", i(11)), order=[desc("__name__")]))
case("__name__ > p2 order by __name__ desc", q("p", where_=ff("__name__", "GREATER_THAN", ref("p/p2")), order=[desc("__name__")]))
case("__name__ in + __name__ desc", q("p", where_=ff("__name__", "IN", arr(ref("p/p1"), ref("p/p3"))), order=[desc("__name__")]))
case("__name__ != p2 + __name__ desc", q("p", where_=ff("__name__", "NOT_EQUAL", ref("p/p2")), order=[desc("__name__")]))
case("a > 1 order by __name__ desc", q("p", where_=ff("a", "GREATER_THAN", i(1)), order=[desc("__name__")]))
case("order by __name__ desc, a", q("p", order=[desc("__name__"), asc("a")]))
case("a != 2 + order by __name__", q("p", where_=ff("a", "NOT_EQUAL", i(2)), order=[asc("__name__")]))
case("a > 1 + order by a, __name__, b", q("p", where_=ff("a", "GREATER_THAN", i(1)), order=[asc("a"), asc("__name__"), asc("b")]))
case("order by a, a", q("p", order=[asc("a"), desc("a")]))
case("where a == 1 order by a", q("p", where_=ff("a", "EQUAL", i(1)), order=[asc("a")]))
case("a > 1, start at [2] (implicit order only)", q("p", where_=ff("a", "GREATER_THAN", i(1)), start=cur(i(2), before=True)))
case("a > 1 order by a, cursor (2, p2)", q("p", where_=ff("a", "GREATER_THAN", i(1)), order=[asc("a")], start=cur(i(2), ref("p/p2"), before=False)))
case("order by a, __name__, start after (2, p2)", q("p", order=[asc("a"), asc("__name__")], start=cur(i(2), ref("p/p2"), before=False)))
case("order by __name__, start after p3", q("p", order=[asc("__name__")], start=cur(ref("p/p3"), before=False)))
case("order by __name__, start at string", q("p", order=[asc("__name__")], start=cur(s("p3"), before=True)))
case("order by __name__, start at other collection", q("p", order=[asc("__name__")], start=cur(ref("t/sa"), before=True)))
case("order by __name__, start at p/p1/p/sub1", q("p", order=[asc("__name__")], start=cur(ref("p/p1/p/sub1"), before=True)))
case("group p order by __name__, start after p/p8", q("p", all_descendants=True, order=[asc("__name__")], start=cur(ref("p/p8"), before=False)))
case("order by a desc, b, start at (2, x)", q("p", order=[desc("a"), asc("b")], start=cur(i(2), s("x"), before=True)))
case("order by a desc, b, start after (2, x)", q("p", order=[desc("a"), asc("b")], start=cur(i(2), s("x"), before=False)))
case("order by a desc, b, end before (2, y)", q("p", order=[desc("a"), asc("b")], end=cur(i(2), s("y"), before=True)))

# --- more filter rules -----------------------------------------------------------------------
case("in + array-contains-any", q("p", where_=AND(ff("a", "IN", arr(i(1), i(2))), ff("tags", "ARRAY_CONTAINS_ANY", arr(s("red"))))))
case("array-contains-any inside or", q("p", where_=OR(ff("tags", "ARRAY_CONTAINS_ANY", arr(s("red"))), ff("a", "EQUAL", i(3)))))
case("in 15 x in 2 (30 disjunctions)", q("p", where_=AND(ff("a", "IN", arr(*[i(n) for n in range(15)])), ff("b", "IN", arr(s("x"), s("y"))))))
case("in [1, null]", q("t", where_=ff("v", "IN", arr(i(1), NULL))))
case("in [1, NaN]", q("t", where_=ff("v", "IN", arr(i(1), NAN))))
case("not-in [1, null]", q("t", where_=ff("v", "NOT_IN", arr(i(1), NULL))))
case("not-in [1, NaN]", q("t", where_=ff("v", "NOT_IN", arr(i(1), NAN))))
case("array-contains-any [1, null]", q("t", where_=ff("v", "ARRAY_CONTAINS_ANY", arr(i(1), NULL))))
case("u is null (missing on most)", q("t", where_=uf("u", "IS_NULL")))
case("b == w or a < 2 (b == w lacks a)", q("p", where_=OR(ff("b", "EQUAL", s("w")), ff("a", "LESS_THAN", i(2)))))
case("not-in + order by b", q("p", where_=ff("a", "NOT_IN", arr(i(2))), order=[asc("b")]))
case("is-nan on a string field", q("p", where_=uf("b", "IS_NAN")))
case("a < 2 or a > 3, limit 2", q("p", where_=OR(ff("a", "LESS_THAN", i(2)), ff("a", "GREATER_THAN", i(3))), limit=2))
case("or with order by b", q("p", where_=OR(ff("a", "EQUAL", i(1)), ff("a", "EQUAL", i(3))), order=[asc("b")]))
case("m.x > 1 and b < y (nested and plain inequality)", q("p", where_=AND(ff("m.x", "GREATER_THAN", i(1)), ff("b", "LESS_THAN", s("y")))))

# --- every collection (empty collection ID), as recursiveDelete in nodejs-firestore uses -----
case("every collection at the root", {"from": [{"collectionId": ""}]})
case("every collection under p/p1", {"from": [{"collectionId": ""}]}, parent="p/p1")
case("every descendant", {"from": [{"collectionId": "", "allDescendants": True}]})
case("every descendant of p/p1", {"from": [{"collectionId": "", "allDescendants": True}]}, parent="p/p1")
case("no from", {})
case("every descendant, limit 2", {"from": [{"collectionId": "", "allDescendants": True}], "limit": 2})
case("every descendant in collection p (recursiveDelete range)", {"from": [{"collectionId": "", "allDescendants": True}],
     "where": AND(ff("__name__", "GREATER_THAN_OR_EQUAL", ref("p/__id-9223372036854775808__")),
                  ff("__name__", "LESS_THAN", ref("p\u0000/__id-9223372036854775808__"))),
     "select": {"fields": [f("__name__")]}})
case("every descendant order by __name__ desc", {"from": [{"collectionId": "", "allDescendants": True}], "orderBy": [desc("__name__")]})
case("every collection order by a", {"from": [{"collectionId": ""}], "orderBy": [asc("a")]})
case("every collection where a == 2", {"from": [{"collectionId": ""}], "where": ff("a", "EQUAL", i(2))})

# --- errors ----------------------------------------------------------------------------------
case("two collections", {"from": [{"collectionId": "p"}, {"collectionId": "t"}]})
case("reserved collection ID", q("__x__"))
case("collection ID with a slash", q("a/b"))
case("parent is a collection", q("p"), parent="p")
case("invalid field path", q("p", where_=ff("a..b", "EQUAL", i(1))))
case("negative limit", q("p", limit=-1))
case("negative offset", q("p", offset=-1))
case("in with a non-array value", q("p", where_=ff("a", "IN", i(1))))
case("in with an empty array", q("p", where_=ff("a", "IN", arr())))
case("in with 31 values", q("p", where_=ff("a", "IN", arr(*[i(n) for n in range(31)]))))
case("in with 30 values", q("p", where_=ff("a", "IN", arr(*[i(n) for n in range(30)]))))
case("not-in with 11 values", q("p", where_=ff("a", "NOT_IN", arr(*[i(n) for n in range(11)]))))
case("not-in with 10 values", q("p", where_=ff("a", "NOT_IN", arr(*[i(n) for n in range(10)]))))
case("array-contains-any with an empty array", q("p", where_=ff("tags", "ARRAY_CONTAINS_ANY", arr())))
case("not-in and !=", q("p", where_=AND(ff("a", "NOT_IN", arr(i(1))), ff("b", "NOT_EQUAL", s("x")))))
case("two not-in", q("p", where_=AND(ff("a", "NOT_IN", arr(i(1))), ff("b", "NOT_IN", arr(s("x"))))))
case("in and not-in", q("p", where_=AND(ff("a", "IN", arr(i(1))), ff("b", "NOT_IN", arr(s("x"))))))
case("two array-contains-any", q("p", where_=AND(ff("tags", "ARRAY_CONTAINS_ANY", arr(s("red"))), ff("tags", "ARRAY_CONTAINS_ANY", arr(s("blue"))))))
case("not-in inside or", q("p", where_=OR(ff("a", "NOT_IN", arr(i(1))), ff("b", "EQUAL", s("x")))))
case("two != on different fields", q("p", where_=AND(ff("a", "NOT_EQUAL", i(1)), ff("b", "NOT_EQUAL", s("x")))))
case("empty composite filter", q("p", where_={"compositeFilter": {"op": "AND", "filters": []}}))
case("composite filter without an operator", q("p", where_={"compositeFilter": {"filters": [ff("a", "EQUAL", i(1))]}}))
case("field filter without an operator", q("p", where_={"fieldFilter": {"field": f("a"), "value": i(1)}}))
case("unary filter without an operator", q("p", where_={"unaryFilter": {"field": f("a")}}))
case("order without a direction", q("p", order=[{"field": f("a")}]))
case("cursor with more values than the order", q("p", order=[asc("a")], start=cur(i(1), ref("p/p1"), i(3), before=True)))
case("cursor on __name__ with a string", q("p", start=cur(s("p3"), before=True)))
case("cursor on __name__ in another collection", q("p", start=cur(ref("t/sa"), before=True)))
case("__name__ filter with a string", q("p", where_=ff("__name__", "EQUAL", s("p3"))))
case("__name__ filter in another collection", q("p", where_=ff("__name__", "EQUAL", ref("t/sa"))))
case("__name__ filter on a subcollection document", q("p", where_=ff("__name__", "EQUAL", ref("p/p1/p/sub1"))))
case("is-nan on __name__", q("p", where_=uf("__name__", "IS_NAN")))
case("order by an invalid field path", q("p", order=[asc("a..b")]))
case("array-contains on __name__", q("p", where_=ff("__name__", "ARRAY_CONTAINS", ref("p/p1"))))
case("not-in on __name__ with a string", q("p", where_=ff("__name__", "NOT_IN", arr(s("p1")))))
case("__name__ < a collection path", q("p", where_=ff("__name__", "LESS_THAN", {"referenceValue": "{documents}/p"})))
case("two array-contains", q("p", where_=AND(ff("tags", "ARRAY_CONTAINS", s("red")), ff("tags", "ARRAY_CONTAINS", s("blue")))))

# --- found by the differential fuzzer (tools/fuzz/, #262) ------------------------------------
# A filter of no kind is refused, wherever it is. array-contains and array-contains-any count
# once per disjunction of the filter in disjunctive normal form: two of them in different
# branches of an `or` are allowed, two that end up in one conjunction are not.
case("empty filter", q("p", where_={}))
case("empty filter in an and", q("p", where_=AND(ff("a", "EQUAL", i(1)), {})))
case("empty filter in an or", q("p", where_=OR(ff("a", "EQUAL", i(1)), {})))
case("empty filter without a collection", {"where": {}})
case("array-contains in two branches of an or", q("p", where_=OR(ff("tags", "ARRAY_CONTAINS", s("red")), ff("tags", "ARRAY_CONTAINS", s("blue")))))
case("array-contains or array-contains-any", q("p", where_=OR(ff("tags", "ARRAY_CONTAINS", s("red")), ff("tags", "ARRAY_CONTAINS_ANY", arr(s("blue"), s("x"))))))
case("array-contains-any in two branches of an or", q("p", where_=OR(ff("tags", "ARRAY_CONTAINS_ANY", arr(s("red"))), ff("tags", "ARRAY_CONTAINS_ANY", arr(s("blue"))))))
case("array-contains and array-contains-any", q("p", where_=AND(ff("tags", "ARRAY_CONTAINS", s("red")), ff("tags", "ARRAY_CONTAINS_ANY", arr(s("blue"))))))
case("an or of array-contains, and array-contains", q("p", where_=AND(OR(ff("tags", "ARRAY_CONTAINS", s("red")), ff("tags", "ARRAY_CONTAINS", s("blue"))), ff("tags", "ARRAY_CONTAINS", s("x")))))
case("array-contains, or an and of two", q("p", where_=OR(ff("tags", "ARRAY_CONTAINS", s("red")), AND(ff("tags", "ARRAY_CONTAINS", s("blue")), ff("tags", "ARRAY_CONTAINS", s("x"))))))
# Which problem of a query comes first: the operators of its filter (negations, not-in next to
# in, array-contains-any or an or, even an or of one filter), whatever their fields; then what
# each part names (the collection, the filter's kinds, paths and values, the orders' and the
# projection's paths); then the orders (the key before other fields, then duplicates) and the
# cursors; then the limit; then whether the orders need a collection; then the filter as a whole.
case("two negations and a reserved collection ID", {"from": [{"collectionId": "__x__"}], "where": AND(ff("a", "NOT_EQUAL", i(1)), uf("b", "IS_NOT_NULL"))})
case("two negations without fields", q("p", where_=AND({"fieldFilter": {"op": "NOT_EQUAL"}}, {"unaryFilter": {"op": "IS_NOT_NULL"}})))
case("not-in in an or of one filter", q("p", where_=OR(ff("a", "NOT_IN", arr(i(1))))))
case("not-in in an and of one filter", q("p", where_=AND(ff("a", "NOT_IN", arr(i(1))))))
case("not-in in an or, and a negative limit", q("p", where_=OR(ff("a", "NOT_IN", arr(i(1)))), limit=-1))
case("duplicate orders and a negative limit", q("p", order=[asc("b"), asc("b")], limit=-1))
case("__name__ twice in the order", q("p", order=[asc("__name__"), asc("__name__")]))
case("an inequality, an order by __name__ then b, and a negative limit", q("p", where_=ff("a", "GREATER_THAN", i(1)), order=[asc("__name__"), asc("b")], limit=-1))
case("a cursor with too many values and a negative limit", q("p", order=[asc("a")], start=cur(i(1), i(2), i(3), before=True), limit=-1))
case("an invalid projection path and duplicate orders", q("p", select={"fields": [f("a..b")]}, order=[asc("a"), asc("a")]))
case("an order without a collection and a negative limit", {"orderBy": [asc("b")], "limit": -1})
case("a filter and an order without a collection", {"where": ff("a", "EQUAL", i(1)), "orderBy": [asc("b")]})
case("array-contains on m and m.x without a collection", {"where": AND(ff("m", "ARRAY_CONTAINS", i(1)), ff("m.x", "ARRAY_CONTAINS", i(1)))})
case("duplicate orders and a __name__ filter that is not a key", q("p", where_=ff("__name__", "LESS_THAN_OR_EQUAL", d("Infinity")), order=[asc("a"), asc("a")]))
# The key's equalities (`is null` and `is NaN` on __name__ among them) come with the filter,
# before the orders; array membership and unary filters on __name__ come last.
case("an equality on the key next to an inequality, and duplicate orders", q("p", where_=OR(AND(ff("__name__", "EQUAL", ref("p/p3")), ff("a", "GREATER_THAN_OR_EQUAL", i(2)))), order=[asc("tags"), asc("tags")]))
case("is-null on __name__ next to an inequality", q("p", where_=OR(ff("tags", "LESS_THAN", mp()), uf("__name__", "IS_NULL"))))
case("is-nan on __name__ and an order after the key", q("p", where_=uf("__name__", "IS_NAN"), order=[asc("__name__"), asc("b")]))
case("array-contains on __name__ and duplicate orders", q("p", where_=ff("__name__", "ARRAY_CONTAINS", ref("p/p1")), order=[asc("a"), asc("a")]))
case("array-contains-any on __name__ and a negative limit", q("p", where_=ff("__name__", "ARRAY_CONTAINS_ANY", arr(ref("p/p1"))), limit=-1))
# A value without a type (`{}`, or no value at all) cannot be converted: in a filter, anywhere in
# its value, after the filter's field and before its operator; in a cursor, after the count of
# its values (a query vector's are in find_nearest.py). A field filter reads its field, its
# value, then its operator; a unary filter its field, then its operator. An empty path is
# "Invalid empty property path string." wherever a query names a field.
UNSET = {}
def ffv(path, op, *value):
    out = {"op": op}
    if path is not None:
        out["field"] = f(path)
    if value:
        out["value"] = value[0]
    return {"fieldFilter": out}
case("a filter without a value", q("p", where_=ffv("a", "EQUAL")))
case("a filter with a value without a type", q("p", where_=ffv("a", "EQUAL", UNSET)))
case("in with an element without a type", q("p", where_=ff("a", "IN", arr(i(1), UNSET))))
case("a map holding a value without a type", q("p", where_=ff("a", "EQUAL", mp(k=UNSET))))
case("a nested array holding a value without a type", q("p", where_=ff("a", "EQUAL", arr(arr(UNSET)))))
case("in without a value", q("p", where_=ffv("a", "IN")))
case("in with 31 values, the last without a type", q("p", where_=ff("a", "IN", arr(*([i(1)] * 30 + [UNSET])))))
case("__name__ without a value", q("p", where_=ffv("__name__", "EQUAL")))
case("an invalid path and no value", q("p", where_=ffv("a..b", "EQUAL")))
case("no field and no value", q("p", where_=ffv(None, "EQUAL")))
case("no operator and no value", q("p", where_=ffv("a", "OPERATOR_UNSPECIFIED")))
case("no operator and an invalid path", q("p", where_=ffv("a..b", "OPERATOR_UNSPECIFIED", i(1))))
case("no operator and no field", q("p", where_=ffv(None, "OPERATOR_UNSPECIFIED", i(1))))
case("no value, then an invalid path", q("p", where_=AND(ffv("a", "EQUAL"), ff("b..c", "EQUAL", i(1)))))
case("no value, then an empty filter", q("p", where_=AND(ffv("a", "EQUAL"), {})))
case("no value, then an invalid order", q("p", where_=ffv("a", "EQUAL"), order=[asc("a..b")]))
case("no value and a reserved collection ID", q("__x__", where_=ffv("a", "EQUAL")))
case("two array-contains, one without a value", q("p", where_=AND(OR(ff("tags", "ARRAY_CONTAINS_ANY", arr(UNSET)), ffv("b", "ARRAY_CONTAINS")))))
case("a cursor value without a type", q("p", order=[asc("a")], start=cur(UNSET, before=True)))
case("a cursor value without a type, and too many values", q("p", order=[asc("a")], start=cur(UNSET, i(1), before=True)))
case("a cursor value without a type on __name__", q("p", order=[asc("__name__")], start=cur(UNSET, before=True)))
case("a cursor value without a type, and duplicate orders", q("p", order=[asc("a"), asc("a")], start=cur(UNSET, before=True)))
case("a unary filter without a field", q("p", where_={"unaryFilter": {"op": "IS_NULL"}}))
case("a unary filter without a field or an operator", q("p", where_={"unaryFilter": {"op": "OPERATOR_UNSPECIFIED"}}))
case("a unary filter with an invalid path and no operator", q("p", where_=uf("a..b", "OPERATOR_UNSPECIFIED")))
case("a unary filter with an empty path", q("p", where_=uf("", "IS_NULL")))
case("a filter with an empty path", q("p", where_=ff("", "EQUAL", i(1))))
case("an order with an empty path", q("p", order=[{"field": {}}]))
case("a projection with an empty path", q("p", select={"fields": [{}]}))
# A vector is one value: no field path goes into it, in a filter, an order or a projection.
case("a filter on a path into a vector", q("t", where_=ff("v.value", "EQUAL", arr(d(1), d(2)))))
case("an order by a path into a vector", q("t", order=[asc("v.value")]))
case("a projection of a path into a vector", q("t", where_=ff("v", "EQUAL", vec(1, 2)), select={"fields": [f("v.value"), f("v")]}))
case("a projection of a path into a vector alone", q("t", where_=ff("v", "EQUAL", vec(1, 2)), select={"fields": [f("v.value")]}))
# How disjunctions are counted: the filter is normalized first (an `and` in an `and` and an `or`
# in an `or` flattened, a filter the same as one before it in its composite left out, an `in`'s
# values compared as a set), then put in disjunctive normal form as a set of conjunctions, each
# a set of conditions (an `in` is an equality per value, an `array-contains-any` an
# `array-contains` per value; values the same bit for bit are one). An `and` that would combine
# more than 60 conjunctions stops the count. Array-contains filters are counted on the
# normalized filter.
def ints(lo, hi): return [i(n) for n in range(lo, hi)]
XY = ff("b", "IN", arr(s("x"), s("y")))
case("in with a value twice, and in 2", q("p", where_=AND(ff("a", "IN", arr(*ints(0, 15), i(0))), XY)))
case("in with 1 and 1.0, and in 2", q("p", where_=AND(ff("a", "IN", arr(*ints(0, 15), d(1.0))), XY)))
case("in with NaN twice, and in 2", q("p", where_=AND(ff("a", "IN", arr(*ints(0, 14), NAN, NAN)), XY)))
case("in with an empty array twice, and in 2", q("p", where_=AND(ff("a", "IN", arr(*ints(0, 14), arr(), arr())), XY)))
case("array-contains-any with a value twice, and in 2", q("p", where_=AND(ff("tags", "ARRAY_CONTAINS_ANY", arr(*ints(0, 15), i(0))), XY)))
case("an or of one equality 31 times", q("p", where_=OR(*[ff("a", "EQUAL", i(1))] * 31)))
case("an or of 61 equalities", q("p", where_=OR(*[ff("a", "EQUAL", i(n)) for n in range(61)])))
case("an or of 61 equalities and an equality", q("p", where_=AND(OR(*[ff("a", "EQUAL", i(n)) for n in range(61)]), ff("b", "EQUAL", s("x")))))
case("an or of an in and an equality it holds, and in 2", q("p", where_=AND(OR(ff("a", "IN", arr(*ints(0, 15))), ff("a", "EQUAL", i(0))), XY)))
case("an or of array-contains-any and an array-contains it holds, and in 2", q("p", where_=AND(OR(ff("tags", "ARRAY_CONTAINS_ANY", arr(*ints(0, 15))), ff("tags", "ARRAY_CONTAINS", i(0))), XY)))
case("an or of in and array-contains-any of one value, and in 2", q("p", where_=AND(OR(ff("a", "IN", arr(*ints(0, 15))), ff("a", "ARRAY_CONTAINS_ANY", arr(i(0)))), XY)))
case("one in twice, its values in another order", q("p", where_=AND(ff("a", "IN", arr(*ints(0, 10))), ff("a", "IN", arr(*reversed(ints(0, 10)))))))
case("one in twice, once in a nested and", q("p", where_=AND(ff("a", "IN", arr(*ints(0, 10))), AND(ff("b", "EQUAL", s("x")), ff("a", "IN", arr(*reversed(ints(0, 10))))))))
case("two ins on one field", q("p", where_=AND(ff("a", "IN", arr(*ints(0, 8))), ff("a", "IN", arr(*ints(0, 7), i(99))))))
case("in 6 and in 10", q("p", where_=AND(ff("a", "IN", arr(*ints(0, 6))), ff("b", "IN", arr(*ints(0, 10))))))
case("in 8 and in 8", q("p", where_=AND(ff("a", "IN", arr(*ints(0, 8))), ff("b", "IN", arr(*ints(0, 8))))))
case("one conjunction in two orders, and in 2", q("p", where_=AND(OR(AND(ff("a", "EQUAL", i(1)), ff("m.x", "EQUAL", i(1))), AND(ff("m.x", "EQUAL", i(1)), ff("a", "EQUAL", i(1))), *[ff("a", "EQUAL", i(n)) for n in range(2, 16)]), XY)))
case("a conjunction and a part of it, and in 2", q("p", where_=AND(OR(ff("a", "EQUAL", i(1)), AND(ff("a", "EQUAL", i(1)), ff("m.x", "EQUAL", i(1))), *[ff("a", "EQUAL", i(n)) for n in range(2, 16)]), XY)))
case("an equality twice in an and", q("p", where_=AND(OR(AND(ff("a", "EQUAL", i(1)), ff("a", "EQUAL", i(1))), *[ff("a", "EQUAL", i(n)) for n in range(1, 16)]), XY)))
case("one array-contains twice", q("p", where_=AND(ff("tags", "ARRAY_CONTAINS", s("red")), ff("tags", "ARRAY_CONTAINS", s("red")))))
case("one array-contains-any twice, its values in another order", q("p", where_=AND(ff("tags", "ARRAY_CONTAINS_ANY", arr(s("red"), s("blue"))), ff("tags", "ARRAY_CONTAINS_ANY", arr(s("blue"), s("red"))))))
case("an array-contains and an or holding it", q("p", where_=AND(ff("tags", "ARRAY_CONTAINS", s("red")), OR(ff("tags", "ARRAY_CONTAINS", s("red")), ff("b", "EQUAL", s("x"))))))

# --- equality on __name__ next to inequalities (#74) -----------------------------------------
# Refused unless __name__ has an inequality too; checked after the other filter checks, and
# before a __name__ value that is not a reference is refused.
case("a != 1 and __name__ == p3", q("p", where_=AND(ff("a", "NOT_EQUAL", i(1)), ff("__name__", "EQUAL", ref("p/p3")))))
case("__name__ == p3 and a > 1, order by a", q("p", where_=AND(ff("__name__", "EQUAL", ref("p/p3")), ff("a", "GREATER_THAN", i(1))), order=[asc("a")]))
case("a not-in [1] and __name__ in [p3]", q("p", where_=AND(ff("a", "NOT_IN", arr(i(1))), ff("__name__", "IN", arr(ref("p/p3"))))))
case("opt is not null and __name__ == p3", q("p", where_=AND(uf("opt", "IS_NOT_NULL"), ff("__name__", "EQUAL", ref("p/p3")))))
case("a is not nan and __name__ == p3", q("p", where_=AND(uf("a", "IS_NOT_NAN"), ff("__name__", "EQUAL", ref("p/p3")))))
case("a > 1 or __name__ == p3", q("p", where_=OR(ff("a", "GREATER_THAN", i(1)), ff("__name__", "EQUAL", ref("p/p3")))))
case("a > 1 and (__name__ == p3 or b == x)", q("p", where_=AND(ff("a", "GREATER_THAN", i(1)), OR(ff("__name__", "EQUAL", ref("p/p3")), ff("b", "EQUAL", s("x"))))))
case("a > 1 and __name__ == t/sa", q("p", where_=AND(ff("a", "GREATER_THAN", i(1)), ff("__name__", "EQUAL", ref("t/sa")))))
case("a > 1, __name__ > p1 and __name__ == p3", q("p", where_=AND(ff("a", "GREATER_THAN", i(1)), ff("__name__", "GREATER_THAN", ref("p/p1")), ff("__name__", "EQUAL", ref("p/p3")))))
case("a > 1, __name__ not-in [p1] and __name__ == p3", q("p", where_=AND(ff("a", "GREATER_THAN", i(1)), ff("__name__", "NOT_IN", arr(ref("p/p1"))), ff("__name__", "EQUAL", ref("p/p3")))))
case("a == 3 and __name__ == p3", q("p", where_=AND(ff("a", "EQUAL", i(3)), ff("__name__", "EQUAL", ref("p/p3")))))
case("opt is null and __name__ == p3", q("p", where_=AND(uf("opt", "IS_NULL"), ff("__name__", "EQUAL", ref("p/p3")))))
case("tags array-contains red and __name__ == p3", q("p", where_=AND(ff("tags", "ARRAY_CONTAINS", s("red")), ff("__name__", "EQUAL", ref("p/p3")))))
case("a > 1 and __name__ != p3", q("p", where_=AND(ff("a", "GREATER_THAN", i(1)), ff("__name__", "NOT_EQUAL", ref("p/p3")))))
case("group p, a > 1 and __name__ == p/p3", q("p", all_descendants=True, where_=AND(ff("a", "GREATER_THAN", i(1)), ff("__name__", "EQUAL", ref("p/p3")))))
case("a != 1, b != x and __name__ == p3", q("p", where_=AND(ff("a", "NOT_EQUAL", i(1)), ff("b", "NOT_EQUAL", s("x")), ff("__name__", "EQUAL", ref("p/p3")))))
case("a > 1, __name__ == p3 and two array-contains", q("p", where_=AND(ff("a", "GREATER_THAN", i(1)), ff("__name__", "EQUAL", ref("p/p3")), ff("tags", "ARRAY_CONTAINS", s("red")), ff("tags", "ARRAY_CONTAINS", s("blue")))))
case("a > 1 and __name__ == a string", q("p", where_=AND(ff("a", "GREATER_THAN", i(1)), ff("__name__", "EQUAL", s("p3")))))
case("a != 1, b != x and __name__ == a string", q("p", where_=AND(ff("a", "NOT_EQUAL", i(1)), ff("b", "NOT_EQUAL", s("x")), ff("__name__", "EQUAL", s("p3")))))
case("__name__ == a string and two array-contains", q("p", where_=AND(ff("__name__", "EQUAL", s("p3")), ff("tags", "ARRAY_CONTAINS", s("red")), ff("tags", "ARRAY_CONTAINS", s("blue")))))
case("__name__ > a string and a > 1", q("p", where_=AND(ff("__name__", "GREATER_THAN", s("p3")), ff("a", "GREATER_THAN", i(1)))))
case("__name__ == a collection path and a > 1", q("p", where_=AND(ff("__name__", "EQUAL", {"referenceValue": "{documents}/p"}), ff("a", "GREATER_THAN", i(1)))))
case("array-contains + array-contains-any", q("p", where_=AND(ff("tags", "ARRAY_CONTAINS", s("red")), ff("tags", "ARRAY_CONTAINS_ANY", arr(s("blue"))))))
case("in 16 x in 2 (32 disjunctions)", q("p", where_=AND(ff("a", "IN", arr(*[i(n) for n in range(16)])), ff("b", "IN", arr(s("x"), s("y"))))))
case("or of 31 equalities", q("p", where_=OR(*[ff("a", "EQUAL", i(n)) for n in range(31)])))
case("in 30 x in 2 (60 disjunctions)", q("p", where_=AND(ff("a", "IN", arr(*[i(n) for n in range(30)])), ff("b", "IN", arr(s("x"), s("y"))))))
case("in 7 x in 9 (63 disjunctions)", q("p", where_=AND(ff("a", "IN", arr(*[i(n) for n in range(7)])), ff("b", "IN", arr(*[s(str(n)) for n in range(9)])))))
case("or of 31 equalities x in 2 (62 disjunctions)", q("p", where_=AND(OR(*[ff("a", "EQUAL", i(n)) for n in range(31)]), ff("b", "IN", arr(s("x"), s("y"))))))
case("array-contains-any with 31 values", q("p", where_=ff("tags", "ARRAY_CONTAINS_ANY", arr(*[i(n) for n in range(31)]))))
case("is-not-null and is-not-nan", q("p", where_=AND(uf("a", "IS_NOT_NULL"), uf("b", "IS_NOT_NAN"))))
# --- field paths (#262) ----------------------------------------------------------------------
# A path is read as everywhere (rest.py): array offsets, `__key__` refused as it is read,
# `__name__` read as `__key__`, then reserved names but `__key__` and `__type__`. The filter's
# paths are read first, then the projection's, then the orders'. A projection is then checked
# as a mask is (`__key__` in a longer path, `__type__`, array offsets), before the orders are
# read. Filters and orders are checked late: after the orders, the limit and whether the orders
# need a collection, filter by filter (what its path holds, whether it needs a collection, the
# checks of __name__), then the orders. There `__type__` may only end a longer path, and the
# messages print paths unquoted, array offsets as `[n]`.
def sel(*paths): return {"fields": [f(p) for p in paths]}
for p in ["__key__", "a.__key__", "`__key__`", "__key__.a b", "__x__", "a.__x__", "__type__.__x__", "a[2147483648]",
          "__type__", "__type__.a", "a.__type__", "a.b.__type__", "a.__type__.b", "a.__type__[0]", "a[0].__type__",
          "__name__.a", "a.__name__", "__name__[0]", "a[0]", "a[0][1]", "a[0].__x__", "__x__[0]", "`__name__`", "`__type__`"]:
    case(f"filter on {p}", q("p", where_=ff(p, "EQUAL", i(1))))
for p in ["__key__", "__x__", "__type__", "a.__type__", "a.__type__.b", "a.__name__", "a[0]", "__name__[0]"]:
    case(f"unary filter on {p}", q("p", where_=uf(p, "IS_NULL")))
    case(f"order by {p}", q("p", order=[asc(p)]))
for p in ["__key__", "__x__", "__type__", "a.__type__", "__type__.a", "a.__name__", "a[0]", "__name__[0]", "a[0].__x__"]:
    case(f"select {p}", q("p", select=sel(p)))
case("a reserved name in a filter before an unknown operator", q("p", where_=ffv("__x__", "OPERATOR_UNSPECIFIED", i(1))))
case("a reserved name in a filter before a value without a type", q("p", where_=ffv("__x__", "EQUAL", UNSET)))
case("an array offset in a filter after a value without a type", q("p", where_=ffv("a[0]", "EQUAL", UNSET)))
case("an array offset in a filter after an unknown operator", q("p", where_=ffv("a[0]", "OPERATOR_UNSPECIFIED", i(1))))
case("an array offset in a filter after in without an array", q("p", where_=ff("a[0]", "IN", i(1))))
case("the filter's paths are read before the projection's", q("p", where_=ff("__x__", "EQUAL", i(1)), select=sel("__y__"), order=[asc("__z__")]))
case("the projection's paths are read before the orders'", q("p", select=sel("__x__"), order=[asc("__y__")]))
case("the projection is checked before the orders are read", q("p", select=sel("a[0]"), order=[asc("__x__")]))
case("an invalid projection path before an invalid order path", q("p", select=sel("a..b"), order=[asc("c..d")]))
case("every projection path is read before any is checked", q("p", select=sel("a[0]", "__x__")))
case("an array offset in the projection before a cursor value without a type", q("p", select=sel("a[0]"), order=[asc("b")], start=cur(UNSET, before=True)))
case("an array offset in a filter before a reserved order path", q("p", where_=ff("a[0]", "EQUAL", i(1)), order=[asc("__x__")]))
case("an array offset in an order after a cursor value without a type", q("p", order=[asc("a[0]")], start=cur(UNSET, before=True)))
case("an array offset in an order after too many cursor values", q("p", order=[asc("a[0]")], start=cur(i(1), i(2), before=True)))
case("an array offset in an order after a negative limit", q("p", order=[asc("a[0]")], limit=-1))
case("an array offset in an order after a negative offset", q("p", order=[asc("a[0]")], offset=-1))
case("an array offset in a filter after a negative limit", q("p", where_=ff("a[0]", "EQUAL", i(1)), limit=-1))
case("an array offset in an order after duplicate orders", q("p", order=[asc("a[0]"), asc("b"), asc("b")]))
case("duplicate orders with array offsets", q("p", order=[asc("a[01]"), asc("a[1]")]))
case("duplicate orders printed unquoted", q("p", order=[asc("`a b`"), asc("`a b`")]))
case("duplicate orders of a quoted path with a dot", q("p", order=[asc("`a.b`"), asc("`a.b`")]))
case("duplicate orders of __name__ in a longer path", q("p", order=[asc("a.__name__"), asc("a.__name__")]))
case("an array offset in an order after a field after the key", q("p", order=[asc("__name__"), asc("a[0]")]))
case("an inequality on an array offset before an order by the key", q("p", where_=ff("a[0]", "GREATER_THAN", i(1)), order=[asc("__name__")]))
case("an array offset in a filter after two array-contains", q("p", where_=AND(ff("a[0]", "ARRAY_CONTAINS", i(1)), ff("b", "ARRAY_CONTAINS", i(1)))))
case("an array offset in a filter after too many disjunctions", q("p", where_=AND(ff("a[0]", "IN", arr(*ints(0, 16))), ff("b", "IN", arr(s("x"), s("y"))))))
case("an array offset in a filter after an order without a collection", {"where": ff("a[0]", "EQUAL", i(1)), "orderBy": [asc("b")]})
case("an array offset in a filter without a collection", {"where": ff("a[0]", "EQUAL", i(1))})
case("__type__ in a filter without a collection", {"where": ff("__type__", "EQUAL", i(1))})
case("a filter without a collection before __type__ in the next", {"where": AND(ff("b", "EQUAL", i(1)), ff("__type__", "EQUAL", i(1)))})
case("__type__ in a filter before a filter without a collection", {"where": AND(ff("__type__", "EQUAL", i(1)), ff("b", "EQUAL", i(1)))})
case("__name__ not a key before a filter without a collection", {"where": AND(ff("__name__", "EQUAL", i(1)), ff("b", "EQUAL", i(1)))})
case("is null on __name__ before a filter without a collection", {"where": AND(uf("__name__", "IS_NULL"), ff("b", "EQUAL", i(1)))})
case("a filter without a collection, quoted path", {"where": ff("`a b`", "EQUAL", i(1))})
case("__name__ not a key before an array offset in the next filter", q("p", where_=AND(ff("__name__", "EQUAL", i(1)), ff("a[0]", "EQUAL", i(1)))))
case("an array offset in a filter before __name__ not a key", q("p", where_=AND(ff("a[0]", "EQUAL", i(1)), ff("__name__", "EQUAL", i(1)))))
case("array-contains on __name__ before an array offset", q("p", where_=AND(ff("__name__", "ARRAY_CONTAINS", ref("p/p1")), ff("a[0]", "EQUAL", i(1)))))
case("__name__ not a key before an array offset in an order", q("p", where_=ff("__name__", "EQUAL", i(1)), order=[asc("a[0]")]))
case("__type__ in a filter before an array offset in an order", q("p", where_=ff("__type__", "EQUAL", i(1)), order=[asc("a[0]")]))
case("an array offset in a filter before __type__ in an order", q("p", where_=ff("a[0]", "EQUAL", i(1)), order=[asc("__type__")]))
case("an order by a path ending in __type__", q("p", where_=ff("a.__type__", "EQUAL", i(1)), order=[asc("__name__")]))
case("an inequality on a path ending in __type__ before an order by the key", q("p", where_=ff("a.__type__", "GREATER_THAN", i(1)), order=[asc("__name__")]))
case("a quoted __name__ is the key", q("p", order=[asc("`__name__`")]))
# A filter's or a cursor's value is read as a written one is (rest.py), with the filter's field
# and before its operator: references as parent names (the documents root and the database
# are), geo points within range, reserved names in maps, `__type__` a string, vectors' shapes.
# A filter on `__name__` reads its references as keys too, late, with its other checks: not
# empty, of the query's database, IDs within the key's limits.
def geo(**c): return {"geoPointValue": c}
for label, value in [("c/d", {"referenceValue": "c/d"}), ("the documents root", {"referenceValue": "{documents}"}),
                     ("the database", {"referenceValue": "{database}"}),
                     ("a collection", {"referenceValue": "{documents}/c"}), ("a reserved collection ID", {"referenceValue": "{documents}/__x__/d"})]:
    case(f"a filter on a reference to {label}", q("p", where_=ff("a", "EQUAL", value)))
case("a filter on a reference in a map", q("p", where_=ff("a", "LESS_THAN", mp(k={"referenceValue": "c/d"}))))
case("in with a reference", q("p", where_=ff("a", "IN", arr(i(1), {"referenceValue": "c/d"}))))
case("a reference before not-in without an array", q("p", where_=ff("a", "NOT_IN", {"referenceValue": "c/d"})))
case("a reference before an unknown operator", q("p", where_=ffv("a", "OPERATOR_UNSPECIFIED", {"referenceValue": "c/d"})))
case("a reference before a value without a type", q("p", where_=ff("a", "EQUAL", arr({"referenceValue": "c/d"}, UNSET))))
case("a value without a type before a reference", q("p", where_=ff("a", "EQUAL", arr(UNSET, {"referenceValue": "c/d"}))))
case("a reference before an array offset in the filter's path", q("p", where_=ff("a[0]", "EQUAL", {"referenceValue": "c/d"})))
case("a filter on a geo point out of range", q("p", where_=ff("a", "EQUAL", geo(latitude=91))))
case("a geo point out of range before not-in without an array", q("p", where_=ff("a", "NOT_IN", geo(longitude=-180.5))))
case("a filter on a map with a reserved name", q("p", where_=ff("a", "EQUAL", mp(__x__=i(1)))))
case("a filter on a map with __type__ not a string", q("p", where_=ff("a", "EQUAL", mp(__type__=i(1)))))
case("a filter on a map with an unknown __type__", q("p", where_=ff("a", "EQUAL", mp(__type__=s("other")))))
case("a filter on a vector of integers", q("p", where_=ff("a", "EQUAL", mp(__type__=s("__vector__"), value=arr(i(1))))))
case("a filter on a vector without values", q("p", where_=ff("a", "EQUAL", mp(__type__=s("__vector__")))))
case("a filter on a vector holding a reference", q("p", where_=ff("a", "EQUAL", mp(__type__=s("__vector__"), value=arr({"referenceValue": "c/d"})))))
case("a filter on a vector holding NaN", q("p", where_=ff("a", "EQUAL", vec("NaN"))))
case("a vector of integers before not-in without an array", q("p", where_=ff("a", "NOT_IN", mp(__type__=s("__vector__"), value=arr(i(1))))))
case("a cursor on a reference to c/d", q("p", order=[asc("a")], start=cur({"referenceValue": "c/d"}, before=True)))
case("a cursor on a geo point out of range", q("p", order=[asc("a")], start=cur(geo(latitude=91), before=True)))
case("a cursor on a vector of integers", q("p", order=[asc("a")], start=cur(mp(__type__=s("__vector__"), value=arr(i(1))), before=True)))
case("a cursor on __name__ at the documents root", q("p", order=[asc("__name__")], start=cur({"referenceValue": "{documents}"}, before=True)))
case("a reference before too many cursor values", q("p", order=[asc("a")], start=cur({"referenceValue": "c/d"}, i(1), before=True)))
case("__name__ equal to the documents root", q("p", where_=ff("__name__", "EQUAL", {"referenceValue": "{documents}"})))
case("__name__ equal to the database", q("p", where_=ff("__name__", "EQUAL", {"referenceValue": "{database}"})))
case("__name__ equal to the documents root, and duplicate orders", q("p", where_=ff("__name__", "EQUAL", {"referenceValue": "{documents}"}), order=[asc("a"), asc("a")]))
case("__name__ in the database, and duplicate orders", q("p", where_=ff("__name__", "IN", arr({"referenceValue": "{database}"})), order=[asc("a"), asc("a")]))
case("__name__ equal to c/d, and duplicate orders", q("p", where_=ff("__name__", "EQUAL", {"referenceValue": "c/d"}), order=[asc("a"), asc("a")]))
case("__name__ equal to a document of another project", q("p", where_=ff("__name__", "EQUAL", {"referenceValue": "projects/other/databases/(default)/documents/p/p1"})))
case("__name__ equal to a document ID over 1500 bytes", q("p", where_=ff("__name__", "EQUAL", {"referenceValue": "{documents}/p/" + "x" * 1501})))
case("__name__ equal to the documents root before another filter's array offset", q("p", where_=AND(ff("__name__", "EQUAL", {"referenceValue": "{documents}"}), ff("a[0]", "EQUAL", i(1)))))
# find_nearest is #26; recorded for reference, not replayed.
case("find_nearest", q("t", findNearest={"vectorField": f("v"), "queryVector": vec(1, 2), "distanceMeasure": "EUCLIDEAN", "limit": 2}))


# --- running ---------------------------------------------------------------------------------

def fill(value):
    if isinstance(value, str):
        return value.replace("{documents}", DOCUMENTS).replace("{database}", DOCUMENTS[:-len("/documents")])
    if isinstance(value, list):
        return [fill(v) for v in value]
    if isinstance(value, dict):
        return {k: fill(v) for k, v in value.items()}
    return value


def call(method, path, body):
    req = urllib.request.Request(f"http://{HOST}/v1/{path}", method=method, data=json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json", "Authorization": "Bearer owner"})
    try:
        with urllib.request.urlopen(req, timeout=30) as res:
            return "OK", "", json.loads(res.read())
    except urllib.error.HTTPError as err:
        raw = err.read()
        try:
            e = json.loads(raw)["error"]
            return e.get("status", str(err.code)), e.get("message", ""), None
        except (ValueError, KeyError):
            return str(err.code), raw.decode(errors="replace"), None


for doc in DATASET:
    status, message, _ = call("POST", f"{DOCUMENTS}:commit", {"writes": [{"update": fill({"name": "{documents}/" + doc["name"], "fields": doc["fields"]})}]})
    assert status == "OK", (doc, status, message)

for c in CASES:
    parent = DOCUMENTS + ("/" + c["parent"] if c["parent"] else "")
    status, message, responses = call("POST", f"{parent}:runQuery", {"structuredQuery": fill(c["query"])})
    outcome = {"status": status}
    if status != "OK":
        outcome["message"] = message.replace(PROJECT, "{project}")
    else:
        outcome["responses"] = []
        fields = {}
        for r in responses:
            entry = {}
            if "document" in r:
                name = r["document"]["name"][len(DOCUMENTS) + 1:]
                entry["document"] = name
                fields[name] = json.loads(json.dumps(r["document"].get("fields", {})).replace(DOCUMENTS, "{documents}"))
            if "skippedResults" in r:
                entry["skipped"] = r["skippedResults"]
            if r.get("done"):
                entry["done"] = True
            outcome["responses"].append(entry)
        if "select" in c["query"]:
            outcome["fields"] = fields
    c["outcome"] = outcome

json.dump({
    "oracle": "cloud-firestore-emulator v1.22.0 (sha256 9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)",
    "generated_by": "tools/oracle/queries.py",
    "dataset": DATASET,
    "cases": CASES,
}, sys.stdout, ensure_ascii=False, indent=1)
print()
