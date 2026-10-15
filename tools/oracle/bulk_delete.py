#!/usr/bin/env python3
"""What does `POST /emulator/v1/projects/{p}[/databases/{d}]:bulkDelete` delete, and how does it
refuse a request? (#36)

Usage (from the repository root), with a freshly started jar under an English locale:
    python3 -I tools/oracle/bulk_delete.py 127.0.0.1:8086 \\
        > crates/hidane/tests/fixtures/bulk_delete.json

The body names a database and collection IDs or namespace IDs, as Datastore's bulk delete
does: the documents of every collection with one of the IDs, at any depth, but not the
documents below them; the namespace `""` (Firestore's only one) is every document, another
namespace none; with both, the documents in both. The database is the body's, whatever the
path names. Each case writes documents to a database of its own, deletes, and lists what is
left with a query over every collection; others send bad bodies. The fixture has the format of
`rest.json` and its normalisation; `crates/hidane/tests/rest.rs` replays it. Stdlib only.
"""

import http.client
import json
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from harness import mask  # noqa: E402

HOST = sys.argv[1]
PROJECT = f"bulk-{time.strftime('%H%M%S')}"
OWNER = {"Authorization": "Bearer owner"}
P = "{project}"
DOCUMENTS = ["a/1", "a/2", "b/1", "x/1/a/3", "x/1/b/4", "a/1/sub/5", "c/1/a/6/d/7"]

CASES = []


def case(name, method, path, body=None, raw=None, headers=None):
    c = {"name": name, "method": method, "path": path, "headers": OWNER if headers is None else headers,
         "compare": "exact"}
    if raw is not None:
        c["raw"] = raw
    elif body is not None:
        c["body"] = body
    CASES.append(c)


def db(database):
    return f"projects/{P}/databases/{database}"


def deleting(name, body, path=None, **more):
    """Writes DOCUMENTS to a database of the case's own, sends `body` (`{db}` is that
    database), then lists what is left."""
    database = db(f"bd{len(CASES)}")
    case(f"{name}: write", "POST", f"/v1/{database}/documents:commit",
         {"writes": [{"update": {"name": f"{database}/documents/{d}", "fields": {}}} for d in DOCUMENTS]})
    text = json.dumps(body).replace("{db}", database)
    case(name, "POST", path or f"/emulator/v1/projects/{P}:bulkDelete", json.loads(text), **more)
    case(f"{name}: left", "POST", f"/v1/{database}/documents:runQuery",
         {"structuredQuery": {"from": [{"allDescendants": True}]}})


deleting("a collection ID", {"database": "{db}", "collection_ids": ["a"]})
deleting("two collection IDs", {"database": "{db}", "collection_ids": ["a", "b"]})
deleting("a collection ID of subcollections", {"database": "{db}", "collection_ids": ["sub"]})
deleting("a collection ID twice", {"database": "{db}", "collectionIds": ["b", "b"]})
deleting("collection IDs that name nothing", {"database": "{db}", "collection_ids": ["", "__x__", "a/1", "zz"]})
deleting("a number as a collection ID", {"database": "{db}", "collection_ids": [1]})
deleting("the namespace \"\"", {"database": "{db}", "namespace_ids": [""]})
deleting("another namespace", {"database": "{db}", "namespaceIds": ["x", "(default)"]})
deleting("a collection ID in the namespace \"\"", {"database": "{db}", "collection_ids": ["a"], "namespace_ids": [""]})
deleting("a collection ID in another namespace", {"database": "{db}", "collection_ids": ["a"], "namespace_ids": ["x"]})
deleting("the path's database is not the body's", {"database": "{db}", "collection_ids": ["b"]},
         path=f"/emulator/v1/projects/{P}/databases/zz:bulkDelete")
deleting("another project's path", {"database": "{db}", "collection_ids": ["b"]},
         path="/emulator/v1/projects/other:bulkDelete")
deleting("without credentials", {"database": "{db}", "collection_ids": ["a"]}, headers={})
deleting("an invalid credential", {"database": "{db}", "collection_ids": ["a"]},
         headers={"Authorization": "Bearer garbage"})
deleting("neither IDs", {"database": "{db}"})
deleting("empty lists", {"database": "{db}", "collection_ids": [], "namespace_ids": None})
deleting("a null collection ID", {"database": "{db}", "collection_ids": [None]})
deleting("collection IDs as a string", {"database": "{db}", "collection_ids": "a"})
deleting("an unknown field", {"database": "{db}", "collection_ids": ["a"], "export_directory": "x"})

base = f"/emulator/v1/projects/{P}:bulkDelete"
case("no body", "POST", base)
case("an empty body", "POST", base, raw="")
case("an array", "POST", base, raw="[]")
case("not JSON", "POST", base, raw="{x")
case("lenient JSON", "POST", base, raw="{'database': 'projects/" + P + "/databases/(default)', collection_ids: ['a']}")
case("a null database", "POST", base, {"database": None, "collection_ids": ["a"]})
case("a number as the database", "POST", base, {"database": 1, "collection_ids": ["a"]})
case("a database name that lacks", "POST", base, {"database": "projects/x", "collection_ids": ["a"]})
for ending in ["/", "/documents", "//", ""]:
    case(f"a database name ending {ending!r}", "POST", base,
         {"database": "projects/p/databases/(default)" + ending if ending else "projects/p/databases/",
          "collection_ids": ["zz"]})
case("an invalid project ID", "POST", base, {"database": "projects/P1/databases/(default)", "collection_ids": ["a"]})
case("project ID '.'", "POST", base, {"database": "projects/./databases/(default)", "collection_ids": ["a"]})
GARBAGE = {"Authorization": "Bearer garbage"}
case("an invalid credential, not JSON", "POST", base, raw="{x", headers=GARBAGE)
case("an invalid credential, an unknown field", "POST", base, {"x": 1}, headers=GARBAGE)
case("an invalid credential, a bad database", "POST", base, {"database": "x", "collection_ids": ["a"]}, headers=GARBAGE)
case("an invalid credential, neither IDs", "POST", base, {"database": db("(default)")}, headers=GARBAGE)
case("a bad database, neither IDs", "POST", base, {"database": "x"})
case("a query string", "POST", base + "?x=1", {"database": db("(default)"), "collection_ids": ["zz"]})
case("GET", "GET", base)
case("PUT", "PUT", base, {"database": db("(default)"), "collection_ids": ["zz"]})
case("a form", "POST", base, raw="database=x", headers={**OWNER, "Content-Type": "application/x-www-form-urlencoded"})
case("a trailing slash", "POST", base + "/", {"database": db("(default)"), "collection_ids": ["zz"]})


def run():
    out = []
    for c in CASES:
        conn = http.client.HTTPConnection(*HOST.split(":"), timeout=30)
        path = c["path"].replace("{project}", PROJECT)
        if "raw" in c:
            body = c["raw"].replace("{project}", PROJECT)
        elif "body" in c:
            body = json.dumps(c["body"]).replace("{project}", PROJECT)
        else:
            body = None
        headers = dict(c["headers"])
        if body is not None and "Content-Type" not in headers:
            headers["Content-Type"] = "application/json"
        conn.request(c["method"], path, body=None if body is None else body.encode(), headers=headers)
        r = conn.getresponse()
        out.append(dict(c, outcome={"status": r.status, "contentType": r.getheader("content-type"),
                                    "body": mask(r.read().decode(), PROJECT)}))
    return out


json.dump({
    "oracle": "cloud-firestore-emulator v1.22.0 (sha256 9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)",
    "generated_by": "tools/oracle/bulk_delete.py",
    "cases": run(),
}, sys.stdout, ensure_ascii=False, indent=1)
print()
