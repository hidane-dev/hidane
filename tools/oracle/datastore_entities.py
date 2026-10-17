#!/usr/bin/env python3
"""How does the official emulator in Datastore mode store and read entities? (#81)

Usage (from the repository root), with a jar freshly started with `--database-mode
datastore-mode` under an English locale:
    python3 -I tools/oracle/datastore_entities.py 127.0.0.1:8086 \\
        > crates/hidane/tests/fixtures/datastore_entities.json

Over REST (`google.datastore.v1.Datastore`'s HTTP bindings, `POST /v1/projects/{p}:…`), in
order, against one fresh process (the IDs it allocates depend on everything allocated before):
how it refuses the Firestore API; `allocateIds` and `reserveIds`; `commit` with each mutation,
each kind of value and each invalid key, value and request; `lookup`; and transactions
(`beginTransaction`, `commit` in one, `rollback`, a read-only one, a lock held by another). The
fixture has the format of `rest.json`; server times, versions and transaction IDs are masked.
`crates/hidane/tests/datastore.rs` replays it. Stdlib only.
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
PROJECT = f"datastore-{time.strftime('%H%M%S')}"
P = "{project}"
JSON = {"Content-Type": "application/json"}

CASES = []


def case(name, path, body=None, method="POST", raw=None, headers=None):
    c = {"name": name, "method": method, "path": path, "headers": JSON if headers is None else headers,
         "compare": "exact"}
    if raw is not None:
        c["raw"] = raw
    elif body is not None:
        c["body"] = body
    CASES.append(c)


def rpc(name, verb, body=None, project=P, **more):
    case(name, f"/v1/projects/{project}:{verb}", body, **more)


def key(*path, ns=None, project=None):
    """A key: `("Kind", "name")`, `("Kind", 5)` or `("Kind", None)` (incomplete) per element."""
    partition = {}
    if project is not None:
        partition["projectId"] = project
    if ns is not None:
        partition["namespaceId"] = ns
    elements = []
    for kind, id_ in path:
        element = {"kind": kind}
        if isinstance(id_, str):
            element["name"] = id_
        elif id_ is not None:
            element["id"] = str(id_)
        elements.append(element)
    out = {"path": elements}
    if partition:
        out["partitionId"] = partition
    return out


def commit(name, *mutations, mode="NON_TRANSACTIONAL", **more):
    rpc(name, "commit", {"mode": mode, "mutations": list(mutations), **more})


def upsert(k, **properties):
    return {"upsert": {"key": k, "properties": properties}}


def insert(k, **properties):
    return {"insert": {"key": k, "properties": properties}}


def update(k, **properties):
    return {"update": {"key": k, "properties": properties}}


def delete(k):
    return {"delete": k}


def lookup(name, *keys, **more):
    rpc(name, "lookup", {"keys": list(keys), **more})


I = lambda n: {"integerValue": str(n)}
S = lambda s: {"stringValue": s}

# --- the Firestore API, refused ---------------------------------------------------------------
DB = f"projects/{P}/databases/(default)"
case("firestore: get a document", f"/v1/{DB}/documents/c/d", method="GET")
case("firestore: commit", f"/v1/{DB}/documents:commit", {"writes": []})
case("firestore: runQuery", f"/v1/{DB}/documents:runQuery", {"structuredQuery": {"from": [{"collectionId": "c"}]}})
case("firestore: batchGet", f"/v1/{DB}/documents:batchGet", {"documents": [f"{DB}/documents/c/d"]})
case("firestore: listCollectionIds", f"/v1/{DB}/documents:listCollectionIds", {})
case("firestore: beginTransaction", f"/v1/{DB}/documents:beginTransaction", {})
case("firestore: list documents", f"/v1/{DB}/documents/c", method="GET")

# --- allocateIds and reserveIds -------------------------------------------------------------
rpc("allocate three", "allocateIds", {"keys": [key(("Task", None)), key(("Task", None)), key(("Other", None))]})
rpc("allocate below a parent", "allocateIds", {"keys": [key(("Parent", "p"), ("Child", None)), key(("Parent", 7), ("Child", None))]})
rpc("allocate in a namespace", "allocateIds", {"keys": [key(("Task", None), ns="ns1")]})
rpc("allocate in another project", "allocateIds", {"keys": [key(("Task", None))]}, project=f"{P}-2")
rpc("allocate in another database", "allocateIds", {"databaseId": "other", "keys": [key(("Task", None))]})
rpc("allocate no keys", "allocateIds", {"keys": []})
rpc("allocate an empty body", "allocateIds", {})
rpc("allocate a complete key", "allocateIds", {"keys": [key(("Task", 5))]})
rpc("allocate a named key", "allocateIds", {"keys": [key(("Task", "a"))]})
rpc("allocate a reserved kind", "allocateIds", {"keys": [key(("__x__", None))]})
rpc("allocate an empty kind", "allocateIds", {"keys": [key(("", None))]})
rpc("allocate under an incomplete parent", "allocateIds", {"keys": [key(("Parent", None), ("Child", None))]})
rpc("allocate a complete key under an incomplete parent", "allocateIds", {"keys": [key(("Parent", None), ("Child", "a"))]})
rpc("allocate in a key of another project", "allocateIds", {"keys": [key(("Task", None), project="elsewhere")]})
rpc("allocate an empty path", "allocateIds", {"keys": [{"path": []}]})
rpc("reserve an id", "reserveIds", {"keys": [key(("Task", 5))]})
rpc("reserve a large id", "reserveIds", {"keys": [key(("Task", 5066549580791808))]})
rpc("reserve a name", "reserveIds", {"keys": [key(("Task", "a"))]})
rpc("reserve an incomplete key", "reserveIds", {"keys": [key(("Task", None))]})
rpc("reserve no keys", "reserveIds", {"keys": []})
rpc("reserve in another database", "reserveIds", {"databaseId": "other", "keys": [key(("Task", 6))]})
rpc("allocate after reserving", "allocateIds", {"keys": [key(("Task", None)), key(("Task", None))]})

# --- commit: every value, read back ---------------------------------------------------------
VALUES = {
    "null": {"nullValue": None},
    "bool": {"booleanValue": True},
    "int": I(-42),
    "int max": I(9223372036854775807),
    "double": {"doubleValue": 1.5},
    "double int": {"doubleValue": 2},
    "nan": {"doubleValue": "NaN"},
    "inf": {"doubleValue": "-Infinity"},
    "timestamp": {"timestampValue": "2020-01-01T00:00:00.123456789Z"},
    "timestamp offset": {"timestampValue": "2020-01-01T09:00:00+09:00"},
    "string": S("héllo"),
    "empty string": S(""),
    "blob": {"blobValue": "AQID"},
    "geo": {"geoPointValue": {"latitude": 1.5, "longitude": -2}},
    "key": {"keyValue": key(("A", "x"), ("B", 3))},
    "key in a namespace": {"keyValue": key(("A", "x"), ns="ns1")},
    "array": {"arrayValue": {"values": [I(1), S("b"), {"nullValue": None}]}},
    "empty array": {"arrayValue": {}},
    "entity": {"entityValue": {"properties": {"x": I(1), "y": {"entityValue": {"properties": {"z": S("deep")}}}}}},
    "entity with a key": {"entityValue": {"key": key(("E", "k")), "properties": {"x": I(1)}}},
    "excluded": {"stringValue": "x", "excludeFromIndexes": True},
    "meaning": {"integerValue": "1", "meaning": 22},
    "excluded array element": {"arrayValue": {"values": [{"integerValue": "1", "excludeFromIndexes": True}]}},
}
for label, value in VALUES.items():
    k = key(("Value", label))
    commit(f"write {label}", upsert(k, v=value))
    lookup(f"read {label}", k)

# --- commit: each mutation ------------------------------------------------------------------
commit("insert a new name", insert(key(("Task", "a")), n=I(1)))
commit("insert it again", insert(key(("Task", "a")), n=I(2)))
commit("insert an incomplete key", insert(key(("Task", None)), n=I(3)))
commit("insert an incomplete key below a parent", insert(key(("Parent", "p"), ("Task", None))))
commit("update it", update(key(("Task", "a")), n=I(4)))
commit("update a missing entity", update(key(("Task", "nope")), n=I(5)))
commit("update a missing entity in a namespace", update(key(("Task", "nope"), ns="ns1")))
commit("update a missing entity below a parent", update(key(("P", 3), ("Task", "nope"))))
commit("upsert an incomplete key", upsert(key(("Task", None))))
commit("delete it", delete(key(("Task", "a"))))
commit("delete a missing entity", delete(key(("Task", "never"))))
commit("delete an incomplete key", delete(key(("Task", None))))
commit("two mutations of one key", upsert(key(("Task", "twice")), n=I(1)), upsert(key(("Task", "twice")), n=I(2)))
commit("an insert and a delete of one key", insert(key(("Task", "id")), n=I(1)), delete(key(("Task", "id"))))
commit("no mutations")
commit("several mutations", upsert(key(("Task", "m1"))), insert(key(("Task", None))), upsert(key(("Task", "m2"))), delete(key(("Task", "m1"))))
lookup("read after several mutations", key(("Task", "m1")), key(("Task", "m2")))
commit("a matching base version, after a lookup", upsert(key(("Task", "bv")), n=I(1)))
commit("a base version that does not match", {"upsert": {"key": key(("Task", "bv")), "properties": {"n": I(2)}}, "baseVersion": "1"})
commit("a base version 0 for a missing entity", {"upsert": {"key": key(("Task", "bv-new")), "properties": {}}, "baseVersion": "0"})
commit("an update time that does not match", {"upsert": {"key": key(("Task", "bv")), "properties": {}}, "updateTime": "2000-01-01T00:00:00Z"})
commit("a property mask", {"update": {"key": key(("Task", "bv")), "properties": {"m": I(9)}}, "propertyMask": {"paths": ["m"]}})
lookup("read after a property mask", key(("Task", "bv")))
commit("an unknown mode", upsert(key(("Task", "mode"))), mode="MODE_UNSPECIFIED")
commit("transactional without a transaction", upsert(key(("Task", "mode"))), mode="TRANSACTIONAL")
commit("non-transactional with a transaction", upsert(key(("Task", "mode"))), transaction="ET9CDwAAAAAA")
rpc("commit an empty body", "commit", {})
rpc("commit no body", "commit")
rpc("commit not JSON", "commit", raw="{x")
rpc("commit an unknown field", "commit", {"mode": "NON_TRANSACTIONAL", "x": 1})
commit("a mutation with no operation", {})
commit("in another database", upsert(key(("Task", "db"))), databaseId="other")
lookup("read in another database", key(("Task", "db")), databaseId="other")
lookup("read it in the default database", key(("Task", "db")))
rpc("commit in another project", "commit", {"mode": "NON_TRANSACTIONAL", "mutations": [upsert(key(("Task", "x")))]}, project=f"{P}-2")
lookup("read in the first project", key(("Task", "x")))

# --- commit: keys refused ------------------------------------------------------------------
for label, k in [
    ("a reserved kind", key(("__x__", "a"))),
    ("a kind __kind__", key(("__kind__", "a"))),
    ("a reserved name", key(("Task", "__x__"))),
    ("an empty kind", key(("", "a"))),
    ("an empty name", key(("Task", ""))),
    ("id 0", key(("Task", 0))),
    ("a negative id", key(("Task", -1))),
    ("an empty path", {"path": []}),
    ("no path", {}),
    ("an incomplete parent", key(("Parent", None), ("Task", "a"))),
    ("another project", key(("Task", "a"), project="elsewhere")),
    ("the same project", key(("Task", "same"), project=P)),
    ("a reserved namespace", key(("Task", "a"), ns="__x__")),
    ("a namespace with a space", key(("Task", "a"), ns="a b")),
    ("a long namespace", key(("Task", "a"), ns="n" * 101)),
    ("a name of 1500 bytes", key(("Task", "n" * 1500))),
    ("a name of 1501 bytes", key(("Task", "n" * 1501))),
    ("a kind of 1501 bytes", key(("K" * 1501, "a"))),
    ("an element with a name and an id", {"path": [{"kind": "Task", "name": "a", "id": "1"}]}),
    ("a deep path", key(*[("K", f"e{i}") for i in range(101)])),
    ("a path of 100", key(*[("K", f"e{i}") for i in range(100)])),
]:
    commit(f"upsert {label}", upsert(k))
lookup("read the same project", key(("Task", "same")))

# --- commit: values refused ----------------------------------------------------------------
for label, value in [
    ("nested arrays", {"arrayValue": {"values": [{"arrayValue": {"values": [I(1)]}}]}}),
    ("an array excluded from indexes", {"arrayValue": {"values": [I(1)]}, "excludeFromIndexes": True}),
    ("an indexed string of 1500 bytes", S("s" * 1500)),
    ("an indexed string of 1501 bytes", S("s" * 1501)),
    ("an excluded string of 1501 bytes", {"stringValue": "s" * 1501, "excludeFromIndexes": True}),
    ("an indexed blob of 1501 bytes", {"blobValue": "AAAA" * 501}),
    ("an excluded blob of 1501 bytes", {"blobValue": "AAAA" * 501, "excludeFromIndexes": True}),
    ("a value without a type", {}),
    ("a timestamp before year 1", {"timestampValue": "0000-12-31T23:59:59Z"}),
    ("a latitude of 91", {"geoPointValue": {"latitude": 91, "longitude": 0}}),
    ("an incomplete key value", {"keyValue": key(("A", None))}),
    ("a key value of another project", {"keyValue": key(("A", "x"), project="elsewhere")}),
    ("a key value with a reserved kind", {"keyValue": key(("__x__", "x"))}),
    ("a key value in a reserved namespace", {"keyValue": key(("A", "x"), ns="__x__")}),
    ("a key value of an invalid project", {"keyValue": key(("A", "x"), project="Bad")}),
    ("a key value of a project with a domain", {"keyValue": key(("A", "x"), project="example.com:p")}),
    ("a key value of a partitioned project", {"keyValue": key(("A", "x"), project="s~p")}),
    ("an entity value keyed in an invalid project", {"entityValue": {"key": key(("A", "x"), project="Bad"), "properties": {}}}),
    ("an invalid project before an empty kind", {"keyValue": key(("", "x"), project="Bad")}),
    ("a meaning of 99", {"integerValue": "1", "meaning": 99}),
    ("an integer as text", {"integerValue": "x"}),
    ("an entity value with a reserved property", {"entityValue": {"properties": {"__x__": I(1)}}}),
]:
    commit(f"write {label}", upsert(key(("Bad", label)), v=value))
for label, properties in [
    ("an empty property name", {"": I(1)}),
    ("a reserved property name", {"__x__": I(1)}),
    ("a property name with a dot", {"a.b": I(1)}),
    ("a property name of 1501 bytes", {"p" * 1501: I(1)}),
    ("a property __key__", {"__key__": I(1)}),
]:
    commit(f"write {label}", {"upsert": {"key": key(("Bad", label)), "properties": properties}})
lookup("read a property name with a dot", key(("Bad", "a property name with a dot")))
commit("501 mutations", *[upsert(key(("Many", f"m{i}"))) for i in range(501)])
commit("500 mutations", *[upsert(key(("Many", f"m{i}"))) for i in range(500)])
commit("an entity of over 1 MiB", upsert(key(("Big", "b")), v={"stringValue": "x" * (1024 * 1024 + 10), "excludeFromIndexes": True}))

# --- an allocated ID already in use ----------------------------------------------------------
# Each in a project of its own, whose first ID is 2^52 + 2^50: a commit that allocates an ID an
# entity has fails, after the mutations before it, and the counter stays past that ID.
FIRST, SECOND = 5629499534213120, 5066549580791808


def in_project(name, suffix, verb, body):
    rpc(name, verb, body, project=f"{P}-{suffix}")


in_project("an allocated id written before in the commit", "ids1", "commit", {"mode": "NON_TRANSACTIONAL", "mutations": [insert(key(("Task", FIRST))), upsert(key(("Task", None))), insert(key(("Task", "after")))]})
in_project("an allocated id: the mutations before it", "ids1", "lookup", {"keys": [key(("Task", FIRST)), key(("Task", "after"))]})
in_project("an allocated id: the next allocation", "ids1", "commit", {"mode": "NON_TRANSACTIONAL", "mutations": [upsert(key(("Task", None)))]})
in_project("an allocated id stored before: store it", "ids2", "commit", {"mode": "NON_TRANSACTIONAL", "mutations": [upsert(key(("Task", FIRST)))]})
in_project("an allocated id stored before", "ids2", "commit", {"mode": "NON_TRANSACTIONAL", "mutations": [insert(key(("Task", None)))]})
in_project("an allocated id of another kind", "ids3", "commit", {"mode": "NON_TRANSACTIONAL", "mutations": [insert(key(("Other", FIRST))), upsert(key(("Task", None)))]})
in_project("an allocated id below a parent in a namespace", "ids4", "commit", {"mode": "NON_TRANSACTIONAL", "mutations": [insert(key(("Parent", 7), ("Task", FIRST), ns="n1")), upsert(key(("Parent", 7), ("Task", None), ns="n1"))]})
in_project("an allocated id deleted before: store it", "ids5", "commit", {"mode": "NON_TRANSACTIONAL", "mutations": [upsert(key(("Task", FIRST)))]})
in_project("an allocated id deleted before: delete it", "ids5", "commit", {"mode": "NON_TRANSACTIONAL", "mutations": [{"delete": key(("Task", FIRST))}]})
in_project("an allocated id deleted before", "ids5", "commit", {"mode": "NON_TRANSACTIONAL", "mutations": [upsert(key(("Task", None)))]})
in_project("allocateIds gives an id in use: store it", "ids6", "commit", {"mode": "NON_TRANSACTIONAL", "mutations": [upsert(key(("Task", FIRST)))]})
in_project("allocateIds gives an id in use", "ids6", "allocateIds", {"keys": [key(("Task", None))]})
in_project("the second allocated id in use", "ids7", "commit", {"mode": "NON_TRANSACTIONAL", "mutations": [insert(key(("Task", SECOND))), upsert(key(("Task", None))), upsert(key(("Task", None)))]})

# --- lookup -----------------------------------------------------------------------------------
lookup("found and missing", key(("Task", "twice")), key(("Task", "never")), key(("Task", "nope"), ns="ns1"))
lookup("one key twice", key(("Task", "twice")), key(("Task", "twice")))
lookup("an incomplete key", key(("Task", None)))
lookup("a reserved kind", key(("__x__", "a")))
lookup("no keys")
rpc("lookup an empty body", "lookup", {})
lookup("strong", key(("Task", "twice")), readOptions={"readConsistency": "STRONG"})
lookup("eventual", key(("Task", "twice")), readOptions={"readConsistency": "EVENTUAL"})
lookup("at a read time in the past", key(("Task", "twice")), readOptions={"readTime": "2020-01-01T00:00:00Z"})
lookup("in an unknown transaction", key(("Task", "twice")), readOptions={"transaction": "ET9CDwAAAAAA"})
lookup("in a transaction that is not base64", key(("Task", "twice")), readOptions={"transaction": "!!"})
lookup("a property mask", key(("Value", "entity")), propertyMask={"paths": ["v.x"]})
lookup("a property mask of a missing property", key(("Value", "entity")), propertyMask={"paths": ["w"]})

# --- transactions ------------------------------------------------------------------------------
rpc("begin a transaction", "beginTransaction", {})
rpc("begin a read-only transaction", "beginTransaction", {"transactionOptions": {"readOnly": {}}})
rpc("begin a read-only transaction at a read time", "beginTransaction", {"transactionOptions": {"readOnly": {"readTime": "2020-01-01T00:00:00Z"}}})
rpc("begin a read-write transaction retrying an unknown one", "beginTransaction", {"transactionOptions": {"readWrite": {"previousTransaction": "ET9CDwAAAAAA"}}})
rpc("roll back an unknown transaction", "rollback", {"transaction": "ET9CDwAAAAAA"})
rpc("roll back no transaction", "rollback", {})
rpc("roll back a transaction that is not base64", "rollback", {"transaction": "!!"})
commit("commit an unknown transaction", upsert(key(("Task", "tx"))), mode="TRANSACTIONAL", transaction="ET9CDwAAAAAA")


def run_sequences(conn_call):
    """Transactions need the IDs the server gives: these steps name them `<t1>`, `<t2>`, …, a
    begin step says which name it defines (`defines`), and a replay fills them in as it goes."""
    out = []
    tx = {}

    def step(name, verb, body, defines=None, suffix=""):
        labelled = json.dumps(body)
        text = labelled
        for label, value in tx.items():
            text = text.replace(f"<{label}>", value)
        status, ctype, answer = conn_call("POST", f"/v1/projects/{PROJECT}{suffix}:{verb}", text)
        recorded = {"name": name, "method": "POST", "path": f"/v1/projects/{P}{suffix}:{verb}",
                    "headers": JSON, "body": json.loads(labelled), "compare": "exact"}
        if defines:
            recorded["defines"] = defines
        recorded["outcome"] = {"status": status, "contentType": ctype, "body": masked(answer)}
        out.append(recorded)
        return answer

    def begin(label, name, options=None, suffix=""):
        answer = step(name, "beginTransaction", {"transactionOptions": options} if options else {},
                      defines=label, suffix=suffix)
        tx[label] = json.loads(answer)["transaction"]

    begin("t1", "sequence: begin")
    step("sequence: lookup in it", "lookup", {"keys": [key(("Task", "twice"))], "readOptions": {"transaction": "<t1>"}})
    step("sequence: commit in it", "commit", {"mode": "TRANSACTIONAL", "transaction": "<t1>", "mutations": [upsert(key(("Task", "twice")), n=I(7))]})
    step("sequence: commit it again", "commit", {"mode": "TRANSACTIONAL", "transaction": "<t1>", "mutations": []})
    step("sequence: roll it back after the commit", "rollback", {"transaction": "<t1>"})
    step("sequence: read the committed write", "lookup", {"keys": [key(("Task", "twice"))]})
    begin("t2", "sequence: begin another")
    step("sequence: roll it back", "rollback", {"transaction": "<t2>"})
    step("sequence: roll it back again", "rollback", {"transaction": "<t2>"})
    step("sequence: commit after the rollback", "commit", {"mode": "TRANSACTIONAL", "transaction": "<t2>", "mutations": []})
    step("sequence: lookup after the rollback", "lookup", {"keys": [key(("Task", "twice"))], "readOptions": {"transaction": "<t2>"}})
    begin("t3", "sequence: begin read-only", {"readOnly": {}})
    step("sequence: lookup in the read-only one", "lookup", {"keys": [key(("Task", "twice"))], "readOptions": {"transaction": "<t3>"}})
    step("sequence: write in the read-only one", "commit", {"mode": "TRANSACTIONAL", "transaction": "<t3>", "mutations": [upsert(key(("Task", "ro")))]})
    step("sequence: commit the read-only one empty", "commit", {"mode": "TRANSACTIONAL", "transaction": "<t3>", "mutations": []})
    # A key one transaction read is locked: another transaction's write to it waits, then is
    # refused. (A write outside any transaction waits until the reader expires, 60 s later.)
    begin("t4", "sequence: begin and read a key")
    step("sequence: read the key", "lookup", {"keys": [key(("Task", "locked"))], "readOptions": {"transaction": "<t4>"}})
    begin("t6", "sequence: begin another writer")
    step("sequence: write the key in the other", "commit", {"mode": "TRANSACTIONAL", "transaction": "<t6>", "mutations": [upsert(key(("Task", "locked")), n=I(1))]})
    step("sequence: commit the reader", "commit", {"mode": "TRANSACTIONAL", "transaction": "<t4>", "mutations": [upsert(key(("Task", "locked")), n=I(2))]})
    step("sequence: the reader's write", "lookup", {"keys": [key(("Task", "locked"))]})
    begin("t5", "sequence: begin a lookup with a read time")
    step("sequence: a read time in a transaction", "lookup", {"keys": [key(("Task", "twice"))], "readOptions": {"transaction": "<t5>", "readTime": "2020-01-01T00:00:00Z"}})
    step("sequence: an incomplete key in a transaction", "commit", {"mode": "TRANSACTIONAL", "transaction": "<t5>", "mutations": [insert(key(("Task", None)))]})
    # An allocated ID the transaction wrote before is an internal error, and nothing is written;
    # one stored before, the error a non-transactional commit gets.
    begin("t7", "sequence: begin in a project of its own", suffix="-ids8")
    step("sequence: an allocated id the transaction wrote", "commit", {"mode": "TRANSACTIONAL", "transaction": "<t7>", "mutations": [insert(key(("Task", FIRST))), upsert(key(("Task", None)))]}, suffix="-ids8")
    step("sequence: nothing written", "lookup", {"keys": [key(("Task", FIRST))]}, suffix="-ids8")
    step("sequence: the next allocation", "commit", {"mode": "NON_TRANSACTIONAL", "mutations": [upsert(key(("Task", None)))]}, suffix="-ids8")
    step("sequence: store an id before a transaction", "commit", {"mode": "NON_TRANSACTIONAL", "mutations": [upsert(key(("Task", FIRST)))]}, suffix="-ids9")
    begin("t8", "sequence: begin after it", suffix="-ids9")
    step("sequence: an allocated id stored before the transaction", "commit", {"mode": "TRANSACTIONAL", "transaction": "<t8>", "mutations": [upsert(key(("Task", None)))]}, suffix="-ids9")
    return out


VERSION = re.compile(r'"(version|commitVersion|snapshotVersion)": "\d+"')
CURSOR = re.compile(r'"(cursor|endCursor|skippedCursor|startCursor)": "[^"]*"')


def masked(text):
    """Server times, versions and transaction IDs, and cursors, which are opaque bytes each
    emulator makes its own way (docs/adr/0009-datastore-mode.md)."""
    text = mask(text, PROJECT)
    text = VERSION.sub(lambda m: f'"{m.group(1)}": "<version>"', text)
    return CURSOR.sub(lambda m: f'"{m.group(1)}": "<cursor>"', text)


def call(method, path, body, headers=None):
    conn = http.client.HTTPConnection(*HOST.split(":"), timeout=30)
    conn.request(method, path, body=None if body is None else body.encode(), headers=headers or JSON)
    r = conn.getresponse()
    return r.status, r.getheader("content-type"), r.read().decode()


def run():
    out = []
    for c in CASES:
        path = c["path"].replace("{project}", PROJECT)
        if "raw" in c:
            body = c["raw"].replace("{project}", PROJECT)
        elif "body" in c:
            body = json.dumps(c["body"]).replace("{project}", PROJECT)
        else:
            body = None
        headers = dict(c["headers"]) if body is not None else {}
        status, ctype, text = call(c["method"], path, body, headers)
        out.append(dict(c, outcome={"status": status, "contentType": ctype, "body": masked(text)}))
    out.extend(run_sequences(call))
    return out


if __name__ == "__main__":
    json.dump({
        "oracle": "cloud-firestore-emulator v1.22.0 (sha256 9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)",
        "generated_by": "tools/oracle/datastore_entities.py",
        "mode": "datastore-mode",
        "cases": run(),
    }, sys.stdout, ensure_ascii=False, indent=1)
    print()
