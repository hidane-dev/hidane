#!/usr/bin/env python3
"""Which documents does a listener's query target hold when it becomes current, and which
queries does a listener refuse? (#338)

Usage (from the repository root), with a freshly started jar under an English locale:
    python3 -I tools/oracle/listen_queries.py 127.0.0.1:8086 \\
        > crates/hidane/tests/fixtures/listen_queries.json
    python3 -I tools/oracle/listen_queries.py 127.0.0.1:8087 enterprise \\
        > crates/hidane/tests/fixtures/listen_queries_enterprise.json

Takes the dataset and the queries of `crates/hidane/tests/fixtures/queries.json` (recorded by
`tools/oracle/queries.py`), seeds the dataset, and adds each query as the target of a Listen
stream of its own, over WebChannel (the browser SDK's transport, which `webchannel.py`
describes): the documents the target sends before `CURRENT`, or the cause of its `REMOVE`.
The second form records the same against a jar started with `--database-edition enterprise`;
the argument only labels the fixture. `crates/hidane/tests/listen_queries.rs` replays both over
gRPC. Stdlib only.
"""

import http.client
import json
import sys
import time
import urllib.parse
from pathlib import Path

HOST = sys.argv[1]
EDITION = sys.argv[2] if len(sys.argv) > 2 else "standard"
if EDITION not in ("standard", "enterprise"):
    raise SystemExit(f"the edition is standard or enterprise, not {EDITION}")
QUERIES = Path(__file__).resolve().parents[2] / "crates/hidane/tests/fixtures/queries.json"
PROJECT = f"listen-{time.strftime('%H%M%S')}"
DB = f"projects/{PROJECT}/databases/(default)"
DOCUMENTS = f"{DB}/documents"
OWNER = {"Authorization": "Bearer owner"}


def fill(value):
    return json.loads(json.dumps(value).replace("{documents}", DOCUMENTS).replace(
        "{database}", DB))


def commit(writes):
    c = http.client.HTTPConnection(*HOST.split(":"), timeout=30)
    c.request("POST", f"/v1/{DOCUMENTS}:commit", body=json.dumps({"writes": writes}),
              headers={**OWNER, "Content-Type": "application/json"})
    r = c.getresponse()
    assert r.status == 200, r.read()
    r.read()


class Frames:
    """`<length>\\n<JSON>` frames of a streaming WebChannel response (lengths in UTF-16 units)."""

    def __init__(self, response):
        self.response, self.buffer = response, ""

    def next(self, deadline):
        while True:
            newline = self.buffer.find("\n")
            if newline >= 0:
                length = int(self.buffer[:newline])
                text, units, i = self.buffer[newline + 1:], 0, 0
                while i < len(text) and units < length:
                    units += 2 if ord(text[i]) > 0xFFFF else 1
                    i += 1
                if units == length:
                    self.buffer = text[i:]
                    return json.loads(text[:i])
            if time.monotonic() > deadline:
                return None
            data = self.response.read1(65536)
            if not data:
                return None
            self.buffer += data.decode("utf-8")


def query(params):
    return urllib.parse.urlencode(params, safe="()")


def listen(target):
    """The documents the target holds when current, in the order sent, or its removal."""
    path = "/google.firestore.v1.Firestore/Listen/channel"
    body = {"headers": "Authorization:Bearer owner\r\n", "count": 1, "ofs": 0,
            "req0___data__": json.dumps({"database": DB, "addTarget": {**target, "targetId": 2}})}
    c = http.client.HTTPConnection(*HOST.split(":"), timeout=30)
    c.request("POST", path + "?" + query({"VER": 8, "database": DB, "RID": 1000, "CVER": 22,
                                           "X-HTTP-Session-Id": "gsessionid", "zx": "x", "t": 1}),
              body=urllib.parse.urlencode(body), headers={"Content-Type": "application/x-www-form-urlencoded"})
    r = c.getresponse()
    sid = Frames(r).next(time.monotonic() + 10)[0][1][1]
    c = http.client.HTTPConnection(*HOST.split(":"), timeout=30)
    c.request("GET", path + "?" + query({"VER": 8, "database": DB, "RID": "rpc", "SID": sid, "AID": 0,
                                          "CI": 0, "TYPE": "xmlhttp", "zx": "x", "t": 1}))
    frames = Frames(c.getresponse())
    documents, outcome, deadline = [], None, time.monotonic() + 10
    while outcome is None:
        frame = frames.next(deadline)
        if frame is None:
            outcome = {"timeout": True}
            break
        for _, messages in frame:
            for m in messages if isinstance(messages, list) else [messages]:
                if not isinstance(m, dict):
                    continue
                if "documentChange" in m:
                    documents.append(m["documentChange"]["document"]["name"][len(DOCUMENTS) + 1:])
                change = m.get("targetChange", {})
                if change.get("targetChangeType") == "CURRENT":
                    outcome = {"documents": documents}
                elif change.get("targetChangeType") == "REMOVE":
                    message = change.get("cause", {}).get("message", "")
                    outcome = {"removed": message.replace(PROJECT, "{project}")}
                if "error" in m:
                    outcome = {"error": m["error"]}
    c = http.client.HTTPConnection(*HOST.split(":"), timeout=30)
    c.request("POST", path + "?" + query({"VER": 8, "database": DB, "SID": sid, "RID": 1001, "TYPE": "terminate", "zx": "x"}),
              headers={"Content-Type": "text/plain;charset=UTF-8"})
    c.getresponse().read()
    return outcome


def main():
    recorded = json.loads(QUERIES.read_text())
    for doc in recorded["dataset"]:
        commit([{"update": {"name": f"{DOCUMENTS}/{doc['name']}", "fields": fill(doc["fields"])}}])
    cases = []
    for case in recorded["cases"]:
        parent = DOCUMENTS + ("/" + case["parent"] if case["parent"] else "")
        target = {"query": {"parent": parent, "structuredQuery": fill(case["query"])}}
        outcome = listen(target)
        cases.append({"name": case["name"], "parent": case["parent"], "query": case["query"],
                      "outcome": outcome})
    json.dump({
        "oracle": "cloud-firestore-emulator v1.22.0 (sha256 9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)",
        "generated_by": "tools/oracle/listen_queries.py",
        **({"edition": EDITION} if EDITION != "standard" else {}),
        "dataset": recorded["dataset"],
        "cases": cases,
    }, sys.stdout, ensure_ascii=False, indent=1)
    print()


main()
