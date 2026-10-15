"""How does the official emulator answer RunAggregationQuery? (#22)

Usage:
    python3 -I tools/oracle/aggregations.py 127.0.0.1:8086 > crates/hidane/tests/fixtures/aggregations.json

Seeds one project with small collections whose field `v` holds values chosen for the edges of
`sum` and `avg` (integer overflow, NaN, infinities, precision, non-numbers), runs every case
through REST `:runAggregationQuery`, and records the responses. The fixture keeps the dataset
and the requests next to the outcomes, so `crates/hidane/tests/aggregations.rs` can replay them
over gRPC. `{documents}` stands for `projects/<project>/databases/(default)/documents`.
"""

import json
import sys
import time
import urllib.error
import urllib.request

HOST = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1:8080"
PROJECT = f"aggregations-{time.strftime('%H%M%S')}"
DOCUMENTS = f"projects/{PROJECT}/databases/(default)/documents"
I64_MAX = 9223372036854775807


def i(n): return {"integerValue": str(n)}
def d(x): return {"doubleValue": x}
def s(x): return {"stringValue": x}
NULL = {"nullValue": None}

COLLECTIONS = {
    "mixed": {"i1": i(1), "i2": i(2), "half": d(0.5), "str": s("x"), "null": NULL, "true": {"booleanValue": True},
              "arr": {"arrayValue": {"values": [i(1)]}}, "map": {"mapValue": {"fields": {"a": i(1)}}}, "missing": None},
    "ints": {"a": i(1), "b": i(2), "c": i(3)},
    "overflow": {"a": i(I64_MAX), "b": i(1)},
    "overflow_back": {"a": i(I64_MAX), "b": i(1), "c": i(-1)},
    "underflow": {"a": i(-I64_MAX - 1), "b": i(-1)},
    "nan": {"a": i(1), "b": d("NaN")},
    "infinities": {"a": d("Infinity"), "b": d("-Infinity")},
    "infinity": {"a": d("Infinity"), "b": i(1)},
    "tenths": {"a": d(0.1), "b": d(0.2)},
    "negzero": {"a": d(-0.0)},
    "precision": {"a": i(9007199254740993), "b": d(1.0)},
    "precision_ints": {"a": i(9007199254740993), "b": i(9007199254740993)},
    "big_avg": {"a": i(I64_MAX), "b": i(I64_MAX)},
    "strings": {"a": s("x"), "b": s("y")},
    "doubles_int_valued": {"a": d(1.0), "b": d(2.0)},
}
DATASET = []
for coll, docs in COLLECTIONS.items():
    for doc_id, value in docs.items():
        DATASET.append({"name": f"{coll}/{doc_id}", "fields": {"w": i(1)} if value is None else {"v": value, "w": i(1)}})
DATASET += [
    {"name": "mixed/i1/sub/x", "fields": {"v": i(10)}},
    {"name": "other/y/sub/z", "fields": {"v": i(20)}},
    {"name": "nested/a", "fields": {"m": {"mapValue": {"fields": {"x": i(5)}}}}},
    {"name": "nested/b", "fields": {"m": {"mapValue": {"fields": {"x": d(1.5)}}}}},
    # Names in another order than `v` and `u` (#262).
    {"name": "ordered/a", "fields": {"v": i(5), "u": i(1), "k": i(1)}},
    {"name": "ordered/b", "fields": {"v": i(1), "u": i(3), "k": i(1)}},
    {"name": "ordered/c", "fields": {"v": i(3), "u": i(2), "k": i(0)}},
    {"name": "ordered/d", "fields": {"v": s("s"), "u": i(0), "k": i(2)}},
    {"name": "ordered/e", "fields": {"u": i(9), "k": i(1)}},
    {"name": "bytes_first/d1", "fields": {"b": {"bytesValue": "AQ=="}}},
    {"name": "bytes_first/d2", "fields": {"b": d(-0.0)}},
    {"name": "time_first/d0", "fields": {"b": {"timestampValue": "2020-01-01T00:00:00Z"}}},
    {"name": "time_first/d1", "fields": {"b": d(1.0)}},
    {"name": "time_first/d2", "fields": {"b": i(0)}},
]


def f(path): return {"fieldPath": path}
def count(alias=None, up_to=None):
    a = {"count": {} if up_to is None else {"upTo": str(up_to)}}
    if alias is not None:
        a["alias"] = alias
    return a
def total(path, alias=None):
    a = {"sum": {"field": f(path)}}
    if alias is not None:
        a["alias"] = alias
    return a
def avg(path, alias=None):
    a = {"avg": {"field": f(path)}}
    if alias is not None:
        a["alias"] = alias
    return a
def q(coll, **kw):
    out = {"from": [{"collectionId": coll, "allDescendants": True} if kw.pop("group", False) else {"collectionId": coll}]}
    out.update(kw)
    return out


CASES = []
def case(name, query, aggregations, parent=""):
    CASES.append({"name": name, "parent": parent, "query": query, "aggregations": aggregations})


for coll in COLLECTIONS:
    case(f"count, sum and avg of {coll}", q(coll), [count("n"), total("v", "sum"), avg("v", "avg")])
case("empty collection", q("nothing"), [count("n"), total("v", "sum"), avg("v", "avg")])
case("count up to 2", q("mixed"), [count("n", up_to=2)])
case("count up to 100", q("mixed"), [count("n", up_to=100)])
case("count up to 0", q("mixed"), [count("n", up_to=0)])
case("count up to -1", q("mixed"), [count("n", up_to=-1)])
case("count with limit 2", q("mixed", limit=2), [count("n")])
case("count with offset 2", q("mixed", offset=2), [count("n")])
case("count with limit 3 and up to 2", q("mixed", limit=3), [count("n", up_to=2)])
case("sum with limit 1 (order by name)", q("ints", limit=1), [total("v", "sum")])
case("count where v > 0", q("mixed", where={"fieldFilter": {"field": f("v"), "op": "GREATER_THAN", "value": i(0)}}), [count("n"), total("v", "sum")])
case("count order by v (documents without v left out)", q("mixed", orderBy=[{"field": f("v"), "direction": "ASCENDING"}]), [count("n")])
case("collection group sub", q("sub", group=True), [count("n"), total("v", "sum")])
case("every collection", {"from": [{"collectionId": ""}]}, [count("n")])
case("every descendant", {"from": [{"collectionId": "", "allDescendants": True}]}, [count("n")])
case("count next to sum leaves out documents without the field", q("mixed"), [count("n"), total("v", "sum")])
case("count next to avg leaves out documents without the field", q("mixed"), [count("n"), avg("v", "avg")])
case("count next to sums of two fields", q("mixed"), [count("n"), total("v", "sv"), total("w", "sw")])
case("count and sum, offset 5 limit 1", q("mixed", offset=5, limit=1), [count("n"), total("v", "sum")])
case("count and sum, limit 6", q("mixed", limit=6), [count("n"), total("v", "sum")])
case("count and sum of w, order by v", q("mixed", orderBy=[{"field": f("v"), "direction": "ASCENDING"}]), [count("n"), total("w", "sum")])
case("sum of a nested field", q("nested"), [total("m.x", "sum"), avg("m.x", "avg")])
case("sum of a missing field", q("ints"), [total("nope", "sum"), avg("nope", "avg")])
case("subcollection under mixed/i1", q("sub"), [count("n")], parent="mixed/i1")
case("default aliases", q("ints"), [count(), total("v"), avg("v")])
case("default alias next to an explicit one", q("ints"), [count("field_1"), total("v")])
case("five aggregations", q("ints"), [count("a"), count("b"), total("v", "c"), avg("v", "d"), count("e", up_to=1)])
case("six aggregations", q("ints"), [count("a"), count("b"), count("c"), count("d"), count("e"), count("f")])
case("no aggregations", q("ints"), [])
case("duplicate alias", q("ints"), [count("x"), total("v", "x")])
case("alias with a dot", q("ints"), [count("a.b")])
case("alias with a space", q("ints"), [count("a b")])
case("reserved alias", q("ints"), [count("__x__")])
case("sum of __name__", q("ints"), [total("__name__", "s")])
case("sum of an invalid field path", q("ints"), [total("a..b", "s")])
case("sum without a field", q("ints"), [{"sum": {}, "alias": "s"}])
case("aggregation without an operator", q("ints"), [{"alias": "x"}])
case("invalid query", q("ints", limit=-1), [count("n")])
# Found by the differential fuzzer (tools/fuzz/, #262): `sum` and `avg` need a collection, as an
# order by a field does; `count` does not. The aggregations are checked before the query, and
# this after the query's limit and before its filter.
case("sum without a collection", {}, [total("v", "s")])
case("avg without a collection", {}, [avg("v", "a")])
case("count and sum of every collection", {"from": [{"collectionId": ""}]}, [count("n"), total("v", "s")])
case("sum of every descendant", {"from": [{"collectionId": "", "allDescendants": True}]}, [total("v", "s")])
case("avg under a document without a collection", {}, [avg("v", "a")], parent="mixed/i1")
case("count under a document without a collection", {}, [count("n")], parent="mixed/i1")
case("sum without a collection, with a filter", {"where": {"fieldFilter": {"field": f("v"), "op": "EQUAL", "value": i(1)}}}, [total("v", "s")])
case("sum without a collection, with a negative limit", {"limit": -1}, [total("v", "s")])
case("sum without a collection, and a count up to -1", {}, [total("v", "s"), count("n", up_to=-1)])
case("no aggregations and an invalid query", q("ints", limit=-1), [])
case("a count up to -1 and an invalid query", q("ints", orderBy=[{"field": f("v")}, {"field": f("v")}]), [count("n", up_to=-1)])
# Which comes first, an aggregation's problem or its query's: the operators of the query's
# filter; then each aggregation as it is read (its operator, `up_to`, the path it reads); then
# what the query names (its collection, filter kinds and paths); then the list of aggregations
# (empty, more than five, `__name__`, aliases); then the rest of the query.
NEGATIONS = {"compositeFilter": {"op": "AND", "filters": [
    {"fieldFilter": {"field": f("a"), "op": "NOT_EQUAL", "value": i(1)}},
    {"fieldFilter": {"field": f("b"), "op": "NOT_EQUAL", "value": i(1)}}]}}
case("a sum of an invalid path and two negations", q("ints", where=NEGATIONS), [total("a..b", "s")])
case("a count up to -1 and a reserved collection ID", q("__x__"), [count("n", up_to=-1)])
case("an aggregation without an operator and an empty filter", q("ints", where={}), [{"alias": "x"}])
case("a sum of an invalid path and a reserved collection ID", q("__x__"), [total("a..b", "s")])
case("no aggregations and a reserved collection ID", q("__x__"), [])
case("six aggregations and an empty filter", q("ints", where={}), [count("a"), count("b"), count("c"), count("d"), count("e"), count("f")])
case("a sum of __name__ and an invalid filter path", q("ints", where={"fieldFilter": {"field": f("a..b"), "op": "EQUAL", "value": i(1)}}), [total("__name__", "s")])
case("a duplicate alias and a negative limit", q("ints", limit=-1), [count("x"), count("x")])
case("a reserved alias and an order without a collection", {"orderBy": [{"field": f("v")}]}, [count("__x__")])
case("a sum of __name__ and duplicate orders", q("ints", orderBy=[{"field": f("v")}, {"field": f("v")}]), [total("__name__", "s")])
case("a query vector that is not a vector and no aggregations", {"findNearest": {"queryVector": {"arrayValue": {}}}}, [])
case("a cursor with too many values and a duplicate alias", {"startAt": {"values": [i(1)]}}, [count("a"), count("a")])
# A filter value without a type is refused with what the query names, before the list (#262).
case("a filter without a value and no aggregations", q("ints", where={"fieldFilter": {"field": f("v"), "op": "EQUAL"}}), [])
# The fields `sum` and `avg` read order the query, so `offset` and `limit` take documents in
# that order (#262): in path order, after the query's orders and its inequalities' fields,
# before the name, in the direction of the query's last order.
case("sum with limit 1 takes the smallest value", q("ordered", limit=1), [total("v", "s")])
case("avg with limit 1 takes the smallest value", q("ordered", limit=1), [avg("v", "a")])
case("count and sum with limit 2", q("ordered", limit=2), [count("n"), total("v", "s")])
case("count and sum with offset 3 (a value that is not a number)", q("ordered", offset=3), [count("n"), total("v", "s")])
case("avg with offset 2", q("ordered", offset=2), [avg("v", "a")])
case("sums of v and u, limit 1 (u first)", q("ordered", limit=1), [total("v", "sv"), total("u", "su")])
case("sums of u and v, limit 1 (u first)", q("ordered", limit=1), [total("u", "su"), total("v", "sv")])
case("an inequality's field comes before the summed field", q("ordered", where={"fieldFilter": {"field": f("k"), "op": "GREATER_THAN", "value": i(0)}}, limit=1), [total("v", "s")])
case("the summed field in the direction of the last order", q("ordered", orderBy=[{"field": f("k"), "direction": "DESCENDING"}], limit=2), [total("v", "s")])
case("the summed field after an ascending order", q("ordered", orderBy=[{"field": f("k"), "direction": "ASCENDING"}], offset=1, limit=1), [total("v", "s")])
case("an order by the summed field", q("ordered", orderBy=[{"field": f("v"), "direction": "DESCENDING"}], limit=1), [total("v", "s")])
case("avg with limit 1: bytes after numbers", q("bytes_first", limit=1), [avg("b")])
case("avg with offset 2: a timestamp after numbers", q("time_first", offset=2), [avg("b")])
# The paths `sum` and `avg` read (#262): read as the aggregation is (array offsets, `__key__`,
# reserved names but `__key__` and `__type__`, rest.py); with the list, after its length and
# the reserved aliases, each aggregation in turn: `__name__` or `__type__` itself is not
# supported, then what its path holds as a filter's would (`__key__` in a longer path,
# `__type__` but at the end of a longer path, array offsets), then its alias.
for p in ["__key__", "a.__key__", "__x__", "__type__", "a.__type__", "a.__type__.b", "__type__[0]", "a.__name__", "a[0]", "a[2147483648]"]:
    case(f"sum of {p}", q("ints"), [total(p, "s")])
case("avg of __type__", q("ints"), [avg("__type__", "a")])
case("a sum of an array offset, then a reserved name", q("ints"), [total("a[0]", "s"), total("__x__", "t")])
case("a sum of __type__, then an array offset", q("ints"), [total("__type__", "s"), total("a[0]", "t")])
case("a sum of an array offset, then __type__", q("ints"), [total("a[0]", "s"), total("__type__", "t")])
case("a sum of __name__, then an array offset", q("ints"), [total("__name__", "s"), total("a[0]", "t")])
case("a sum of an array offset and a count up to -1", q("ints"), [total("a[0]", "s"), count("n", up_to=-1)])
case("a sum of an array offset and six aggregations", q("ints"), [total("a[0]", "s"), count("a"), count("b"), count("c"), count("d"), count("e")])
case("a sum of an array offset and a reserved alias", q("ints"), [total("a[0]", "s"), count("__x__")])
case("a sum of __name__ and a reserved alias after it", q("ints"), [total("__name__", "s"), count("__x__")])
case("a sum of __type__ and a duplicate alias", q("ints"), [total("__type__", "x"), count("x")])
case("a sum of an array offset and a duplicate alias", q("ints"), [total("a[0]", "x"), count("x")])
case("a duplicate alias before a sum of __name__", q("ints"), [count("x"), count("x"), total("__name__", "s")])
case("a duplicate alias before a reserved alias", q("ints"), [count("x"), count("x"), count("__y__")])
case("a reserved filter path before a sum of __type__", q("ints", where={"fieldFilter": {"field": f("__x__"), "op": "EQUAL", "value": i(1)}}), [total("__type__", "s")])
case("a filter without a value before a sum of an array offset", q("ints", where={"fieldFilter": {"field": f("v"), "op": "EQUAL"}}), [total("a[0]", "s")])
case("a sum of an array offset before duplicate orders", q("ints", orderBy=[{"field": f("v")}, {"field": f("v")}]), [total("a[0]", "s")])
case("a sum of __type__ before an array offset in a filter", q("ints", where={"fieldFilter": {"field": f("a[0]"), "op": "EQUAL", "value": i(1)}}), [total("__type__", "s")])
case("an array offset in the projection before a sum of __type__", q("ints", select={"fields": [f("a[0]")]}), [total("__type__", "s")])
case("a sum of __type__ without a collection", {}, [total("__type__", "s")])
case("a sum of __type__ and a negative limit", q("ints", limit=-1), [total("__type__", "s")])

# Found by the differential fuzzer, seed 20261014 (#262): a descending key scan (__name__
# descending, twice too) is refused before the fields a sum or an avg reads need an index;
# array-contains on __name__ is a filter on a field, so a query with one is no key scan. The
# fields a sum or an avg reads count as orders toward the most filters and orders a query has.
NAME_DESC = {"field": f("__name__"), "direction": "DESCENDING"}
NAME_IN = {"fieldFilter": {"field": f("__name__"), "op": "ARRAY_CONTAINS_ANY", "value": {"arrayValue": {"values": [{"referenceValue": "{documents}/ints/a"}]}}}}
def in_eqs(values, n): return {"compositeFilter": {"op": "AND", "filters": [
    {"fieldFilter": {"field": f("a"), "op": "IN", "value": {"arrayValue": {"values": [i(k) for k in range(values)]}}}},
    *[{"fieldFilter": {"field": f(f"e{k}"), "op": "EQUAL", "value": i(1)}} for k in range(n)]]}}
case("avg with __name__ descending twice", q("ints", orderBy=[NAME_DESC, NAME_DESC]), [avg("v", "a")])
case("count with __name__ descending twice, without a collection", {"orderBy": [NAME_DESC, NAME_DESC]}, [count("n")])
case("count with array-contains-any on __name__ and __name__ descending", q("ints", where=NAME_IN, orderBy=[NAME_DESC]), [count("n")])
case("avg with array-contains-any on __name__ and __name__ descending, without a collection", {"where": NAME_IN, "orderBy": [NAME_DESC]}, [avg("v", "a")])
case("a count with in 11 and eight equalities", q("ints", where=in_eqs(11, 8)), [count("n")])
case("an avg with in 11 and eight equalities", q("ints", where=in_eqs(11, 8)), [avg("v", "a")])
case("an avg with in 14 and six equalities", q("ints", where=in_eqs(14, 6)), [avg("v", "a")])
case("a sum and an avg of two fields with in 14 and six equalities", q("ints", where=in_eqs(14, 6)), [total("v", "s"), avg("u", "a")])
case("a sum of an equality's field with in 11 and eight equalities", q("ints", where=in_eqs(11, 8)), [total("e0", "s")])

# A `sum` or an `avg` and the query's projection (#262), checked with the list, after the
# reserved aliases and before each aggregation's own checks: a projection of no field but the
# name returns keys only, and a sum or an avg of any other field is refused; any other
# projection refuses a field it does not hold, then any sum or avg at all.
def sel(*paths): return {"fields": [f(p) for p in paths]}
for label, projection, aggs in [
    ("an empty projection", {}, [total("v", "s")]),
    ("an empty projection, a count", {}, [count("n")]),
    ("a projection of no fields, an avg", sel(), [avg("v", "a")]),
    ("a projection of the name", sel("__name__"), [total("v", "s")]),
    ("a projection of the name, a count", sel("__name__"), [count("n")]),
    ("a projection of the field", sel("v"), [total("v", "s")]),
    ("a projection of another field", sel("w"), [total("v", "s")]),
    ("a projection of the field and another", sel("v", "w"), [total("v", "s")]),
    ("a projection of a map, a field in it", sel("m"), [total("m.x", "s")]),
    ("a projection of a field in a map, the map", sel("m.x"), [total("m", "s")]),
    ("a projection of a quoted path with a dot", sel("`m.x`"), [total("m.x", "s")]),
    ("a projection of the field and the name", sel("v", "__name__"), [total("v", "s")]),
    ("a projection of the name and another field", sel("__name__", "w"), [total("v", "s")]),
    ("a projection, a count", sel("v"), [count("n")]),
    ("a projection, a missing field before a present one", sel("v"), [total("w", "s"), total("v", "t")]),
    ("a projection, a present field before a missing one", sel("v"), [total("v", "s"), total("w", "t")]),
    ("an empty projection, a sum of __name__", {}, [total("__name__", "s")]),
    ("an empty projection, a sum of __type__", {}, [total("__type__", "s")]),
    ("a projection, a sum of __type__", sel("w"), [total("__type__", "s")]),
    ("an empty projection, a sum of an array offset", {}, [total("a[0]", "s")]),
    ("a projection, a sum of an array offset", sel("w"), [total("a[0]", "s")]),
    ("an empty projection, a reserved alias", {}, [total("v", "__x__")]),
    ("an empty projection, a duplicate alias", {}, [total("v", "x"), count("x")]),
    ("an empty projection, a count up to -1", {}, [total("v", "s"), count("n", up_to=-1)]),
]:
    case(f"{label}", q("ints", select=projection), aggs)
case("a projection before two array-contains", q("ints", select=sel("v"), where={"compositeFilter": {"op": "AND", "filters": [
    {"fieldFilter": {"field": f("b"), "op": "ARRAY_CONTAINS", "value": i(1)}}, {"fieldFilter": {"field": f("c"), "op": "ARRAY_CONTAINS", "value": i(1)}}]}}), [total("v", "s")])
case("an empty projection before duplicate orders", q("ints", select={}, orderBy=[{"field": f("v")}, {"field": f("v")}]), [total("v", "s")])
case("a projection before an order by the name", q("ints", select=sel("w"), orderBy=[{"field": f("__name__")}]), [total("v", "s")])
# A sum or an avg orders the query by its field, so a query on the name (ordered by it, or
# with a filter on it other than array membership) needs every field it reads among its orders
# or the fields of its inequalities: "This query requires an index that has fields [...] after
# __name__", the fields in path order. Checked after the array-contains filters and the key's
# equalities, before the disjunctions and the orders; a descending key scan comes first.
def by(*paths): return [{"field": f(p.split(" ")[0]), "direction": "DESCENDING" if p.endswith(" desc") else "ASCENDING"} for p in paths]
def key_filter(op, value): return {"fieldFilter": {"field": f("__name__"), "op": op, "value": value}}
REF = {"referenceValue": "{documents}/ints/a"}
for label, query, aggs in [
    ("ordered by the name", q("ints", orderBy=by("__name__")), [total("v", "s")]),
    ("ordered by the name, a count", q("ints", orderBy=by("__name__")), [count("n")]),
    ("ordered by the field, then the name", q("ints", orderBy=by("v", "__name__")), [total("v", "s")]),
    ("ordered by another field, then the name", q("ints", orderBy=by("w", "__name__")), [total("v", "s")]),
    ("ordered by the field, then the name, two sums", q("ints", orderBy=by("v", "__name__")), [total("v", "s"), total("w", "t")]),
    ("ordered by the name, three fields", q("ints", orderBy=by("__name__")), [avg("m.x", "a"), total("v", "s"), avg("`x y`", "b")]),
    ("ordered by the name, a field twice", q("ints", orderBy=by("__name__")), [total("v", "s"), total("v", "t")]),
    ("ordered by another field, then the name descending", q("ints", orderBy=by("w", "__name__ desc")), [total("v", "s")]),
    ("ordered by the name descending", q("ints", orderBy=by("__name__ desc")), [total("v", "s")]),
    ("ordered by the name twice", q("ints", orderBy=by("__name__", "__name__ desc")), [total("v", "s")]),
    ("ordered by the name, then another field", q("ints", orderBy=by("__name__", "w")), [total("v", "s")]),
    ("a key inequality", q("ints", where=key_filter("GREATER_THAN", REF)), [total("v", "s")]),
    ("a key equality", q("ints", where=key_filter("EQUAL", REF)), [total("v", "s")]),
    ("a key equality, ordered by the field", q("ints", where=key_filter("EQUAL", REF), orderBy=by("v")), [total("v", "s")]),
    ("a key equality, ordered by another field", q("ints", where=key_filter("EQUAL", REF), orderBy=by("w")), [total("v", "s")]),
    ("a key in", q("ints", where=key_filter("IN", {"arrayValue": {"values": [REF]}})), [total("v", "s")]),
    ("is null on the key", q("ints", where={"unaryFilter": {"field": f("__name__"), "op": "IS_NULL"}}), [total("v", "s")]),
    ("is null on the key, ordered by the field", q("ints", where={"unaryFilter": {"field": f("__name__"), "op": "IS_NULL"}}, orderBy=by("v")), [total("v", "s")]),
    ("a key equality in an or", q("ints", where={"compositeFilter": {"op": "OR", "filters": [{"fieldFilter": {"field": f("v"), "op": "EQUAL", "value": i(1)}}, key_filter("EQUAL", REF)]}}), [total("v", "s")]),
    ("an inequality on the field, ordered by the name", q("ints", where={"fieldFilter": {"field": f("v"), "op": "GREATER_THAN", "value": i(1)}}, orderBy=by("__name__")), [total("v", "s")]),
    ("an inequality on another field, ordered by the name", q("ints", where={"fieldFilter": {"field": f("w"), "op": "GREATER_THAN", "value": i(1)}}, orderBy=by("__name__")), [total("v", "s")]),
    ("an inequality on another field, ordered by the name descending", q("ints", where={"fieldFilter": {"field": f("w"), "op": "GREATER_THAN", "value": i(1)}}, orderBy=by("__name__ desc")), [total("v", "s")]),
    ("ordered by the name, a negative limit", q("ints", orderBy=by("__name__"), limit=-1), [total("v", "s")]),
    ("ordered by the name, without a collection", {"orderBy": by("__name__")}, [total("v", "s")]),
    ("ordered by the name, two array-contains", q("ints", orderBy=by("__name__"), where={"compositeFilter": {"op": "AND", "filters": [
        {"fieldFilter": {"field": f("b"), "op": "ARRAY_CONTAINS", "value": i(1)}}, {"fieldFilter": {"field": f("c"), "op": "ARRAY_CONTAINS", "value": i(1)}}]}}), [total("v", "s")]),
    ("ordered by the name, too many disjunctions", q("ints", orderBy=by("__name__"), where={"compositeFilter": {"op": "AND", "filters": [
        {"fieldFilter": {"field": f("b"), "op": "IN", "value": {"arrayValue": {"values": [i(n) for n in range(16)]}}}},
        {"fieldFilter": {"field": f("c"), "op": "IN", "value": {"arrayValue": {"values": [i(1), i(2)]}}}}]}}), [total("v", "s")]),
    ("an inequality next to a key equality, ordered by the name", q("ints", orderBy=by("__name__"), where={"compositeFilter": {"op": "AND", "filters": [
        {"fieldFilter": {"field": f("w"), "op": "GREATER_THAN", "value": i(1)}}, key_filter("EQUAL", REF)]}}), [total("v", "s")]),
    ("ordered by the name, then the field", q("ints", orderBy=by("__name__", "v")), [total("v", "s")]),
    ("ordered by the name, then one of two fields", q("ints", orderBy=by("__name__", "v")), [total("v", "s"), avg("w", "a")]),
    ("array membership on the key", q("ints", where=key_filter("ARRAY_CONTAINS_ANY", {"arrayValue": {"values": [REF]}})), [total("v", "s")]),
    ("array membership on the key, duplicate orders", q("ints", where=key_filter("ARRAY_CONTAINS", REF), orderBy=by("w", "w")), [total("v", "s")]),
    ("ordered by the name, a sum of __type__", q("ints", orderBy=by("__name__")), [total("__type__", "s")]),
    ("ordered by the name, a duplicate alias", q("ints", orderBy=by("__name__")), [total("v", "x"), count("x")]),
]:
    case(label, query, aggs)


def fill(value):
    if isinstance(value, str):
        return value.replace("{documents}", DOCUMENTS)
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


if __name__ == "__main__":
    for doc in DATASET:
        status, message, _ = call("POST", f"{DOCUMENTS}:commit", {"writes": [{"update": {"name": f"{DOCUMENTS}/{doc['name']}", "fields": doc["fields"]}}]})
        assert status == "OK", (doc, status, message)
    for c in CASES:
        parent = DOCUMENTS + ("/" + c["parent"] if c["parent"] else "")
        body = {"structuredAggregationQuery": {"structuredQuery": fill(c["query"]), "aggregations": c["aggregations"]}}
        status, message, responses = call("POST", f"{parent}:runAggregationQuery", body)
        outcome = {"status": status}
        if status != "OK":
            outcome["message"] = message.replace(PROJECT, "{project}")
        else:
            outcome["responses"] = [
                {k: ("<time>" if k == "readTime" else v) for k, v in r.items()} for r in responses]
        c["outcome"] = outcome
    json.dump({
        "oracle": "cloud-firestore-emulator v1.22.0 (sha256 9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)",
        "generated_by": "tools/oracle/aggregations.py",
        "dataset": DATASET,
        "cases": CASES,
    }, sys.stdout, ensure_ascii=False, indent=1)
    print()
