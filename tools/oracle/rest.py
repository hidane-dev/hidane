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
