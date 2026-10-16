#!/usr/bin/env python3
"""How does the official emulator store, order and compare the BSON types of an enterprise
database (#338)?

Usage (from the repository root), with a jar freshly started with `--database-edition enterprise`
under an English locale:
    python3 -I tools/oracle/bson_types.py 127.0.0.1:8089 \\
        > crates/hidane/tests/fixtures/bson_types.json

The SDKs write the BSON types as maps of one reserved key: `{__min__: null}`, `{__max__: null}`,
`{__oid__: "<24 hex digits>"}`, `{__regex__: {pattern, options}}`, `{__int__: <integer>}`,
`{__decimal128__: "<decimal>"}`, `{__request_timestamp__: {seconds, increment}}` and
`{__binary__: <subtype byte, then the data>}`. This records, over REST:

- `writes`: each type's valid and invalid encodings (and maps that only look like one), written
  as a field of a document of its own: the commit's outcome, and the document as read back;
- `orders`: one document per value of every type, BSON and native, ordered by the field
  ascending and descending (names in order);
- `filters`: each comparison operator against values of every type, over the same documents
  (names, as a set);
- `pipelines`: `type()`, `is_type()` and the comparisons of pipelines on those values.

`crates/hidane/tests/bson_types.rs` replays it. Stdlib only.
"""

import http.client
import json
import sys
import time

HOST = sys.argv[1]
PROJECT = f"bson-{time.strftime('%H%M%S')}"
DATABASE = f"projects/{PROJECT}/databases/(default)"
OWNER = {"Authorization": "Bearer owner", "Content-Type": "application/json"}
ORACLE = ("cloud-firestore-emulator v1.22.0 "
          "(sha256 9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)")


def call(method, path, body=None):
    c = http.client.HTTPConnection(*HOST.split(":"), timeout=60)
    c.request(method, path, body=None if body is None else json.dumps(body), headers=OWNER)
    r = c.getresponse()
    text = r.read().decode()
    return r.status, (json.loads(text) if text.strip() else None)


def outcome(status, body):
    if status == 200:
        return {"status": "OK"}
    error = body["error"]
    return {"status": error["status"], "message": error.get("message", "")}


NULL = {"nullValue": None}


def s(x):
    return {"stringValue": x}


def i(x):
    return {"integerValue": str(x)}


def d(x):
    return {"doubleValue": x}


def mp(**fields):
    return {"mapValue": {"fields": fields}}


def m(fields):
    return {"mapValue": {"fields": fields}}


def b64(data):
    import base64
    return {"bytesValue": base64.b64encode(bytes(data)).decode()}


MIN = m({"__min__": NULL})
MAX = m({"__max__": NULL})


def oid(x):
    return m({"__oid__": s(x)})


def regex(pattern, options):
    return m({"__regex__": mp(pattern=s(pattern), options=s(options))})


def int32(x):
    return m({"__int__": i(x)})


def dec(x):
    return m({"__decimal128__": s(x)})


def bts(seconds, increment):
    return m({"__request_timestamp__": mp(seconds=i(seconds), increment=i(increment))})


def binary(subtype, *data):
    return m({"__binary__": b64([subtype, *data])})


# Values, valid or not, and maps that only look like a BSON value.
WRITES = {
    "min key": MIN,
    "max key": MAX,
    "min key holding 1": m({"__min__": i(1)}),
    "max key with another key": m({"__max__": NULL, "a": i(1)}),
    "object id": oid("507f191e810c19729de860ea"),
    "object id in capitals": oid("507F191E810C19729DE860EA"),
    "object id too short": oid("foo"),
    "object id of 24 non-hex characters": oid("zzzzzzzzzzzzzzzzzzzzzzzz"),
    "object id that is an integer": m({"__oid__": i(1)}),
    "regex": regex("^foo", "i"),
    "regex without options": regex("^foo", ""),
    "regex with every option": regex("a", "imsux"),
    "regex with an unknown option": regex("foo", "a"),
    "regex with options out of order": regex("a", "xi"),
    "regex with a repeated option": regex("a", "ii"),
    "regex that does not compile": regex("(", ""),
    "regex without a pattern": m({"__regex__": mp(options=s("i"))}),
    "regex without options key": m({"__regex__": mp(pattern=s("a"))}),
    "regex with another key": m({"__regex__": mp(pattern=s("a"), options=s(""), other=i(1))}),
    "regex that is a string": m({"__regex__": s("a")}),
    "int32": int32(-57),
    "int32 zero": int32(0),
    "int32 max": int32(2147483647),
    "int32 min": int32(-2147483648),
    "int32 too large": int32(2147483648),
    "int32 too small": int32(-2147483650),
    "int32 that is a double": m({"__int__": d(1.0)}),
    "int32 that is a string": m({"__int__": s("1")}),
    "decimal128": dec("-1.2e3"),
    "decimal128 NaN": dec("NaN"),
    "decimal128 infinity": dec("-Infinity"),
    "decimal128 negative zero": dec("-0"),
    "decimal128 with 34 digits": dec("0.1234567890123456789012345678901234"),
    "decimal128 with 35 digits": dec("0.12345678901234567890123456789012345"),
    "decimal128 empty": dec(""),
    "decimal128 with spaces": dec("1 23. 4"),
    "decimal128 of letters": dec("abc"),
    "decimal128 exponent too large": dec("1e6145"),
    "decimal128 that is a double": m({"__decimal128__": d(1.5)}),
    "bson timestamp": bts(57, 1),
    "bson timestamp of zero": bts(0, 0),
    "bson timestamp seconds too large": bts(4294967296, 2),
    "bson timestamp increment too large": bts(2, 4294967296),
    "bson timestamp negative seconds": bts(-1, 2),
    "bson timestamp negative increment": bts(1, -2),
    "bson timestamp without increment": m({"__request_timestamp__": mp(seconds=i(1))}),
    "bson timestamp with another key": m({"__request_timestamp__": mp(seconds=i(1), increment=i(1), x=i(1))}),
    "bson timestamp that is an integer": m({"__request_timestamp__": i(1)}),
    "binary": binary(128, 5, 6, 7),
    "binary of subtype 0": binary(0, 1, 2),
    "binary of a subtype alone": binary(3),
    "binary empty": m({"__binary__": b64([])}),
    "binary that is a string": m({"__binary__": s("x")}),
    "an unknown reserved key": m({"__foo__": i(1)}),
    "two BSON keys": m({"__int__": i(1), "__oid__": s("507f191e810c19729de860ea")}),
    "a BSON value in an array": {"arrayValue": {"values": [int32(1), MAX]}},
    "a BSON value in a map": mp(a=int32(1)),
}

# One document per value, BSON and native, for orders and filters.
VALUES = {
    "null": NULL, "min": MIN, "max": MAX, "false": {"booleanValue": False}, "true": {"booleanValue": True},
    "nan": d("NaN"), "ninf": d("-Infinity"), "neg": i(-5), "zero": i(0), "one": i(1), "onef": d(1.0),
    "half": d(0.5), "two": i(2), "inf": d("Infinity"), "big": i(9223372036854775807),
    "i32neg": int32(-5), "i32zero": int32(0), "i32one": int32(1), "i32two": int32(2),
    "decnan": dec("NaN"), "decninf": dec("-Infinity"), "decneg": dec("-1.2e3"), "deczero": dec("0"),
    "decnz": dec("-0"), "decone": dec("1"), "dechalf": dec("0.5"), "decsmall": dec("1.2e-3"),
    "decinf": dec("Infinity"), "decbig": dec("9223372036854775808"),
    "ts": {"timestampValue": "2020-01-01T00:00:00Z"},
    "bts1": bts(1, 1), "bts2": bts(1, 2), "bts3": bts(2, 0),
    "str": s("a"), "strb": s("b"),
    "bytes": b64([1, 2]), "bin1": binary(1, 1, 2), "bin2": binary(1, 1, 3), "bin128": binary(128, 5),
    "ref": {"referenceValue": "{database}/documents/c/x"},
    "oid1": oid("507f191e810c19729de860ea"), "oid2": oid("507f191e810c19729de860eb"),
    "geo": {"geoPointValue": {"latitude": 1, "longitude": 2}},
    "re1": regex("a", ""), "re2": regex("a", "i"), "re3": regex("b", ""),
    "arr": {"arrayValue": {"values": [i(1)]}},
    "vec": mp(__type__=s("__vector__"), value={"arrayValue": {"values": [d(1.0)]}}),
    "map": mp(a=i(1)),
}

# Values each filter compares with.
OPERANDS = {
    "min": MIN, "max": MAX, "int32 1": int32(1), "integer 1": i(1), "double 1": d(1.0),
    "decimal 1": dec("1"), "decimal 1.0": dec("1.0"), "decimal NaN": dec("NaN"), "double NaN": d("NaN"),
    "decimal -0": dec("-0"), "object id": oid("507f191e810c19729de860ea"), "regex": regex("a", ""),
    "bson timestamp": bts(1, 2), "binary": binary(1, 1, 2), "bytes": b64([1, 2]), "null": NULL,
}


def fill(value):
    return json.loads(json.dumps(value).replace("{database}", DATABASE))


def run_query(structured):
    status, body = call("POST", f"/v1/{DATABASE}/documents:runQuery", {"structuredQuery": structured})
    if status != 200:
        return outcome(status, body)
    names = [r["document"]["name"].rsplit("/", 1)[1] for r in body if "document" in r]
    return {"status": "OK", "documents": names}


def main():
    writes = []
    for n, (name, value) in enumerate(WRITES.items()):
        path = f"w/w{n}"
        status, body = call("POST", f"/v1/{DATABASE}/documents:commit", {"writes": [
            {"update": {"name": f"{DATABASE}/documents/{path}", "fields": {"v": fill(value)}}}]})
        entry = {"name": name, "value": value, "outcome": outcome(status, body)}
        if status == 200:
            _, doc = call("GET", f"/v1/{DATABASE}/documents/{path}")
            entry["read"] = doc.get("fields", {}).get("v")
        writes.append(entry)

    status, body = call("POST", f"/v1/{DATABASE}/documents:commit", {"writes": [
        {"update": {"name": f"{DATABASE}/documents/v/{k}", "fields": {"v": fill(v)}}} for k, v in VALUES.items()]})
    assert status == 200, body
    orders = []
    for direction in ("ASCENDING", "DESCENDING"):
        orders.append({"direction": direction, "outcome": run_query({
            "from": [{"collectionId": "v"}],
            "orderBy": [{"field": {"fieldPath": "v"}, "direction": direction},
                        {"field": {"fieldPath": "__name__"}, "direction": "ASCENDING"}]})})
    filters = []
    for op in ("EQUAL", "NOT_EQUAL", "LESS_THAN", "LESS_THAN_OR_EQUAL", "GREATER_THAN", "GREATER_THAN_OR_EQUAL"):
        for label, operand in OPERANDS.items():
            filters.append({"op": op, "operand": label, "outcome": run_query({
                "from": [{"collectionId": "v"}],
                "where": {"fieldFilter": {"field": {"fieldPath": "v"}, "op": op, "value": operand}}})})
    for op, operands in (("IN", ["int32 1", "decimal 1"]), ("NOT_IN", ["int32 1", "min"]),
                         ("IN", ["object id", "regex", "bson timestamp", "binary"])):
        filters.append({"op": op, "operand": ", ".join(operands), "outcome": run_query({
            "from": [{"collectionId": "v"}],
            "where": {"fieldFilter": {"field": {"fieldPath": "v"}, "op": op,
                                      "value": {"arrayValue": {"values": [OPERANDS[o] for o in operands]}}}}})})

    pipelines = []
    for k in VALUES:
        stages = [{"name": "documents", "args": [{"referenceValue": f"/v/{k}"}]},
                  {"name": "select", "args": [mp(
                      t={"functionValue": {"name": "type", "args": [{"fieldReferenceValue": "v"}]}},
                      n={"functionValue": {"name": "is_type", "args": [{"fieldReferenceValue": "v"}, s("number")]}},
                      e={"functionValue": {"name": "equal", "args": [{"fieldReferenceValue": "v"}, i(1)]}},
                      lt={"functionValue": {"name": "less_than", "args": [{"fieldReferenceValue": "v"}, int32(2)]}})]}]
        status, body = call("POST", f"/v1/{DATABASE}/documents:executePipeline",
                            {"structuredPipeline": {"pipeline": {"stages": stages}}})
        result = outcome(status, body)
        if status == 200:
            result["fields"] = [r.get("fields", {}) for resp in body for r in resp.get("results", [])]
        pipelines.append({"document": k, "outcome": result})

    json.dump({
        "oracle": ORACLE,
        "generated_by": "tools/oracle/bson_types.py",
        "edition": "enterprise",
        "values": VALUES,
        "operands": OPERANDS,
        "writes": writes,
        "orders": orders,
        "filters": filters,
        "pipelines": pipelines,
    }, sys.stdout, ensure_ascii=False, indent=1)
    print()


main()
