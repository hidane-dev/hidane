#!/usr/bin/env python3
"""How does the official emulator in Datastore mode answer queries? (#81)

Usage (from the repository root), with a jar freshly started with `--database-mode
datastore-mode` under an English locale:
    python3 -I tools/oracle/datastore_queries.py 127.0.0.1:8086 \\
        > crates/hidane/tests/fixtures/datastore_queries.json

Seeds one project with a dataset (`DATASET`: a value of every type in property `v` of kind
`T`, entities with several properties of kind `P`, an ancestry, a namespace), then sends, over
REST in order, `runQuery` with structured queries (kinds, every filter operator against values
of every type, composite filters, ancestors, keys, orders, projections, `distinctOn`, limits,
offsets, cursors, cursors of other queries (a distinct query's, which has no key; one whose
values end before a property; in aggregations), ancestors in disjunctions, the key projected
and compared, a limit of 0 over filters that bound a property to nothing, the entity values of
arrays (not indexed), the queries it refuses, and a query without a kind in a transaction), the
metadata kinds `__kind__`, `__namespace__` and `__property__` (in transactions too), key
operands of an invalid project or in a reserved namespace, a property filtered by equality and
ordered (a row per value), and `runAggregationQuery` (the rows a sum reads from arrays, in each
order and under equalities). A cursor is opaque: a step that reads one names it (`defines`,
with the JSON path it came from), and later steps send it as `<name>`, so a replay sends its
own. The fixture has the format of `datastore_entities.json`; server times,
versions, transaction IDs and cursors are masked. `crates/hidane/tests/datastore.rs` replays
it. Stdlib only.
"""

import http.client
import json
import re
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from harness import mask  # noqa: E402

HOST = sys.argv[1]
PROJECT = f"dsquery-{time.strftime('%H%M%S')}"
P = "{project}"
JSON = {"Content-Type": "application/json"}


def key(*path, ns=None):
    out = {"path": [dict(kind=k, **({"name": i} if isinstance(i, str) else {"id": str(i)})) for k, i in path]}
    if ns is not None:
        out["partitionId"] = {"namespaceId": ns}
    return out


I = lambda n: {"integerValue": str(n)}
D = lambda x: {"doubleValue": x}
S = lambda s: {"stringValue": s}
B = lambda b: {"booleanValue": b}
NULL = {"nullValue": None}
TS = lambda t: {"timestampValue": t}
KEY = lambda *p: {"keyValue": key(*p)}
ARR = lambda *v: {"arrayValue": {"values": list(v)} if v else {}}
ENT = lambda **p: {"entityValue": {"properties": p}}

VALUES = {
    "null": NULL, "false": B(False), "true": B(True),
    "int-1": I(-1), "int0": I(0), "int1": I(1), "int2": I(2), "intbig": I(1577836800000000),
    "dbl-inf": D("-Infinity"), "dbl-0.5": D(-0.5), "dbl0": D(0), "dbl1": D(1), "dbl1.5": D(1.5), "dblinf": D("Infinity"), "nan": D("NaN"),
    "ts": TS("2020-01-01T00:00:00Z"), "ts2": TS("2021-06-01T12:00:00.5Z"),
    "str": S("a"), "strb": S("b"), "strempty": S(""), "strutf": S("é"),
    "blob": {"blobValue": "YQ=="}, "blobb": {"blobValue": "Yg=="},
    "geo": {"geoPointValue": {"latitude": 1, "longitude": 2}}, "geo2": {"geoPointValue": {"latitude": 1, "longitude": 3}},
    "key": KEY(("A", "x")), "key2": KEY(("A", 5)), "keyns": {"keyValue": key(("A", "x"), ns="ns1")},
    "arr": ARR(I(5), S("z")), "arrempty": ARR(), "arrdup": ARR(I(1), I(1)),
    "ent": ENT(x=I(1)),
    "excluded": {"integerValue": "1", "excludeFromIndexes": True},
    "exarr": ARR({"integerValue": "1", "excludeFromIndexes": True}, I(7)),
}
DATASET = [(key(("T", name)), {"v": value}) for name, value in VALUES.items()]
DATASET.append((key(("T", "nov")), {"w": I(1)}))
DATASET += [
    (key(("T", 3)), {"v": I(3)}),
    (key(("T", -3)), {"v": I(-3)}),
]
PEOPLE = [
    ("p1", {"a": I(1), "b": S("x"), "tags": ARR(S("red"), S("blue")), "m": ENT(x=I(1))}),
    ("p2", {"a": I(2), "b": S("y"), "tags": ARR(S("red"))}),
    ("p3", {"a": I(2), "b": S("x"), "tags": ARR()}),
    ("p4", {"a": I(3), "b": S("z"), "tags": ARR(S("blue"), S("green"))}),
    ("p5", {"a": I(3), "b": S("y")}),
    ("p6", {"a": I(-1), "b": S("x"), "tags": ARR(S("green"))}),
    ("p7", {"b": S("w")}),
    ("p8", {"a": D(4.5), "b": S("x")}),
]
DATASET += [(key(("P", name)), props) for name, props in PEOPLE]
DATASET += [
    (key(("Q", "q1")), {"n": I(1)}),
    (key(("Q", "q1"), ("C", "c1")), {"n": I(2)}),
    (key(("Q", "q1"), ("C", "c2"), ("G", "g1")), {"n": I(3)}),
    (key(("Q", "q2"), ("C", "c3")), {"n": I(4)}),
    (key(("Q", 1), ("C", "c4")), {"n": I(5)}),
    (key(("C", "top")), {"n": I(6)}),
    (key(("T", "inns"), ns="ns1"), {"v": I(1)}),
    (key(("Other", "o"), ns="ns2"), {"v": I(2)}),
]
DATASET += [(key(("Many", f"m{i:03d}")), {"i": I(i)}) for i in range(350)]
# For cursors of other queries: a namespace of its own, so that queries without a kind see
# only these.
DATASET += [(key(("L", f"l{i}"), ns="xc"), {"a": I(i), "b": I(10 - i)}) for i in (1, 2, 3)]
DATASET.append((key(("L", "n1"), ns="xcns"), {}))
# Arrays of entity values, in a namespace of their own: their properties are not indexed.
DATASET += [
    (key(("Arr", "r1"), ns="xa"), {"arr": ARR(ENT(x=I(5)), ENT(x=I(6))), "e": ENT(t=ARR(I(3), I(4))), "n": I(1)}),
    (key(("Arr", "r2"), ns="xa"), {"arr": ARR(ENT(x=I(7))), "e": ENT(t=ARR(I(8))), "n": I(2)}),
]
# The rows a sum reads, in a namespace of their own: an array's values, ordered among the others.
DATASET += [
    (key(("G", "g1"), ns="xg"), {"a": ARR(I(5), I(1)), "p": I(5), "e": ENT(x={"blobValue": "AQ=="})}),
    (key(("G", "g2"), ns="xg"), {"a": ARR(I(3)), "p": I(5), "e": ENT(x=D(1.0))}),
    (key(("H", "h1"), ns="xg"), {"a": I(1), "z": I(9)}),
    (key(("H", "h2"), ns="xg"), {"a": I(2), "z": I(1)}),
    (key(("E", "e1"), ns="xg"), {"b": ARR(I(1), I(2)), "c": I(1), "d": ARR(I(5), I(6))}),
    (key(("E", "e2"), ns="xg"), {"b": ARR(I(2), I(3)), "c": I(2), "d": I(7)}),
]
# More cursors of other queries, in a namespace of their own.
DATASET += [
    (key(("D", "d1"), ns="xd"), {"t": ARR(NULL)}),
    (key(("D", "d2"), ns="xd"), {"a": I(1), "b": I(1), "c": I(5), "t": ARR(I(4))}),
    (key(("D", "d3"), ns="xd"), {"a": I(2), "b": I(2), "c": I(6)}),
    (key(("D", "d4"), ns="xd"), {"a": I(3), "b": NULL}),
    (key(("DP", "p1"), ns="xd"), {"b": I(5)}),
]

CASES = []


def case(name, verb, body, defines=None):
    c = {"name": name, "method": "POST", "path": f"/v1/projects/{P}:{verb}", "headers": JSON,
         "body": body, "compare": "exact"}
    if defines:
        c["defines"] = defines
    CASES.append(c)


def query(name, q, defines=None, **more):
    case(name, "runQuery", {"query": q, **more}, defines)


def kind(k):
    return [{"name": k}]


def pf(prop, op, value):
    return {"propertyFilter": {"property": {"name": prop}, "op": op, "value": value}}


def AND(*fs):
    return {"compositeFilter": {"op": "AND", "filters": list(fs)}}


def OR(*fs):
    return {"compositeFilter": {"op": "OR", "filters": list(fs)}}


def asc(prop):
    return {"property": {"name": prop}}


def desc(prop):
    return {"property": {"name": prop}, "direction": "DESCENDING"}


# --- kinds and orders ----------------------------------------------------------------------
query("kind T", {"kind": kind("T")})
query("kind P", {"kind": kind("P")})
query("kind Q with its children", {"kind": kind("C")})
query("no kind", {})
query("a kind that has no entities", {"kind": kind("Nothing")})
query("in namespace ns1", {"kind": kind("T")}, partitionId={"namespaceId": "ns1"})
query("no kind in namespace ns2", {}, partitionId={"namespaceId": "ns2"})
query("order by v", {"kind": kind("T"), "order": [asc("v")]})
query("order by v descending", {"kind": kind("T"), "order": [desc("v")]})
query("order by __key__", {"kind": kind("T"), "order": [asc("__key__")]})
query("order by __key__ descending", {"kind": kind("T"), "order": [desc("__key__")]})
query("order by a then b", {"kind": kind("P"), "order": [asc("a"), asc("b")]})
query("order by b then a descending", {"kind": kind("P"), "order": [asc("b"), desc("a")]})
query("order by tags", {"kind": kind("P"), "order": [asc("tags")]})
query("order by tags descending", {"kind": kind("P"), "order": [desc("tags")]})
query("order by a missing property", {"kind": kind("P"), "order": [asc("nope")]})
query("order by an entity value", {"kind": kind("P"), "order": [asc("m")]})
query("order by a property of an entity value", {"kind": kind("P"), "order": [asc("m.x")]})
query("order twice by a", {"kind": kind("P"), "order": [asc("a"), desc("a")]})
query("order without a kind", {"order": [asc("v")]})
query("order by __key__ without a kind", {"order": [asc("__key__")]})

# --- filters on v, every operator against values of every type -------------------------------
OPERANDS = {"null": NULL, "int1": I(1), "dbl1": D(1), "true": B(True), "str a": S("a"), "blob a": {"blobValue": "YQ=="},
            "ts": TS("2020-01-01T00:00:00Z"), "int as ts": I(1577836800000000), "nan": D("NaN"), "geo": VALUES["geo"],
            "key": KEY(("A", "x")), "empty array": ARR(), "array": ARR(I(5)), "entity": ENT(x=I(1))}
for op in ["EQUAL", "NOT_EQUAL", "LESS_THAN", "LESS_THAN_OR_EQUAL", "GREATER_THAN", "GREATER_THAN_OR_EQUAL"]:
    for label, value in OPERANDS.items():
        query(f"v {op} {label}", {"kind": kind("T"), "filter": pf("v", op, value)})
query("v IN [int1, str a, null]", {"kind": kind("T"), "filter": pf("v", "IN", ARR(I(1), S("a"), NULL))})
query("v NOT_IN [int1, str a]", {"kind": kind("T"), "filter": pf("v", "NOT_IN", ARR(I(1), S("a")))})
query("v NOT_IN [null]", {"kind": kind("T"), "filter": pf("v", "NOT_IN", ARR(NULL))})
query("v IN []", {"kind": kind("T"), "filter": pf("v", "IN", ARR())})
query("v IN a value, not an array", {"kind": kind("T"), "filter": pf("v", "IN", I(1))})
query("v IN 31 values", {"kind": kind("T"), "filter": pf("v", "IN", ARR(*[I(i) for i in range(31)]))})
query("v IN 30 values", {"kind": kind("T"), "filter": pf("v", "IN", ARR(*[I(i) for i in range(30)]))})
query("v NOT_IN 11 values", {"kind": kind("T"), "filter": pf("v", "NOT_IN", ARR(*[I(i) for i in range(11)]))})
query("v NOT_IN 10 values", {"kind": kind("T"), "filter": pf("v", "NOT_IN", ARR(*[I(i) for i in range(10)]))})
query("v > 0 ordered by v descending", {"kind": kind("T"), "filter": pf("v", "GREATER_THAN", I(0)), "order": [desc("v")]})
query("v > 0 ordered by __key__", {"kind": kind("T"), "filter": pf("v", "GREATER_THAN", I(0)), "order": [asc("__key__")]})
query("w = 1 without a kind", {"filter": pf("w", "EQUAL", I(1))})
query("an unknown operator", {"kind": kind("T"), "filter": pf("v", "OPERATOR_UNSPECIFIED", I(1))})
query("a filter without a property", {"kind": kind("T"), "filter": {"propertyFilter": {"op": "EQUAL", "value": I(1)}}})
query("a filter without a value", {"kind": kind("T"), "filter": {"propertyFilter": {"property": {"name": "v"}, "op": "EQUAL"}}})
query("an empty filter", {"kind": kind("T"), "filter": {}})

# --- several properties, composites ----------------------------------------------------------
query("a = 2 and b = x", {"kind": kind("P"), "filter": AND(pf("a", "EQUAL", I(2)), pf("b", "EQUAL", S("x")))})
query("a > 1 and b = y", {"kind": kind("P"), "filter": AND(pf("a", "GREATER_THAN", I(1)), pf("b", "EQUAL", S("y")))})
query("a > 1 and b > w", {"kind": kind("P"), "filter": AND(pf("a", "GREATER_THAN", I(1)), pf("b", "GREATER_THAN", S("w")))})
query("a > 1 ordered by b", {"kind": kind("P"), "filter": pf("a", "GREATER_THAN", I(1)), "order": [asc("b")]})
query("a = 2 or b = z", {"kind": kind("P"), "filter": OR(pf("a", "EQUAL", I(2)), pf("b", "EQUAL", S("z")))})
query("a = 2 or b = z, ordered by a", {"kind": kind("P"), "filter": OR(pf("a", "EQUAL", I(2)), pf("b", "EQUAL", S("z"))), "order": [asc("a")]})
query("(a = 3 or a = 1) and b = x", {"kind": kind("P"), "filter": AND(OR(pf("a", "EQUAL", I(3)), pf("a", "EQUAL", I(1))), pf("b", "EQUAL", S("x")))})
query("an and of one filter", {"kind": kind("P"), "filter": AND(pf("a", "EQUAL", I(2)))})
query("an empty and", {"kind": kind("P"), "filter": AND()})
query("tags = red", {"kind": kind("P"), "filter": pf("tags", "EQUAL", S("red"))})
query("tags = red and tags = blue", {"kind": kind("P"), "filter": AND(pf("tags", "EQUAL", S("red")), pf("tags", "EQUAL", S("blue")))})
query("tags > c", {"kind": kind("P"), "filter": pf("tags", "GREATER_THAN", S("c"))})
query("tags > c ordered by tags", {"kind": kind("P"), "filter": pf("tags", "GREATER_THAN", S("c")), "order": [asc("tags")]})
query("tags != red", {"kind": kind("P"), "filter": pf("tags", "NOT_EQUAL", S("red"))})
query("tags IN [red, green]", {"kind": kind("P"), "filter": pf("tags", "IN", ARR(S("red"), S("green")))})
query("tags NOT_IN [red]", {"kind": kind("P"), "filter": pf("tags", "NOT_IN", ARR(S("red")))})
query("m.x = 1", {"kind": kind("P"), "filter": pf("m.x", "EQUAL", I(1))})
query("m = entity", {"kind": kind("P"), "filter": pf("m", "EQUAL", ENT(x=I(1)))})
query("v = 1 on excluded values", {"kind": kind("T"), "filter": pf("v", "EQUAL", I(1)), "order": [asc("__key__")]})
query("v = 7, an array with an excluded element", {"kind": kind("T"), "filter": pf("v", "EQUAL", I(7))})

# --- keys and ancestors ------------------------------------------------------------------------
query("__key__ = T/str", {"kind": kind("T"), "filter": pf("__key__", "EQUAL", KEY(("T", "str")))})
query("__key__ > T/str", {"kind": kind("T"), "filter": pf("__key__", "GREATER_THAN", KEY(("T", "str")))})
query("__key__ < T/3", {"kind": kind("T"), "filter": pf("__key__", "LESS_THAN", KEY(("T", 3)))})
query("__key__ = a string", {"kind": kind("T"), "filter": pf("__key__", "EQUAL", S("str"))})
query("__key__ = a key of another kind", {"kind": kind("T"), "filter": pf("__key__", "EQUAL", KEY(("P", "p1")))})
query("__key__ IN two keys", {"kind": kind("T"), "filter": pf("__key__", "IN", ARR(KEY(("T", "str")), KEY(("T", 3))))})
query("__key__ != T/str", {"kind": kind("T"), "filter": pf("__key__", "NOT_EQUAL", KEY(("T", "str")))})
query("__key__ > T/str without a kind", {"filter": pf("__key__", "GREATER_THAN", KEY(("T", "str")))})
query("ancestor Q/q1", {"filter": pf("__key__", "HAS_ANCESTOR", KEY(("Q", "q1")))})
query("ancestor Q/q1, kind C", {"kind": kind("C"), "filter": pf("__key__", "HAS_ANCESTOR", KEY(("Q", "q1")))})
query("ancestor Q/q1/C/c2", {"filter": pf("__key__", "HAS_ANCESTOR", KEY(("Q", "q1"), ("C", "c2")))})
query("ancestor Q/1", {"filter": pf("__key__", "HAS_ANCESTOR", KEY(("Q", 1)))})
query("ancestor and a property", {"kind": kind("C"), "filter": AND(pf("__key__", "HAS_ANCESTOR", KEY(("Q", "q1"))), pf("n", "GREATER_THAN", I(1)))})
query("ancestor on a property", {"kind": kind("C"), "filter": pf("n", "HAS_ANCESTOR", KEY(("Q", "q1")))})
query("ancestor of a string", {"kind": kind("C"), "filter": pf("__key__", "HAS_ANCESTOR", S("q1"))})
query("ancestor in another namespace", {"filter": pf("__key__", "HAS_ANCESTOR", {"keyValue": key(("Q", "q1"), ns="ns1")})})
query("ancestor in an or", {"kind": kind("C"), "filter": OR(pf("__key__", "HAS_ANCESTOR", KEY(("Q", "q1"))), pf("n", "EQUAL", I(4)))})

# --- projections, distinct, keys only --------------------------------------------------------
query("keys only", {"kind": kind("P"), "projection": [{"property": {"name": "__key__"}}]})
query("project a", {"kind": kind("P"), "projection": [{"property": {"name": "a"}}]})
query("project a and b", {"kind": kind("P"), "projection": [{"property": {"name": "a"}}, {"property": {"name": "b"}}]})
query("project tags", {"kind": kind("P"), "projection": [{"property": {"name": "tags"}}]})
query("project tags, distinct on tags", {"kind": kind("P"), "projection": [{"property": {"name": "tags"}}], "distinctOn": [{"name": "tags"}]})
query("project v, every type", {"kind": kind("T"), "projection": [{"property": {"name": "v"}}]})
query("project b, distinct on b", {"kind": kind("P"), "projection": [{"property": {"name": "b"}}], "distinctOn": [{"name": "b"}]})
query("project a and b, distinct on b", {"kind": kind("P"), "projection": [{"property": {"name": "a"}}, {"property": {"name": "b"}}], "distinctOn": [{"name": "b"}]})
query("distinct on b without a projection", {"kind": kind("P"), "distinctOn": [{"name": "b"}]})
query("project a, filtered on a", {"kind": kind("P"), "projection": [{"property": {"name": "a"}}], "filter": pf("a", "EQUAL", I(2))})
query("project a, ordered by b", {"kind": kind("P"), "projection": [{"property": {"name": "a"}}], "order": [asc("b")]})
query("project an entity value", {"kind": kind("P"), "projection": [{"property": {"name": "m"}}]})
query("project a missing property", {"kind": kind("P"), "projection": [{"property": {"name": "nope"}}]})
query("project a twice", {"kind": kind("P"), "projection": [{"property": {"name": "a"}}, {"property": {"name": "a"}}]})
query("project __key__ and a", {"kind": kind("P"), "projection": [{"property": {"name": "__key__"}}, {"property": {"name": "a"}}]})
query("keys only without a kind", {"projection": [{"property": {"name": "__key__"}}]})

# --- limits, offsets, cursors, batches --------------------------------------------------------
query("limit 3", {"kind": kind("P"), "limit": 3}, defines={"c1": "batch.endCursor"})
query("from the end of limit 3", {"kind": kind("P"), "limit": 3, "startCursor": "<c1>"}, defines={"c2": "batch.endCursor"})
query("from there, to the end", {"kind": kind("P"), "startCursor": "<c2>"})
query("up to the end of limit 3", {"kind": kind("P"), "endCursor": "<c1>"})
query("between two cursors", {"kind": kind("P"), "startCursor": "<c1>", "endCursor": "<c2>"})
query("a result's cursor", {"kind": kind("P"), "limit": 1, "order": [asc("a")]}, defines={"c3": "batch.entityResults.0.cursor"})
query("from a result's cursor, ordered", {"kind": kind("P"), "order": [asc("a")], "startCursor": "<c3>"})
query("offset 2", {"kind": kind("P"), "offset": 2})
query("offset 2 limit 2", {"kind": kind("P"), "offset": 2, "limit": 2}, defines={"c4": "batch.endCursor"})
query("offset past the end", {"kind": kind("P"), "offset": 20})
query("limit 0", {"kind": kind("P"), "limit": 0})
query("a negative limit", {"kind": kind("P"), "limit": -1})
query("a negative offset", {"kind": kind("P"), "offset": -1})
query("a cursor that is not ours", {"kind": kind("P"), "startCursor": "AAAA"})
query("a cursor that is not base64", {"kind": kind("P"), "startCursor": "!!"})
query("a cursor of another query", {"kind": kind("T"), "startCursor": "<c1>"})
query("350 entities", {"kind": kind("Many")}, defines={"c5": "batch.endCursor"})
query("350 entities, from the end of the first batch", {"kind": kind("Many"), "startCursor": "<c5>"})
query("350 entities, limit 310", {"kind": kind("Many"), "limit": 310})
query("350 entities, offset 340", {"kind": kind("Many"), "offset": 340})


# --- cursors of other queries ---------------------------------------------------------------
# A cursor is a position in the values its query ordered by, then the key: another query reads
# it in its own order and directions, keys last.
def xc(name, q, defines=None, ns="xc"):
    query(name, q, defines, partitionId={"namespaceId": ns})


L = kind("L")
SOURCES = {"key": {"kind": L, "order": [asc("__key__")]}, "key descending": {"kind": L, "order": [desc("__key__")]},
           "a": {"kind": L, "order": [asc("a")]}, "a descending": {"kind": L, "order": [desc("a")]},
           "no kind": {}, "a, key descending": {"kind": L, "order": [asc("a"), desc("__key__")]},
           "a descending, key": {"kind": L, "order": [desc("a"), asc("__key__")]},
           "key, b": {"kind": L, "order": [asc("__key__"), asc("b")]}}
TARGETS = {"key": {"kind": L}, "key descending": {"kind": L, "order": [desc("__key__")]},
           "a": {"kind": L, "order": [asc("a")]}, "a descending": {"kind": L, "order": [desc("a")]},
           "no kind": {}, "no kind, key descending": {"order": [desc("__key__")]},
           "a then b": {"kind": L, "order": [asc("a"), asc("b")]}, "b then a": {"kind": L, "order": [asc("b"), asc("a")]},
           "b": {"kind": L, "order": [asc("b")]}}
for source, q in SOURCES.items():
    xc(f"cursor source: {source}", dict(q, limit=2), defines={f"x {source}": "batch.endCursor"})
xc("cursor source: in another namespace", {"kind": L, "limit": 1}, defines={"x other namespace": "batch.endCursor"},
   ns="xcns")
for source in SOURCES:
    for target, q in TARGETS.items():
        for which in ("startCursor", "endCursor"):
            xc(f"{which} of '{source}' in '{target}'", dict(q, **{which: f"<x {source}>"}))
for name, q, ns in [
    ("in another namespace", {"kind": L, "startCursor": "<x key>"}, "xcns"),
    ("of another namespace", {"kind": L, "startCursor": "<x other namespace>"}, "xc"),
    ("of another namespace, another kind", {"kind": kind("M"), "startCursor": "<x other namespace>"}, "xc"),
    ("another kind", {"kind": kind("M"), "startCursor": "<x key>"}, "xc"),
    ("limit 0", {"kind": L, "limit": 0, "startCursor": "<x key>"}, "xc"),
    ("offset 5", {"kind": L, "offset": 5, "startCursor": "<x key>"}, "xc"),
    ("a filter that keeps nothing", {"kind": L, "filter": pf("a", "GREATER_THAN", I(5)), "startCursor": "<x key>"}, "xc"),
    ("key and b, a negative limit", {"kind": L, "limit": -1, "startCursor": "<x key, b>"}, "xc"),
    ("key and b, another kind", {"kind": kind("M"), "startCursor": "<x key, b>"}, "xc"),
    ("key and b, another namespace", {"kind": L, "startCursor": "<x key, b>"}, "xcns"),
    ("key and b, a projection with an equality", {"kind": L, "projection": [{"property": {"name": "a"}}],
                                                  "filter": pf("a", "EQUAL", I(1)), "startCursor": "<x key, b>"}, "xc"),
    ("key and b, 36 disjunctions", {"kind": L, "filter": AND(pf("d", "IN", ARR(*[I(i) for i in range(6)])),
                                                              pf("e", "IN", ARR(*[I(i) for i in range(6)]))),
                                    "startCursor": "<x key, b>"}, "xc"),
    ("key and b, an order twice", {"kind": L, "order": [asc("a"), asc("a")], "startCursor": "<x key, b>"}, "xc"),
    ("key and b, an order without a kind", {"order": [asc("a")], "startCursor": "<x key, b>"}, "xc"),
    ("key and b, distinct on", {"kind": L, "projection": [{"property": {"name": "a"}}, {"property": {"name": "b"}}],
                                "distinctOn": [{"name": "b"}], "order": [asc("a")], "startCursor": "<x key, b>"}, "xc"),
    ("key and b, as an end cursor", {"kind": L, "endCursor": "<x key, b>"}, "xc"),
    ("in another namespace, a projection with an equality", {"kind": L, "projection": [{"property": {"name": "a"}}],
                                                             "filter": pf("a", "EQUAL", I(1)), "startCursor": "<x key>"}, "xcns"),
    ("in another namespace, distinct on", {"kind": L, "projection": [{"property": {"name": "a"}}, {"property": {"name": "b"}}],
                                           "distinctOn": [{"name": "b"}], "order": [asc("a")], "startCursor": "<x key>"}, "xcns"),
    ("in another namespace, __kind__ descending", {"kind": kind("__kind__"), "order": [desc("__key__")],
                                                    "startCursor": "<x key>"}, "xcns"),
    ("key descending, b, no entities", {"kind": kind("Empty"), "order": [asc("b")], "startCursor": "<x key descending>"}, "xc"),
    ("key descending, b, a filter that keeps nothing", {"kind": L, "order": [asc("b")], "filter": pf("b", "GREATER_THAN", I(100)),
                                                         "startCursor": "<x key descending>"}, "xc"),
    ("key descending, b, limit 1", {"kind": L, "order": [asc("b")], "limit": 1, "startCursor": "<x key descending>"}, "xc"),
    ("key descending as the end cursor, b", {"kind": L, "order": [asc("b")], "endCursor": "<x key descending>"}, "xc"),
]:
    xc(f"a cursor of another query: {name}", q, ns=ns)

# A distinct query's cursor has no key: no namespace or kind of its own. A cursor's key that
# meets a value is compared with it, or is an internal error at a row it reaches (just before its
# row and that value ordered as the first order, or just past it and ordered the other way);
# just before its row with a null value, whatever the rows. A key that meets the key of an order
# that goes on past it is not compared. An aggregation orders by its sums' properties too.
XD = "xd"
D = kind("D")
SOURCES_D = {"t descending": {"kind": D, "order": [desc("t")], "limit": 1}, "t": {"kind": D, "order": [asc("t")], "limit": 1},
             "b descending": {"kind": D, "order": [desc("b")], "limit": 1}, "distinct on b": {"kind": D, "distinctOn": [{"name": "b"}], "limit": 1},
             "no kind": {"limit": 2}, "key": {"kind": D, "limit": 1}}
TARGETS_D = {"b, a": [asc("b"), asc("a")], "b descending, a": [desc("b"), asc("a")], "b, a descending": [asc("b"), desc("a")],
             "b descending, a descending": [desc("b"), desc("a")], "key, b": [asc("__key__"), asc("b")],
             "key descending, b": [desc("__key__"), asc("b")]}
for source, q in SOURCES_D.items():
    xc(f"cursor source D: {source}", q, defines={f"xd {source}": "batch.endCursor"}, ns=XD)
for source in SOURCES_D:
    for target, order in TARGETS_D.items():
        for which in ("startCursor", "endCursor"):
            xc(f"{which} of D '{source}' in '{target}'", {"kind": D, "order": order, which: f"<xd {source}>"}, ns=XD)
for which in ("startCursor", "endCursor"):
    xc(f"{which} of D 'distinct on b' in another kind", {"kind": kind("DP"), "order": [asc("b")], which: "<xd distinct on b>"}, ns=XD)
    xc(f"{which} of D 'distinct on b' in another kind, by its key", {"kind": kind("DP"), which: "<xd distinct on b>"}, ns=XD)
    xc(f"{which} of D 'distinct on b' in another namespace", {"kind": D, "order": [asc("b")], which: "<xd distinct on b>"}, ns="xc")
xc("cursor source D: c compared, a projected", {"kind": D, "filter": pf("c", "GREATER_THAN", I(0)),
                                               "projection": [{"property": {"name": "a"}}], "limit": 1},
   defines={"xd c then a": "batch.endCursor"}, ns=XD)
for name, order, aggregations in [
    ("a count, ordered by a", [asc("a")], [{"count": {}, "alias": "n"}]),
    ("an average of b, ordered by a", [asc("a")], [{"avg": {"property": {"name": "b"}}, "alias": "m"}, {"count": {}, "alias": "n"}]),
    ("sums of b and c", [], [{"sum": {"property": {"name": "b"}}, "alias": "s"}, {"sum": {"property": {"name": "c"}}, "alias": "t"}]),
]:
    for which in ("startCursor", "endCursor"):
        case(f"{which} of D 'c then a' in {name}", "runAggregationQuery",
             {"aggregationQuery": {"nestedQuery": {"kind": D, "order": order, which: "<xd c then a>"},
                                   "aggregations": aggregations}, "partitionId": {"namespaceId": XD}})

# --- metadata ----------------------------------------------------------------------------------
query("__kind__", {"kind": kind("__kind__")})
query("__kind__ in ns1", {"kind": kind("__kind__")}, partitionId={"namespaceId": "ns1"})
query("__kind__ above P", {"kind": kind("__kind__"), "filter": pf("__key__", "GREATER_THAN", KEY(("__kind__", "P")))})
query("__namespace__", {"kind": kind("__namespace__")})
query("__namespace__ keys only", {"kind": kind("__namespace__"), "projection": [{"property": {"name": "__key__"}}]})
query("__property__", {"kind": kind("__property__")})
query("__property__ of P", {"kind": kind("__property__"), "filter": pf("__key__", "HAS_ANCESTOR", KEY(("__kind__", "P")))})
query("__property__ keys only", {"kind": kind("__property__"), "projection": [{"property": {"name": "__key__"}}]})
query("__x__", {"kind": kind("__x__")})

# --- refused ------------------------------------------------------------------------------------
query("two kinds", {"kind": [{"name": "T"}, {"name": "P"}]})
query("an empty kind name", {"kind": [{"name": ""}]})
query("a property name __x__", {"kind": kind("T"), "filter": pf("__x__", "EQUAL", I(1))})
query("in another project's namespace", {"kind": kind("T")}, partitionId={"projectId": "elsewhere"})
# What a query does not order by follows its last order's direction; the key, unless a filter
# compares it, ascending.
query("project a, order by b descending", {"kind": kind("P"), "projection": [asc("a")], "order": [desc("b")]})
query("project a, b, order by b descending", {"kind": kind("P"), "projection": [asc("a"), asc("b")], "order": [desc("b")]})
query("a > 0 order by b descending", {"kind": kind("P"), "filter": pf("a", "GREATER_THAN", I(0)), "order": [desc("b")]})
query("distinct on b, a, order by b descending", {"kind": kind("P"), "distinctOn": [{"name": "b"}, {"name": "a"}],
                                                  "order": [desc("b")]})
query("the key above p1, order by b descending", {"kind": kind("P"), "filter": pf("__key__", "GREATER_THAN", KEY(("P", "p1"))),
                                                   "order": [desc("b")]})
query("the key above p1, project a, order by b descending", {
    "kind": kind("P"), "filter": pf("__key__", "GREATER_THAN", KEY(("P", "p1"))), "projection": [asc("a")],
    "order": [desc("b")]})
query("order by b descending", {"kind": kind("P"), "order": [desc("b")]})
query("a reserved namespace", {"kind": kind("T")}, partitionId={"namespaceId": "__x__"})
case("a query and a GQL query", "runQuery", {"query": {"kind": kind("T")}, "gqlQuery": {"queryString": "SELECT * FROM T"}})
case("neither query", "runQuery", {})
case("an unknown field", "runQuery", {"query": {"kind": kind("T"), "x": 1}})
query("in a read-only transaction's read time, too old", {"kind": kind("T")}, readOptions={"readTime": "2020-01-01T00:00:00Z"})
query("eventual", {"kind": kind("T")}, readOptions={"readConsistency": "EVENTUAL"})
query("in a new transaction", {"kind": kind("P"), "limit": 1}, readOptions={"newTransaction": {}})
# Without a kind, a read-write transaction cannot lock what it reads: an internal error.
query("no kind, in a new transaction", {}, readOptions={"newTransaction": {}})
query("no kind, in a new read-only transaction", {"limit": 1}, readOptions={"newTransaction": {"readOnly": {}}})

# --- aggregations -----------------------------------------------------------------------------
def agg(name, nested, *aggregations):
    case(name, "runAggregationQuery", {"aggregationQuery": {"nestedQuery": nested, "aggregations": list(aggregations)}})


agg("count P", {"kind": kind("P")}, {"count": {}, "alias": "n"})
agg("count, sum and avg of a", {"kind": kind("P")}, {"count": {}, "alias": "n"}, {"sum": {"property": {"name": "a"}}, "alias": "s"}, {"avg": {"property": {"name": "a"}}, "alias": "m"})
agg("count without an alias", {"kind": kind("P")}, {"count": {}})
agg("sum and avg without aliases", {"kind": kind("P")}, {"sum": {"property": {"name": "a"}}}, {"avg": {"property": {"name": "a"}}})
agg("count up to 3", {"kind": kind("P")}, {"count": {"upTo": "3"}, "alias": "n"})
agg("count with a filter", {"kind": kind("P"), "filter": pf("b", "EQUAL", S("x"))}, {"count": {}, "alias": "n"})
agg("count with a limit", {"kind": kind("P"), "limit": 2}, {"count": {}, "alias": "n"})
agg("sum of v, every type", {"kind": kind("T")}, {"sum": {"property": {"name": "v"}}, "alias": "s"}, {"avg": {"property": {"name": "v"}}, "alias": "m"})
agg("sum of a missing property", {"kind": kind("P")}, {"sum": {"property": {"name": "nope"}}, "alias": "s"}, {"avg": {"property": {"name": "nope"}}, "alias": "m"})
agg("count of no entities", {"kind": kind("Nothing")}, {"count": {}, "alias": "n"})
agg("two aliases alike", {"kind": kind("P")}, {"count": {}, "alias": "n"}, {"count": {}, "alias": "n"})
agg("an alias __x__", {"kind": kind("P")}, {"count": {}, "alias": "__x__"})
agg("no aggregations", {"kind": kind("P")})
agg("six aggregations", {"kind": kind("P")}, *[{"count": {}, "alias": f"n{i}"} for i in range(6)])
agg("five aggregations", {"kind": kind("P")}, *[{"count": {}, "alias": f"n{i}"} for i in range(5)])
agg("count with a projection", {"kind": kind("P"), "projection": [{"property": {"name": "a"}}]}, {"count": {}, "alias": "n"})
TIMES = pf("__key__", "IN", ARR(KEY(("T", "ts")), KEY(("T", "ts2")), KEY(("T", "int2"))))
agg("sum and avg of timestamps, as microseconds", {"kind": kind("T"), "filter": TIMES},
    {"sum": {"property": {"name": "v"}}, "alias": "s"}, {"avg": {"property": {"name": "v"}}, "alias": "m"})
agg("sum of projected timestamps", {"kind": kind("T"), "filter": TIMES, "projection": [{"property": {"name": "v"}}]},
    {"sum": {"property": {"name": "v"}}, "alias": "s"})
case("an aggregation without a query", "runAggregationQuery", {"aggregationQuery": {"aggregations": [{"count": {}}]}})
case("an aggregation without a kind, in a new transaction", "runAggregationQuery",
     {"aggregationQuery": {"nestedQuery": {}, "aggregations": [{"count": {}}]}, "readOptions": {"newTransaction": {}}})
# Without a kind in a read-write transaction, the aggregations' own refusals come before the
# internal error, the query's after it.
COUNT = {"count": {}}
SUM = lambda p: {"sum": {"property": {"name": p}}}  # noqa: E731
for name, nested, aggregations in [
    ("no aggregations", {}, []),
    ("six aggregations", {}, [{"count": {}, "alias": f"n{i}"} for i in range(6)]),
    ("an alias __x__", {}, [{"count": {}, "alias": "__x__"}]),
    ("two aliases alike", {}, [{"count": {}, "alias": "n"}, {"count": {}, "alias": "n"}]),
    ("a sum of __key__", {}, [SUM("__key__")]),
    ("a sum of a, projecting b", {"projection": [{"property": {"name": "b"}}]}, [SUM("a")]),
    ("a sum of a, keys only", {"projection": [{"property": {"name": "__key__"}}]}, [SUM("a")]),
    ("a sum of a", {}, [SUM("a")]),
    ("a negative limit", {"limit": -1}, [COUNT]),
    ("a negative offset", {"offset": -1}, [COUNT]),
]:
    case(f"an aggregation without a kind, in a new transaction: {name}", "runAggregationQuery",
         {"aggregationQuery": {"nestedQuery": nested, "aggregations": aggregations}, "readOptions": {"newTransaction": {}}})

# An ancestor anywhere in a filter is the whole query's: `RunQuery` refuses one that not every
# disjunction holds to, an aggregation counts the descendants that match what is left, a
# disjunct that was the ancestor alone dropped.
ANCESTOR = pf("__key__", "HAS_ANCESTOR", KEY(("Q", "q1")))
for name, f in [
    ("the ancestor or n = 4", OR(ANCESTOR, pf("n", "EQUAL", I(4)))),
    ("n = 4 or the ancestor", OR(pf("n", "EQUAL", I(4)), ANCESTOR)),
    ("the ancestor and n = 2, or n = 4", OR(AND(ANCESTOR, pf("n", "EQUAL", I(2))), pf("n", "EQUAL", I(4)))),
    ("the ancestor, or n = 2, or n = 4", OR(ANCESTOR, pf("n", "EQUAL", I(2)), pf("n", "EQUAL", I(4)))),
    ("the ancestor alone in a conjunction, or n = 4", OR(AND(ANCESTOR), pf("n", "EQUAL", I(4)))),
    ("n = 2, and the ancestor or n = 4", AND(pf("n", "EQUAL", I(2)), OR(ANCESTOR, pf("n", "EQUAL", I(4))))),
    ("the ancestor alone in a disjunction, or n = 4", OR(OR(ANCESTOR), pf("n", "EQUAL", I(4)))),
    ("the ancestor and n = 2, or the ancestor and n = 4", OR(AND(ANCESTOR, pf("n", "EQUAL", I(2))), AND(ANCESTOR, pf("n", "EQUAL", I(4))))),
]:
    query(f"an ancestor in a disjunction: {name}", {"kind": kind("C"), "filter": f})
    agg(f"an ancestor in a disjunction: {name}", {"kind": kind("C"), "filter": f}, COUNT)

# A limit of 0 has more results unless the filter's inequalities bound a property to nothing.
for name, f in [
    ("a < null", pf("a", "LESS_THAN", NULL)),
    ("a <= null", pf("a", "LESS_THAN_OR_EQUAL", NULL)),
    ("a > 3 and a < 3", AND(pf("a", "GREATER_THAN", I(3)), pf("a", "LESS_THAN", I(3)))),
    ("a >= 3 and a <= 3", AND(pf("a", "GREATER_THAN_OR_EQUAL", I(3)), pf("a", "LESS_THAN_OR_EQUAL", I(3)))),
    ("a > 'x' and a < 5", AND(pf("a", "GREATER_THAN", S("x")), pf("a", "LESS_THAN", I(5)))),
    ("a = 1 and a > 5", AND(pf("a", "EQUAL", I(1)), pf("a", "GREATER_THAN", I(5)))),
    ("a < false", pf("a", "LESS_THAN", B(False))),
    ("a < null or b = 'x'", OR(pf("a", "LESS_THAN", NULL), pf("b", "EQUAL", S("x")))),
    ("a < null, or a > 3 and a < 3", OR(pf("a", "LESS_THAN", NULL), AND(pf("a", "GREATER_THAN", I(3)), pf("a", "LESS_THAN", I(3))))),
]:
    query(f"limit 0, {name}", {"kind": kind("P"), "filter": f, "limit": 0})
query("limit 0, offset 1, a < null", {"kind": kind("P"), "filter": pf("a", "LESS_THAN", NULL), "limit": 0, "offset": 1})

# The properties of an array's entity values are not indexed: no filter, order or sum reads
# them; an entity value's array is.
XA = {"partitionId": {"namespaceId": "xa"}}
query("an array's entity values, filtered", {"kind": kind("Arr"), "filter": pf("arr.x", "EQUAL", I(5))}, **XA)
query("an array's entity values, ordered", {"kind": kind("Arr"), "order": [asc("arr.x")]}, **XA)
query("an entity value's array, filtered", {"kind": kind("Arr"), "filter": pf("e.t", "EQUAL", I(3))}, **XA)
case("a sum over an array's entity values", "runAggregationQuery",
     {"aggregationQuery": {"nestedQuery": {"kind": kind("Arr")}, "aggregations": [
         {"sum": {"property": {"name": "arr.x"}}, "alias": "s"}, {"count": {}, "alias": "n"}]}, **XA})
case("a sum over an entity value's array", "runAggregationQuery",
     {"aggregationQuery": {"nestedQuery": {"kind": kind("Arr")}, "aggregations": [
         {"sum": {"property": {"name": "e.t"}}, "alias": "s"}, {"avg": {"property": {"name": "e.t"}}, "alias": "m"}]}, **XA})

# A sum or an average reads a row for each value of an array, ordered after the query's orders,
# before the key unless the query orders on it or compares it, in the direction of its last order.
XG = {"partitionId": {"namespaceId": "xg"}}
for name, q in [
    ("no order", {}),
    ("the key ascending", {"order": [asc("__key__")]}),
    ("the key descending", {"order": [desc("__key__")]}),
    ("p ascending", {"order": [asc("p")]}),
    ("p descending", {"order": [desc("p")]}),
    ("p ascending, the key descending", {"order": [asc("p"), desc("__key__")]}),
    ("p descending, the key ascending", {"order": [desc("p"), asc("__key__")]}),
    ("the key descending, p ascending", {"order": [desc("__key__"), asc("p")]}),
    ("the key compared", {"filter": pf("__key__", "GREATER_THAN", {"keyValue": key(("G", "0"), ns="xg")})}),
    ("the key compared, descending", {"filter": pf("__key__", "GREATER_THAN", {"keyValue": key(("G", "0"), ns="xg")}), "order": [desc("__key__")]}),
    ("a compared, descending", {"filter": pf("a", "GREATER_THAN", I(0)), "order": [desc("a")]}),
]:
    for limit in (1, 2):
        case(f"the rows of a sum, {name}, limit {limit}", "runAggregationQuery",
             {"aggregationQuery": {"nestedQuery": {"kind": kind("G"), **q, "limit": limit}, "aggregations": [
                 {"sum": {"property": {"name": "a"}}, "alias": "s"}, {"count": {}, "alias": "n"}]}, **XG})
# A property filtered by equality and ordered has a row for each of its values; a sum reads it
# as the value of the first disjunct the entity meets (an `IN` a disjunct per value, in order).
for name, f, order in [
    ("b = 1, ordered by b", pf("b", "EQUAL", I(1)), [asc("b")]),
    ("b in [1, 2], ordered by b", pf("b", "IN", ARR(I(1), I(2))), [asc("b")]),
    ("b in [1, 2], ordered by b descending", pf("b", "IN", ARR(I(1), I(2))), [desc("b")]),
]:
    query(name, {"kind": kind("E"), "filter": f, "order": order}, **XG)
for name, f, summed in [
    ("b in [1, 2]", pf("b", "IN", ARR(I(1), I(2))), ["b"]),
    ("b in [2, 1]", pf("b", "IN", ARR(I(2), I(1))), ["b"]),
    ("c = 2 or b = 1", OR(pf("c", "EQUAL", I(2)), pf("b", "EQUAL", I(1))), ["b"]),
    ("b = 1 and d = 5", AND(pf("b", "EQUAL", I(1)), pf("d", "EQUAL", I(5))), ["b", "d"]),
    ("b in [3, 1]", pf("b", "IN", ARR(I(3), I(1))), ["b", "d"]),
]:
    case(f"sums where {name}", "runAggregationQuery",
         {"aggregationQuery": {"nestedQuery": {"kind": kind("E"), "filter": f}, "aggregations": [
             {"sum": {"property": {"name": p}}, "alias": p} for p in summed] + [{"count": {}, "alias": "n"}]}, **XG})

# Two properties summed order the rows by name, whatever the order of the aggregations.
for first, second in [("z", "a"), ("a", "z")]:
    case(f"the rows of sums of {first} then {second}, limit 1", "runAggregationQuery",
         {"aggregationQuery": {"nestedQuery": {"kind": kind("H"), "limit": 1}, "aggregations": [
             {"sum": {"property": {"name": first}}, "alias": first}, {"sum": {"property": {"name": second}}, "alias": second}]}, **XG})
for name, order in [("ascending", asc("__key__")), ("descending", desc("__key__"))]:
    case(f"the average of an entity value's property, the key {name}, limit 1", "runAggregationQuery",
         {"aggregationQuery": {"nestedQuery": {"kind": kind("G"), "order": [order], "limit": 1}, "aggregations": [
             {"avg": {"property": {"name": "e.x"}}, "alias": "m"}]}, **XG})

# A disjunction of an ancestor alone is an internal error, once the ancestor's operand is checked.
for name, f in [("a string", pf("__key__", "HAS_ANCESTOR", S("b"))), ("null", pf("__key__", "HAS_ANCESTOR", NULL)),
                ("a key, on b", pf("b", "HAS_ANCESTOR", KEY(("Q", "q1")))), ("a key", pf("__key__", "HAS_ANCESTOR", KEY(("Q", "q1"))))]:
    query(f"a disjunction of an ancestor alone, of {name}", {"filter": AND(OR(f))})

# In a read-write transaction, `__namespace__` and `__kind__` find nothing; in a read-only one,
# what they find outside.
for meta in ("__namespace__", "__kind__"):
    query(f"{meta} in a new transaction", {"kind": kind(meta)}, readOptions={"newTransaction": {}})
    query(f"{meta} past 1, in a new transaction", {"kind": kind(meta), "offset": 1}, readOptions={"newTransaction": {}})
    query(f"{meta} in a new read-only transaction", {"kind": kind(meta)}, readOptions={"newTransaction": {"readOnly": {}}})

# A key operand's project must be valid (an aggregation's error is internal); its namespace may
# be reserved.
BAD_PROJECT = {"keyValue": {"partitionId": {"projectId": "Bad"}, "path": [{"kind": "A", "name": "x"}]}}
RESERVED_NS = {"keyValue": key(("A", "x"), ns="__x__")}
query("a key of an invalid project", {"kind": kind("P"), "filter": pf("a", "GREATER_THAN", BAD_PROJECT)})
agg("a key of an invalid project", {"kind": kind("P"), "filter": pf("a", "GREATER_THAN", BAD_PROJECT)}, {"count": {}, "alias": "n"})
query("a key of an invalid project, then a key of id 0", {"kind": kind("P"), "filter": AND(pf("a", "GREATER_THAN", BAD_PROJECT), pf("b", "LESS_THAN", KEY(("A", 0))))})
query("a key in a reserved namespace", {"kind": kind("P"), "filter": pf("a", "EQUAL", RESERVED_NS)})
agg("a key in a reserved namespace", {"kind": kind("P"), "filter": pf("a", "EQUAL", RESERVED_NS)}, {"count": {}, "alias": "n"})
query("the key in a reserved namespace", {"kind": kind("P"), "filter": pf("__key__", "EQUAL", RESERVED_NS)})

# The key may be projected and compared by equality.
for name, f, projection in [
    ("key = q1, keys only", pf("__key__", "EQUAL", KEY(("Q", "q1"))), ["__key__"]),
    ("key in [q1], keys only", pf("__key__", "IN", ARR(KEY(("Q", "q1")))), ["__key__"]),
    ("key = q1, projecting the key and n", pf("__key__", "EQUAL", KEY(("Q", "q1"))), ["__key__", "n"]),
]:
    for kinds in (kind("Q"), []):
        q = {"kind": kinds, "filter": f, "projection": [{"property": {"name": p}} for p in projection]}
        query(f"{name}{'' if kinds else ', without a kind'}", q)
        agg(f"{name}{'' if kinds else ', without a kind'}", q, COUNT)


VERSION = re.compile(r'"(version|commitVersion|snapshotVersion)": "\d+"')
CURSOR = re.compile(r'"(cursor|endCursor|skippedCursor|startCursor)": "[^"]*"')


def masked(text):
    text = mask(text, PROJECT)
    text = VERSION.sub(lambda m: f'"{m.group(1)}": "<version>"', text)
    return CURSOR.sub(lambda m: f'"{m.group(1)}": "<cursor>"', text)


def call(method, path, body):
    conn = http.client.HTTPConnection(*HOST.split(":"), timeout=60)
    conn.request(method, path, body=body.encode(), headers=JSON)
    r = conn.getresponse()
    return r.status, r.getheader("content-type"), r.read().decode()


def at(value, path):
    for part in path.split("."):
        value = value[int(part)] if isinstance(value, list) else value[part]
    return value


def seed():
    mutations = [{"upsert": {"key": k, "properties": props}} for k, props in DATASET]
    for start in range(0, len(mutations), 500):
        body = json.dumps({"mode": "NON_TRANSACTIONAL", "mutations": mutations[start:start + 500]})
        status, _, text = call("POST", f"/v1/projects/{PROJECT}:commit", body)
        assert status == 200, text


def run():
    seed()
    out = []
    cursors = {}
    for c in CASES:
        text = json.dumps(c["body"]).replace("{project}", PROJECT)
        for label, value in cursors.items():
            text = text.replace(f'"<{label}>"', json.dumps(value))
        status, ctype, answer = call(c["method"], c["path"].replace("{project}", PROJECT), text)
        for label, path in (c.get("defines") or {}).items():
            cursors[label] = at(json.loads(answer), path)
        out.append(dict(c, outcome={"status": status, "contentType": ctype, "body": masked(answer)}))
    return out


if __name__ == "__main__":
    json.dump({
        "oracle": "cloud-firestore-emulator v1.22.0 (sha256 9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)",
        "generated_by": "tools/oracle/datastore_queries.py",
        "mode": "datastore-mode",
        "dataset": [{"key": k, "properties": props} for k, props in DATASET],
        "cases": run(),
    }, sys.stdout, ensure_ascii=False, indent=1)
    print()
