"""Runs one section of steps through google-cloud-firestore (the Python server SDK) and prints a
transcript without times or generated IDs, so the transcripts of two emulators can be diffed.
FIRESTORE_EMULATOR_HOST selects the emulator; start it fresh for each section.

Usage: python3 transcript.py documents|queries|transactions|listen > transcript.json

The sections and their step names are the same in tools/oracle/sdk_go/ and
tools/oracle/sdk_java/; tools/oracle/README.md says how to run them.
"""

import base64
import datetime
import json
import math
import os
import re
import sys
import threading
import time

import grpc
from google.api_core.exceptions import GoogleAPICallError
from google.cloud import firestore
from google.cloud.firestore_v1 import And, FieldFilter, GeoPoint, Increment, Maximum, Minimum, Or
from google.cloud.firestore_v1.base_vector_query import DistanceMeasure
from google.cloud.firestore_v1.document import DocumentReference
from google.cloud.firestore_v1.field_path import FieldPath
from google.cloud.firestore_v1.query_profile import ExplainOptions, QueryExplainError
from google.cloud.firestore_v1.vector import Vector

SECTION = sys.argv[1] if len(sys.argv) == 2 else None
if SECTION not in ("documents", "queries", "transactions", "listen"):
    sys.exit(__doc__.split("\n\n")[1])
db = firestore.Client(project=f"demo-python-{SECTION}")
out = []


def mask(message):
    return re.sub(r"\d{10,}", "<n>", message)


def error_of(e):
    """A server error as its gRPC code and message; anything else as a client error."""
    if isinstance(e, GoogleAPICallError) and e.grpc_status_code is not None:
        return {"code": e.grpc_status_code.name, "message": mask(e.message)}
    return {"client": mask(str(e))}


def norm(v, times=True):
    """A Firestore value as JSON: NaN and infinities as strings, timestamps as RFC 3339 (or
    "timestamp" when `times` is false), the other non-JSON types as tagged objects."""
    if v is None or isinstance(v, (bool, str, int)):
        return v
    if isinstance(v, float):
        if math.isnan(v):
            return "NaN"
        if math.isinf(v):
            return "Infinity" if v > 0 else "-Infinity"
        return v
    if isinstance(v, datetime.datetime):
        return v.rfc3339() if times and hasattr(v, "rfc3339") else "timestamp"
    if isinstance(v, bytes):
        return {"bytes": base64.b64encode(v).decode()}
    if isinstance(v, GeoPoint):
        return {"geopoint": [v.latitude, v.longitude]}
    if isinstance(v, DocumentReference):
        return {"reference": v.path}
    if isinstance(v, Vector):
        return {"vector": [norm(x) for x in v]}
    if isinstance(v, dict):
        return {k: norm(v[k], times) for k in sorted(v)}
    if isinstance(v, (list, tuple)):
        return [norm(x, times) for x in v]
    return {"unknown": repr(v)}


def step(name, fn):
    try:
        out.append({"name": name, "ok": norm(fn())})
    except Exception as e:  # noqa: BLE001 - every failure is part of the transcript
        out.append({"name": name, "error": error_of(e)})


def data(ref):
    return ref.get().to_dict()


def ts(t):
    return t.timestamp_pb() if t is not None else None


def ids(snaps):
    return [s.reference.path for s in snaps]


# --- documents ------------------------------------------------------------------------------

def documents():
    users = db.collection("users")
    alice = users.document("alice")
    first = {}

    def set_alice():
        first["w"] = alice.set({"name": "Alice", "age": 30, "tags": ["a", "b"], "nested": {"x": 1}})
        return "ok"
    step("set alice", set_alice)

    def get_alice():
        s = alice.get()
        return {"exists": s.exists, "data": s.to_dict(),
                "createEqualsUpdate": ts(s.create_time) == ts(s.update_time),
                "updateEqualsWrite": ts(s.update_time) == ts(first["w"].update_time)}
    step("get alice", get_alice)
    step("update dotted", lambda: (alice.update({"age": 31, "nested.y": 2}), data(alice))[1])
    step("set merge", lambda: (alice.set({"city": "Tokyo", "nested": {"z": 3}}, merge=True),
                               data(alice))[1])
    step("set mergeFields", lambda: (alice.set({"city": "Osaka", "age": 99}, merge=["city"]),
                                     data(alice))[1])
    step("get with a field mask",
         lambda: alice.get(field_paths=["name", "nested.x"]).to_dict())
    step("create existing", lambda: alice.create({"x": 1}))
    step("update missing", lambda: users.document("nobody").update({"x": 1}))
    stale = db.write_option(last_update_time=first["w"].update_time)
    step("update with a stale lastUpdateTime", lambda: alice.update({"age": 32}, option=stale))
    step("delete with a stale lastUpdateTime", lambda: alice.delete(option=stale))

    def update_current():
        current = db.write_option(last_update_time=alice.get().update_time)
        alice.update({"age": 32}, option=current)
        return data(alice)
    step("update with the current updateTime", update_current)
    step("delete a missing document", lambda: (users.document("nobody").delete(), "ok")[1])
    step("delete a missing document that must exist",
         lambda: users.document("nobody").delete(option=db.write_option(exists=True)))

    def identical_set():
        before = alice.get()
        w = alice.set(before.to_dict())
        after = alice.get()
        return {"writeTimeIsOldUpdateTime": ts(w.update_time) == ts(before.update_time),
                "updateTimeUnchanged": ts(after.update_time) == ts(before.update_time)}
    step("identical set keeps updateTime", identical_set)
    step("getAll", lambda: [{"id": s.id, "exists": s.exists}
                            for s in db.get_all([users.document("nobody"), alice])])

    def batch():
        b = db.batch()
        b.set(users.document("bob"), {"n": 1})
        b.set(users.document("carol"), {"n": 2})
        b.delete(alice)
        b.set(db.document("users/bob/posts/p1"), {"title": "hi"})
        b.set(db.document("ghosts/g/items/i"), {"n": 3})
        return len(b.commit())
    step("batch", batch)

    def failing_batch():
        b = db.batch()
        b.set(users.document("eve"), {"n": 1})
        b.update(users.document("nobody"), {"x": 1})
        try:
            b.commit()
            commit = "ok"
        except Exception as e:  # noqa: BLE001
            commit = error_of(e)
        return {"commit": commit, "eveExists": users.document("eve").get().exists}
    step("a failing batch writes nothing", failing_batch)
    step("listDocuments users", lambda: [d.id for d in users.list_documents()])
    step("listDocuments ghosts (missing parent)",
         lambda: [d.id for d in db.collection("ghosts").list_documents()])
    step("listCollections root", lambda: [c.id for c in db.collections()])
    step("listCollections bob", lambda: [c.id for c in users.document("bob").collections()])

    def bulk_writer():
        results = {}
        bw = db.bulk_writer()
        bw.on_write_result(lambda ref, result, _: results.__setitem__(ref.id, "ok"))

        def on_error(failure, _):
            results[failure.operation.reference.id] = f"error {code_name(failure.code)}"
            return False
        bw.on_write_error(on_error)
        bw.create(users.document("dave"), {"n": 1})
        bw.create(users.document("bob"), {"n": 9})
        bw.update(users.document("nobody"), {"n": 1})
        bw.delete(users.document("carol"))
        bw.close()
        return [results.get(k) for k in ("dave", "bob", "nobody", "carol")]
    step("bulkWriter", bulk_writer)
    step("after bulkWriter", lambda: sorted(d.id for d in users.list_documents()))

    def add():
        _, ref = db.collection("auto").add({"n": 1})
        return {"idLength": len(ref.id), "data": data(ref)}
    step("add with a generated ID", add)

    def server_timestamp():
        ref = users.document("ts")
        w = ref.set({"n": 1, "at": firestore.SERVER_TIMESTAMP,
                     "nested": {"at": firestore.SERVER_TIMESTAMP}})
        s = ref.get()
        at = s.get("at")
        return {"isTimestamp": isinstance(at, datetime.datetime),
                "millisecondPrecision": ts(at).nanos % 1_000_000 == 0,
                "notAfterWrite": (ts(at).seconds, ts(at).nanos)
                <= (ts(w.update_time).seconds, ts(w.update_time).nanos),
                "sameInNested": ts(at) == ts(s.get("nested.at"))}
    step("serverTimestamp", server_timestamp)

    def increment():
        ref = users.document("counter")
        ref.set({"n": 1, "f": 1.5})
        ref.update({"n": Increment(2), "f": Increment(1), "fresh": Increment(5)})
        return data(ref)
    step("increment", increment)

    def maximum_minimum():
        ref = users.document("bounds")
        ref.set({"a": 5, "b": 5, "c": 1.5})
        ref.update({"a": Maximum(7), "b": Minimum(3), "c": Maximum(1), "fresh": Minimum(2)})
        return data(ref)
    step("maximum and minimum", maximum_minimum)

    def array_union_remove():
        ref = users.document("tags")
        ref.set({"tags": ["a", 1]})
        ref.update({"tags": firestore.ArrayUnion(["b", "a", 1.0])})
        after_union = ref.get().get("tags")
        ref.update({"tags": firestore.ArrayRemove(["a", 1])})
        return {"afterUnion": after_union, "afterRemove": ref.get().get("tags")}
    step("arrayUnion and arrayRemove", array_union_remove)

    def delete_field():
        ref = users.document("del")
        ref.set({"keep": 1, "drop": 2, "m": {"x": 1, "y": 2}})
        ref.update({"drop": firestore.DELETE_FIELD, "m.x": firestore.DELETE_FIELD})
        return data(ref)
    step("delete a field", delete_field)

    def merge_with_transforms():
        ref = users.document("merge")
        ref.set({"a": 1, "list": ["x"]})
        ref.set({"b": Increment(3), "list": firestore.ArrayUnion(["y"])}, merge=True)
        return data(ref)
    step("merge with transforms", merge_with_transforms)

    def quoted_field_path():
        ref = users.document("paths")
        ref.set({"a.b": {"c": 1}, "plain": 1})
        ref.update({FieldPath("a.b", "c").to_api_repr(): 2, "`x y`": 3})
        return data(ref)
    step("update quoted field paths", quoted_field_path)

    def value_types():
        ref = users.document("types")
        ref.set({
            "null": None, "true": True, "int": 9223372036854775807, "negative": -42,
            "double": 0.1, "nan": float("nan"), "inf": float("inf"), "-inf": float("-inf"),
            "string": "h\u00e9llo \u2603 \U0001f600", "bytes": b"\x00\x01\x02\xff",
            "timestamp": datetime.datetime(2024, 1, 2, 3, 4, 5, 123456,
                                           tzinfo=datetime.timezone.utc),
            "geopoint": GeoPoint(35.68, 139.76), "reference": db.document("users/bob"),
            "array": [1, "a", None, {"m": True}], "map": {"a": {"b": {"c": 1}}},
            "emptyMap": {}, "emptyArray": [], "vector": Vector([1.0, 2.5]),
        })
        return data(ref)
    step("value types", value_types)


def code_name(code):
    for status in grpc.StatusCode:
        if status.value[0] == code:
            return status.name
    return str(code)


# --- queries --------------------------------------------------------------------------------

def queries():
    cities = db.collection("cities")
    rows = {
        "SF": {"name": "San Francisco", "state": "CA", "country": "USA", "capital": False,
               "population": 860000, "regions": ["west_coast", "norcal"], "loc": {"lat": 37.7}},
        "LA": {"name": "Los Angeles", "state": "CA", "country": "USA", "capital": False,
               "population": 3900000, "regions": ["west_coast", "socal"], "loc": {"lat": 34.0}},
        "DC": {"name": "Washington, D.C.", "state": None, "country": "USA", "capital": True,
               "population": 680000, "regions": ["east_coast"]},
        "TOK": {"name": "Tokyo", "state": None, "country": "Japan", "capital": True,
                "population": 9000000, "regions": ["kanto", "honshu"], "loc": {"lat": 35.7}},
        "BJ": {"name": "Beijing", "state": None, "country": "China", "capital": True,
               "population": 21500000, "regions": ["jingjinji", "hebei"]},
        "NAN": {"name": "Nowhere", "country": "None", "population": float("nan")},
    }

    def seed():
        b = db.batch()
        for key, row in rows.items():
            b.set(cities.document(key), row)
        b.set(db.document("cities/SF/landmarks/golden-gate"),
              {"name": "Golden Gate Bridge", "type": "bridge"})
        b.set(db.document("cities/SF/landmarks/legion"), {"name": "Legion of Honor", "type": "museum"})
        b.set(db.document("cities/TOK/landmarks/national"), {"name": "National Museum", "type": "museum"})
        b.set(db.document("cities/BJ/landmarks/forbidden"), {"name": "Forbidden City", "type": "museum"})
        orders = db.collection("orders")
        for key, row in {"a": {"total": 10, "qty": 1, "status": "paid"},
                         "b": {"total": 2.5, "qty": 3, "status": "paid"},
                         "c": {"total": 7, "qty": 2, "status": "open"},
                         "d": {"total": "n/a", "qty": 1, "status": "open"},
                         "e": {"qty": 5, "status": "void"}}.items():
            b.set(orders.document(key), row)
        b.set(db.document("orders/a/lines/1"), {"qty": 4})
        b.set(db.document("orders/b/lines/1"), {"qty": 6})
        b.set(db.document("big/x"), {"n": 9007199254740991})
        b.set(db.document("big/y"), {"n": 9007199254740991})
        for key, v in {"a": [1.0, 0.0], "b": [0.0, 1.0], "c": [1.0, 1.0], "d": [3.0, 4.0]}.items():
            b.set(db.document(f"vectors/{key}"), {"v": Vector(v), "k": key})
        b.commit()
        return "ok"
    step("seed", seed)

    def where(*args):
        return lambda: ids(cities.where(filter=FieldFilter(*args)).get())
    desc = firestore.Query.DESCENDING
    step("all", lambda: ids(cities.get()))
    step("where ==", where("state", "==", "CA"))
    step("where == null (IS_NULL)", where("state", "==", None))
    step("where != null (IS_NOT_NULL)", where("state", "!=", None))
    step("where == NaN (IS_NAN)", where("population", "==", float("nan")))
    step("where <", where("population", "<", 1000000))
    step("where >= orderBy desc", lambda: ids(
        cities.where(filter=FieldFilter("population", ">=", 1000000))
        .order_by("population", direction=desc).get()))
    step("where !=", where("country", "!=", "USA"))
    step("where in", where("country", "in", ["Japan", "China"]))
    step("where not-in", where("country", "not-in", ["USA", "Japan"]))
    step("array-contains", where("regions", "array_contains", "west_coast"))
    step("array-contains-any", where("regions", "array_contains_any", ["west_coast", "kanto"]))
    step("nested field", where("loc.lat", ">", 35))
    step("two inequalities", lambda: ids(
        cities.where(filter=FieldFilter("population", ">", 700000))
        .where(filter=FieldFilter("name", "<", "U")).get()))
    step("or", lambda: ids(cities.where(filter=Or(filters=[
        FieldFilter("capital", "==", True), FieldFilter("state", "==", "CA")])).get()))
    step("or of and", lambda: ids(cities.where(filter=Or(filters=[
        And(filters=[FieldFilter("state", "==", "CA"), FieldFilter("population", ">", 1000000)]),
        FieldFilter("country", "==", "Japan")])).get()))
    step("orderBy limit", lambda: ids(cities.order_by("name").limit(3).get()))
    step("limitToLast", lambda: ids(cities.order_by("name").limit_to_last(2).get()))
    step("offset", lambda: ids(cities.order_by("population").offset(2).limit(2).get()))
    step("startAt / endBefore values", lambda: ids(
        cities.order_by("population").start_at([860000]).end_before([9000000]).get()))
    step("startAfter a snapshot", lambda: ids(
        cities.order_by("country").start_after(cities.document("SF").get()).get()))

    def paging():
        pages = []
        q = cities.order_by("population").limit(2)
        while True:
            snaps = q.get()
            if not snaps:
                return pages
            pages.append(ids(snaps))
            q = cities.order_by("population").start_after(snaps[-1]).limit(2)
    step("paging with startAfter", paging)
    step("select", lambda: [{"id": s.id, "data": s.to_dict()} for s in
                            cities.where(filter=FieldFilter("capital", "==", True))
                            .select(["name", "loc.lat"]).get()])
    step("documentId in", lambda: ids(cities.where(filter=FieldFilter(
        FieldPath.document_id(), "in", [cities.document("TOK"), cities.document("SF")])).get()))
    step("documentId range", lambda: ids(
        cities.where(filter=FieldFilter(FieldPath.document_id(), ">=", cities.document("L")))
        .where(filter=FieldFilter(FieldPath.document_id(), "<", cities.document("T"))).get()))
    step("orderBy documentId desc", lambda: ids(
        cities.order_by(FieldPath.document_id(), direction=desc).get()))
    step("collectionGroup", lambda: ids(db.collection_group("landmarks").where(
        filter=FieldFilter("type", "==", "museum")).get()))
    step("collectionGroup orderBy name",
         lambda: ids(db.collection_group("landmarks").order_by("name").get()))
    step("subcollection", lambda: ids(cities.document("SF").collection("landmarks").get()))
    step("stream", lambda: [s.id for s in
                            cities.where(filter=FieldFilter("capital", "==", True)).stream()])
    step("in with 31 values", where("population", "in", list(range(31))))

    def explain():
        results = cities.where(filter=FieldFilter("capital", "==", True)).get(
            explain_options=ExplainOptions(analyze=True))
        try:
            results.get_explain_metrics()
            metrics = "present"
        except QueryExplainError as e:
            metrics = {"client": str(e)}
        return {"docs": ids(results), "metrics": metrics}
    step("explain", explain)

    @firestore.transactional
    def query_in_transaction(tx):
        snaps = list(tx.get(cities.where(filter=FieldFilter("capital", "==", True))))
        tx.update(cities.document("DC"), {"visited": True})
        return ids(snaps)
    step("query in a transaction", lambda: query_in_transaction(db.transaction()))

    @firestore.transactional
    def read_only_query(tx):
        return ids(tx.get(cities.where(filter=FieldFilter("state", "==", "CA"))))
    step("read-only transaction query", lambda: read_only_query(db.transaction(read_only=True)))
    step("partition query",
         lambda: len(list(db.collection_group("landmarks").get_partitions(2))))

    orders = db.collection("orders")

    def agg(query):
        return {r.alias: r.value for r in query.get()[0]}
    step("count", lambda: agg(orders.count(alias="count")))
    step("count with a filter", lambda: agg(
        orders.where(filter=FieldFilter("status", "==", "open")).count(alias="count")))
    step("count with a limit", lambda: agg(orders.limit(2).count(alias="count")))
    step("sum and average", lambda: agg(
        orders.sum("total", alias="total").avg("total", alias="mean")))
    step("count next to sum", lambda: agg(orders.count(alias="n").sum("qty", alias="qty")))
    step("average of nothing", lambda: agg(
        orders.where(filter=FieldFilter("status", "==", "none"))
        .avg("total", alias="mean").sum("total", alias="total")))
    step("sum beyond 2^53", lambda: agg(db.collection("big").sum("n", alias="n")))
    step("collection group count and sum", lambda: agg(
        db.collection_group("lines").count(alias="n").sum("qty", alias="qty")))

    @firestore.transactional
    def count_in_transaction(tx):
        result = agg_in(orders.where(filter=FieldFilter("status", "==", "paid"))
                        .count(alias="count"), tx)
        tx.set(orders.document("f"), {"total": 1, "qty": 1, "status": "paid"})
        return result

    def agg_in(query, tx):
        return {r.alias: r.value for r in query.get(transaction=tx)[0]}
    step("count in a transaction", lambda: count_in_transaction(db.transaction()))
    step("count after the transaction", lambda: agg(
        orders.where(filter=FieldFilter("status", "==", "paid")).count(alias="count")))

    def nearest(measure):
        return lambda: [[s.id, s.get("distance")] for s in db.collection("vectors").find_nearest(
            "v", Vector([1.0, 0.5]), limit=3, distance_measure=measure,
            distance_result_field="distance").get()]
    step("findNearest euclidean", nearest(DistanceMeasure.EUCLIDEAN))
    step("findNearest dot product", nearest(DistanceMeasure.DOT_PRODUCT))

    def recursive_delete(ref):
        def run():
            deleted = db.recursive_delete(ref)
            return {"deleted": deleted, "cities": ids(cities.get()),
                    "landmarks": ids(db.collection_group("landmarks").get())}
        return run
    step("recursiveDelete a document", recursive_delete(cities.document("SF")))
    step("recursiveDelete a collection", recursive_delete(cities))


# --- transactions ---------------------------------------------------------------------------

def transactions():
    c = db.collection("txn")

    def attempts(fn, tx=None):
        n = [0]

        @firestore.transactional
        def run(t):
            n[0] += 1
            return fn(t)
        result = run(tx if tx is not None else db.transaction())
        return {"attempts": n[0], "result": result}

    def get(t, ref):
        return next(iter(t.get(ref)))

    def create_when_missing(t):
        s = get(t, c.document("a"))
        if not s.exists:
            t.set(c.document("a"), {"n": 0})
        return s.exists
    step("create when missing", lambda: {**attempts(create_when_missing), "data": data(c.document("a"))})

    def read_modify_write(t):
        n = get(t, c.document("a")).get("n")
        t.update(c.document("a"), {"n": n + 1})
        return n
    step("read-modify-write", lambda: {**attempts(read_modify_write), "data": data(c.document("a"))})
    step("getAll", lambda: attempts(lambda t: [
        {"id": s.id, "exists": s.exists} for s in t.get_all([c.document("a"), c.document("missing")])]))

    def write_only(t):
        t.set(c.document("b"), {"n": 1})
        t.delete(c.document("nothing"))
    step("write-only transaction", lambda: {**attempts(write_only), "data": data(c.document("b"))})
    snapshot = {}

    def read_only():
        snapshot["time"] = c.document("a").get().read_time
        return attempts(lambda t: get(t, c.document("a")).to_dict(), db.transaction(read_only=True))
    step("read-only", read_only)

    def read_only_past():
        c.document("a").update({"n": 100})
        return attempts(lambda t: next(iter(t.get(c.document("a"), read_time=snapshot["time"]))).to_dict(),
                        db.transaction(read_only=True))
    step("read-only at a past read time", read_only_past)
    step("read-only cannot write", lambda: attempts(
        lambda t: t.set(c.document("ro"), {"n": 1}), db.transaction(read_only=True)))

    def failing_callback():
        def fail(t):
            get(t, c.document("a"))
            raise RuntimeError("boom")
        try:
            attempts(fail)
            failed = None
        except RuntimeError as e:
            failed = str(e)
        start = time.monotonic()
        c.document("a").set({"n": 1})
        return {"failed": failed, "outsideWriteWaited": time.monotonic() - start > 1}
    step("a failing callback rolls back and releases its locks", failing_callback)

    def create_existing():
        def create(t):
            get(t, c.document("b"))
            t.create(c.document("b"), {"n": 2})
        return attempts(create)
    step("create of an existing document", create_existing)

    def holding(ref, seconds, by):
        """Runs a transaction that reads `ref`, holds it for `seconds` and writes it, in a
        thread; returns the thread, an event set once it has read, and its outcome."""
        reading = threading.Event()
        outcome = {}

        def body(t):
            get(t, ref)
            reading.set()
            time.sleep(seconds)
            t.set(ref, {"by": by})

        def run():
            try:
                outcome["transaction"] = attempts(body)
            except Exception as e:  # noqa: BLE001
                outcome["transaction"] = error_of(e)
        thread = threading.Thread(target=run)
        thread.start()
        reading.wait()
        return thread, outcome

    def outside_write(ref):
        start = time.monotonic()
        try:
            ref.set({"by": "outside"})
            result = "ok"
        except Exception as e:  # noqa: BLE001
            result = error_of(e)
        return result, time.monotonic() - start

    def outside_waits():
        ref = c.document("w")
        thread, outcome = holding(ref, 0.5, "transaction")
        result, waited = outside_write(ref)
        thread.join()
        return {"outside": result, "outsideWaited": waited > 0.3, **outcome, "final": data(ref)}
    step("an outside write waits for the transaction that read the document", outside_waits)

    def outside_gives_up():
        ref = c.document("t")
        thread, outcome = holding(ref, 3, "transaction")
        result, _ = outside_write(ref)
        thread.join()
        return {"outside": result, **outcome, "final": data(ref)}
    step("an outside write gives up after the lock timeout", outside_gives_up)

    def contention():
        # Both read the counter, then commit 300 ms apart: each commit waits for the other's
        # lock, the first wait times out after 2 s and ends the deadlock, one transaction is
        # retried. (Commits at the same instant can both time out, which makes the number of
        # attempts depend on the SDK's backoff.)
        ref = c.document("counter")
        ref.set({"n": 0})
        runs = []

        def run(delay):
            def inc(t):
                n = get(t, ref).get("n")
                time.sleep(delay)
                t.update(ref, {"n": n + 1})
            runs.append(attempts(inc)["attempts"])
        threads = [threading.Thread(target=run, args=(delay,)) for delay in (0.1, 0.4)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join()
        return {"totalAttempts": sum(runs), "final": data(ref)}
    step("two transactions increment the same counter", contention)


# --- listen ---------------------------------------------------------------------------------

def listen():
    c = db.collection("cities")
    log = []
    lock = threading.Lock()
    last = [time.monotonic()]

    def record(listener, entry):
        with lock:
            log.append({"listener": listener, **entry})
            last[0] = time.monotonic()

    def quiet():
        time.sleep(0.15)
        while time.monotonic() - last[0] < 0.3:
            time.sleep(0.05)

    def section(name, fn):
        log.append({"step": name})
        fn()
        quiet()

    def query_listener(name, query):
        def callback(docs, changes, _read_time):
            record(name, {
                "docs": [[d.id, norm(d.to_dict(), times=False)] for d in docs],
                "changes": [[ch.type.name.lower(), ch.document.id, ch.old_index, ch.new_index]
                            for ch in changes]})
        return query.on_snapshot(callback)

    def doc_listener(name, ref):
        def callback(docs, _changes, _read_time):
            snap = docs[0] if docs else None
            record(name, {"exists": bool(snap and snap.exists),
                          "data": norm(snap.to_dict(), times=False) if snap and snap.exists else None})
        return ref.on_snapshot(callback)

    def seed():
        b = db.batch()
        b.set(c.document("SF"), {"pop": 860, "state": "CA"})
        b.set(c.document("LA"), {"pop": 3900, "state": "CA"})
        b.set(c.document("TOK"), {"pop": 9000, "state": None})
        b.commit()
    section("seed", seed)
    watches = []

    def attach():
        watches.append(query_listener("all", c))
        watches.append(query_listener("pop > 1000", c.where(filter=FieldFilter("pop", ">", 1000))))
        watches.append(query_listener("CA by pop", c.where(filter=FieldFilter("state", "==", "CA"))
                                      .order_by("pop")))
        watches.append(query_listener("top 2 by pop", c.order_by(
            "pop", direction=firestore.Query.DESCENDING).limit(2)))
        watches.append(doc_listener("doc SF", c.document("SF")))
        watches.append(doc_listener("doc NY", c.document("NY")))
        watches.append(query_listener("group landmarks", db.collection_group("landmarks")))
    section("attach listeners", attach)
    section("add NY (enters several queries)", lambda: c.document("NY").set({"pop": 8300, "state": "NY"}))
    section("update SF (stays in all)", lambda: c.document("SF").update({"pop": 870}))
    section("update LA below 1000 (leaves pop > 1000, moves in order)",
            lambda: c.document("LA").update({"pop": 999}))
    section("update TOK above all (changes the top 2)", lambda: c.document("TOK").update({"pop": 9500}))
    section("no-op write", lambda: c.document("SF").set({"pop": 870, "state": "CA"}))
    section("write to another collection", lambda: db.document("other/x").set({"n": 1}))

    def batch():
        b = db.batch()
        b.delete(c.document("TOK"))
        b.set(c.document("BJ"), {"pop": 21500, "state": None})
        b.update(c.document("NY"), {"state": "CA"})
        b.commit()
    section("batch: delete TOK, add BJ, change NY state", batch)
    section("add a landmark", lambda: db.document("cities/SF/landmarks/gg").set({"name": "Golden Gate"}))
    section("server timestamp", lambda: c.document("SF").update({"at": firestore.SERVER_TIMESTAMP}))
    section("delete NY", lambda: c.document("NY").delete())

    def detach():
        for w in watches:
            w.unsubscribe()
    section("detach", detach)

    # Listeners fire in no particular order, so within each step the snapshots are grouped by
    # listener (each listener's own order is kept).
    for entry in log:
        if "step" in entry:
            out.append({"step": entry["step"], "listeners": {}})
        else:
            listener = entry.pop("listener")
            out[-1]["listeners"].setdefault(listener, []).append(entry)
    for s in out:
        s["listeners"] = {k: s["listeners"][k] for k in sorted(s["listeners"])}


{"documents": documents, "queries": queries, "transactions": transactions, "listen": listen}[SECTION]()
print(json.dumps(out, indent=1, ensure_ascii=False, allow_nan=False))
sys.stdout.flush()
# Watch threads and the BulkWriter's executor can outlive the client; do not wait for them.
os._exit(0)
