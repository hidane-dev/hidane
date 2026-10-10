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
        body = {"structuredAggregationQuery": {"structuredQuery": c["query"], "aggregations": c["aggregations"]}}
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
