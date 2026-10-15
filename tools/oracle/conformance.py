#!/usr/bin/env python3
"""How does the official emulator answer the requests googleapis/conformance-tests says client
SDKs send? (#44)

Usage (from the repository root), with a freshly started jar under an English locale:
    git clone https://github.com/googleapis/conformance-tests.git /tmp/conformance-tests
    git -C /tmp/conformance-tests checkout 998891a8dae00df121e460420cceb0abfc4e2ea1
    python3 -I tools/oracle/conformance.py 127.0.0.1:8086 /tmp/conformance-tests/firestore/v1 \\
        > crates/hidane/tests/fixtures/conformance.json

The Firestore conformance tests (Apache-2.0, https://github.com/googleapis/conformance-tests)
say which request a client SDK sends for a call: a commit for `create`, `set`, `update` (with a
map or with field paths) and `delete`, a GetDocument for `get`, a RunQuery for a query built
clause by clause. They test SDKs; this sends each request one of them gives to the emulator, as
REST, and records the answer, so that hidane is checked on the requests SDKs send. Left out:
the tests a client refuses before sending anything (`isError`), and `listen`, whose server
answers a client reads.

Each test's documents move below `t<n>/x/`, so that no two tests share one. An update or a
delete comes after a create of its document (empty), so that the precondition `exists: true`
those requests carry holds. The fixture has the format of `rest.json` and its normalisation,
with the timestamps a `REQUEST_TIME` transform writes masked besides;
`crates/hidane/tests/rest.rs` replays it. Stdlib only.
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
TESTS = Path(sys.argv[2])
COMMIT = "998891a8dae00df121e460420cceb0abfc4e2ea1"
PROJECT = f"conformance-{time.strftime('%H%M%S')}"
OWNER = {"Authorization": "Bearer owner"}
DATABASE = "projects/projectID/databases/(default)"
DOCUMENTS = DATABASE + "/documents"
B = "/v1/projects/{project}/databases/(default)/documents"

CASES = []


def case(name, method, path, body=None):
    c = {"name": name, "method": method, "path": path, "headers": OWNER, "compare": "exact"}
    if body is not None:
        c["body"] = body
    CASES.append(c)


def moved(value, n):
    """`value` with every name of the tests' database below `t<n>/x/` of the recording's."""
    text = json.dumps(value)
    text = text.replace(DOCUMENTS + "/", f"projects/{{project}}/databases/(default)/documents/t{n}/x/")
    text = text.replace(DOCUMENTS, f"projects/{{project}}/databases/(default)/documents/t{n}/x")
    text = text.replace(DATABASE, "projects/{project}/databases/(default)")
    return json.loads(text)


WRITES = ("create", "set", "update", "updatePaths", "delete")
n = 0
for path in sorted(TESTS.glob("*.json")):
    for test in json.loads(path.read_text())["tests"]:
        kind = next(k for k in test if k not in ("description", "comment"))
        spec = test[kind]
        if spec.get("isError") or kind == "listen":
            continue
        n += 1
        name = f"{path.stem}: {test['description']}"
        if kind in WRITES:
            if kind in ("update", "updatePaths", "delete"):
                document = moved(spec["docRefPath"], n)
                case(f"{name} (its document, created first)", "POST", B + ":commit",
                     {"writes": [{"update": {"name": document, "fields": {}}}]})
            case(name, "POST", B + ":commit", {"writes": moved(spec["request"]["writes"], n)})
        elif kind == "get":
            case(name, "GET", "/v1/" + moved(spec["request"]["name"], n))
        elif kind == "query":
            parent = moved(spec["collPath"], n).rsplit("/", 1)[0]
            case(name, "POST", f"/v1/{parent}:runQuery", {"structuredQuery": moved(spec["query"], n)})
        else:
            raise SystemExit(f"{path.name}: a test of an unknown kind, {kind}")


def norm(text):
    # Versions of the years 2021 to 2099, in microseconds (`tools/fuzz/diff.py`, `rest.py`), and
    # timestamps of those years, which only the server writes (`REQUEST_TIME`).
    text = re.sub(r"(version \()(1[6-9]|[23][0-9]|40|41)[0-9]{14}(?![0-9])", r"\1<version>", text)
    text = re.sub(r'"timestampValue": "20(2[1-9]|[3-9][0-9])-[^"]*"', '"timestampValue": "<now>"', text)
    return mask(text, PROJECT)


def run():
    out = []
    for c in CASES:
        conn = http.client.HTTPConnection(*HOST.split(":"), timeout=10)
        path = c["path"].replace("{project}", PROJECT)
        body = json.dumps(c["body"]).replace("{project}", PROJECT) if "body" in c else None
        headers = dict(c["headers"])
        if body is not None:
            headers["Content-Type"] = "application/json"
        conn.request(c["method"], path, body=None if body is None else body.encode(), headers=headers)
        r = conn.getresponse()
        out.append(dict(c, outcome={"status": r.status, "contentType": r.getheader("content-type"),
                                    "body": norm(r.read().decode())}))
    return out


json.dump({
    "oracle": "cloud-firestore-emulator v1.22.0 (sha256 9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)",
    "generated_by": "tools/oracle/conformance.py",
    "source": f"https://github.com/googleapis/conformance-tests/tree/{COMMIT}/firestore/v1 (Apache-2.0)",
    "cases": run(),
}, sys.stdout, ensure_ascii=False, indent=1)
print()
