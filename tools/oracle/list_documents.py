"""How does ListDocuments answer without a collection ID? (#116)

Usage (from the repository root, `grpcurl` on PATH):
    python3 -I tools/oracle/list_documents.py 127.0.0.1:8086 > crates/hidane/tests/fixtures/list_documents.json

An empty `collection_id` lists the documents of every collection directly under the parent,
in name order; page tokens continue across collections. Records, after seeding one project:

- `grpc`: ListDocuments requests and the document names they return (relative to
  `documents/`), or their error. `pages` cases follow `next_page_token` to the end and record
  each page; `at_first_commit` cases read at the commit time of the first seed batch.
- `rest`: `GET …/documents/` and `GET …/documents/{document}/` (a trailing slash is an empty
  collection ID), with the status and the names or the error.

`crates/hidane/tests/list_documents.rs` replays them against hidane.
"""

import http.client
import json
import re
import subprocess
import sys

HOST = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1:8080"
DB = "projects/list/databases/(default)"
DOCS = f"{DB}/documents"
OWNER = "Bearer owner"

FIRST = ["c/a", "d/b"]
SECOND = ["c/b", "a/z", "B/x", "c/m/sub/x", "e/q/sub/y", "c/a/s/1", "c/a/t/2"]

GRPC = [
    # (name, request without parent, parent relative to documents, owner?)
    ("root", {}, "", True),
    ("root, anonymous", {}, "", False),
    ("a document", {}, "c/a", True),
    ("a missing document with subcollections", {}, "c/m", True),
    ("a document with nothing below", {}, "c/zz", True),
    ("a mask", {"mask": {"field_paths": ["n"]}}, "", True),
    ("order by name", {"order_by": "__name__"}, "", True),
    ("order by name ascending", {"order_by": "__name__ asc"}, "", True),
    ("order by name descending", {"order_by": "__name__ desc"}, "", True),
    ("order by a field", {"order_by": "n"}, "", True),
    ("order by a field descending", {"order_by": "n desc"}, "", True),
    ("show missing", {"show_missing": True}, "", True),
    ("show missing, anonymous", {"show_missing": True}, "", False),
    ("show missing and an order", {"show_missing": True, "order_by": "n"}, "", True),
    ("show missing, negative page size", {"show_missing": True, "page_size": -1}, "", True),
    ("negative page size, an order", {"page_size": -1, "order_by": "n"}, "", True),
    ("descending, negative page size", {"page_size": -1, "order_by": "__name__ desc"}, "", True),
    ("collection, descending", {"collection_id": "c", "order_by": "__name__ desc"}, "", True),
    ("collection, show missing, an order, negative page size", {"collection_id": "c", "show_missing": True, "order_by": "n", "page_size": -1}, "", True),
    ("collection, show missing", {"collection_id": "c", "show_missing": True}, "", True),
    ("pages of 1", {"page_size": 1, "pages": True}, "", True),
    ("pages of 2", {"page_size": 2, "pages": True}, "", True),
    ("pages of 2 under a document", {"page_size": 2, "pages": True}, "c/a", True),
    ("at the first commit", {"at_first_commit": True}, "", True),
]

REST = [
    ("root with a slash", "/"),
    ("a document with a slash", "/c/a/"),
    ("a collection with a slash", "/c/"),
    ("a subcollection with a slash", "/c/a/s/"),
    ("two slashes", "//"),
    ("a collection and two slashes", "/c//"),
    ("an empty segment inside", "/c/a//s"),
    ("root with a slash, show missing", "/?showMissing=true"),
    ("root with a slash, a page of 1", "/?pageSize=1"),
    ("root with a slash, a mask", "/?mask.fieldPaths=n"),
    ("root without a slash", ""),
]


def rest(method, path, body=None, authorization=OWNER):
    c = http.client.HTTPConnection(*HOST.split(":"), timeout=10)
    headers = {"Content-Type": "application/json"}
    if authorization:
        headers["Authorization"] = authorization
    c.request(method, path, body=None if body is None else json.dumps(body), headers=headers)
    r = c.getresponse()
    raw = r.read().decode()
    try:
        return r.status, json.loads(raw)
    except ValueError:
        return r.status, raw


def commit(paths):
    writes = [{"update": {"name": f"{DOCS}/{p}", "fields": {"n": {"integerValue": str(i)}, "m": {"booleanValue": True}}}}
              for i, p in enumerate(paths)]
    status, body = rest("POST", f"/v1/{DOCS}:commit", {"writes": writes})
    assert status == 200, body
    return body["commitTime"]


def grpcurl(request, owner):
    args = ["grpcurl", "-plaintext", "-import-path", "proto", "-proto", "google/firestore/v1/firestore.proto"]
    if owner:
        args += ["-H", f"authorization: {OWNER}"]
    args += ["-d", json.dumps(request), HOST, "google.firestore.v1.Firestore/ListDocuments"]
    out = subprocess.run(args, capture_output=True, text=True, timeout=60)
    text = out.stdout + out.stderr
    m = re.search(r"Code: (\w+)\n\s*Message: (.*)", text)
    if m:
        return {"code": re.sub(r"(?<!^)(?=[A-Z])", "_", m.group(1)).upper(), "message": m.group(2).strip()}
    body = json.loads(out.stdout)
    return {"documents": [{"name": d["name"][len(DOCS) + 1:], "fields": sorted(d.get("fields", {}))} for d in body.get("documents", [])],
            "next_page_token": body.get("nextPageToken", "")}


def run(request, parent, owner, first_commit):
    request = dict(request)
    pages = request.pop("pages", False)
    if request.pop("at_first_commit", False):
        request["read_time"] = first_commit
    request = {"parent": f"{DOCS}/{parent}" if parent else DOCS, **request}
    if not pages:
        out = grpcurl(request, owner)
        out.pop("next_page_token", None)
        return out
    result = []
    while True:
        out = grpcurl(request, owner)
        if "code" in out:
            return out
        result.append([d["name"] for d in out["documents"]])
        if not out["next_page_token"]:
            return {"pages": result}
        request["page_token"] = out["next_page_token"]


rest("POST", "/reset")
first_commit = commit(FIRST)
commit(SECOND)
fixture = {
    "oracle": "cloud-firestore-emulator v1.22.0 (sha256 9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)",
    "generated_by": "tools/oracle/list_documents.py",
    "seed": [FIRST, SECOND],
    "grpc": [
        {"name": n, "request": r, "parent": p, "owner": o, "outcome": run(r, p, o, first_commit)}
        for n, r, p, o in GRPC
    ],
    "rest": [],
}
for name, suffix in REST:
    status, body = rest("GET", f"/v1/{DOCS}{suffix}")
    if isinstance(body, str):
        outcome = {"status": status, "body": body}
    elif "documents" in body or status == 200:
        outcome = {"status": status, "documents": [d["name"][len(DOCS) + 1:] for d in body.get("documents", [])],
                   "next_page": bool(body.get("nextPageToken"))}
    else:
        outcome = {"status": status, "error": body.get("error", body)}
    fixture["rest"].append({"name": name, "path": suffix, "outcome": outcome})
json.dump(fixture, sys.stdout, ensure_ascii=False, indent=1)
print()
