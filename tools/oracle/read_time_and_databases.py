"""How old may a `read_time` be, and how many databases may a project have? (#86)

Usage (from the repository root, `grpcurl` on PATH, against a freshly started official emulator):
    python3 -I tools/oracle/read_time_and_databases.py 127.0.0.1:8086 [COUNT] \
        > crates/hidane/tests/fixtures/read_time_and_databases.json

Read times. The script writes `c/d`, then makes each read of `READS` at `now - age` for every age
of `AGES`: over gRPC (GetDocument of the document, of a missing one and of one in a database
nothing was written to, ListDocuments, RunQuery) and over REST (`GET ?readTime=`, `:batchGet`,
`:runQuery`, `:runAggregationQuery`, `:listCollectionIds`, `:beginTransaction` with a read-only
`readTime`). A read time up to seven days old (604,800 s of `now - read_time`) is served, though
the emulator started seconds before and the document was written just now: one older than the
data reads its absence (`NOT_FOUND`, `missing`, no documents), not an error. An older one answers
`FAILED_PRECONDITION` "The requested 'read_time' is too old." over every read. The REST
`?readTime=` parameter fails at every age (docs/parity-exceptions.md). The ages stay a minute clear
of seven days, so the time a request takes does not move an answer across it; probing by hand
placed the boundary at seven days to within the ~10 ms a request took.

The fixture is recorded seconds after the write. Observed by hand and not recorded, because it
takes an hour: on one emulator, an hour after a document was written and left unread, every read
before its creation (GetDocument, BatchGetDocuments, ListDocuments, RunQuery, ListCollectionIds)
answered `UNKNOWN` with no message (500 over REST), while its first version, replaced minutes
after the write, was still served 100 minutes after being replaced. On another emulator, which read
its documents every 15 s, reads before their creation still read their absence 76 minutes later.
What drops that history is not known (#86).

Databases. Writes one document to `(default)` and to COUNT (1,000 by default) named databases
`db-1`, `db-2`, … of one project, which the writes create, and reads the first and the last back.
None is refused. (By hand, 20,000 named databases in one project, and 2,000 projects of two
databases each, were all accepted.)

The fixture keeps ages, never absolute times: answers are masked as `harness.mask` does (server
times `<time>`, transaction IDs `<id>`). `crates/hidane/tests/read_time_and_databases.rs` replays
both against hidane.
"""

import json
import re
import subprocess
import sys
from datetime import datetime, timedelta, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from harness import send  # noqa: E402

HOST = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1:8080"
COUNT = int(sys.argv[2]) if len(sys.argv) > 2 else 1000
ORACLE = "cloud-firestore-emulator v1.22.0 (sha256 9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)"
DB = "projects/read-time/databases/(default)"
DOCS = f"{DB}/documents"
EMPTY = "projects/read-time-empty/databases/(default)/documents"
DAY = 86_400
# Seconds before now: just now, a minute, under an hour, two hours, a minute either side of seven
# days, a year.
AGES = [0, 60, 3_540, 7_200, 7 * DAY - 60, 7 * DAY + 60, 365 * DAY]
QUERY = {"from": [{"collectionId": "c"}]}
# `{readTime}` is filled with `now - age`, RFC 3339 with microseconds.
READS = [
    {"name": "GetDocument of the written document", "transport": "grpc", "method": "GetDocument",
     "body": {"name": f"{DOCS}/c/d", "readTime": "{readTime}"}},
    {"name": "GetDocument of a missing document", "transport": "grpc", "method": "GetDocument",
     "body": {"name": f"{DOCS}/c/missing", "readTime": "{readTime}"}},
    {"name": "GetDocument in a database nothing was written to", "transport": "grpc", "method": "GetDocument",
     "body": {"name": f"{EMPTY}/c/d", "readTime": "{readTime}"}},
    {"name": "ListDocuments", "transport": "grpc", "method": "ListDocuments",
     "body": {"parent": DOCS, "collectionId": "c", "readTime": "{readTime}"}},
    {"name": "RunQuery", "transport": "grpc", "method": "RunQuery",
     "body": {"parent": DOCS, "structuredQuery": QUERY, "readTime": "{readTime}"}},
    {"name": "REST GET ?readTime=", "transport": "rest", "method": "GET",
     "path": f"/v1/{DOCS}/c/d?readTime={{readTime}}", "body": None},
    {"name": "REST :batchGet", "transport": "rest", "method": "POST", "path": f"/v1/{DOCS}:batchGet",
     "body": {"documents": [f"{DOCS}/c/d", f"{DOCS}/c/missing"], "readTime": "{readTime}"}},
    {"name": "REST :runQuery", "transport": "rest", "method": "POST", "path": f"/v1/{DOCS}:runQuery",
     "body": {"structuredQuery": QUERY, "readTime": "{readTime}"}},
    {"name": "REST :runAggregationQuery", "transport": "rest", "method": "POST", "path": f"/v1/{DOCS}:runAggregationQuery",
     "body": {"structuredAggregationQuery": {"structuredQuery": QUERY, "aggregations": [{"alias": "n", "count": {}}]},
              "readTime": "{readTime}"}},
    {"name": "REST :listCollectionIds", "transport": "rest", "method": "POST", "path": f"/v1/{DOCS}:listCollectionIds",
     "body": {"readTime": "{readTime}"}},
    {"name": "REST :beginTransaction read-only", "transport": "rest", "method": "POST", "path": f"/v1/{DOCS}:beginTransaction",
     "body": {"options": {"readOnly": {"readTime": "{readTime}"}}}},
]
OWNER = {"Authorization": "Bearer owner"}


def masked(value):
    """`harness.mask`'s normalisation, on parsed JSON."""
    if isinstance(value, dict):
        out = {}
        for k, v in value.items():
            if k in ("createTime", "updateTime", "readTime", "commitTime") and isinstance(v, str):
                out[k] = "<time>"
            elif k == "transaction" and isinstance(v, str):
                out[k] = "<id>"
            else:
                out[k] = masked(v)
        return out
    if isinstance(value, list):
        return [masked(v) for v in value]
    return value


def fill(value, read_time):
    return json.loads(json.dumps(value).replace("{readTime}", read_time))


def grpc(method, body):
    """Status, message and the names of the documents answered, relative to `documents/`."""
    out = subprocess.run(
        ["grpcurl", "-plaintext", "-import-path", "crates/hidane-proto/proto", "-proto", "google/firestore/v1/firestore.proto",
         "-H", "authorization: Bearer owner", "-d", json.dumps(body), HOST, f"google.firestore.v1.Firestore/{method}"],
        capture_output=True, text=True, timeout=120)
    m = re.search(r"Code: (\w+)\n\s*Message: (.*)", out.stdout + out.stderr)
    if m:
        return {"status": re.sub(r"(?<!^)(?=[A-Z])", "_", m.group(1)).upper(), "message": m.group(2).strip()}
    messages, text, decoder = [], out.stdout.strip(), json.JSONDecoder()
    while text:
        message, end = decoder.raw_decode(text)
        messages.append(message)
        text = text[end:].strip()
    names = []
    for message in messages:
        for document in [message] if method == "GetDocument" else message.get("documents", [message.get("document")]):
            if document:
                names.append(document["name"].split("/documents/", 1)[1])
    return {"status": "OK", "message": "", "documents": names}


def read(spec, read_time):
    body = fill(spec["body"], read_time)
    if spec["transport"] == "grpc":
        return grpc(spec["method"], body)
    status, _, text = send(HOST, spec["method"], spec["path"].replace("{readTime}", read_time), body, OWNER)
    return {"status": status, "response": masked(json.loads(text)) if text else None}


def read_times():
    status, _, text = send(HOST, "POST", f"/v1/{DOCS}:commit",
                           {"writes": [{"update": {"name": f"{DOCS}/c/d", "fields": {"a": {"integerValue": "1"}}}}]}, OWNER)
    assert status == 200, text
    cases = []
    for age in AGES:
        answers = []
        for spec in READS:
            read_time = (datetime.now(timezone.utc) - timedelta(seconds=age)).strftime("%Y-%m-%dT%H:%M:%S.%fZ")
            answers.append(read(spec, read_time))
        cases.append({"age": age, "answers": answers})
    return {"reads": READS, "cases": cases}


def databases():
    project = "projects/databases"
    names = ["(default)"] + [f"db-{n}" for n in range(1, COUNT + 1)]
    writes = {}
    for name in names:
        docs = f"{project}/databases/{name}/documents"
        status, _, text = send(HOST, "POST", f"/v1/{docs}:commit",
                               {"writes": [{"update": {"name": f"{docs}/c/d", "fields": {"n": {"stringValue": name}}}}]})
        key = str(status) if status == 200 else f"{status} {text}"
        writes[key] = writes.get(key, 0) + 1
    reads = []
    for name in (names[1], names[-1]):
        status, _, text = send(HOST, "GET", f"/v1/{project}/databases/{name}/documents/c/d")
        reads.append({"database": name, "status": status, "response": masked(json.loads(text))})
    return {"count": COUNT, "writes": writes, "reads": reads}


json.dump({
    "oracle": ORACLE,
    "generated_by": "tools/oracle/read_time_and_databases.py",
    "read_time": read_times(),
    "databases": databases(),
}, sys.stdout, ensure_ascii=False, indent=1)
print()
