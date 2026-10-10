#!/usr/bin/env python3
"""Ask the official Firestore emulator how it orders values and document names.

Usage: python3 -I tools/oracle/value_order.py HOST:PORT > fixture.json

The emulator must be the official jar (see docs/parity-exceptions.md). Stdlib only.

Values: every case is written to two collections as field `v`. In collection A document names
follow the case index, in collection B they run backwards. Both are read back with
`orderBy v ASCENDING`. Equal values are ordered by document name, so a pair of neighbours that
swaps between A and B is a tie; a pair that keeps its order is strictly ordered.

Paths: one document per path, read back with a collection-group query on `items` ordered by
`__name__`.
"""

import json
import sys
import urllib.error
import urllib.request
from pathlib import Path

BASE = "projects/demo-oracle/databases/(default)/documents"


def call(host, method, path, body=None):
    req = urllib.request.Request(
        f"http://{host}/v1/{path}",
        method=method,
        data=None if body is None else json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(req, timeout=30) as res:
            return json.loads(res.read() or b"null")
    except urllib.error.HTTPError as err:
        raise RuntimeError(err.read().decode(errors="replace")) from None


def write(host, name, fields):
    call(host, "POST", f"{BASE}:commit",
         {"writes": [{"update": {"name": f"{BASE}/{name}", "fields": fields}}]})


def run_query(host, query):
    rows = call(host, "POST", f"{BASE}:runQuery", {"structuredQuery": query})
    return [r["document"]["name"].removeprefix(BASE + "/") for r in rows if "document" in r]


def main():
    host = sys.argv[1]
    cases = json.loads(Path(__file__).with_name("value_order_cases.json").read_text())
    values = cases["values"]
    n = len(values)

    accepted, rejected = [], []
    for i, (label, value) in enumerate(values):
        try:
            write(host, f"orderA/{i:03d}", {"v": value})
            write(host, f"orderB/{n - 1 - i:03d}", {"v": value})
            accepted.append(i)
        except RuntimeError as err:
            rejected.append({"label": label, "value": value, "error": json.loads(str(err))["error"]["message"]})

    order_by_v = {"orderBy": [{"field": {"fieldPath": "v"}, "direction": "ASCENDING"}]}
    a = [int(name.split("/")[1]) for name in run_query(host, {"from": [{"collectionId": "orderA"}], **order_by_v})]
    b = [n - 1 - int(name.split("/")[1]) for name in run_query(host, {"from": [{"collectionId": "orderB"}], **order_by_v})]
    assert sorted(a) == sorted(accepted) == sorted(b), "query did not return every written document"
    position_in_b = {case: pos for pos, case in enumerate(b)}

    groups = [[a[0]]]
    for prev, case in zip(a, a[1:]):
        if position_in_b[case] < position_in_b[prev]:
            groups[-1].append(case)   # swapped with the names: a tie
        else:
            groups.append([case])     # kept its order: strictly greater

    rejected_paths = []
    for path in cases["paths"]:
        try:
            write(host, path, {"p": {"stringValue": path}})
        except RuntimeError as err:
            rejected_paths.append({"path": path, "error": json.loads(str(err))["error"]["message"]})
    collection_group = {"from": [{"collectionId": "items", "allDescendants": True}]}
    path_order = run_query(host, collection_group)

    json.dump({
        "oracle": "cloud-firestore-emulator v1.22.0 (sha256 9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)",
        "generated_by": "tools/oracle/value_order.py",
        "values": {
            "query": "runQuery orderBy v ASCENDING; ties found by reversing document names",
            "groups": [[{"label": values[i][0], "value": values[i][1]} for i in g] for g in groups],
            "rejected": rejected,
        },
        "paths": {
            "query": "collection group `items`, default order (__name__ ASCENDING)",
            "order": path_order,
            "rejected": rejected_paths,
        },
    }, sys.stdout, ensure_ascii=False, indent=1)
    print()


if __name__ == "__main__":
    main()
