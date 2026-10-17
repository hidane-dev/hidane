#!/usr/bin/env python3
"""Which problem of a Datastore commit or lookup does the official emulator report first? (#81)

Usage (from the repository root), with a jar freshly started with `--database-mode
datastore-mode` under an English locale:
    python3 -I tools/oracle/datastore_commit_checks.py 127.0.0.1:8086 \\
        > crates/hidane/tests/fixtures/datastore_commit_checks.json

The differential fuzzer (tools/fuzz/, `--mode datastore-mode`) found commits and lookups refused
for another problem than hidane reported, or not refused. Over REST, this sends each problem of
`MUTATION` alone (`:commit`, a mutation of entity `K/a`), every ordered pair of them as two
mutations (the second of `K/b`), and every ordered pair that fits in one mutation (two problems
that set the same part of a mutation do not); each problem of `REQUEST` (the commit's mode,
transaction and mutations) with each of `MUTATION`; each problem of `LOOKUP` (a key, the read
options) alone and every ordered pair; and the commits and lookups of `ALONE`, too large to
pair; and the requests of `SEQUENCES` in turn (how mutations apply, base versions). Each runs in a
project of its own, after `SEED` wrote entity `K/b`.

What the recording shows. A commit's mode and transaction come first. Then each mutation in
turn: its operation, its entity's key, the key (namespace, then path), the properties (names,
flattened names, values: meaning, length, geo points, key values, lists excluded from indexes),
the key's project and database, a base version with an incomplete key, the property mask's
paths. Then a non-transactional commit's entities written twice, and the transaction. Then, each
mutation in turn: a resolution strategy without a detection strategy, nested arrays, more than
5,000 index values, the base version of an insert (none) and of an update (not zero), the
`FAIL` strategy against the stored version, whether the entity exists, and its size (1,048,576
bytes, counted as a Firestore document is). A base version that does not match is written over
unless the strategy is `FAIL`. A lookup checks each key (reserved names and namespaces pass),
then its read options' shape (a consistency left unspecified, a read time before the epoch, an
empty transaction), the property mask, then the read time and the transaction. Keys print in
messages as protocol-buffer text: IDs from 2,000,000,000 up in hexadecimal, strings escaped in
octal up to U+00FF and as `\\u` above.

Each case is a request and its answer in the format of `datastore_entities.json` (without what
`every_case` gives them all), or its `steps`, each a request and its answer, on one line; a
`FAIL` strategy's stored version is masked in messages. A string recorded as `{repeat:<c>:<n>}` (in a request
or an answer) stands for `<c>` repeated `<n>` times, so the fixture stays small; the test
expands it the same way.
`crates/hidane/tests/datastore.rs` replays it. Stdlib only.
"""

import http.client
import itertools
import json
import re
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from harness import mask  # noqa: E402

HOST = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1:8086"
RUN = time.strftime("%H%M%S")
JSON = {"Content-Type": "application/json"}
EVERY_CASE = {"method": "POST", "headers": JSON, "compare": "exact"}
VERSION = re.compile(r'"(version|commitVersion|snapshotVersion)": "\d+"')
CURSOR = re.compile(r'"(cursor|endCursor|skippedCursor|startCursor)": "[^"]*"')
ONE = {"integerValue": "1"}


def key(*path, ns=None, project=None, database=None):
    elements = []
    for kind, id_ in zip(path[::2], path[1::2]):
        element = {"kind": kind}
        if isinstance(id_, str):
            element["name"] = id_
        elif id_ is not None:
            element["id"] = str(id_)
        elements.append(element)
    k = {"path": elements}
    partition = {}
    if ns is not None:
        partition["namespaceId"] = ns
    if project is not None:
        partition["projectId"] = project
    if database is not None:
        partition["databaseId"] = database
    if partition:
        k["partitionId"] = partition
    return k


# A problem of a mutation: the parts of it it sets. `op` is the mutation's operation, `key` its
# key (`None`: none), `properties` properties added, `entity` other fields of the entity, and the
# mutation's own `propertyMask`, `baseVersion`, `updateTime`, `conflictResolutionStrategy`.
MUTATION = {
    "no operation": {"op": None},
    "no key": {"key": None},
    "an empty path": {"key": {"path": []}},
    "an incomplete update": {"op": "update", "key": key("K", None)},
    "an incomplete delete": {"op": "delete", "key": key("K", None)},
    "an incomplete parent": {"key": key("P", None, "K", "a")},
    "a reserved kind": {"key": key("__x__", "a")},
    "an empty kind": {"key": key("", "a")},
    "a long kind": {"key": key("k" * 1501, "a")},
    "a reserved name": {"key": key("K", "__x__")},
    "an empty name": {"key": key("K", "")},
    "a long name": {"key": key("K", "a" * 1501)},
    "a zero ID": {"key": key("K", 0)},
    "a negative ID": {"key": key("K", -1)},
    "a reserved namespace": {"key": key("K", "a", ns="__x__")},
    "an illegal namespace": {"key": key("K", "a", ns="é")},
    "another project": {"key": key("K", "a", project="other")},
    "another database": {"key": key("K", "a", database="other")},
    "a reserved property": {"properties": {"__x__": ONE}},
    "an empty property name": {"properties": {"": ONE}},
    "a long property name": {"properties": {"p" * 1501: ONE}},
    "a long nested name": {"properties": {"e": {"entityValue": {"properties": {"q" * 1500: ONE}}}}},
    "a value without a type": {"properties": {"v": {}}},
    "an array in an array": {"properties": {"arr": {"arrayValue": {"values": [{"arrayValue": {}}]}}}},
    "an array excluded from indexes": {"properties": {"ex": {"arrayValue": {"values": [ONE]},
                                                             "excludeFromIndexes": True}}},
    "a long indexed string": {"properties": {"s": {"stringValue": "s" * 1501}}},
    "a long indexed blob": {"properties": {"b": {"blobValue": "A" * 2004}}},
    "a meaning on an integer": {"properties": {"m": {"integerValue": "1", "meaning": 14}}},
    "an incomplete key value": {"properties": {"k": {"keyValue": key("K", None)}}},
    "a key value with an empty path": {"properties": {"k2": {"keyValue": {"path": []}}}},
    "a key value of another project": {"properties": {"k3": {"keyValue": key("K", "x", project="other")}}},
    "a latitude out of range": {"properties": {"g": {"geoPointValue": {"latitude": 91, "longitude": 0}}}},
    "a reserved nested property": {"properties": {"e2": {"entityValue": {"properties": {"__y__": ONE}}}}},
    "a nested entity with a key": {"properties": {"e3": {"entityValue": {"key": key("K", None),
                                                                         "properties": {"x": ONE}}}}},
    "a mask with a space": {"propertyMask": {"paths": ["x y"]}},
    "a mask with an empty path": {"propertyMask": {"paths": [""]}},
    "a mask with two dots": {"propertyMask": {"paths": ["a..b"]}},
    "an insert at a base version": {"op": "insert", "baseVersion": "1"},
    "an update at base version zero": {"op": "update", "baseVersion": "0"},
    "a base version with an incomplete key": {"op": "insert", "key": key("K", None), "baseVersion": "1"},
    "a base version and an update time": {"baseVersion": "1", "updateTime": "2001-01-01T00:00:00Z"},
    "a resolution strategy alone": {"conflictResolutionStrategy": "SERVER_VALUE"},
    "an update of what does not exist": {"op": "update", "key": key("K", "nothing")},
}

# A problem of a commit request: its fields beside the mutations, or mutations added.
REQUEST = {
    "no mode": {"mode": None},
    "a transaction without the mode": {"mode": "NON_TRANSACTIONAL", "transaction": "AAAA"},
    "the mode without a transaction": {"mode": "TRANSACTIONAL"},
    "a transaction that is not base64": {"mode": "TRANSACTIONAL", "transaction": "*"},
    "an unknown transaction": {"mode": "TRANSACTIONAL", "transaction": "AAAAAAAAAAAAAAAAAAAAAAAA"},
    "the same entity twice": {"more": [{"upsert": {"key": key("K", "c")}}, {"delete": key("K", "c")}]},
    "another database ID": {"databaseId": "other"},
}

LOOKUP = {
    "no keys": {"keys": []},
    "an empty path": {"key": {"path": []}},
    "an incomplete key": {"key": key("K", None)},
    "a reserved kind": {"key": key("__x__", "a")},
    "a reserved name": {"key": key("K", "__x__")},
    "an empty kind": {"key": key("", "a")},
    "a zero ID": {"key": key("K", 0)},
    "a reserved namespace": {"key": key("K", "a", ns="__x__")},
    "an illegal namespace": {"key": key("K", "a", ns="é")},
    "another project": {"key": key("K", "a", project="other")},
    "another database": {"key": key("K", "a", database="other")},
    "a key twice": {"key": key("K", "b"), "twice": True},
    "an unspecified consistency": {"readOptions": {"readConsistency": "READ_CONSISTENCY_UNSPECIFIED"}},
    "a read time before the epoch": {"readOptions": {"readTime": "1969-12-31T23:59:59Z"}},
    "a read time too old": {"readOptions": {"readTime": "2001-01-01T00:00:00Z"}},
    "a read time in the future": {"readOptions": {"readTime": "2999-01-01T00:00:00Z"}},
    "an empty transaction": {"readOptions": {"transaction": ""}},
    "a transaction that is not base64": {"readOptions": {"transaction": "*"}},
    "an unknown transaction": {"readOptions": {"transaction": "AAAAAAAAAAAAAAAAAAAAAAAA"}},
    "a property mask with a space": {"propertyMask": {"paths": ["x y"]}},
    "a property mask with an empty path": {"propertyMask": {"paths": [""]}},
}

# What is too large to pair: entities at and over the limits (the size of `K/a` with a string `s`
# and 100 bytes in `t` is 158 bytes and the string's), a path too long, too many mutations.
def sized(n, name="big"):
    return {"mode": "NON_TRANSACTIONAL", "mutations": [{"upsert": {"key": key("K", "a"), "properties": {
        "s": {"stringValue": "x" * n, "excludeFromIndexes": True}, "t": {"stringValue": "y" * 100}}}}]}


def indexed(n):
    return {"mode": "NON_TRANSACTIONAL", "mutations": [{"upsert": {"key": key("K", "a"), "properties": {
        "arr": {"arrayValue": {"values": [ONE] * n}}}}}]}


ALONE = [
    ("an entity of 1,048,576 bytes", "commit", sized(1048418)),
    ("an entity of 1,048,577 bytes", "commit", sized(1048419)),
    ("a string of 1,048,488 bytes", "commit", {"mode": "NON_TRANSACTIONAL", "mutations": [{"upsert": {
        "key": key("K", "a"), "properties": {"s": {"stringValue": "x" * 1048488, "excludeFromIndexes": True}}}}]}),
    ("5,000 indexed values", "commit", indexed(5000)),
    ("5,001 indexed values", "commit", indexed(5001)),
    ("a path of 101 elements", "commit", {"mode": "NON_TRANSACTIONAL", "mutations": [
        {"upsert": {"key": {"path": [{"kind": "K", "name": "a"}] * 101}}}]}),
    ("a path of 101 elements, then a reserved kind", "commit", {"mode": "NON_TRANSACTIONAL", "mutations": [
        {"upsert": {"key": {"path": [{"kind": "K", "name": "a"}] * 101}}}, {"upsert": {"key": key("__x__", "a")}}]}),
    ("a reserved kind, then a path of 101 elements", "commit", {"mode": "NON_TRANSACTIONAL", "mutations": [
        {"upsert": {"key": key("__x__", "a")}}, {"upsert": {"key": {"path": [{"kind": "K", "name": "a"}] * 101}}}]}),
    ("a lookup of a path of 101 elements", "lookup", {"keys": [{"path": [{"kind": "K", "name": "a"}] * 101}]}),
    ("501 mutations", "commit", {"mode": "NON_TRANSACTIONAL", "mutations": [
        {"upsert": {"key": key("K", n + 1)}} for n in range(501)]}),
    ("500 mutations", "commit", {"mode": "NON_TRANSACTIONAL", "mutations": [
        {"upsert": {"key": key("K", n + 1)}} for n in range(500)]}),
]

def nontx(*mutations):
    return ("commit", {"mode": "NON_TRANSACTIONAL", "mutations": list(mutations)})


def tx(*mutations):
    return ("commit", {"mode": "TRANSACTIONAL", "transaction": "{tx}", "mutations": list(mutations)})


BEGIN = ("beginTransaction", {})
FAIL = {"conflictResolutionStrategy": "FAIL"}


def look(*names):
    return ("lookup", {"keys": [key("K", n) for n in names]})


# Requests in turn, in one project after `SEED`: how mutations apply, and base versions. `{tx}`
# stands for the transaction the last `beginTransaction` began.
SEQUENCES = [
    ("a non-transactional commit keeps the mutations before one that fails", [
        nontx({"upsert": {"key": key("K", "x")}}, {"update": {"key": key("K", "nothing")}}), look("x")]),
    ("a transaction writes nothing when a mutation fails", [
        BEGIN, tx({"upsert": {"key": key("K", "y")}}, {"update": {"key": key("K", "nothing")}}), look("y")]),
    ("a transaction's mutations see those before them", [
        BEGIN, tx({"insert": {"key": key("K", "f")}}, {"update": {"key": key("K", "f"), "properties": {"x": ONE}}}),
        look("f")]),
    ("an upsert, then a delete, in a transaction", [
        BEGIN, tx({"upsert": {"key": key("K", "d")}}, {"delete": key("K", "d")}), look("d")]),
    ("a delete, then an upsert, in a transaction", [
        BEGIN, tx({"delete": key("K", "b")}, {"upsert": {"key": key("K", "b"), "properties": {"z": ONE}}}),
        look("b")]),
    ("a base version that does not match is written over", [
        nontx({"upsert": {"key": key("K", "b"), "properties": {"a": {"integerValue": "2"}}}, "baseVersion": "7"}),
        nontx({"update": {"key": key("K", "b"), "properties": {"a": {"integerValue": "3"}}}, "baseVersion": "7",
               "conflictResolutionStrategy": "SERVER_VALUE"}),
        nontx({"delete": key("K", "b"), "updateTime": "2001-01-01T00:00:00Z"}), look("b")]),
    ("FAIL against the stored version", [
        nontx({"upsert": {"key": key("K", "m")}, "baseVersion": "0", **FAIL}),
        nontx({"upsert": {"key": key("K", "n")}, "baseVersion": "7", **FAIL}),
        nontx({"upsert": {"key": key("K", "b")}, "baseVersion": "7", **FAIL}),
        nontx({"delete": key("K", "b"), "baseVersion": "7", **FAIL}),
        nontx({"insert": {"key": key("K", "b")}, "baseVersion": "0", **FAIL}),
        nontx({"update": {"key": key("K", "nothing")}, "baseVersion": "7", **FAIL}),
        nontx({"upsert": {"key": key("K", "b")}, "updateTime": "2001-01-01T00:00:00Z", **FAIL}),
        nontx({"delete": key("K", "zz"), "baseVersion": "0", **FAIL}), look("b", "m", "n")]),
    ("read times about the epoch", [
        ("lookup", {"keys": [key("K", "b")], "readOptions": {"readTime": "1970-01-01T00:00:00Z"}}),
        ("lookup", {"keys": [key("K", "b")], "readOptions": {"readTime": "1970-01-01T00:00:00.000001Z"}}),
        ("lookup", {"keys": [key("K", "b")], "readOptions": {"readTime": "1969-12-31T23:59:59.999999Z"}})]),
    ("names of entity values", [
        nontx({"upsert": {"key": key("K", "a"), "properties": {"e": {"entityValue": {"properties": {
            "q" * 1498: ONE}}}}}}),
        nontx({"upsert": {"key": key("K", "a"), "properties": {"e": {"entityValue": {"properties": {
            "q" * 1499: ONE}}}}}}),
        nontx({"upsert": {"key": key("K", "a"), "properties": {"e": {"arrayValue": {"values": [{"entityValue": {
            "properties": {"q" * 1499: ONE}}}]}}}}}),
        nontx({"upsert": {"key": key("K", "a"), "properties": {"e": {"entityValue": {"properties": {
            "__q__" + "q" * 1500: ONE}}}}}}),
        nontx({"upsert": {"key": key("K", "a"), "properties": {"e" * 1501: {"entityValue": {"properties": {
            "q": ONE}}}}}})]),
]

SEED = {"mode": "NON_TRANSACTIONAL", "mutations": [{"upsert": {"key": key("K", "b"), "properties": {"a": ONE}}}]}


def mutation(problem, default_key):
    p = dict(problem)
    op = p.pop("op", "upsert")
    k = p.pop("key", default_key)
    properties = p.pop("properties", {})
    if op is None:
        return p
    if op == "delete":
        return {"delete": k, **p}
    entity = {"properties": properties}
    if k is not None:
        entity["key"] = k
    return {op: entity, **p}


def fits(a, b):
    return not ((set(a) & set(b)) - {"properties"})


def merged(a, b):
    out = dict(a)
    for part, value in b.items():
        out[part] = {**out[part], **value} if part == "properties" and part in out else value
    return out


def commit(*mutations, request=None):
    body = {"mode": "NON_TRANSACTIONAL", "mutations": list(mutations)}
    for part, value in (request or {}).items():
        if part == "more":
            body["mutations"] = body["mutations"] + value
        elif value is None:
            body.pop(part, None)
        else:
            body[part] = value
    return body


def lookup(*problems):
    body = {"keys": []}
    for p in problems:
        for part, value in p.items():
            if part == "key":
                body["keys"].append(value)
                if p.get("twice"):
                    body["keys"].append(value)
            elif part == "readOptions":
                body.setdefault("readOptions", {}).update(value)
            elif part == "propertyMask":
                body["propertyMask"] = value
    if not any("keys" in p for p in problems) and not body["keys"]:
        body["keys"].append(key("K", "b"))
    return body


def cases():
    a, b = key("K", "a"), key("K", "b")
    out = []
    for name, p in MUTATION.items():
        out.append((f"commit: {name}", "commit", commit(mutation(p, a))))
    for (n1, p1), (n2, p2) in itertools.permutations(MUTATION.items(), 2):
        out.append((f"commit: {n1}, then {n2}", "commit", commit(mutation(p1, a), mutation(p2, b))))
    for (n1, p1), (n2, p2) in itertools.permutations(MUTATION.items(), 2):
        if fits(p1, p2):
            out.append((f"commit: {n1} with {n2}", "commit", commit(mutation(merged(p1, p2), a))))
    for (n1, r), (n2, p) in itertools.product(REQUEST.items(), MUTATION.items()):
        out.append((f"commit: {n1}, {n2}", "commit", commit(mutation(p, a), request=r)))
    for (n1, r1), (n2, r2) in itertools.permutations(REQUEST.items(), 2):
        if not (set(r1) & set(r2)):
            out.append((f"commit: {n1}, {n2}", "commit", commit({"upsert": {"key": a}}, request={**r1, **r2})))
    for name, p in LOOKUP.items():
        out.append((f"lookup: {name}", "lookup", lookup(p)))
    for (n1, p1), (n2, p2) in itertools.permutations(LOOKUP.items(), 2):
        if not (set(p1) & set(p2) & {"keys", "propertyMask", "readOptions"}):
            out.append((f"lookup: {n1}, then {n2}", "lookup", lookup(p1, p2)))
    for name, verb, body in ALONE:
        out.append((f"{verb}: {name}", verb, body))
    return out


def compress(text):
    """Runs of 200 or more of one character as `{repeat:<c>:<n>}`."""
    return re.sub(r"(.)\1{199,}", lambda m: f"{{repeat:{m.group(1)}:{len(m.group(0))}}}", text)


# The stored version a `FAIL` strategy prints: a time in microseconds.
STORED = re.compile(r"the stored version \(\d{16}\)")


def masked(text, project):
    text = STORED.sub("the stored version (<version>)", text)
    text = VERSION.sub(lambda m: f'"{m.group(1)}": "<version>"', mask(text, project))
    return CURSOR.sub(lambda m: f'"{m.group(1)}": "<cursor>"', text)


def call(project, verb, body):
    conn = http.client.HTTPConnection(*HOST.split(":"), timeout=60)
    conn.request("POST", f"/v1/projects/{project}:{verb}", body=json.dumps(body).encode(), headers=JSON)
    r = conn.getresponse()
    return r.status, r.getheader("content-type"), r.read().decode()


def main():
    recorded = []
    for n, (name, verb, body) in enumerate(cases()):
        project = f"cc{RUN}-{n}"
        status, _, text = call(project, "commit", SEED)
        assert status == 200, text
        status, ctype, text = call(project, verb, body)
        recorded.append({"name": name, "path": f"/v1/projects/{{project}}:{verb}",
                         "body": json.loads(compress(json.dumps(body))),
                         "outcome": {"status": status, "contentType": ctype,
                                     "body": compress(masked(text, project))}})
    for n, (name, steps) in enumerate(SEQUENCES):
        project = f"cs{RUN}-{n}"
        status, _, text = call(project, "commit", SEED)
        assert status == 200, text
        transaction = None
        recorded_steps = []
        for verb, body in steps:
            sent = json.loads(json.dumps(body).replace("{tx}", transaction or "{tx}"))
            status, ctype, text = call(project, verb, sent)
            if verb == "beginTransaction":
                transaction = json.loads(text)["transaction"]
            recorded_steps.append({"path": f"/v1/projects/{{project}}:{verb}", "body": json.loads(compress(json.dumps(body))),
                                   "outcome": {"status": status, "contentType": ctype,
                                               "body": compress(masked(text, project))}})
        recorded.append({"name": f"sequence: {name}", "steps": recorded_steps})
    sys.stdout.write('{\n "oracle": "cloud-firestore-emulator v1.22.0 (sha256 '
                     '9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)",\n'
                     ' "generated_by": "tools/oracle/datastore_commit_checks.py",\n'
                     f' "every_case": {json.dumps(EVERY_CASE)},\n "seed": {json.dumps(SEED)},\n "cases": [\n')
    sys.stdout.write(",\n".join("  " + json.dumps(c, ensure_ascii=False, separators=(",", ":")) for c in recorded))
    sys.stdout.write("\n ]\n}\n")


if __name__ == "__main__":
    main()
