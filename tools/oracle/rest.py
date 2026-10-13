"""How does the official emulator answer REST requests? (#34)

Usage:
    python3 -I tools/oracle/rest.py 127.0.0.1:8086 > crates/hidane/tests/fixtures/rest.json

Sends a sequence of REST requests to one project and records each answer: status, content
type and body. `crates/hidane/tests/rest.rs` sends the same sequence to hidane and compares the
bodies byte for byte, so the layout (protobuf-java JsonFormat's) is checked too, after the same
normalisation: the project ID becomes `{project}`, server times (`createTime`, `updateTime`,
`readTime`, `commitTime`) `<time>`, transaction IDs `<id>`, page tokens `<token>` and
generated document IDs `<auto>` (hidane's values differ by design, docs/parity-exceptions.md).
Field names in the data are in name order, so the official emulator's write order and hidane's
name order (#108) agree.

Cases with `compare: "status"` check only the status and content type: their messages print
the official emulator's internal keys (docs/parity-exceptions.md). Requests that hang the
official emulator (a `parent` in a `:listCollectionIds` body, unparsable query parameters,
`?transaction=` on GET) are left out and covered by hidane's own tests.
"""

import http.client
import json
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from harness import mask  # noqa: E402

HOST = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1:8080"
PROJECT = f"rest-{time.strftime('%H%M%S')}"
OWNER = {"Authorization": "Bearer owner"}

B = "/v1/projects/{project}/databases/(default)/documents"
NAME = "projects/{project}/databases/(default)/documents"

CASES = []
def case(name, method, path, body=None, headers=OWNER, compare="exact", raw=None):
    c = {"name": name, "method": method, "path": path, "headers": headers, "compare": compare}
    if raw is not None:
        c["raw"] = raw
    elif body is not None:
        c["body"] = body
    CASES.append(c)

def i(n): return {"integerValue": str(n)}
def d(x): return {"doubleValue": x}
def s(x): return {"stringValue": x}

VALUES = {
    "arr": {"arrayValue": {"values": [i(1), s("s"), {"mapValue": {"fields": {"k": i(1)}}}]}},
    "arr0": {"arrayValue": {}},
    "big": i(9223372036854775807),
    "bool": {"booleanValue": True},
    "bytes": {"bytesValue": "AQI="},
    "dbl": d(1.0),
    "dbl2": d(1.5),
    "geo": {"geoPointValue": {"latitude": 1.5, "longitude": -2}},
    "geo0": {"geoPointValue": {}},
    "int": i(42),
    "map": {"mapValue": {"fields": {"a": i(1), "b": {"mapValue": {"fields": {"c": s("x")}}}}}},
    "map0": {"mapValue": {}},
    "nan": d("NaN"),
    "neg0": d(-0.0),
    "neginf": d("-Infinity"),
    "null": {"nullValue": None},
    "posinf": d("Infinity"),
    "ref": {"referenceValue": NAME + "/c/b"},
    "str": s("<>&='\"\\\n\t\u0001éあ\U0001F600 x/y"),
    "ts": {"timestampValue": "2020-01-01T00:00:00.123456789Z"},
    "ts0": {"timestampValue": "2020-01-01T00:00:00Z"},
    "tsms": {"timestampValue": "2020-01-01T00:00:00.120Z"},
}
DOUBLES = [1e7, 9999999.0, 1e21, 1e-7, 0.001, 0.0001, 123456789.0, 1.7976931348623157e308, 0.1, -1.5e-10, 100.0, 1e16, 1e22, 12345.6789]

case("create every value type with PATCH", "PATCH", B + "/c/a", {"fields": VALUES})
case("doubles as Java prints them", "PATCH", B + "/c/doubles", {"fields": {f"d{n:02}": d(x) for n, x in enumerate(DOUBLES)}})
case("get a document", "GET", B + "/c/a")
# Masked fields come back in an internal hash order on the official emulator and in name order
# on hidane (#108); `bool` and `int` happen to agree. Aggregations list their aliases in name
# order for the same reason (the official emulator keeps request order).
case("get with a mask (repeated query parameter)", "GET", B + "/c/a?mask.fieldPaths=int&mask.fieldPaths=bool")
case("create an empty document", "PATCH", B + "/c/e", {})
case("list a collection", "GET", B + "/c?pageSize=2")
case("list with mask _none_ and showMissing (Emulator UI)", "GET", B + "/c?showMissing=true&mask.fieldPaths=_none_&pageSize=300")
case("list with showMissing, no auth", "GET", B + "/c?showMissing=true", headers={})
case("list an empty subcollection", "GET", B + "/c/e/sub")
case("get a missing document", "GET", B + "/c/missing")
case("get the database root", "GET", B)
case("v1beta1 and ?key=", "GET", "/v1beta1/projects/{project}/databases/(default)/documents/c/e?key=abc&alt=json")
case("percent-encoded path", "GET", "/v1/projects/{project}/databases/%28default%29/documents/c/e")
case("unknown query parameter", "GET", B + "/c/e?unknownParam=1")
case("missing document with a space in its ID", "GET", B + "/c/a%20b")
case("create with documentId", "POST", B + "/c?documentId=new", {"fields": {"x": i(1)}})
# In its own collection: a generated ID could sort anywhere among the documents of `c`.
case("create with a generated ID", "POST", B + "/gen", {"fields": {}})
case("create an existing document", "POST", B + "/c?documentId=new", {"fields": {}}, compare="status")
case("create a document with a non-ASCII ID", "POST", B + "/c?documentId=%E3%81%82", {"fields": {"x": i(2)}})
case("get it", "GET", B + "/c/%E3%81%82")
case("update with updateMask and mask", "PATCH", B + "/c/new?updateMask.fieldPaths=x&updateMask.fieldPaths=zz&mask.fieldPaths=x", {"fields": {"x": i(7)}})
case("update with currentDocument.exists=false on an existing document", "PATCH", B + "/c/new?currentDocument.exists=false", {}, compare="status")
case("delete", "DELETE", B + "/c/new")
case("delete with currentDocument.exists=true on a missing document", "DELETE", B + "/c/new?currentDocument.exists=true", compare="status")
case("commit", "POST", B + ":commit", {"writes": [
    {"update": {"name": NAME + "/c/b", "fields": {"n": i(1)}}},
    {"update": {"name": NAME + "/c/b2", "fields": {}}, "updateTransforms": [{"fieldPath": "n", "increment": i(2)}]},
    {"delete": NAME + "/c/gone"},
]})
case("empty commit", "POST", B + ":commit", {})
case("commit without a body", "POST", B + ":commit")
case("commit with a text/plain body (web SDK)", "POST", B + ":commit", raw='{"writes": []}', headers={"Content-Type": "text/plain", **OWNER})
case("commit with lenient ProtoJSON (number as integer, string as double, snake_case)", "POST", B + ":commit", {"writes": [
    {"update": {"name": NAME + "/c/lenient", "fields": {"i": {"integerValue": 5}, "d": {"doubleValue": "1.5"}}}, "current_document": {"exists": False}},
]})
case("commit with invalid JSON", "POST", B + ":commit", raw="{not json")
case("commit with a wrong type", "POST", B + ":commit", {"writes": "x"})
case("commit with an unknown field", "POST", B + ":commit", {"unknownField": 1})
case("commit a reserved field name", "POST", B + ":commit", {"writes": [{"update": {"name": NAME + "/c/r", "fields": {"__x__": i(1)}}}]})
case("batchGet", "POST", B + ":batchGet", {"documents": [NAME + "/c/e", NAME + "/c/zz"]})
case("batchGet with no documents", "POST", B + ":batchGet", {"documents": []})
case("batchGet in a new transaction", "POST", B + ":batchGet", {"documents": [NAME + "/c/e"], "newTransaction": {"readOnly": {}}})
case("runQuery with a projection", "POST", B + ":runQuery", {"structuredQuery": {"from": [{"collectionId": "c"}], "select": {"fields": [{"fieldPath": "int"}]}, "where": {"fieldFilter": {"field": {"fieldPath": "int"}, "op": "EQUAL", "value": i(42)}}}})
case("runQuery with offset (skippedResults)", "POST", B + ":runQuery", {"structuredQuery": {"from": [{"collectionId": "c"}], "offset": 1, "limit": 3, "select": {"fields": []}}})
case("runQuery with no result", "POST", B + ":runQuery", {"structuredQuery": {"from": [{"collectionId": "nothing"}]}})
case("runQuery under a document", "POST", B + "/c/e:runQuery", {"structuredQuery": {"from": [{"collectionId": "sub"}]}})
case("runQuery in a new transaction", "POST", B + ":runQuery", {"structuredQuery": {"from": [{"collectionId": "nothing"}]}, "newTransaction": {"readWrite": {}}})
case("invalid query", "POST", B + ":runQuery", {"structuredQuery": {"from": [{"collectionId": "c"}], "limit": -1}})
case("runAggregationQuery", "POST", B + ":runAggregationQuery", {"structuredAggregationQuery": {"structuredQuery": {"from": [{"collectionId": "c"}]}, "aggregations": [{"avg": {"field": {"fieldPath": "int"}}, "alias": "a"}, {"count": {}, "alias": "n"}, {"sum": {"field": {"fieldPath": "int"}}, "alias": "s"}]}})
case("beginTransaction", "POST", B + ":beginTransaction", {})
case("rollback a malformed transaction", "POST", B + ":rollback", {"transaction": "AAAA"})
case("rollback an unknown transaction", "POST", B + ":rollback", {"transaction": "EWQAAAAAAAAA"})
case("listCollectionIds", "POST", B + ":listCollectionIds", {})
case("listCollectionIds under a document", "POST", B + "/c/a:listCollectionIds", {})
case("listCollectionIds without auth", "POST", B + ":listCollectionIds", {}, headers={})
case("batchWrite", "POST", B + ":batchWrite", {"writes": [{"update": {"name": NAME + "/c/bw", "fields": {}}}, {"delete": NAME + "/c/zz"}]})
case("batchWrite with a failing precondition", "POST", B + ":batchWrite", {"writes": [{"update": {"name": NAME + "/c/x", "fields": {}}, "currentDocument": {"exists": True}}]}, compare="status")
case("partitionQuery", "POST", B + ":partitionQuery", {})
case("unknown verb", "POST", B + ":nosuchverb", {})
case("write is not REST", "POST", B + ":write", {})
case("listen is not REST", "POST", B + ":listen", {})
case("PUT is not a method", "PUT", B + "/c/a", {})
case("GET with a verb", "GET", B + ":commit")

# Differences the differential fuzzer found (tools/fuzz/, #262), minimised. A batchGet answers a
# document named more than once once, where it is first named (`c/e` exists, `c/zz` does not).
case("batchGet with documents named twice", "POST", B + ":batchGet", {"documents": [NAME + "/c/zz", NAME + "/c/e", NAME + "/c/zz", NAME + "/c/e"]})
case("batchGet with documents named twice, in a new transaction", "POST", B + ":batchGet", {"documents": [NAME + "/c/e", NAME + "/c/e"], "newTransaction": {"readOnly": {}}})
case("batchGet with documents named twice and a mask", "POST", B + ":batchGet", {"documents": [NAME + "/c/a", NAME + "/c/a"], "mask": {"fieldPaths": ["int"]}})
# A read mask (`mask.fieldPaths`) is checked like a field path: reserved names, `__name__` inside
# a path, the empty path. On a write it is checked after the write's names (its document, its
# fields' names, its update mask) and before its values and its precondition, and a write whose
# read mask is invalid writes nothing.
case("PATCH with a read mask that does not parse writes nothing", "PATCH", B + "/fz/a?mask.fieldPaths=x%20y", {"fields": {"a": i(1)}})
case("PATCH with a read mask that does not parse, the document", "GET", B + "/fz/a")
case("PATCH creating with a read mask that does not parse", "PATCH", B + "/fz/a?currentDocument.exists=false&mask.fieldPaths=%5C", {})
case("PATCH creating with a read mask that does not parse, the collection", "GET", B + "/fz")
case("POST with a read mask that does not parse writes nothing", "POST", B + "/fz?documentId=a&mask.fieldPaths=a-b", {})
case("POST with a generated ID and a read mask that does not parse", "POST", B + "/fz?mask.fieldPaths=a-b", {})
case("POST with a read mask that does not parse, the collection", "GET", B + "/fz")
case("PATCH with a reserved name in the read mask", "PATCH", B + "/fz/a?mask.fieldPaths=a&mask.fieldPaths=__x__", {})
case("PATCH with __name__ inside a read mask path", "PATCH", B + "/fz/a?mask.fieldPaths=a.__name__", {})
case("PATCH with an empty read mask path", "PATCH", B + "/fz/a?mask.fieldPaths=", {})
case("PATCH with a reserved name in the read mask, the collection", "GET", B + "/fz")
case("PATCH with a read mask of __name__", "PATCH", B + "/fz/b?mask.fieldPaths=__name__", {"fields": {"a": i(1)}})
case("an invalid read mask comes after a reserved field name", "PATCH", B + "/fz/b?mask.fieldPaths=x%20y", {"fields": {"__x__": i(1)}})
case("an invalid read mask comes after the update mask", "PATCH", B + "/fz/b?mask.fieldPaths=__x__&updateMask.fieldPaths=__y__", {})
case("an invalid read mask comes before nested arrays", "PATCH", B + "/fz/b?mask.fieldPaths=x%20y", {"fields": {"a": {"arrayValue": {"values": [{"arrayValue": {}}]}}}})
case("an invalid read mask comes before the precondition", "PATCH", B + "/fz/b?mask.fieldPaths=x%20y&currentDocument.exists=false", {"fields": {"a": i(2)}})
case("an invalid read mask comes before an update time precondition", "PATCH", B + "/fz/b?mask.fieldPaths=x%20y&currentDocument.updateTime=2001-01-01T00:00:00Z", {})
case("PATCH with an invalid read mask leaves the document as it was", "GET", B + "/fz/b")
case("GET with a reserved name in the read mask", "GET", B + "/fz/b?mask.fieldPaths=__x__")
case("GET with __name__ inside a read mask path", "GET", B + "/fz/b?mask.fieldPaths=a.__name__")
case("GET with an empty read mask path", "GET", B + "/fz/b?mask.fieldPaths=")
case("GET with a read mask of __name__", "GET", B + "/fz/b?mask.fieldPaths=__name__")
case("list with a reserved name in the read mask", "GET", B + "/fz?mask.fieldPaths=__x__")
case("list with an empty read mask path", "GET", B + "/fz?mask.fieldPaths=")
case("batchGet with a reserved name in the mask", "POST", B + ":batchGet", {"documents": [NAME + "/fz/b"], "mask": {"fieldPaths": ["__x__"]}})
case("batchGet with an empty mask path", "POST", B + ":batchGet", {"documents": [NAME + "/fz/b"], "mask": {"fieldPaths": [""]}})
# RunQuery and RunAggregationQuery need a structured query, checked after the transaction the
# request names (a malformed one comes first) and before a new transaction or a read time.
case("runQuery without a query", "POST", B + ":runQuery", {})
case("runQuery without a query, in a new transaction", "POST", B + ":runQuery", {"newTransaction": {"readOnly": {}}})
case("runQuery without a query, at a future read time", "POST", B + ":runQuery", {"readTime": "2999-01-01T00:00:00Z"})
case("runQuery without a query, with a malformed transaction", "POST", B + ":runQuery", {"transaction": "AAAA"})
case("runQuery with a malformed transaction and an invalid query", "POST", B + ":runQuery", {"structuredQuery": {"from": [{"collectionId": "c"}], "orderBy": [{"field": {"fieldPath": "a"}}, {"field": {"fieldPath": "a"}}]}, "transaction": "AAAA"})
case("runAggregationQuery without a query", "POST", B + ":runAggregationQuery", {})
case("runAggregationQuery without a query, in a new transaction", "POST", B + ":runAggregationQuery", {"newTransaction": {}})
case("runAggregationQuery without a query, with a malformed transaction", "POST", B + ":runAggregationQuery", {"transaction": "AAAA"})
case("runAggregationQuery with no aggregations and a malformed transaction", "POST", B + ":runAggregationQuery", {"structuredAggregationQuery": {}, "transaction": "AAAA"})
case("runAggregationQuery with a count and no query", "POST", B + ":runAggregationQuery", {"structuredAggregationQuery": {"aggregations": [{"count": {}}]}})
# Doubles are read correctly rounded (protobuf-java reads them through BigDecimal): these geo
# point coordinates lose their last digit when a double is read with a faster, inexact parser.
case("doubles read correctly rounded", "PATCH", B + "/fz/geo", {"fields": {
    "a": {"geoPointValue": {"latitude": 38.141717608275684, "longitude": -20.053480988037194}},
    "b": {"arrayValue": {"values": [{"geoPointValue": {"longitude": 117.96957752590589}}]}},
    "c": d(0.1234567890123456789), "e": d(9007199254740993), "f": d(5e-324)}})
case("doubles read correctly rounded, in a commit", "POST", B + ":commit", {"writes": [
    {"update": {"name": NAME + "/fz/geo2", "fields": {"g": {"geoPointValue": {"latitude": -89.99999999999997, "longitude": -20.053480988037194}}}}}]})
case("doubles read correctly rounded, read back", "POST", B + ":batchGet", {"documents": [NAME + "/fz/geo2"]})
# A body of JSON `null` is not valid for any request that reads a body (POST and PATCH), once the
# request is routed; GET and DELETE ignore their bodies.
case("batchGet with a null body", "POST", B + ":batchGet", raw="null")
case("runQuery with a null body", "POST", B + ":runQuery", raw="null")
case("runAggregationQuery with a null body", "POST", B + ":runAggregationQuery", raw="null")
case("commit with a null body", "POST", B + ":commit", raw="null")
case("batchWrite with a null body", "POST", B + ":batchWrite", raw="null")
case("beginTransaction with a null body", "POST", B + ":beginTransaction", raw=" null\n")
case("rollback with a null body", "POST", B + ":rollback", raw="null")
case("listCollectionIds with a null body", "POST", B + "/fz/b:listCollectionIds", raw="null")
case("PATCH with a null body", "PATCH", B + "/fz/null", raw="null")
case("POST with a null body", "POST", B + "/fz?documentId=null", raw="null")
case("a null body writes nothing", "GET", B + "/fz/null")
case("GET with a null body", "GET", B + "/fz/b", raw="null")
case("DELETE with a null body", "DELETE", B + "/fz/null", raw="null")
case("an unknown verb with a null body", "POST", B + ":nosuchverb", raw="null")
# A value without a type (`{}`) cannot be converted, over every way of writing, in a field, an
# array, a map or a transform's operand. It is found with the names of a write: in the order of
# the fields, depth first, next to reserved names (a special map's values are all read before
# its `__type__`), after the write's document name and before its update mask, its transforms
# and every later check (sequences, values, preconditions, verifies).
U = {}
def upd(name, fields, **extra):
    return dict({"update": {"name": NAME + "/" + name, "fields": fields}}, **extra)
def commit(*writes):
    return {"writes": list(writes)}
case("PATCH a value without a type", "PATCH", B + "/fz/unset", {"fields": {"v": U}})
case("POST a value without a type", "POST", B + "/fz?documentId=unset", {"fields": {"m": {"mapValue": {"fields": {"k": U}}}}})
case("batchWrite a value without a type", "POST", B + ":batchWrite", commit(upd("fz/unset", {"v": U})))
case("commit a value without a type in an array", "POST", B + ":commit", commit(upd("fz/unset", {"v": {"arrayValue": {"values": [U]}}})))
case("a value without a type writes nothing", "GET", B + "/fz/unset")
case("a reserved name, then a value without a type", "POST", B + ":commit", commit(upd("fz/unset", {"__x__": i(1), "v": U})))
case("a value without a type, then a reserved name", "POST", B + ":commit", commit(upd("fz/unset", {"v": U, "__x__": i(1)})))
case("a nested reserved name, then a value without a type", "POST", B + ":commit", commit(upd("fz/unset", {"m": {"mapValue": {"fields": {"__x__": i(1)}}}, "v": U})))
case("a value without a type, then a nested reserved name", "POST", B + ":commit", commit(upd("fz/unset", {"m": {"mapValue": {"fields": {"a": U, "__x__": i(1)}}}})))
case("a reserved name in a write before a value without a type", "POST", B + ":commit", commit(upd("fz/a1", {"__x__": i(1)}), upd("fz/a2", {"v": U})))
case("a value without a type before an invalid document name", "POST", B + ":commit", commit(upd("fz/a1", {"v": U}), upd("fz", {})))
case("a value without a type before the update mask", "POST", B + ":commit", commit(upd("fz/a1", {"v": U}, updateMask={"fieldPaths": ["a..b"]})))
case("a value without a type outside the update mask", "POST", B + ":commit", commit(upd("fz/a1", {"v": U, "w": i(1)}, updateMask={"fieldPaths": ["w"]})))
case("a value without a type before nested arrays", "POST", B + ":commit", commit(upd("fz/a1", {"a": {"arrayValue": {"values": [{"arrayValue": {}}]}}, "v": U})))
case("a value without a type in a nested array", "POST", B + ":batchWrite", commit(upd("fz/a1", {"v": {"arrayValue": {"values": [{"arrayValue": {"values": [U]}}]}}})))
case("a value without a type before an empty field name", "POST", B + ":commit", commit(upd("fz/a1", {"": U})))
case("a value without a type before a transform path", "POST", B + ":commit", commit(upd("fz/a1", {"v": U}, updateTransforms=[{"fieldPath": "a.__x__", "increment": i(1)}])))
case("a value without a type before the sequence of writes", "POST", B + ":commit", commit(upd("fz/a1", {"v": U}), upd("fz/a1", {}, currentDocument={"exists": False}), {"delete": NAME + "/fz/a1"}))
case("a value without a type before a precondition", "POST", B + ":commit", commit(upd("fz/a1", {"v": U}, currentDocument={"updateTime": "2001-01-01T00:00:00Z"})))
case("a value without a type before a verify", "POST", B + ":commit", commit({"verify": NAME + "/fz/zz", "currentDocument": {"exists": True}}, upd("fz/a1", {"v": U})))
case("a vector holding a value without a type", "POST", B + ":commit", commit(upd("fz/a1", {"v": {"mapValue": {"fields": {"__type__": s("__vector__"), "value": {"arrayValue": {"values": [i(1), U]}}}}}})))
case("a __type__ without a type", "POST", B + ":commit", commit(upd("fz/a1", {"v": {"mapValue": {"fields": {"__type__": U}}}})))
case("a value without a type after a __type__ that is not a string", "POST", B + ":commit", commit(upd("fz/a1", {"v": {"mapValue": {"fields": {"__type__": i(1), "x": U}}}})))
case("a value without a type next to an unknown __type__", "POST", B + ":commit", commit(upd("fz/a1", {"v": {"mapValue": {"fields": {"__type__": s("other"), "x": U}}}})))
case("an increment without a type", "POST", B + ":commit", commit(upd("fz/a1", {}, updateTransforms=[{"fieldPath": "x", "increment": U}])))
case("an increment holding a value without a type", "POST", B + ":commit", commit(upd("fz/a1", {}, updateTransforms=[{"fieldPath": "x", "increment": {"mapValue": {"fields": {"k": U}}}}])))
case("a maximum without a type", "POST", B + ":commit", commit({"transform": {"document": NAME + "/fz/a1", "fieldTransforms": [{"fieldPath": "x", "maximum": U}]}}))
case("an array union holding a value without a type", "POST", B + ":commit", commit(upd("fz/a1", {}, updateTransforms=[{"fieldPath": "x", "appendMissingElements": {"values": [U]}}])))
case("an array removal holding a value without a type", "POST", B + ":commit", commit({"transform": {"document": NAME + "/fz/a1", "fieldTransforms": [{"fieldPath": "x", "removeAllFromArray": {"values": [i(1), U]}}]}}))
case("an invalid transform path before an operand without a type", "POST", B + ":commit", commit(upd("fz/a1", {}, updateTransforms=[{"fieldPath": "a..b", "increment": U}])))
case("an operand without a type before nested transforms", "POST", B + ":commit", commit(upd("fz/a1", {}, updateTransforms=[{"fieldPath": "a", "increment": i(1)}, {"fieldPath": "a.b", "increment": U}])))
case("values without a type write nothing", "GET", B + "/fz/a1")
# A vector is one value, not a map: no field path goes into it. An update mask path into one
# finds nothing in the request's vector (the stored field is deleted there, if it is a map) and
# leaves a stored vector as it is, unless the request puts a map there, which replaces it.
VEC = {"mapValue": {"fields": {"__type__": s("__vector__"), "value": {"arrayValue": {"values": [d(1.0)]}}}}}
case("a vector", "PATCH", B + "/fz/vec", {"fields": {"v": VEC}})
case("an update mask path into a vector, nothing given", "PATCH", B + "/fz/vec?updateMask.fieldPaths=v.value", {})
case("an update mask path deeper into a vector, nothing given", "PATCH", B + "/fz/vec?updateMask.fieldPaths=v.value.x", {})
case("a read mask path into a vector", "GET", B + "/fz/vec?mask.fieldPaths=v.value")
case("an update mask path into a vector, a map given", "PATCH", B + "/fz/vec?updateMask.fieldPaths=v.value", {"fields": {"v": {"mapValue": {"fields": {"value": {"arrayValue": {"values": [d(2.0)]}}}}}}})
case("an update mask path into a map, a vector given", "PATCH", B + "/fz/vec?updateMask.fieldPaths=v.value", {"fields": {"v": VEC}})
case("an update mask path into a vector given for a new document", "PATCH", B + "/fz/vec2?updateMask.fieldPaths=_.value&updateMask.fieldPaths=n", {"fields": {"n": i(1), "_": VEC}})
case("an update mask path into a vector in a commit", "POST", B + ":commit", {"writes": [
    {"update": {"name": NAME + "/fz/vec3", "fields": {"n": i(1), "v": VEC}}, "updateMask": {"fieldPaths": ["v.value", "n"]}}]})
case("an update mask path into a vector in a commit, the document", "GET", B + "/fz/vec3")
case("a map", "PATCH", B + "/fz/vec4", {"fields": {"v": {"mapValue": {"fields": {"a": i(1)}}}}})
case("an update mask path into a map left as it is, a vector given", "PATCH", B + "/fz/vec4?updateMask.fieldPaths=v.value", {"fields": {"v": VEC}})
case("a read mask path into a vector written", "PATCH", B + "/fz/vec5?mask.fieldPaths=v.value", {"fields": {"v": VEC}})


def norm(text):
    return mask(text, PROJECT)


def run():
    out = []
    for c in CASES:
        conn = http.client.HTTPConnection(*HOST.split(":"), timeout=10)
        path = c["path"].replace("{project}", PROJECT)
        if "raw" in c:
            body = c["raw"]
        elif "body" in c:
            body = json.dumps(c["body"]).replace("{project}", PROJECT)
        else:
            body = None
        headers = dict(c["headers"])
        if body is not None and "Content-Type" not in headers:
            headers["Content-Type"] = "application/json"
        conn.request(c["method"], path, body=body, headers=headers)
        r = conn.getresponse()
        c = dict(c, outcome={"status": r.status, "contentType": r.getheader("content-type"), "body": norm(r.read().decode())})
        out.append(c)
    return out


json.dump({
    "oracle": "cloud-firestore-emulator v1.22.0 (sha256 9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)",
    "generated_by": "tools/oracle/rest.py",
    "cases": run(),
}, sys.stdout, ensure_ascii=False, indent=1)
print()
