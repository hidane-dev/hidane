"""How does the official emulator run `find_nearest`, and how does it check vector values? (#118)

Usage:
    python3 -I tools/oracle/find_nearest.py 127.0.0.1:8086 > crates/hidane/tests/fixtures/find_nearest.json

Seeds one collection of vectors over REST, then records:

- `queries`: `:runQuery` with `findNearest` (distance measures, thresholds, the distance field,
  ties, filters, projections, orderings, collection groups) and every validation error, with
  pairs that show which check comes first. Each answer is the documents' names with their
  fields but the vector, or the error.
- `aggregations`: `:runAggregationQuery` over a `findNearest` query.
- `writes`: `:commit` of malformed and edge-case vector values (`{__type__: "__vector__",
  value: [...]}`) and other `__type__` maps.

A vector field path such as `m.v` names a nested field in hidane, as in production; the
official emulator reads it as one field named `m.v` (docs/parity-exceptions.md), so no case
uses one. `crates/hidane/tests/find_nearest.rs` replays everything against hidane.
"""

import http.client
import json
import sys

HOST = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1:8080"
DB = "projects/find-nearest/databases/(default)"
DOCS = f"{DB}/documents"


def vec(*xs):
    return {"mapValue": {"fields": {"__type__": {"stringValue": "__vector__"},
                                    "value": {"arrayValue": {"values": [{"doubleValue": x} for x in xs]}}}}}


def typed(t, value=None, **extra):
    fields = {"__type__": t, **extra}
    if value is not None:
        fields["value"] = value
    return {"mapValue": {"fields": fields}}


def arr(*values):
    return {"arrayValue": {"values": list(values)}}


S = lambda s: {"stringValue": s}
D = lambda d: {"doubleValue": d}
I = lambda i: {"integerValue": str(i)}

SEED = {
    "z": {"v": vec(1, 0), "k": I(1)},
    "y": {"v": vec(0, 1), "k": I(2)},
    "x": {"v": vec(2, 2), "k": I(1)},
    "w": {"v": vec(1, 0, 0)},
    "u": {"v": vec(0, 0)},
    "t": {"v": arr(D(1), D(1))},
    "s": {"v": I(5)},
    "r": {"v": vec(-1, -1)},
    "q": {"k": I(3)},
}
SUB = {"g/x/v/sub": {"v": vec(1, 1)}}

E = {"vectorField": {"fieldPath": "v"}, "queryVector": vec(1, 1), "distanceMeasure": "EUCLIDEAN", "limit": 10}
COSINE = {**E, "distanceMeasure": "COSINE"}
DOT = {**E, "distanceMeasure": "DOT_PRODUCT"}
BAD_WHERE = {"where": {"fieldFilter": {"field": {"fieldPath": "a..b"}, "op": "EQUAL", "value": I(1)}}}
CURSOR = {"orderBy": [{"field": {"fieldPath": "__name__"}}], "startAt": {"values": [{"referenceValue": f"{DOCS}/v/y"}]}}

QUERIES = [
    # (name, findNearest, other StructuredQuery fields, collection selector)
    ("euclidean, distance field", {**E, "distanceResultField": "d"}, {}),
    ("cosine, a zero vector last", {**COSINE, "distanceResultField": "d"}, {}),
    ("dot product, largest first", {**DOT, "distanceResultField": "d"}, {}),
    ("dot product, threshold", {**DOT, "distanceThreshold": 1.5}, {}),
    ("cosine, threshold", {**COSINE, "distanceThreshold": 0.1}, {}),
    ("euclidean, threshold 0", {**E, "distanceThreshold": 0}, {}),
    ("euclidean, threshold on a distance", {**E, "distanceThreshold": 1.0}, {}),
    ("euclidean, negative threshold", {**E, "distanceThreshold": -1}, {}),
    ("distance field replaces a field", {**E, "distanceResultField": "k"}, {}),
    ("distance field with a dot", {**E, "distanceResultField": "a.b", "limit": 1}, {}),
    ("distance field not a path", {**E, "distanceResultField": "a..b", "limit": 1}, {}),
    ("distance field __name__", {**E, "distanceResultField": "__name__"}, {}),
    ("distance field reserved", {**E, "distanceResultField": "__x__"}, {}),
    ("a filter first", E, {"where": {"fieldFilter": {"field": {"fieldPath": "k"}, "op": "EQUAL", "value": I(1)}}}),
    ("a projection drops the distance", {**E, "distanceResultField": "d"}, {"select": {"fields": [{"fieldPath": "k"}]}}),
    ("a projection of the distance", {**E, "distanceResultField": "d"}, {"select": {"fields": [{"fieldPath": "d"}]}}),
    ("ties ordered by the query", E, {"orderBy": [{"field": {"fieldPath": "k"}, "direction": "DESCENDING"}]}),
    ("ties ordered by name", E, {"orderBy": [{"field": {"fieldPath": "__name__"}}]}),
    ("name descending", E, {"orderBy": [{"field": {"fieldPath": "__name__"}, "direction": "DESCENDING"}]}),
    ("a limit of 2", {**E, "limit": 2}, {}),
    ("a limit of 1000", {**E, "limit": 1000}, {}),
    ("a limit of 0", {**E, "limit": 0}, {}),
    ("a negative limit", {**E, "limit": -1}, {}),
    ("a limit of 1001", {**E, "limit": 1001}, {}),
    ("no limit", {k: v for k, v in E.items() if k != "limit"}, {}),
    ("a query vector of 3 dimensions", {**E, "queryVector": vec(1, 0, 0)}, {}),
    ("a query vector that is not one", {**E, "queryVector": I(1)}, {}),
    ("a query vector that is an array", {**E, "queryVector": arr(D(1), D(1))}, {}),
    ("no query vector", {k: v for k, v in E.items() if k != "queryVector"}, {}),
    ("an empty query vector", {**E, "queryVector": vec()}, {}),
    ("a query vector of integers", {**E, "queryVector": typed(S("__vector__"), arr(I(1), I(1)))}, {}),
    ("a query vector with NaN", {**E, "queryVector": vec("NaN", 1)}, {}),
    ("a query vector with an extra key", {**E, "queryVector": typed(S("__vector__"), arr(D(1)), x=D(1))}, {}),
    ("a query vector without values", {**E, "queryVector": typed(S("__vector__"))}, {}),
    ("no distance measure", {k: v for k, v in E.items() if k != "distanceMeasure"}, {}),
    ("no vector field", {k: v for k, v in E.items() if k != "vectorField"}, {}),
    ("a vector field that is not a path", {**E, "vectorField": {"fieldPath": "a..b"}}, {}),
    ("a vector field nobody has", {**E, "vectorField": {"fieldPath": "nope"}}, {}),
    ("the name as vector field", {**E, "vectorField": {"fieldPath": "__name__"}}, {}),
    ("a query limit", E, {"limit": 1}),
    ("an offset", E, {"offset": 1}),
    ("a cursor", E, CURSOR),
    ("query vector before limit", {**E, "queryVector": I(1), "limit": 0}, {}),
    ("query vector before measure", {**E, "queryVector": I(1), "distanceMeasure": "DISTANCE_MEASURE_UNSPECIFIED"}, {}),
    ("limit before measure", {**E, "limit": 0, "distanceMeasure": "DISTANCE_MEASURE_UNSPECIFIED"}, {}),
    ("measure before vector field", {**E, "distanceMeasure": "DISTANCE_MEASURE_UNSPECIFIED", "vectorField": {"fieldPath": "a..b"}}, {}),
    ("vector field before query limit", {**E, "vectorField": {"fieldPath": "a..b"}}, {"limit": 1}),
    ("query limit before offset", E, {"limit": 1, "offset": 1}),
    ("offset before cursor", E, {"offset": 1, **CURSOR}),
    ("cursor before distance field", {**E, "distanceResultField": "__x__"}, CURSOR),
    ("the query before find_nearest", {**E, "queryVector": I(1)}, BAD_WHERE),
    ("a collection group", E, {"from": [{"collectionId": "v", "allDescendants": True}]}),
]


def call(method, path, body=None):
    c = http.client.HTTPConnection(*HOST.split(":"), timeout=10)
    c.request(method, path, body=None if body is None else json.dumps(body),
              headers={"Content-Type": "application/json", "Authorization": "Bearer owner"})
    r = c.getresponse()
    raw = r.read().decode()
    try:
        return r.status, json.loads(raw)
    except ValueError:
        return r.status, raw


def outcome(status, body):
    if status != 200:
        return {"status": status, "message": body["error"].get("message", "")}
    return [{"name": r["document"]["name"][len(DOCS) + 1:],
             "fields": {k: v for k, v in r["document"].get("fields", {}).items() if k != "v"}}
            for r in body if "document" in r]


def commit(fields):
    status, body = call("POST", f"/v1/{DOCS}:commit",
                        {"writes": [{"update": {"name": f"{DOCS}/writes/w", "fields": fields}}]})
    return {"status": status, "message": body["error"]["message"] if status != 200 else ""}


WRITES = [
    ("a vector", {"v": vec(1, 2)}),
    ("a vector of 2048 dimensions", {"v": vec(*([1.0] * 2048))}),
    ("a vector of 2049 dimensions", {"v": vec(*([1.0] * 2049))}),
    ("a vector with infinity", {"v": vec("Infinity", "-Infinity")}),
    ("a vector with NaN", {"v": vec("NaN")}),
    ("a vector of integers", {"v": typed(S("__vector__"), arr(I(1)))}),
    ("NaN before an integer", {"v": typed(S("__vector__"), arr(D("NaN"), I(1)))}),
    ("an empty vector", {"v": vec()}),
    ("no values", {"v": typed(S("__vector__"))}),
    ("values not an array", {"v": typed(S("__vector__"), I(1))}),
    ("an extra key", {"v": typed(S("__vector__"), arr(D(1)), x=D(1))}),
    ("an extra key and no values", {"v": typed(S("__vector__"), x=D(1))}),
    ("an extra key and values not an array", {"v": typed(S("__vector__"), I(1), x=D(1))}),
    ("an extra key and an empty vector", {"v": typed(S("__vector__"), arr(), x=D(1))}),
    ("an extra key and 2049 dimensions", {"v": typed(S("__vector__"), arr(*([D(1)] * 2049)), x=D(1))}),
    ("2049 integers", {"v": typed(S("__vector__"), arr(*([I(1)] * 2049)))}),
    ("a vector in a map", {"m": {"mapValue": {"fields": {"v": vec(1)}}}}),
    ("a vector in an array", {"a": arr(vec(1))}),
    ("an unknown type", {"v": typed(S("other"), arr(D(1)))}),
    ("an unknown type in a map", {"m": {"mapValue": {"fields": {"n": {"mapValue": {"fields": {"v": typed(S("other"))}}}}}}}),
    ("an unknown type in an array", {"zz": arr(typed(S("other")))}),
    ("a type that is an integer", {"v": typed(I(1))}),
    ("a type that is a boolean", {"v": typed({"booleanValue": True})}),
    ("a type that is a double", {"v": typed(D(1))}),
    ("a type that is null", {"v": typed({"nullValue": None})}),
    ("a type that is a map", {"v": typed({"mapValue": {}})}),
    ("a type that is an array", {"v": typed(arr())}),
    ("a type that is a timestamp", {"v": typed({"timestampValue": "2020-01-01T00:00:00Z"})}),
    ("a type that is bytes", {"v": typed({"bytesValue": "AA=="})}),
    ("a type that is a reference", {"v": typed({"referenceValue": f"{DOCS}/c/x"})}),
    ("a type that is a geo point", {"v": typed({"geoPointValue": {"latitude": 1, "longitude": 1}})}),
    ("a reserved name in a map", {"m": {"mapValue": {"fields": {"__x__": I(1)}}}}),
    ("a reserved top-level name", {"__x__": I(1)}),
]

call("POST", "/reset")
status, _ = call("POST", f"/v1/{DOCS}:commit", {"writes": [
    {"update": {"name": f"{DOCS}/{p if '/' in p else 'v/' + p}", "fields": f}} for p, f in {**SEED, **SUB}.items()]})
assert status == 200
fixture = {
    "oracle": "cloud-firestore-emulator v1.22.0 (sha256 9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)",
    "generated_by": "tools/oracle/find_nearest.py",
    "seed": {**{f"v/{k}": v for k, v in SEED.items()}, **SUB},
    "queries": [],
    "aggregations": [],
    "writes": [],
}
for name, find, extra in QUERIES:
    query = {"from": [{"collectionId": "v"}], "findNearest": find, **extra}
    fixture["queries"].append({"name": name, "query": query, "outcome": outcome(*call("POST", f"/v1/{DOCS}:runQuery", {"structuredQuery": query}))})
for name, find, aggregations in [
    ("count of the nearest 2", {**E, "limit": 2}, [{"alias": "n", "count": {}}]),
    ("sum and average of the nearest 3", {**E, "limit": 3}, [{"alias": "s", "sum": {"field": {"fieldPath": "k"}}}, {"alias": "a", "avg": {"field": {"fieldPath": "k"}}}]),
]:
    request = {"structuredAggregationQuery": {"structuredQuery": {"from": [{"collectionId": "v"}], "findNearest": find}, "aggregations": aggregations}}
    status, body = call("POST", f"/v1/{DOCS}:runAggregationQuery", request)
    fixture["aggregations"].append({"name": name, "request": request,
                                    "outcome": [r["result"]["aggregateFields"] for r in body if "result" in r] if status == 200 else {"status": status, "message": body["error"]["message"]}})
for name, fields in WRITES:
    fixture["writes"].append({"name": name, "fields": fields, "outcome": commit(fields)})
json.dump(fixture, sys.stdout, ensure_ascii=False, indent=1)
print()
