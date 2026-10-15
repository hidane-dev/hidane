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
import re
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
# A read time is whole microseconds and not before the epoch, checked before its age, and before
# the rest of a query once Security Rules allow it (#262).
QC = {"from": [{"collectionId": "c"}]}
case("runQuery at a read time with nanoseconds", "POST", B + ":runQuery", {"structuredQuery": QC, "readTime": "2020-01-01T00:00:00.000000001Z"})
case("runQuery at a future read time with nanoseconds", "POST", B + ":runQuery", {"structuredQuery": QC, "readTime": "2999-01-01T00:00:00.000000001Z"})
case("runQuery at a read time before the epoch", "POST", B + ":runQuery", {"structuredQuery": QC, "readTime": "0001-01-01T00:00:00Z"})
case("batchGet at a read time with nanoseconds", "POST", B + ":batchGet", {"documents": [NAME + "/c/d"], "readTime": "2020-01-01T00:00:00.000000001Z"})
case("runAggregationQuery at a read time before the epoch", "POST", B + ":runAggregationQuery",
     {"structuredAggregationQuery": {"structuredQuery": QC, "aggregations": [{"count": {}}]}, "readTime": "0001-01-01T00:00:00Z"})
case("runQuery with a negative limit, at a future read time", "POST", B + ":runQuery", {"structuredQuery": {**QC, "limit": -1}, "readTime": "2999-01-01T00:00:00Z"})
case("runAggregationQuery with duplicate orders, at a read time too old", "POST", B + ":runAggregationQuery",
     {"structuredAggregationQuery": {"structuredQuery": {**QC, "orderBy": [{"field": {"fieldPath": "a"}}, {"field": {"fieldPath": "a"}}]}, "aggregations": [{"count": {}}]},
      "readTime": "2020-01-01T00:00:00Z"})
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
# protobuf-java's JsonFormat reads a string field from any JSON primitive: a number or a boolean
# is its text (as written; these are written the way hidane writes them back).
case("numbers and booleans as strings", "PATCH", B + "/fz/num", raw='{"fields": {"a": {"stringValue": 1}, "b": {"stringValue": -3}, "c": {"stringValue": 1.5}, "d": {"stringValue": 0.1}, "e": {"stringValue": 12345678901234567890}, "f": {"stringValue": true}, "g": {"stringValue": false}}}')
case("a number as a document name", "POST", B + ":batchWrite", raw='{"writes": [{"update": {"name": 1}}]}')
case("a number as a document to get", "POST", B + ":batchGet", raw='{"documents": [1]}')
case("a number as a collection ID", "POST", B + ":runQuery", raw='{"structuredQuery": {"from": [{"collectionId": 1}]}}')
case("a number as a field path", "POST", B + ":commit", {"writes": [{"update": {"name": NAME + "/fz/num", "fields": {}}, "updateMask": {"fieldPaths": [1]}}]})
case("a number as a string in a filter", "POST", B + ":runQuery", raw='{"structuredQuery": {"where": {"fieldFilter": {"field": {"fieldPath": "v"}, "op": "NOT_IN", "value": {"mapValue": {"fields": {"k": {"stringValue": 1}}}}}}}}')
case("an object as a string", "PATCH", B + "/fz/num2", raw='{"fields": {"a": {"stringValue": {}}}}')
case("a number as bytes", "PATCH", B + "/fz/num2", raw='{"fields": {"a": {"bytesValue": 1}}}')
case("a boolean as an integer", "PATCH", B + "/fz/num2", raw='{"fields": {"a": {"integerValue": true}}}')
# PATCH and DELETE take any path below `documents` as a document name (`documents/*/**`), POST
# any as a parent and a collection ID (`documents/**/{collection_id}`): a path that names no
# document is refused by the name's checks, not with a bare 404.
case("PATCH a collection", "PATCH", B + "/fz", {})
case("PATCH a path with an empty ID", "PATCH", B + "/fz//d", {})
case("PATCH a path starting with a slash", "PATCH", B + "//fz/d", {})
case("PATCH a path ending with a slash", "PATCH", B + "/fz/d/", {})
case("PATCH a subcollection", "PATCH", B + "/fz/d/e", {})
case("PATCH a collection, with a null body", "PATCH", B + "/fz", raw="null")
case("DELETE a collection", "DELETE", B + "/fz")
case("DELETE a path with an empty ID", "DELETE", B + "/fz//d")
case("DELETE a path starting with a slash", "DELETE", B + "//fz/d")
case("DELETE a path ending with a slash", "DELETE", B + "/fz/d/")
case("DELETE a subcollection", "DELETE", B + "/fz/d/e")
case("POST to a document", "POST", B + "/fz/d", {})
case("POST to a document with an empty documentId", "POST", B + "/fz/d?documentId=", {})
case("POST to a path with an empty ID", "POST", B + "/fz//d", {})
case("POST to a path starting with a slash", "POST", B + "//fz", {})
case("POST to a path ending with a slash", "POST", B + "/fz/d/", {})
case("POST to a document, with a null body", "POST", B + "/fz/d", raw="null")
case("PATCH under a reserved collection ID", "PATCH", B + "/__x__/d", {})
# The documents root as a document name has a message of its own.
case("commit a transform of the documents root", "POST", B + ":commit", {"writes": [{"transform": {"document": NAME}}]})
case("commit an update of the documents root", "POST", B + ":commit", {"writes": [{"update": {"name": NAME, "fields": {}}}]})
case("commit a delete of the documents root", "POST", B + ":commit", {"writes": [{"delete": NAME}]})
case("batchGet the documents root", "POST", B + ":batchGet", {"documents": [NAME]})
case("batchGet another project's documents root", "POST", B + ":batchGet", {"documents": ["projects/other/databases/(default)/documents"]})
# Whitespace around each segment of a REST path is trimmed: the characters Unicode calls
# White_Space (tab to carriage return, space, U+0085, U+00A0, U+1680, U+2000 to U+200A, U+2028,
# U+2029, U+202F, U+205F, U+3000), in every segment, `projects`, `databases` and `documents`
# included, before a verb, and in the emulator's delete path. Not other control characters,
# U+200B or U+FEFF, not inside an ID, not a `documentId`, not a name in a body; a segment of
# whitespace only is an empty one.
case("PATCH a document ID with whitespace around it", "PATCH", B + "/ws/%20a%09", {"fields": {"n": i(1)}})
case("GET it without the whitespace", "GET", B + "/ws/a")
case("GET it with other whitespace around it", "GET", B + "/ws/%E3%80%80a%C2%A0%0A")
case("GET with whitespace around the collection ID", "GET", B + "/%E2%80%A8ws%20/a")
case("GET with whitespace around every segment", "GET", "/v1/projects%20/%20{project}/databases%0D/%E2%80%80(default)/documents%E2%80%AF/ws/a%C2%85")
case("whitespace inside an ID is kept", "GET", B + "/ws/a%20b")
case("a control character is not whitespace", "GET", B + "/ws/a%01")
case("a zero width space is not whitespace", "GET", B + "/ws/a%E2%80%8B")
case("a byte order mark is not whitespace", "GET", B + "/ws/%EF%BB%BFa")
case("PATCH a document under it", "PATCH", B + "/ws/a/sub/x", {"fields": {}})
case("list a collection with whitespace around its ID", "GET", B + "/ws%20")
case("create in a collection with whitespace around its ID, a documentId with a space", "POST", B + "/%20ws?documentId=b%20", {"fields": {}})
case("list it again", "GET", B + "/ws")
case("GET with whitespace around a documentId that has a space", "GET", "/v1beta1/projects/{project}/databases/(default)/documents/ws%20/b%20")
case("runQuery under a document with whitespace around its ID", "POST", B + "/ws/a%20:runQuery", {"structuredQuery": {"from": [{"collectionId": "sub"}]}})
case("listCollectionIds under a document with whitespace around its ID", "POST", B + "/ws/%20a:listCollectionIds", {})
case("runQuery at the root, documents with whitespace", "POST", B + "%20:runQuery", {"structuredQuery": {"from": [{"collectionId": "ws"}], "select": {"fields": []}}})
case("an ID of whitespace only is an empty ID", "GET", B + "/ws/%20")
case("DELETE an ID of whitespace only", "DELETE", B + "/ws/%09")
case("DELETE with whitespace around the ID", "DELETE", B + "/ws/a%20")
case("the document is gone", "GET", B + "/ws/a")
case("PATCH another document", "PATCH", B + "/ws/c", {"fields": {}})
case("the emulator's delete with whitespace around its segments", "DELETE", "/emulator/v1/projects/%20{project}/databases/(default)%09/documents/ws%20/c%E3%80%80")
case("that document is gone", "GET", B + "/ws/c")
case("the emulator's delete of a collection with whitespace around its ID", "DELETE", "/emulator/v1/projects/{project}/databases/(default)/documents/%20ws/a%20/sub")
case("the document under it is gone", "GET", B + "/ws/a/sub/x")
case("the emulator's clear with whitespace around the project and the database", "DELETE", "/emulator/v1/projects/{project}%20/databases/%E3%80%80(default)/documents")
case("the database is empty", "GET", B + "/ws")
# A transform of `__name__` writes the field `__key__` (`transforms.py`), which a read mask or an
# update mask of `__name__` does not name, and which reads and queries return.
case("a transform of __name__ writes the field __key__", "POST", B + ":commit", {"writes": [
    {"update": {"name": NAME + "/nm/a", "fields": {"a": i(1)}}, "updateTransforms": [{"fieldPath": "__name__", "increment": i(1)}]},
]})
case("a read mask of __name__ does not name __key__", "GET", B + "/nm/a?mask.fieldPaths=__name__")
case("an update mask of __name__ leaves __key__", "PATCH", B + "/nm/a?updateMask.fieldPaths=__name__", {"fields": {}})
case("a GET returns __key__", "GET", B + "/nm/a")
case("a list returns __key__", "GET", B + "/nm")
case("a query returns __key__", "POST", B + ":runQuery", {"structuredQuery": {"from": [{"collectionId": "nm"}]}})
# A field path is read the same way everywhere: segments of a name (unquoted, or in backticks)
# and any number of array offsets (`a[0][1]`, at most 2^31 - 1). A name `__key__` is refused as
# soon as it is read, before the rest of the path; `__name__` is `__key__` from then on. Then a
# reserved name other than `__key__` and `__type__` is refused, printed with the rest of the
# path. What a mask, a transform or a query may hold comes later, segment by segment: a mask
# refuses `__type__`, `__key__` in a longer path and array offsets ("Invalid property path"); a
# transform only array offsets, after its operation and the transforms before it. A write's
# update mask reads all its paths before checking any of them that way, then its transforms; a
# read mask comes after the write's update mask and transforms.
def mask_commit(*paths):
    return commit(upd("fp/a", {"a": i(1)}, updateMask={"fieldPaths": list(paths)}))
def tr_commit(*transforms, fields=None):
    return commit(upd("fp/t", fields or {}, updateTransforms=list(transforms)))
def inc(path, operand=None):
    return {"fieldPath": path, "increment": operand or i(1)}
for p in ["_.__type__", "__type__.a", "a[0]", "a[0][1]", "`a`[0]", "a[2147483647]", "a[2147483648]", "a[99999999999999999999]",
          "__key__", "a.__key__", "`__key__`", "__key__.a b", "__key__[0]", "a b.__key__", "a[2147483648].__key__",
          "__type__.__x__", "__x__.__name__", "a[0].__x__.b[07]", "`a`.__x__.`b c`", "__x__[0]",
          "__name__.__type__", "__type__.__name__", "a[0].__name__", "__name__[0]", "__type__[0]", "a[0].__type__",
          "a]", "a[]", "a[x]", "a[0]b"]:
    case(f"update mask {p}", "POST", B + ":commit", mask_commit(p))
case("update mask: every path is read before any is checked", "POST", B + ":commit", mask_commit("__type__", "__x__"))
case("update mask: a path is read whole before the next", "POST", B + ":commit", mask_commit("__key__", "a b"))
case("update mask: an array offset before a reserved name in the next path", "POST", B + ":commit", mask_commit("a[0]", "__x__"))
case("update mask: an array offset, then __type__", "POST", B + ":commit", mask_commit("a[0]", "__type__"))
case("PATCH update mask with __type__ in a longer path", "PATCH", B + "/fp/b?updateMask.fieldPaths=z.__type__&updateMask.fieldPaths=b&updateMask.fieldPaths=b", {})
case("PATCH update mask with an array offset", "PATCH", B + "/fp/b?updateMask.fieldPaths=a%5B0%5D", {})
case("PATCH: the update mask before the read mask", "PATCH", B + "/fp/b?updateMask.fieldPaths=a%5B0%5D&mask.fieldPaths=__x__", {"fields": {"a": i(1)}})
case("PATCH read mask: every path is read before any is checked", "PATCH", B + "/fp/b?updateMask.fieldPaths=a&mask.fieldPaths=__type__&mask.fieldPaths=__x__", {"fields": {"a": i(1)}})
case("PATCH read mask with __type__", "PATCH", B + "/fp/b?mask.fieldPaths=__type__", {"fields": {"a": i(1)}})
case("POST read mask with an array offset", "POST", B + "/fp?documentId=c&mask.fieldPaths=a%5B0%5D", {"fields": {"a": i(1)}})
case("POST read mask with __key__", "POST", B + "/fp?documentId=c&mask.fieldPaths=__key__", {"fields": {"a": i(1)}})
case("GET read mask with an array offset", "GET", B + "/fp/a?mask.fieldPaths=a%5B0%5D")
case("GET read mask with __key__ quoted", "GET", B + "/fp/a?mask.fieldPaths=%60__key__%60")
case("GET read mask: __key__ in a later path before an array offset", "GET", B + "/fp/a?mask.fieldPaths=a%5B0%5D&mask.fieldPaths=__key__")
case("GET read mask: __type__ before a reserved name in a later path", "GET", B + "/fp/a?mask.fieldPaths=__type__&mask.fieldPaths=__x__")
case("list read mask with __key__", "GET", B + "/fp?mask.fieldPaths=__key__")
case("list read mask with __type__", "GET", B + "/fp?mask.fieldPaths=__type__")
case("batchGet mask with an array offset", "POST", B + ":batchGet", {"documents": [NAME + "/fp/a"], "mask": {"fieldPaths": ["a[0]"]}})
case("batchGet mask with __type__", "POST", B + ":batchGet", {"documents": [NAME + "/fp/a"], "mask": {"fieldPaths": ["__type__"]}})
for p in ["__key__", "a.__key__", "`__key__`", "__type__.__x__", "__x__.__name__", "a[0]", "a[0][1]", "a[0].__x__", "__key__[0]",
          "__type__.a[0]", "a[0].__name__", "a[2147483648]"]:
    case(f"transform {p}", "POST", B + ":commit", tr_commit(inc(p)))
case("transform with an array offset and no operation", "POST", B + ":commit", tr_commit({"fieldPath": "a[0]"}))
case("transform with an array offset and an operand that is not a number", "POST", B + ":commit", tr_commit(inc("a[0]", s("x"))))
case("transform with an array offset and a reserved name in its operand", "POST", B + ":commit", tr_commit(inc("a[0]", {"mapValue": {"fields": {"__x__": i(1)}}})))
case("transform with an array offset and no server value", "POST", B + ":commit", tr_commit({"fieldPath": "a[0]", "setToServerValue": "SERVER_VALUE_UNSPECIFIED"}))
case("transform with an array offset, then a reserved name", "POST", B + ":commit", tr_commit(inc("a[0]"), inc("__x__")))
case("transforms of a field and an array offset in it", "POST", B + ":commit", tr_commit(inc("a"), inc("a[0]")))
case("transforms of an array offset in a field and the field", "POST", B + ":commit", tr_commit(inc("a[0]"), inc("a")))
case("nested transforms before an array offset", "POST", B + ":commit", tr_commit(inc("m"), inc("m.n"), inc("a[0]")))
case("an update mask path before a transform path", "POST", B + ":commit", commit(upd("fp/t", {"a": i(1)}, updateMask={"fieldPaths": ["__type__"]}, updateTransforms=[inc("__x__")])))
case("an update mask with an array offset before a transform path", "POST", B + ":commit", commit(upd("fp/t", {"a": i(1)}, updateMask={"fieldPaths": ["a[0]"]}, updateTransforms=[inc("__key__")])))
case("an array offset in a transform of the first write before a second write", "POST", B + ":commit", commit(upd("fp/t", {}, updateTransforms=[inc("a[0]")]), {"update": {"name": NAME + "/fp"}}))
# A transform may name `__type__`, at any depth: the field is written like any other.
case("transforms of __type__", "POST", B + ":commit", commit(
    upd("fp/ty", {}, updateTransforms=[inc("__type__"), inc("m.__type__"), {"fieldPath": "n.__type__", "appendMissingElements": {"values": [s("__vector__")]}}]),
    upd("fp/ty2", {}, updateTransforms=[inc("__type__.x")])))
case("transforms of __type__, the documents", "POST", B + ":batchGet", {"documents": [NAME + "/fp/ty", NAME + "/fp/ty2"]})
case("a transform of __type__ inside a vector", "POST", B + ":commit", commit(
    upd("fp/ty3", {"v": {"mapValue": {"fields": {"__type__": s("__vector__"), "value": {"arrayValue": {"values": [d(1.0)]}}}}}},
        updateTransforms=[{"fieldPath": "v.__type__", "maximum": i(3)}])))
case("a transform of __type__ inside a vector, the document", "GET", B + "/fp/ty3")
case("nested transforms of __type__", "POST", B + ":commit", tr_commit(inc("a.b"), inc("__type__.c"), inc("__type__")))
case("transforms with array offsets write nothing", "GET", B + "/fp/t")

# protobuf-java's JsonFormat (on Gson) reads a field that is not a message leniently: an array of
# one string, number or boolean is that value, at any depth; a string field takes a number as
# written (`1.50`, `1e5`, `-0`) or a boolean; a boolean field the strings "true" and "false"; an
# integer field a decimal with an exact integer value ("1e2", 1.0); a double field a decimal
# string ("1.", ".5", "+1") within a double's range, "NaN", "Infinity" and "-Infinity" but not "nan" or "inf"; an enum a
# value's number; the null value any number.
JSON_VALUES = [
    '{"nullValue": 0}', '{"nullValue": 1}', '{"nullValue": "0"}', '{"nullValue": [0]}', '{"nullValue": "NULL_VALUE"}',
    '{"nullValue": "x"}', '{"nullValue": true}', '{"nullValue": {}}',
    '{"doubleValue": "nan"}', '{"doubleValue": "inf"}', '{"doubleValue": "infinity"}', '{"doubleValue": "-inf"}', '{"doubleValue": "+Infinity"}',
    '{"doubleValue": " 1"}', '{"doubleValue": "0x10"}', '{"doubleValue": "1d"}', '{"doubleValue": true}', '{"doubleValue": []}', '{"doubleValue": [1, 2]}',
    '{"doubleValue": "1."}', '{"doubleValue": ".5"}', '{"doubleValue": "+1"}', '{"doubleValue": "1e5"}', '{"doubleValue": [1.5]}', '{"doubleValue": ["1.5"]}', '{"doubleValue": [[1]]}',
    '{"stringValue": ["a"]}', '{"stringValue": [1]}', '{"stringValue": [["a"]]}', '{"stringValue": [true]}',
    '{"stringValue": []}', '{"stringValue": ["a", "b"]}', '{"stringValue": [null]}', '{"stringValue": [{}]}',
    '{"stringValue": 1e5}', '{"stringValue": 1.50}', '{"stringValue": -0}', '{"stringValue": 1E5}', '{"stringValue": 0.10}',
    '{"stringValue": 100000000000000000000000}', '{"stringValue": -0.0}', '{"stringValue": 1e-7}', '{"stringValue": [1.50]}',
    '{"integerValue": [1]}', '{"integerValue": ["1"]}', '{"integerValue": "1e2"}', '{"integerValue": 1e2}', '{"integerValue": 1.0}', '{"integerValue": "1.0"}',
    '{"integerValue": "+1"}', '{"integerValue": "01"}', '{"integerValue": 1.5}', '{"integerValue": " 1"}', '{"integerValue": "0x10"}', '{"integerValue": "1.5e0"}',
    '{"booleanValue": "true"}', '{"booleanValue": "false"}', '{"booleanValue": [true]}', '{"booleanValue": ["true"]}',
    '{"booleanValue": "True"}', '{"booleanValue": "1"}', '{"booleanValue": 1}', '{"booleanValue": "TRUE"}',
    '{"bytesValue": ["AQI="]}', '{"bytesValue": true}', '{"bytesValue": 1234}', '{"bytesValue": 1}',
    '{"timestampValue": ["2020-01-01T00:00:00Z"]}', '{"timestampValue": 1}',
    '{"geoPointValue": {"latitude": "1.5", "longitude": [2]}}', '{"geoPointValue": {"latitude": true}}',
    '{"referenceValue": ["' + NAME + '/c/d"]}',
    '{"mapValue": {"fields": {"a": {"integerValue": [1]}}}}',
    '{"arrayValue": {"values": [{"stringValue": [2.50]}]}}',
    # Past the largest double: out of range, not an infinity (#262).
    '{"doubleValue": "1e400"}', '{"doubleValue": "-1e400"}', '{"doubleValue": "1.8e308"}',
    '{"doubleValue": "1.7976931348623157e308"}', '{"doubleValue": "1e-400"}',
    '{"geoPointValue": {"latitude": "1e400", "longitude": 0}}',
    # An array in an array is converted before it is refused: a problem in a map in it comes first.
    '{"arrayValue": {"values": [{"arrayValue": {"values": [{"mapValue": {"fields": {"a": {"arrayValue": {"values": [{"arrayValue": {}}]}}}}}]}}]}}',
    '{"arrayValue": {"values": [{"arrayValue": {"values": [{"integerValue": "2"}]}}, {"mapValue": {"fields": {"a": {"arrayValue": {"values": [{"arrayValue": {}}]}}}}}]}}',
]
for n, v in enumerate(JSON_VALUES):
    case(f"JsonFormat: {v.replace(NAME, '{name}')}", "PATCH", B + f"/js/v{n}", raw='{"fields": {"v": ' + v + '}}')
case("JsonFormat: a document name in an array", "POST", B + ":commit", raw='{"writes": [{"update": {"name": ["' + NAME + '/js/w"], "fields": {}}}]}')
case("JsonFormat: exists as a string", "POST", B + ":commit", raw='{"writes": [{"update": {"name": "' + NAME + '/js/w2", "fields": {}}, "currentDocument": {"exists": "true"}}]}', compare="status")
case("JsonFormat: exists as a string in an array", "POST", B + ":commit", raw='{"writes": [{"update": {"name": "' + NAME + '/js/w3", "fields": {}}, "currentDocument": {"exists": ["false"]}}]}')
case("JsonFormat: a limit and an offset as a string and an array", "POST", B + ":runQuery", raw='{"structuredQuery": {"from": [{"collectionId": "js"}], "select": {"fields": []}, "limit": "2", "offset": [1]}}')
case("JsonFormat: a limit as an object", "POST", B + ":runQuery", raw='{"structuredQuery": {"from": [{"collectionId": "js"}], "limit": {"value": "2"}}}')
case("JsonFormat: an operator in an array", "POST", B + ":runQuery", raw='{"structuredQuery": {"from": [{"collectionId": "js"}], "where": {"fieldFilter": {"field": {"fieldPath": "v"}, "op": ["EQUAL"], "value": {"nullValue": 0}}}}}')
case("JsonFormat: an operator by number", "POST", B + ":runQuery", raw='{"structuredQuery": {"from": [{"collectionId": "js"}], "where": {"fieldFilter": {"field": {"fieldPath": "v"}, "op": 5, "value": {"integerValue": "100"}}}}}')
case("JsonFormat: an operator by number as a string", "POST", B + ":runQuery", raw='{"structuredQuery": {"from": [{"collectionId": "js"}], "where": {"fieldFilter": {"field": {"fieldPath": "v"}, "op": "5", "value": {"booleanValue": "true"}}}}}')
case("JsonFormat: an operator in lowercase", "POST", B + ":runQuery", raw='{"structuredQuery": {"from": [{"collectionId": "js"}], "where": {"fieldFilter": {"field": {"fieldPath": "v"}, "op": "equal", "value": {"nullValue": 0}}}}}')
case("JsonFormat: a boolean as a string in a query", "POST", B + ":runQuery", raw='{"structuredQuery": {"from": [{"collectionId": "js", "allDescendants": "true"}], "select": {"fields": []}}}')
case("JsonFormat: a repeated field as an object", "POST", B + ":runQuery", raw='{"structuredQuery": {"from": {"collectionId": "js"}}}')
case("JsonFormat: a repeated field as a string", "POST", B + ":batchGet", raw='{"documents": "' + NAME + '/js/v0"}')
case("JsonFormat: a mask path as a number", "POST", B + ":batchGet", raw='{"documents": ["' + NAME + '/js/v34"], "mask": {"fieldPaths": [["v"]]}}')

# A reference is read as a document's parent name (the database alone, with or without
# `/documents`, is a parent), then as a key: an empty one is "Key path is empty.", and its IDs
# have the key's limits. A geo point's latitude is within -90 to 90 and its longitude within
# -180 to 180, NaN in neither, printed as Java prints a double. Both are checked with the names
# of the write, value by value, in order and depth first, next to reserved names and values
# without a type, also in transforms' operands.
def ref(n): return {"referenceValue": n}
def geo(**c): return {"geoPointValue": c}
DB_NAME = "projects/{project}/databases/(default)"
for label, value in [
    ("a reference to c/d", ref("c/d")), ("a reference to the documents root", ref(NAME)), ("a reference to the database", ref(DB_NAME)),
    ("a reference to a collection", ref(NAME + "/c")), ("a reference with an empty ID", ref(NAME + "/c//d")),
    ("a reference with a trailing slash", ref(NAME + "/c/d/")), ("a reference to the database with a trailing slash", ref(DB_NAME + "/")),
    ("a reference to another database and project", ref("projects/other/databases/db2/documents/c/d")),
    ("a reference to a subcollection's document", ref(NAME + "/c/d/e/f")), ("an empty reference", ref("")),
    ("a reference without documents", ref(DB_NAME + "/x")), ("a reference to a database only half named", ref("projects/p/databases")),
    ("a reference with a reserved collection ID", ref(NAME + "/__x__/d")), ("a reference with a reserved document ID", ref(NAME + "/c/__x__")),
    ("a reference with a numeric document ID", ref(NAME + "/c/__id5__")), ("a reference with the ID .", ref(NAME + "/c/.")),
    ("a reference with a collection ID over 1500 bytes", ref(NAME + "/" + "c" * 1501 + "/d")),
    ("a reference with a document ID over 1500 bytes", ref(NAME + "/c/" + "d" * 1501)),
    ("a latitude of 91", geo(latitude=91)), ("a latitude just over 90", geo(latitude=90.0000001)), ("a latitude of -91", geo(latitude=-91)),
    ("a latitude of 1e300", geo(latitude=1e300)), ("a latitude of infinity", geo(latitude="Infinity")), ("a latitude of NaN", geo(latitude="NaN")),
    ("a longitude of 180.5", geo(latitude=0, longitude=180.5)), ("a longitude of -180.5", geo(longitude=-180.5)), ("a longitude of NaN", geo(longitude="NaN")),
    ("a longitude of 1.23456789e7", geo(longitude=12345678.9)), ("a latitude and a longitude out of range", geo(latitude=91, longitude=181)),
    ("the corners of the range", geo(latitude=-90, longitude=180)),
]:
    case(f"commit {label}", "POST", B + ":commit", commit(upd("rf/a", {"v": value})))
case("PATCH a reference in a map", "PATCH", B + "/rf/b", {"fields": {"m": {"mapValue": {"fields": {"k": ref("c/d")}}}}})
case("batchWrite a geo point in an array", "POST", B + ":batchWrite", commit(upd("rf/a", {"v": {"arrayValue": {"values": [geo(latitude=91)]}}})))
case("POST a geo point in a map", "POST", B + "/rf?documentId=c", {"fields": {"m": {"mapValue": {"fields": {"k": geo(longitude=180.5)}}}}})
case("a reference before a reserved name in a later field", "POST", B + ":commit", commit(upd("rf/a", {"v": ref("c/d"), "__x__": i(1)})))
case("a reserved name before a reference in a later field", "POST", B + ":commit", commit(upd("rf/a", {"__x__": i(1), "v": ref("c/d")})))
case("a reference before a value without a type", "POST", B + ":commit", commit(upd("rf/a", {"v": {"arrayValue": {"values": [ref("c/d"), U]}}})))
case("a value without a type before a reference", "POST", B + ":commit", commit(upd("rf/a", {"v": {"arrayValue": {"values": [U, ref("c/d")]}}})))
case("a geo point before a reserved name in its map", "POST", B + ":commit", commit(upd("rf/a", {"v": {"mapValue": {"fields": {"x": geo(latitude=91), "__y__": i(1)}}}})))
case("a geo point before a reference", "POST", B + ":commit", commit(upd("rf/a", {"v": geo(latitude=91), "u": ref("c/d")})))
case("a reference before a geo point", "POST", B + ":commit", commit(upd("rf/a", {"u": ref("c/d"), "v": geo(latitude=91)})))
case("a reference before nested arrays", "POST", B + ":commit", commit(upd("rf/a", {"n": {"arrayValue": {"values": [{"arrayValue": {}}]}}, "v": ref("c/d")})))
case("a reference before the update mask", "POST", B + ":commit", commit(upd("rf/a", {"v": ref("c/d")}, updateMask={"fieldPaths": ["__x__"]})))
case("a reference before the sequence of writes", "POST", B + ":commit", commit(upd("rf/a", {"v": ref("c/d")}), upd("rf/a", {}, currentDocument={"exists": False}), {"delete": NAME + "/rf/a"}))
case("a reference before the names of a later write", "POST", B + ":commit", commit(upd("rf/a", {"v": ref("c/d")}), upd("rf/b", {"__x__": i(1)})))
case("a reference before a precondition", "POST", B + ":commit", commit(upd("rf/a", {"v": ref("c/d")}, currentDocument={"exists": True})))
case("a reference in a vector", "POST", B + ":commit", commit(upd("rf/a", {"v": {"mapValue": {"fields": {"__type__": s("__vector__"), "value": {"arrayValue": {"values": [ref("c/d")]}}}}}})))
case("a reference next to an unknown __type__", "POST", B + ":commit", commit(upd("rf/a", {"v": {"mapValue": {"fields": {"__type__": s("other"), "k": ref("c/d")}}}})))
case("a reference in an array union", "POST", B + ":commit", commit(upd("rf/a", {}, updateTransforms=[{"fieldPath": "t", "appendMissingElements": {"values": [ref("c/d")]}}])))
case("a geo point in an array removal", "POST", B + ":commit", commit({"transform": {"document": NAME + "/rf/a", "fieldTransforms": [{"fieldPath": "t", "removeAllFromArray": {"values": [geo(latitude=91)]}}]}}))
case("a reference in an increment's operand", "POST", B + ":commit", commit(upd("rf/a", {}, updateTransforms=[{"fieldPath": "t", "increment": {"mapValue": {"fields": {"r": ref("c/d")}}}}])))
case("a geo point as a maximum", "POST", B + ":commit", commit(upd("rf/a", {}, updateTransforms=[{"fieldPath": "t", "maximum": geo(latitude=91)}])))
case("the writes that were not refused", "GET", B + "/rf")

# A version precondition on a deleted document is checked against the time it was deleted: a
# document that never existed, or was deleted while missing, has version 0. (The message's
# versions of the years 2021 to 2099 are masked as `<version>`, as the fuzzer does.)
OLD = {"updateTime": "2001-01-01T00:00:00Z"}
case("a version precondition on a document that never existed", "POST", B + ":commit", commit(upd("dv/never", {}, currentDocument=OLD)))
case("a document to delete", "POST", B + ":commit", commit(upd("dv/a", {"n": i(1)})))
case("its deletion", "POST", B + ":commit", commit({"delete": NAME + "/dv/a"}))
case("a version precondition on a deleted document", "POST", B + ":commit", commit(upd("dv/a", {}, currentDocument=OLD)))
case("a version precondition on a deleted document, for a delete", "POST", B + ":commit", commit({"delete": NAME + "/dv/a", "currentDocument": OLD}))
case("a version precondition on a deleted document, for a transform", "POST", B + ":commit", commit({"transform": {"document": NAME + "/dv/a", "fieldTransforms": [{"fieldPath": "n", "increment": i(1)}]}, "currentDocument": OLD}))
case("a version precondition on a deleted document, in a batchWrite", "POST", B + ":batchWrite", commit(upd("dv/a", {}, currentDocument=OLD)))
case("a delete of a missing document", "POST", B + ":commit", commit({"delete": NAME + "/dv/missing"}))
case("a version precondition after a delete of a missing document", "POST", B + ":commit", commit(upd("dv/missing", {}, currentDocument=OLD)))
case("a document deleted twice", "POST", B + ":commit", commit(upd("dv/b", {"n": i(1)})))
case("a document deleted twice, the first deletion", "POST", B + ":commit", commit({"delete": NAME + "/dv/b"}))
case("a document deleted twice, the second deletion", "POST", B + ":commit", commit({"delete": NAME + "/dv/b"}))
case("a version precondition on a document deleted twice", "POST", B + ":commit", commit(upd("dv/b", {}, currentDocument=OLD)))

# ListDocuments' `orderBy` is a query's order: clauses separated by commas, each a field path
# and `asc` or `desc` in any case, with whitespace around ("Invalid order by clause" for
# anything else), checked as a query's orders are; the documents without the fields are left out.
case("documents to list in order", "POST", B + ":commit", commit(*[upd(f"lo/{d}", f) for d, f in [
    ("a", {"n": i(3), "m": s("x")}), ("b", {"n": i(1)}), ("c", {"m": s("y")}), ("d", {"n": i(2), "m": s("x")}),
    ("e", {"n": s("s")}), ("f", {"n": i(1), "m": s("a")}), ("g", {})]]))
for order in ["n", "n%20desc", "n%20asc", "n%20DESC", "n%20%20desc", "m,n", "m%20desc,n", "n,%20m", "n,__name__%20desc", "__name__",
              "a.b", "%60a%20b%60", "x%20desc", "n,", ",n", "%20", "n%20desc%20x", "n%20ascending", "m,m", "n%20desc,n",
              "__name__,n", "__name__%20desc", "__x__", "a%5B0%5D", "a..b"]:
    case(f"list ordered by {order}", "GET", B + "/lo?orderBy=" + order)
case("list ordered by a field, a page of 2", "GET", B + "/lo?orderBy=n&pageSize=2")
# A full page has a next page, ordered or not (#262).
case("list ordered by a field, a page of all five", "GET", B + "/lo?orderBy=n&pageSize=5")
case("list ordered by a field, a page of more than five", "GET", B + "/lo?orderBy=n%20desc&pageSize=6")
case("list ordered by a field, a mask", "GET", B + "/lo?orderBy=n%20desc&mask.fieldPaths=m")
case("list ordered by a field, showing missing documents", "GET", B + "/lo?orderBy=n&showMissing=true")


# A body is read as Gson reads it in lenient mode, which protobuf-java's JsonFormat turns on
# (#262): one byte order mark, then the prefix `)]}'` and a newline; comments; names and strings
# in single quotes or none; `true`, `false` and `null` in any case; `=` or `=>` after a name and
# `;` between pairs or elements; nothing after the first value is read. An element left out is a
# null, which no repeated field takes; whitespace alone is no value. GET and DELETE ignore their
# body.
LJ = NAME + "/lj/"
for name, raw in [
    ("single quotes", "{'writes': [{'update': {'name': '" + LJ + "a', 'fields': {'v': {'stringValue': 'it\\'s \"x\"'}}}}]}"),
    ("a byte order mark", "\ufeff{\"writes\": []}"),
    ("two byte order marks", "\ufeff\ufeff{\"writes\": []}"),
    ("the non-execute prefix", " )]}'\n{\"writes\": []}"),
    ("the prefix without its newline", ")]}'{\"writes\": []}"),
    ("comments", "/* a */ {\"writes\": // b\n [] # c\n}"),
    ("an open comment after the value", "{\"writes\": []} /* open"),
    ("unquoted names and strings", "{writes: [{update: {name: '" + LJ + "b', fields: {v.w: {stringValue: a\"b'c}, x: {doubleValue: NaN}, y: {integerValue: 007}, z: {booleanValue: TrUe}, n: {nullValue: NULL}}}}]}"),
    ("= and => after names, ; between pairs", "{\"writes\" => [{\"update\" = {\"name\": \"" + LJ + "c\"; \"fields\": {}}}]}"),
    ("; between elements", "{\"writes\": [{\"delete\": \"" + LJ + "x\"}; {\"delete\": \"" + LJ + "y\"}]}"),
    ("an element left out", "{\"writes\": [{\"delete\": \"" + LJ + "x\"},]}"),
    ("a pair after the last", "{\"writes\": [];}"),
    ("a second value", "{\"writes\": []} {\"writes\": 1} ]]"),
    ("an unquoted value that ends at a slash", "{\"writes\": [{\"delete\": " + LJ + "x}]}"),
    ("raw control characters and a line continuation", "{\"writes\": [{\"update\": {\"name\": \"" + LJ + "d\", \"fields\": {\"v\": {\"stringValue\": \"a\tb\\\nc\"}}}}]}"),
    ("an escape Gson does not know", "{\"writes\": [{\"update\": {\"name\": \"" + LJ + "e\", \"fields\": {\"v\": {\"stringValue\": \"\\x41\"}}}}]}"),
    ("a keyword with letters after it", "{\"writes\": [{\"update\": {\"name\": \"" + LJ + "f\", \"fields\": {\"v\": {\"booleanValue\": truex}}}}]}"),
    ("a form feed between tokens", "{\"writes\":\f[]}"),
    ("whitespace alone", "  \n"),
    ("a comment alone", "/* a */"),
]:
    case(f"commit, lenient JSON: {name}", "POST", B + ":commit", raw=raw)
case("the documents lenient JSON wrote", "POST", B + ":batchGet", {"documents": [LJ + "a", LJ + "b", LJ + "c", LJ + "d"]})
case("runQuery, lenient JSON", "POST", B + ":runQuery", raw="{structuredQuery: {from: [{collectionId: lj}], orderBy: [{field: {fieldPath: __name__}}]}}")
case("PATCH, whitespace alone", "PATCH", B + "/lj/g", raw=" ")
case("GET, a body that does not read", "GET", B + "/lj/a", raw="{not json")
case("DELETE, a body that does not read", "DELETE", B + "/lj/zz", raw="{not json")
case("an unknown verb, a body that does not read", "POST", B + ":nosuchverb", raw="{not json")

def norm(text):
    # Versions of the years 2021 to 2099, in microseconds (`tools/fuzz/diff.py`).
    text = re.sub(r"(version \()(1[6-9]|[23][0-9]|40|41)[0-9]{14}(?![0-9])", r"\1<version>", text)
    return mask(text, PROJECT)


def run():
    out = []
    for c in CASES:
        conn = http.client.HTTPConnection(*HOST.split(":"), timeout=10)
        path = c["path"].replace("{project}", PROJECT)
        if "raw" in c:
            body = c["raw"].replace("{project}", PROJECT)
        elif "body" in c:
            body = json.dumps(c["body"]).replace("{project}", PROJECT)
        else:
            body = None
        headers = dict(c["headers"])
        if body is not None and "Content-Type" not in headers:
            headers["Content-Type"] = "application/json"
        conn.request(c["method"], path, body=None if body is None else body.encode(), headers=headers)
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
