#!/usr/bin/env python3
"""What do the official emulator's answers in Datastore mode tell of what came before? (#81)

Usage (from the repository root), with a jar freshly started with `--database-mode
datastore-mode` under an English locale:
    python3 -I tools/oracle/datastore_sequences.py 127.0.0.1:8086 \\
        > crates/hidane/tests/fixtures/datastore_sequences.json

Each sequence of `SEQUENCES` runs in a project of its own, its requests in turn over REST:
the kinds and namespaces `__kind__` and `__namespace__` list after keys are read and written,
there or not (`TOUCHED`); a lookup's property mask (`LOOKUP_MASKS`) and a write's
(`COMMIT_MASKS`), nested paths included; reads in a read-only transaction and at a read time
(`snapshots`); the versions and times commits and queries answer (`versions`); and a write
whose client gives up waiting for a lock (`given_up`).

What the recording shows. Every key a lookup or a commit names is listed, even one that is not
there or a mutation that fails; queries, `allocateIds` and `reserveIds` list nothing. A mask
path is dotted, with backticks for names that hold dots; a lookup keeps the values it names
inside entity values (not inside arrays) and leaves out an entity value left empty; a write
sets each path it names to the written value, or removes it, and keeps the rest of the stored
entity. A read-only transaction reads the entities as they were when it began, or at the read
time it names; a read time reads them as they were then, back to the data's beginning (before
it, nothing is there). A non-transactional commit applies each mutation at a time of its own,
and its commit time is the last one's; a transaction's mutations share one. The deletion of a
key that is not there answers the version it was deleted at, or, if it never was there, the
time it was read missing (before the commit's). A query's snapshot version is its first
result's version or, with no results, its read time in seconds. A write that waits for a lock
goes on once its client gives up, and is applied when the lock is released.

Times and versions are labelled `<t1>`, `<t2>`, ... in the order they first appear in the
sequence's answers (the same time or version, the same label), a version in seconds
`<seconds>`; transaction IDs and cursors are masked. A request names an earlier answer's time
as `{time:<step>:<path>}` or `{time:<step>:<path>:<microseconds>}` (moved by that many), and a
transaction as `{tx}` (the last one begun) or `{tx:<name>}` (the one a step begun `as` that
name). `crates/hidane/tests/datastore.rs` replays it. Stdlib only.
"""

import http.client
import json
import re
import socket
import sys
import time
from datetime import datetime, timezone

HOST = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1:8086"
RUN = time.strftime("%H%M%S")
JSON = {"Content-Type": "application/json"}

I = lambda n: {"integerValue": str(n)}
E = lambda **p: {"entityValue": {"properties": p}}
ARR = lambda *v: {"arrayValue": {"values": list(v)} if v else {}}


def key(kind, name="x", ns=None):
    out = {"path": [{"kind": kind, "name": name}]}
    if ns is not None:
        out["partitionId"] = {"namespaceId": ns}
    return out


def step(verb, body, **options):
    return dict({"verb": verb, "body": body}, **options)


def nontx(*mutations):
    return step("commit", {"mode": "NON_TRANSACTIONAL", "mutations": list(mutations)})


def tx(*mutations, name="tx"):
    return step("commit", {"mode": "TRANSACTIONAL", "transaction": f"{{tx:{name}}}", "mutations": list(mutations)})


def begin(name="tx", **options):
    return step("beginTransaction", {"transactionOptions": options} if options else {}, **{"as": name})


def lookup(*keys, **more):
    return step("lookup", dict({"keys": list(keys)}, **more))


def query(q, **more):
    return step("runQuery", dict({"query": q}, **more))


def count(q, **more):
    return step("runAggregationQuery", dict({"aggregationQuery": {"nestedQuery": q, "aggregations": [{"count": {}, "alias": "n"}]}}, **more))


def kind(k):
    return {"kind": [{"name": k}]}


KINDS = query(kind("__kind__"))
NAMESPACES = query(kind("__namespace__"))
SEQUENCES = []

# --- the kinds and namespaces listed after keys are read and written -------------------------
TOUCHED = [
    ("a lookup of a key not there", [lookup({"path": [{"kind": "Task", "id": "9223372036854775807"}]})]),
    ("a query", [query(kind("Q1"))]),
    ("an aggregation", [count(kind("G1"))]),
    ("the deletion of a key not there", [nontx({"delete": key("D1")})]),
    ("allocateIds", [step("allocateIds", {"keys": [{"path": [{"kind": "A1"}]}]})]),
    ("reserveIds", [step("reserveIds", {"keys": [{"path": [{"kind": "R1", "id": "5"}]}]})]),
    ("a lookup of a child key", [lookup({"path": [{"kind": "P1", "name": "p"}, {"kind": "C1", "name": "c"}]})]),
    ("a lookup in a namespace", [lookup(key("L1", ns="l"))]),
    ("an insert then a delete, in one commit", [nontx({"insert": {"key": key("X1")}}, {"delete": key("X1")})]),
    ("an insert, later a delete", [nontx({"insert": {"key": key("X2")}}), nontx({"delete": key("X2")})]),
    ("an update of a key not there", [nontx({"update": {"key": key("F1")}})]),
    ("an insert of a key there", [nontx({"upsert": {"key": key("E1")}}),
                                  nontx({"insert": {"key": key("E2")}}, {"insert": {"key": key("E1")}})]),
    ("a lookup in a transaction", [begin(), lookup(key("T1"), readOptions={"transaction": "{tx}"})]),
    ("a delete in a transaction", [begin(), tx({"delete": key("T2")})]),
    ("a lookup with a mask", [lookup(key("M1"), propertyMask={"paths": ["a"]})]),
    ("a lookup refused", [lookup(key("B1"), readOptions={"readConsistency": "READ_CONSISTENCY_UNSPECIFIED"})]),
    ("a lookup of two keys", [lookup(key("W1"), key("W2", ns="w"))]),
    ("a lookup of a reserved kind", [lookup(key("__x__"))]),
    ("a transaction: an upsert, then an update of a key not there",
     [begin(), tx({"upsert": {"key": key("A")}}, {"update": {"key": key("B")}})]),
    ("a transaction: an update of a key not there, then an upsert",
     [begin(), tx({"update": {"key": key("B")}}, {"upsert": {"key": key("A")}})]),
    ("an update of a key not there, then an upsert",
     [nontx({"update": {"key": key("B")}}, {"upsert": {"key": key("A")}})]),
    ("an update with base version 0", [nontx({"update": {"key": key("V")}, "baseVersion": "0"})]),
    ("an upsert too large", [nontx({"upsert": {"key": key("S"), "properties": {
        "s": {"stringValue": "x" * 1048500, "excludeFromIndexes": True}}}})]),
    ("a lookup in a reserved namespace", [lookup(key("R", ns="__r__"))]),
    ("an upsert of an incomplete key", [nontx({"upsert": {"key": {"path": [{"kind": "I1"}]}}})]),
    ("an insert of an incomplete key in a transaction", [begin(), tx({"insert": {"key": {"path": [{"kind": "I2"}]}}})]),
]
for name, steps in TOUCHED:
    SEQUENCES.append((f"listed: {name}", steps + [KINDS, NAMESPACES]))

# --- property masks -----------------------------------------------------------------------
STORED = {"a": I(1), "a.b": I(2), "e": E(x=I(3), y=E(z=I(4)), w=ARR(E(q=I(5)), I(6))),
          "empty": E(), "arr": ARR(E(x=I(7)), E(y=I(8))), "b": I(9), "`c`": I(10)}
LOOKUP_MASKS = [["e.x"], ["e"], ["e.y.z"], ["e.y"], ["e.w.q"], ["e.w"], ["empty.x"], ["empty"], ["arr.x"], ["arr"],
                ["a.b"], ["`a.b`"], ["a"], ["a", "a.b"], ["e.x", "e.y.z"], ["e.x", "e"], ["e", "e.x"], ["nope"],
                ["e.nope"], ["a.nope"], ["`e`.x"], ["`e`.`x`"], ["```c```"], ["e.y.z", "e.y"], ["arr.x", "arr.y"],
                ["b.c"], [], ["e.x", "a.b", "nope"]]
SEQUENCES.append(("a lookup's property masks", [nontx({"upsert": {"key": key("K", "m"), "properties": STORED}})] + [
    lookup(key("K", "m"), propertyMask={"paths": paths}) for paths in LOOKUP_MASKS]))

BASE = {"a": I(1), "a.b": I(2), "e": E(x=I(3), y=E(z=I(4)), v=I(40)), "keep": I(5)}
NEW = {"a": I(11), "a.b": I(12), "e": E(x=I(13), y=E(z=I(14), n=I(15)), u=I(16)), "f": E(g=I(17)), "new": I(18)}
COMMIT_MASKS = [["a"], ["a.b"], ["`a.b`"], ["e.x"], ["e"], ["e.y.z"], ["e.y"], ["e.u"], ["e.v"], ["f.g"], ["f"],
                ["f.h"], ["new"], ["gone"], ["keep"], ["e.x", "e.v"], ["e.y.n"], ["a.c"], ["e.x.deeper"], [],
                ["new.x"], ["keep.x"]]
for paths in COMMIT_MASKS:
    for op in ("upsert", "update"):
        SEQUENCES.append((f"an {op} with the mask {json.dumps(paths)}", [
            nontx({"upsert": {"key": key("K", "m"), "properties": BASE}}),
            nontx({op: {"key": key("K", "m"), "properties": NEW}, "propertyMask": {"paths": paths}}),
            lookup(key("K", "m"))]))
SEQUENCES.append(("an insert with a mask", [
    nontx({"insert": {"key": key("K", "m"), "properties": NEW}, "propertyMask": {"paths": ["e.x", "new"]}}),
    lookup(key("K", "m"))]))
SEQUENCES.append(("a mask in a transaction, after a write of the same key", [
    begin(), tx({"upsert": {"key": key("K", "m"), "properties": BASE}},
                {"upsert": {"key": key("K", "m"), "properties": NEW}, "propertyMask": {"paths": ["e.y", "f"]}}),
    lookup(key("K", "m"))]))


# --- reads in a read-only transaction and at a read time --------------------------------------
def snapshots():
    ups = lambda kind_, name, v: {"upsert": {"key": key(kind_, name), "properties": {"v": I(v)}}}
    keys = [key("A", n) for n in ("a", "b", "c", "gone", "never")]
    steps = [nontx(ups("A", "a", 1), ups("A", "b", 1), ups("A", "gone", 1)),  # 0
             nontx({"delete": key("A", "gone")}),  # 1
             begin("ro", readOnly={}),  # 2
             lookup(*keys, readOptions={"transaction": "{tx:ro}"}),  # 3: when it began
             nontx(ups("A", "a", 2), {"delete": key("A", "b")}, ups("A", "c", 1), ups("B", "x", 1))]  # 4
    ro = {"readOptions": {"transaction": "{tx:ro}"}}
    steps += [lookup(*keys, **ro), query(kind("A"), **ro), query(kind("B"), **ro),
              query(dict(kind("A"), projection=[{"property": {"name": "__key__"}}]), **ro), count(kind("A"), **ro),
              query(kind("__kind__"), **ro), query(kind("Z"), **ro),
              step("runQuery", {"gqlQuery": {"queryString": "SELECT * FROM A WHERE v = 1"}, **ro}),
              lookup(*keys)]
    times = {"before the first write": "{time:0:mutationResults.0.version:-1}",
             "at the first write": "{time:0:mutationResults.0.version}",
             "at the second write of a commit": "{time:0:mutationResults.1.version}",
             "at the deletion": "{time:1:commitTime}",
             "a microsecond after the deletion": "{time:1:commitTime:1}",
             "when the transaction began": "{time:3:readTime}",
             "at the last write's first mutation": "{time:4:mutationResults.0.version}",
             "at the last write": "{time:4:commitTime}"}
    for at in times.values():
        steps += [lookup(*keys, readOptions={"readTime": at}),
                  query(kind("A"), readOptions={"readTime": at}),
                  count(kind("A"), readOptions={"readTime": at})]
    steps += [begin("at", readOnly={"readTime": "{time:1:commitTime}"}),
              lookup(*keys, readOptions={"transaction": "{tx:at}"}),
              query(kind("A"), readOptions={"transaction": "{tx:at}"}),
              lookup(*keys, readOptions={"newTransaction": {"readOnly": {}}}),
              query(kind("A"), readOptions={"newTransaction": {"readOnly": {}}}),
              lookup(*keys, readOptions={"newTransaction": {"readOnly": {"readTime": "{time:0:commitTime}"}}}),
              query(kind("A"), readOptions={"newTransaction": {"readOnly": {"readTime": "{time:0:commitTime}"}}}),
              nontx(ups("A", "a", 3)),
              lookup(keys[0], **ro),
              step("commit", {"mode": "TRANSACTIONAL", "transaction": "{tx:ro}", "mutations": []}),
              lookup(keys[0], **ro)]
    return steps


SEQUENCES.append(("reads in a read-only transaction and at read times", snapshots()))


# --- the versions and times commits and queries answer --------------------------------------
def versions():
    ups = lambda name, v=1: {"upsert": {"key": key("A", name), "properties": {"v": I(v)}}}
    return [
        begin(), tx(ups("a"), ups("b"), {"delete": key("A", "never")}, ups("a", 2)),
        nontx(ups("c"), ups("d"), {"delete": key("A", "never")}),
        nontx({"delete": key("A", "never")}),
        nontx({"delete": key("A", "c")}),
        nontx({"delete": key("A", "c")}),
        begin(), tx({"delete": key("A", "c")}),
        begin(), tx({"delete": key("A", "never3")}, ups("f"), {"delete": key("A", "never4")}),
        begin(), lookup(key("A", "never2"), readOptions={"transaction": "{tx}"}), tx({"delete": key("A", "never2")}),
        lookup(key("A", "c"), key("A", "never")),
        nontx(ups("e"), {"insert": {"key": key("A", "a")}}),
        lookup(key("A", "e")),
        nontx(),
        begin(), tx(),
        nontx({"insert": {"key": {"path": [{"kind": "A"}]}}}, {"insert": {"key": {"path": [{"kind": "A"}]}}}),
        nontx(ups("a", 2)),
        query(kind("A")),
        query(dict(kind("A"), order=[{"property": {"name": "v"}, "direction": "DESCENDING"}])),
        query(dict(kind("A"), limit=1)),
        query(dict(kind("A"), offset=2)),
        query(dict(kind("A"), offset=20)),
        query(dict(kind("A"), projection=[{"property": {"name": "__key__"}}])),
        query(dict(kind("A"), projection=[{"property": {"name": "v"}}])),
        query(kind("Z")),
        query(kind("__kind__")),
        query(kind("__namespace__")),
        query(dict(kind("__kind__"), filter={"propertyFilter": {"property": {"name": "__key__"}, "op": "EQUAL", "value": {
            "keyValue": {"path": [{"kind": "__kind__", "name": "none"}]}}}})),
        count(kind("A")),
    ]


SEQUENCES.append(("the versions and times of commits and queries", versions()))

# --- a write whose client gives up waiting for a lock ----------------------------------------
SEQUENCES.append(("a write goes on once its client gives up waiting", [
    begin("reader"), lookup(key("K", "a"), readOptions={"transaction": "{tx:reader}"}),
    step("commit", {"mode": "NON_TRANSACTIONAL", "mutations": [{"upsert": {"key": key("K", "a"), "properties": {"n": I(1)}}}]},
         gives_up_after=2),
    step("rollback", {"transaction": "{tx:reader}"}),
    dict(lookup(key("K", "a")), after=0.5)]))

LABELLED = re.compile(r'"(createTime|updateTime|readTime|commitTime|version|commitVersion|snapshotVersion)": "([^"]*)"')
CURSOR = re.compile(r'"(cursor|endCursor|skippedCursor|startCursor)": "[^"]*"')


def micros(text):
    """A timestamp's or a version's microseconds."""
    if text.isdigit():
        return int(text)
    whole, _, fraction = text.rstrip("Z").partition(".")
    t = datetime.strptime(whole, "%Y-%m-%dT%H:%M:%S").replace(tzinfo=timezone.utc)
    return int(t.timestamp()) * 1_000_000 + int((fraction + "000000")[:6])


def stamp(m):
    t = datetime.fromtimestamp(m // 1_000_000, timezone.utc)
    return t.strftime("%Y-%m-%dT%H:%M:%S") + f".{m % 1_000_000:06d}Z"


def labelled(text, project, labels):
    def label(m):
        value = m.group(2)
        if m.group(1) in ("version", "commitVersion", "snapshotVersion") and int(value) < 10 ** 12:
            return f'"{m.group(1)}": "<seconds>"'
        at = micros(value)
        if at not in labels:
            labels.append(at)
        return f'"{m.group(1)}": "<t{labels.index(at) + 1}>"'
    text = LABELLED.sub(label, text.replace(project, "{project}"))
    text = re.sub(r'"transaction": "[^"]*"', '"transaction": "<id>"', text)
    return CURSOR.sub(lambda m: f'"{m.group(1)}": "<cursor>"', text)


def at(value, path):
    for part in path.split("."):
        value = value[int(part)] if isinstance(value, list) else value[part]
    return value


def filled(body, answers, transactions):
    text = json.dumps(body)
    text = re.sub(r"\{tx(?::([a-z]+))?\}", lambda m: transactions.get(m.group(1) or "last", ""), text)
    return re.sub(r"\{time:(\d+):([A-Za-z0-9.]+)(?::(-?\d+))?\}",
                  lambda m: stamp(micros(str(at(answers[int(m.group(1))], m.group(2)))) + int(m.group(3) or 0)), text)


CONNECTION = []


def call(project, verb, body, timeout=60):
    """One request, on a connection kept open (a new one when it is to be given up on)."""
    if timeout != 60:
        conn = http.client.HTTPConnection(*HOST.split(":"), timeout=timeout)
        try:
            conn.request("POST", f"/v1/projects/{project}:{verb}", body=body.encode(), headers=JSON)
            r = conn.getresponse()
            return r.status, r.getheader("content-type"), r.read().decode()
        except (socket.timeout, TimeoutError):
            return None, None, None
        finally:
            conn.close()
    if not CONNECTION:
        CONNECTION.append(http.client.HTTPConnection(*HOST.split(":"), timeout=timeout))
    conn = CONNECTION[0]
    conn.request("POST", f"/v1/projects/{project}:{verb}", body=body.encode(), headers=JSON)
    r = conn.getresponse()
    return r.status, r.getheader("content-type"), r.read().decode()


def main():
    recorded = []
    for n, (name, steps) in enumerate(SEQUENCES):
        project = f"seq{RUN}-{n}"
        answers, transactions, labels, out = [], {}, [], []
        for s in steps:
            if "after" in s:
                time.sleep(s["after"])
            status, ctype, text = call(project, s["verb"], filled(s["body"], answers, transactions),
                                       s.get("gives_up_after", 60))
            answer = json.loads(text) if text else {}
            answers.append(answer)
            if s["verb"] == "beginTransaction" and "transaction" in answer:
                transactions[s["as"]] = transactions["last"] = answer["transaction"]
            recorded_step = {k: v for k, v in s.items() if k not in ("verb", "body")}
            recorded_step.update({"path": f"/v1/projects/{{project}}:{s['verb']}", "body": s["body"]})
            recorded_step["outcome"] = ({"status": None} if status is None else
                                        {"status": status, "contentType": ctype,
                                         "body": labelled(text, project, labels)})
            out.append(recorded_step)
        recorded.append({"name": name, "steps": out})
    sys.stdout.write('{\n "oracle": "cloud-firestore-emulator v1.22.0 (sha256 '
                     '9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)",\n'
                     ' "generated_by": "tools/oracle/datastore_sequences.py",\n'
                     ' "mode": "datastore-mode",\n'
                     f' "every_case": {json.dumps({"method": "POST", "headers": JSON})},\n "cases": [\n')
    sys.stdout.write(",\n".join("  " + json.dumps(c, ensure_ascii=False, separators=(",", ":")) for c in recorded))
    sys.stdout.write("\n ]\n}\n")


if __name__ == "__main__":
    main()
