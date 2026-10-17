"""Runs one section of steps through google-cloud-datastore (the Python client library of
Datastore) and prints a transcript, so the transcripts of two emulators in Datastore mode can be
diffed (#81). DATASTORE_EMULATOR_HOST selects the emulator, started with `--database-mode
datastore-mode`; start it fresh for each section.

Usage: python3 transcript.py entities|queries|transactions > transcript.json

tools/oracle/README.md says how to run it.
"""

import base64
import datetime
import json
import math
import os
import re
import sys

from google.api_core.exceptions import GoogleAPICallError
from google.cloud import datastore
from google.cloud.datastore.helpers import GeoPoint
from google.cloud.datastore.query import And, Or, PropertyFilter

SECTION = sys.argv[1] if len(sys.argv) == 2 else None
if SECTION not in ("entities", "queries", "transactions"):
    sys.exit(__doc__.split("\n\n")[1])
PROJECT = f"demo-datastore-python-{SECTION}"
client = datastore.Client(project=PROJECT)
out = []


def mask(message):
    return re.sub(r"\d{10,}", "<n>", message.replace(PROJECT, "{project}"))


def error_of(e):
    """A server error as its gRPC code and message; anything else as a client error."""
    if isinstance(e, GoogleAPICallError) and e.grpc_status_code is not None:
        return {"code": e.grpc_status_code.name, "message": mask(e.message)}
    return {"client": f"{type(e).__name__}: {mask(str(e))}"}


def key_of(key):
    """A key as its path, and its namespace if it has one."""
    out = {"path": list(key.flat_path)}
    if key.namespace:
        out["namespace"] = key.namespace
    return out


def norm(v):
    """A Datastore value as JSON: NaN and infinities as strings, times as RFC 3339, the other
    non-JSON types as tagged objects."""
    if v is None or isinstance(v, (bool, str, int)):
        return v
    if isinstance(v, float):
        if math.isnan(v):
            return "NaN"
        if math.isinf(v):
            return "Infinity" if v > 0 else "-Infinity"
        return v
    if isinstance(v, datetime.datetime):
        return v.isoformat()
    if isinstance(v, bytes):
        return {"bytes": base64.b64encode(v).decode()}
    if isinstance(v, GeoPoint):
        return {"geopoint": [v.latitude, v.longitude]}
    if isinstance(v, datastore.Key):
        return {"key": key_of(v)}
    if isinstance(v, datastore.Entity):
        entity = {"properties": {k: norm(v[k]) for k in sorted(v)}}
        if v.key is not None:
            entity["key"] = key_of(v.key)
        if v.exclude_from_indexes:
            entity["excluded"] = sorted(v.exclude_from_indexes)
        return entity
    if isinstance(v, dict):
        return {k: norm(v[k]) for k in sorted(v)}
    if isinstance(v, (list, tuple, set)):
        return [norm(x) for x in v]
    return {"unknown": repr(v)}


def step(name, fn):
    try:
        out.append({"name": name, "ok": norm(fn())})
    except Exception as e:  # noqa: BLE001 - every failure is part of the transcript
        out.append({"name": name, "error": error_of(e)})


def entity(key, exclude=(), **properties):
    e = datastore.Entity(key=key, exclude_from_indexes=exclude)
    e.update(properties)
    return e


def keys(entities):
    return [key_of(e.key) for e in entities]


# --- entities -------------------------------------------------------------------------------

def entities():
    task = client.key("Task", "t1")
    when = datetime.datetime(2020, 1, 2, 3, 4, 5, 678000, tzinfo=datetime.timezone.utc)
    every = entity(
        task, exclude=("long",),
        text="a", count=3, ratio=0.5, done=False, nothing=None, at=when, raw=b"\x00\x01",
        place=GeoPoint(35.0, 139.0), owner=client.key("User", "u1"), tags=["x", "y"],
        empty=[], nested=entity(None, x=1, y="z"), long="b" * 2000, big=2 ** 62,
        nan=float("nan"), inf=float("inf"), ninf=float("-inf"), zero=-0.0)
    step("put every type", lambda: (client.put(every), "ok")[1])
    step("get it back", lambda: client.get(task))
    step("get a missing entity", lambda: client.get(client.key("Task", "nobody")))

    def get_multi():
        missing = []
        found = client.get_multi([task, client.key("Task", "nobody"), client.key("Task", "t1")],
                                 missing=missing)
        return {"found": keys(found), "missing": keys(missing)}
    step("get several, one missing, one twice", get_multi)
    step("overwrite it", lambda: (client.put(entity(task, text="b")), client.get(task))[1])

    def incomplete():
        e = entity(client.key("Task"), text="generated")
        client.put(e)
        return {"id": e.key.id, "read": client.get(e.key)}
    step("put an incomplete key", incomplete)
    step("allocate three IDs",
         lambda: [k.id for k in client.allocate_ids(client.key("Task"), 3)])
    step("allocate an ID under a parent",
         lambda: [key_of(k) for k in client.allocate_ids(client.key("List", "l1", "Task"), 1)])
    step("reserve an ID", lambda: (client.reserve_ids_multi([client.key("Task", 5)]), "ok")[1])
    step("reserve IDs in sequence",
         lambda: (client.reserve_ids_sequential(client.key("Task", 10), 3), "ok")[1])
    child = client.key("List", "l1", "Task", "c1")
    step("put a child", lambda: (client.put(entity(child, text="child")), client.get(child))[1])
    step("get a child's parent, never written", lambda: client.get(child.parent))
    other = datastore.Client(project=PROJECT, namespace="ns1")
    step("put in a namespace",
         lambda: (other.put(entity(other.key("Task", "t1"), text="ns")), other.get(other.key("Task", "t1")))[1])
    step("the default namespace keeps its own",
         lambda: client.get(task)["text"])
    step("put several",
         lambda: (client.put_multi([entity(client.key("Many", i), i=i) for i in range(1, 4)]),
                  keys(client.get_multi([client.key("Many", i) for i in range(1, 4)])))[1])
    step("delete several",
         lambda: (client.delete_multi([client.key("Many", 1), client.key("Many", 2)]),
                  keys(client.get_multi([client.key("Many", i) for i in range(1, 4)])))[1])
    step("delete a missing entity", lambda: (client.delete(client.key("Task", "nobody")), "ok")[1])
    step("delete it", lambda: (client.delete(task), client.get(task))[1])
    step("an indexed string of 1501 bytes",
         lambda: client.put(entity(client.key("Task", "long"), text="a" * 1501)))
    step("an indexed string of 1500 bytes",
         lambda: (client.put(entity(client.key("Task", "long"), text="a" * 1500)), "ok")[1])
    step("a property named __x__", lambda: client.put(entity(client.key("Task", "r"), __x__=1)))
    step("an array in an array", lambda: client.put(entity(client.key("Task", "r"), a=[[1]])))
    step("an entity over 1 MiB",
         lambda: client.put(entity(client.key("Task", "huge"), exclude=("a",), a="a" * 1_100_000)))
    step("a key name __x__", lambda: client.put(entity(client.key("Task", "__x__"), a=1)))
    step("a kind __x__", lambda: client.put(entity(client.key("__x__", "a"), a=1)))
    step("an ID 0 key", lambda: client.get(client.key("Task", 0)))
    step("a key of another project",
         lambda: client.get(datastore.Key("Task", "a", project="elsewhere")))


# --- queries --------------------------------------------------------------------------------

def queries():
    people = [
        ("p1", dict(a=1, b="x", tags=["red", "blue"])),
        ("p2", dict(a=2, b="y", tags=["red"])),
        ("p3", dict(a=2, b="x", tags=[])),
        ("p4", dict(a=3, b="z", tags=["blue", "green"])),
        ("p5", dict(a=3, b="y")),
        ("p6", dict(a=-1, b="x", tags=["green"])),
        ("p7", dict(b="w")),
        ("p8", dict(a=4.5, b="x")),
    ]
    client.put_multi([entity(client.key("P", name), **props) for name, props in people])
    family = client.key("F", "root")
    client.put_multi([
        entity(family, n=0),
        entity(client.key("F", "root", "C", "c1"), n=1),
        entity(client.key("F", "root", "C", "c2"), n=2),
        entity(client.key("C", "c3"), n=3),
    ])
    other = datastore.Client(project=PROJECT, namespace="ns1")
    other.put(entity(other.key("P", "n1"), a=1, b="x"))

    def run(q, **fetch):
        return keys(q.fetch(**fetch))

    def query(**kw):
        return client.query(kind="P", **kw)

    def filtered(*filters, **kw):
        q = query(**kw)
        for f in filters:
            q.add_filter(filter=f)
        return q

    step("kind P", lambda: run(query()))
    for op, value in [("=", 2), ("<", 2), ("<=", 2), (">", 2), (">=", 2), ("!=", 2), ("=", "x"),
                      ("IN", [1, 3]), ("NOT_IN", [2, 3]), ("=", None), ("=", 4.5), ("=", 2.0)]:
        step(f"a {op} {value!r}", lambda op=op, value=value: run(filtered(PropertyFilter("a", op, value))))
    step("tags = red", lambda: run(filtered(PropertyFilter("tags", "=", "red"))))
    step("tags IN [red, green]", lambda: run(filtered(PropertyFilter("tags", "IN", ["red", "green"]))))
    step("a = 2 and b = x", lambda: run(filtered(And([PropertyFilter("a", "=", 2), PropertyFilter("b", "=", "x")]))))
    step("a = 1 or b = y", lambda: run(filtered(Or([PropertyFilter("a", "=", 1), PropertyFilter("b", "=", "y")]))))
    step("a > 1 and b > w", lambda: run(filtered(PropertyFilter("a", ">", 1), PropertyFilter("b", ">", "w"))))
    step("order by a", lambda: run(query(order=["a"])))
    step("order by -a, b", lambda: run(query(order=["-a", "b"])))
    step("order by b, -__key__", lambda: run(query(order=["b", "-__key__"])))
    step("a > 1 order by b", lambda: run(filtered(PropertyFilter("a", ">", 1), order=["b"])))
    step("__key__ > p4", lambda: run(filtered(PropertyFilter("__key__", ">", client.key("P", "p4")))))
    step("keys only", lambda: run(query(projection=["__key__"])))

    def projected():
        return [norm(e) for e in query(projection=["a", "b"]).fetch()]
    step("project a, b", projected)
    step("distinct on b",
         lambda: [norm(e) for e in query(projection=["b"], distinct_on=["b"]).fetch()])
    step("limit 2", lambda: run(query(), limit=2))
    step("offset 6", lambda: run(query(), offset=6))
    step("limit 2, offset 3", lambda: run(query(), limit=2, offset=3))

    def pages():
        seen = []
        cursor = None
        while True:
            it = query(order=["b"]).fetch(limit=3, start_cursor=cursor)
            page = next(it.pages)
            seen.append(keys(page))
            cursor = it.next_page_token
            if cursor is None or len(seen) > 5:
                return seen
    step("pages of 3 by cursor", pages)
    step("children of F root", lambda: run(client.query(kind="C", ancestor=family)))
    step("everything under F root", lambda: run(client.query(ancestor=family)))
    step("kind P in ns1", lambda: keys(other.query(kind="P").fetch()))
    step("kinds", lambda: [e.key.name for e in client.query(kind="__kind__").fetch()])
    step("namespaces",
         lambda: [key_of(e.key) for e in client.query(kind="__namespace__").fetch()])
    step("a kind with no entities", lambda: run(client.query(kind="Nothing")))

    def aggregate(q, build):
        aq = client.aggregation_query(q)
        build(aq)
        return [{r.alias: norm(r.value) for r in results} for results in aq.fetch()]
    step("count P", lambda: aggregate(query(), lambda aq: aq.count(alias="n")))
    step("count b = x", lambda: aggregate(filtered(PropertyFilter("b", "=", "x")), lambda aq: aq.count(alias="n")))
    step("sum and avg of a",
         lambda: aggregate(query(), lambda aq: (aq.sum("a", alias="s"), aq.avg("a", alias="m"))))
    step("count with a limit",
         lambda: [{r.alias: r.value for r in rs}
                  for rs in client.aggregation_query(query()).count(alias="n").fetch(limit=2)])
    step("IN with 31 values", lambda: run(filtered(PropertyFilter("a", "IN", list(range(31))))))
    step("NOT_IN with 11 values", lambda: run(filtered(PropertyFilter("a", "NOT_IN", list(range(11))))))
    step("order twice by a", lambda: run(query(order=["a", "-a"])))
    step("project a, filter a = 1",
         lambda: run(filtered(PropertyFilter("a", "=", 1), projection=["a"])))


# --- transactions -----------------------------------------------------------------------------

def transactions():
    account = client.key("Account", "a1")
    client.put(entity(account, balance=100))

    def transfer():
        with client.transaction():
            current = client.get(account)
            current["balance"] -= 30
            client.put(current)
        return client.get(account)["balance"]
    step("read and write in a transaction", transfer)

    def rolled_back():
        tx = client.transaction()
        tx.begin()
        tx.put(entity(account, balance=0))
        tx.rollback()
        return client.get(account)["balance"]
    step("roll back", rolled_back)

    def failing():
        with client.transaction():
            client.put(entity(account, balance=1))
            raise RuntimeError("stop")
    step("an exception rolls back", failing)
    step("after it", lambda: client.get(account)["balance"])

    def read_only():
        with client.transaction(read_only=True) as tx:
            return client.get(account, transaction=tx)["balance"]
    step("read only", read_only)

    def read_only_write():
        with client.transaction(read_only=True) as tx:
            tx.put(entity(account, balance=5))
    step("a write in a read-only transaction", read_only_write)

    def query_in():
        parent = client.key("Bank", "b1")
        client.put_multi([entity(client.key("Bank", "b1", "Acct", i), n=i) for i in range(1, 4)])
        with client.transaction():
            q = client.query(kind="Acct", ancestor=parent)
            return [e["n"] for e in q.fetch()]
    step("an ancestor query in a transaction", query_in)

    def aggregate_in():
        parent = client.key("Bank", "b1")
        with client.transaction():
            aq = client.aggregation_query(client.query(kind="Acct", ancestor=parent)).count(alias="n")
            return [[r.value for r in rs] for rs in aq.fetch()]
    step("a count in a transaction", aggregate_in)

    def missing_in():
        with client.transaction() as tx:
            return client.get(client.key("Account", "nobody"), transaction=tx)
    step("a missing entity in a transaction", missing_in)

    def after_rollback():
        tx = client.transaction()
        tx.begin()
        tx.rollback()
        return client.get(account, transaction=tx)
    step("read in a rolled-back transaction", after_rollback)

    def commit_twice():
        tx = client.transaction()
        tx.begin()
        tx.put(entity(account, balance=50))
        tx.commit()
        tx.commit()
    step("commit twice", commit_twice)
    step("after it, the balance", lambda: client.get(account)["balance"])

    def inserted():
        with client.transaction() as tx:
            e = entity(client.key("Account"), balance=1)
            tx.put(e)
        return {"complete": e.key.id is not None, "read": client.get(e.key)["balance"]}
    step("an incomplete key in a transaction", inserted)


{"entities": entities, "queries": queries, "transactions": transactions}[SECTION]()
print(json.dumps(out, indent=1, ensure_ascii=False, allow_nan=False))
sys.stdout.flush()
os._exit(0)
