#!/usr/bin/env python3
"""How long does a write wait for another transaction's lock in Datastore mode? (#81)

Usage (from the repository root), with a jar freshly started with `--database-mode
datastore-mode` under an English locale:
    python3 -I tools/oracle/datastore_locks.py 127.0.0.1:8086 \\
        > crates/hidane/tests/fixtures/datastore_locks.json

In each scenario, in a project of its own, a transaction reads what a write then needs: a key
(`lookup`) or every entity of a kind (`runQuery`, `runAggregationQuery`; an aggregation its
aggregations refuse locks nothing). The transaction stays
in use while the write waits (a `lookup` in it every 10 s), so it never expires. The write
comes from another transaction or from none, to what is locked or to something else. The
scenarios run at once; each records its requests, their answers and how long the write took, in
whole seconds. The official emulator gives up on a write in a transaction after 2 s, as in
Firestore Native mode, and on a write outside a transaction after 60 s ("Transaction lock
timeout."). `crates/hidane/tests/datastore.rs` replays it. Stdlib only.
"""

import http.client
import json
import re
import sys
import threading
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from harness import mask  # noqa: E402

HOST = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1:8086"
RUN = time.strftime("%H%M%S")
JSON = {"Content-Type": "application/json"}
VERSION = re.compile(r'"(version|commitVersion|snapshotVersion)": "\d+"')
CURSOR = re.compile(r'"(cursor|endCursor|skippedCursor|startCursor)": "[^"]*"')
KEEP_ALIVE = 10


def key(name):
    return {"path": [{"kind": "K", "name": name}]}


LOOKUP = ("lookup", {"keys": [key("a")], "readOptions": {"transaction": "{reader}"}})
QUERY = ("runQuery", {"query": {"kind": [{"name": "K"}]}, "readOptions": {"transaction": "{reader}"}})
AGGREGATE = ("runAggregationQuery", {"aggregationQuery": {"nestedQuery": {"kind": [{"name": "K"}]},
                                                          "aggregations": [{"count": {}}]},
                                     "readOptions": {"transaction": "{reader}"}})


def refused(nested, *aggregations):
    return ("runAggregationQuery", {"aggregationQuery": {"nestedQuery": nested, "aggregations": list(aggregations)},
                                    "readOptions": {"transaction": "{reader}"}})

# A scenario: what the reader reads, the write (`in` a transaction or `outside` one) and its key.
SCENARIOS = [
    ("a write in another transaction to a key read", LOOKUP, "in", "a"),
    ("a write outside a transaction to a key read", LOOKUP, "outside", "a"),
    ("a write outside a transaction to another key", LOOKUP, "outside", "b"),
    ("a write in another transaction to a kind queried", QUERY, "in", "c"),
    ("a write outside a transaction to a kind queried", QUERY, "outside", "c"),
    ("a write outside a transaction to a kind aggregated", AGGREGATE, "outside", "d"),
    # An aggregation its aggregations refuse locks nothing; one its query refuses, the kind.
    ("a write in another transaction to a kind an aggregation without aggregations read",
     refused({"kind": [{"name": "K"}]}), "in", "e"),
    ("a write in another transaction to a kind an aggregation summing __key__ read",
     refused({"kind": [{"name": "K"}]}, {"sum": {"property": {"name": "__key__"}}}), "in", "f"),
    ("a write in another transaction to a kind an aggregation with a negative limit read",
     refused({"kind": [{"name": "K"}], "limit": -1}, {"count": {}}), "in", "g"),
]


def masked(text, project):
    text = VERSION.sub(lambda m: f'"{m.group(1)}": "<version>"', mask(text, project))
    return CURSOR.sub(lambda m: f'"{m.group(1)}": "<cursor>"', text)


def call(project, verb, body):
    conn = http.client.HTTPConnection(*HOST.split(":"), timeout=300)
    conn.request("POST", f"/v1/projects/{project}:{verb}", body=json.dumps(body).encode(), headers=JSON)
    r = conn.getresponse()
    return r.status, r.getheader("content-type"), r.read().decode()


def run(n, name, read, how, target, out):
    project = f"locks{RUN}-{n}"
    steps = []

    def step(verb, body, values):
        sent = json.loads(json.dumps(body).replace("{reader}", values.get("reader", "")).replace(
            "{writer}", values.get("writer", "")))
        status, ctype, text = call(project, verb, sent)
        steps.append({"path": f"/v1/projects/{{project}}:{verb}", "body": body,
                      "outcome": {"status": status, "contentType": ctype, "body": masked(text, project)}})
        return text

    values = {}
    step("commit", {"mode": "NON_TRANSACTIONAL", "mutations": [{"upsert": {"key": key("a")}}]}, values)
    values["reader"] = json.loads(step("beginTransaction", {}, values))["transaction"]
    step(*read, values)
    if how == "in":
        values["writer"] = json.loads(step("beginTransaction", {}, values))["transaction"]
        write = ("commit", {"mode": "TRANSACTIONAL", "transaction": "{writer}",
                            "mutations": [{"upsert": {"key": key(target), "properties": {"n": {"integerValue": "1"}}}}]})
    else:
        write = ("commit", {"mode": "NON_TRANSACTIONAL",
                            "mutations": [{"upsert": {"key": key(target), "properties": {"n": {"integerValue": "1"}}}}]})
    done = threading.Event()

    def keep_alive():
        while not done.wait(KEEP_ALIVE):
            call(project, "lookup", {"keys": [key("z")], "readOptions": {"transaction": values["reader"]}})

    keeper = threading.Thread(target=keep_alive)
    keeper.start()
    start = time.monotonic()
    step(*write, values)
    waited = round(time.monotonic() - start)
    done.set()
    keeper.join()
    out[n] = {"name": name, "steps": steps, "waited": waited}


def main():
    out = [None] * len(SCENARIOS)
    threads = [threading.Thread(target=run, args=(n, *s, out)) for n, s in enumerate(SCENARIOS)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    json.dump({
        "oracle": "cloud-firestore-emulator v1.22.0 (sha256 9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)",
        "generated_by": "tools/oracle/datastore_locks.py",
        "mode": "datastore-mode",
        "every_case": {"method": "POST", "headers": JSON, "compare": "exact"},
        "keep_alive_seconds": KEEP_ALIVE,
        "cases": out,
    }, sys.stdout, ensure_ascii=False, indent=1)
    print()


if __name__ == "__main__":
    main()
