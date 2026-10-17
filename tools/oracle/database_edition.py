#!/usr/bin/env python3
"""How does `POST /emulator/v1/projects/{p}/databases/{d}:setDatabaseEdition` answer, and what
does it change? (#36)

Usage (from the repository root), with a freshly started jar under an English locale:
    python3 -I tools/oracle/database_edition.py 127.0.0.1:8086 \\
        > crates/hidane/tests/fixtures/database_edition.json
    python3 -I tools/oracle/database_edition.py 127.0.0.1:8087 enterprise \\
        > crates/hidane/tests/fixtures/database_edition_enterprise.json

The second records the same against a jar started with `--database-edition enterprise`; the
argument only labels the fixture. The body `{"edition": "STANDARD" | "ENTERPRISE"}` sets the
edition of the database the path names, and of no other; the jar's `--database-edition` is
every other database's. Each step that follows a change asks the database what it is with two
requests whose answers differ by edition: a commit of an array in an array (an enterprise
database takes it), and a pipeline with an unknown stage (a standard database refuses any
pipeline first). Then the bodies and paths it refuses. A body whose `database` is not the
path's is left out: the official emulator never answers it. The fixture has the format of
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
EDITION = sys.argv[2] if len(sys.argv) > 2 else "standard"
if EDITION not in ("standard", "enterprise"):
    raise SystemExit(f"the edition is standard or enterprise, not {EDITION}")
PROJECT = f"edition-{time.strftime('%H%M%S')}"
OWNER = {"Authorization": "Bearer owner"}
P = "{project}"

CASES = []


def case(name, method, path, body=None, raw=None, headers=None):
    c = {"name": name, "method": method, "path": path, "headers": OWNER if headers is None else headers,
         "compare": "exact"}
    if raw is not None:
        c["raw"] = raw
    elif body is not None:
        c["body"] = body
    CASES.append(c)


def db(database, project=P):
    return f"projects/{project}/databases/{database}"


def setting(name, body=None, database="(default)", project=P, **more):
    case(name, "POST", f"/emulator/v1/projects/{project}/databases/{database}:setDatabaseEdition", body, **more)


def asking(name, database="(default)", project=P):
    """What the database is: an enterprise database takes an array in an array, and reads a
    pipeline's stages (a standard database refuses any pipeline)."""
    d = db(database, project)
    case(f"{name}: an array in an array", "POST", f"/v1/{d}/documents:commit",
         {"writes": [{"update": {"name": f"{d}/documents/c/{len(CASES)}",
                                 "fields": {"v": {"arrayValue": {"values": [{"arrayValue": {}}]}}}}}]})
    case(f"{name}: a pipeline", "POST", f"/v1/{d}/documents:executePipeline",
         {"structuredPipeline": {"pipeline": {"stages": [{"name": "nope"}]}}})


asking("at first")
setting("enterprise", {"edition": "ENTERPRISE"})
asking("after enterprise")
asking("another database", "other")
asking("another project's", project=f"{P}-2")
setting("standard", {"edition": "STANDARD"})
asking("after standard")
setting("enterprise, as a number", {"edition": 2})
asking("after 2")
setting("standard, as a number", {"edition": 1})
asking("after 1")
setting("enterprise, as a number's text", {"edition": "2"})
asking("after \"2\"")
setting("a named database", {"edition": "ENTERPRISE"}, database="named")
asking("the named database", "named")
asking("the default database, next to it")
setting("standard, in an array", {"edition": ["STANDARD"]}, database="named")
asking("after [STANDARD]", "named")
setting("lenient JSON", raw="{edition: 'ENTERPRISE'}", database="lenient")
asking("after lenient JSON", "lenient")
setting("the path's database in the body", {"database": db("body"), "edition": "ENTERPRISE"}, database="body")
asking("after the path's database in the body", "body")
setting("an empty database in the body", {"database": "", "edition": "ENTERPRISE"}, database="empty")
asking("after an empty database in the body", "empty")
setting("without credentials", {"edition": "ENTERPRISE"}, database="anyone", headers={})
setting("an invalid credential", {"edition": "STANDARD"}, database="anyone", headers={"Authorization": "Bearer garbage"})
asking("after anyone", "anyone")
case("a query string", "POST", f"/emulator/v1/projects/{P}/databases/query:setDatabaseEdition?x=1", {"edition": "ENTERPRISE"})
asking("after a query string", "query")
setting("an invalid database ID", {"edition": "ENTERPRISE"}, database="BAD!")
setting("enterprise again", {"edition": "ENTERPRISE"}, database="cleared")
case("clear the database", "DELETE", f"/emulator/v1/projects/{P}/databases/cleared/documents")
asking("after clearing it", "cleared")

# What it refuses.
for name, body in [("an empty body", {}), ("a null edition", {"edition": None}),
                   ("edition unspecified", {"edition": "DATABASE_EDITION_UNSPECIFIED"}), ("edition 0", {"edition": 0}),
                   ("edition 3", {"edition": 3}), ("edition -1", {"edition": -1}),
                   ("the database alone", {"database": db("refused")}),
                   ("lowercase", {"edition": "enterprise"}), ("another spelling", {"edition": "Enterprise"}),
                   ("an unknown edition", {"edition": "FOO"}), ("EDITION_UNSPECIFIED", {"edition": "EDITION_UNSPECIFIED"}),
                   ("a boolean", {"edition": True}), ("an unknown field", {"x": 1}),
                   ("an unknown field next to the edition", {"edition": "ENTERPRISE", "x": 1})]:
    setting(name, body, database="refused")
setting("no body", database="refused")
setting("an empty text", raw="", database="refused")
setting("an array", raw="[]", database="refused")
setting("not JSON", raw="{x", database="refused")
asking("after the refusals", "refused")
base = f"/emulator/v1/projects/{P}/databases/(default):setDatabaseEdition"
case("GET", "GET", base)
case("PUT", "PUT", base, {"edition": "ENTERPRISE"})
case("a trailing slash", "POST", base + "/", {"edition": "ENTERPRISE"})
case("no database", "POST", f"/emulator/v1/projects/{P}:setDatabaseEdition", {"edition": "ENTERPRISE"})
case("an empty database ID", "POST", f"/emulator/v1/projects/{P}/databases/:setDatabaseEdition", {"edition": "ENTERPRISE"})
case("an empty project ID", "POST", "/emulator/v1/projects//databases/(default):setDatabaseEdition", {"edition": "ENTERPRISE"})
case("a database ID with a slash", "POST", f"/emulator/v1/projects/{P}/databases/a/b:setDatabaseEdition", {"edition": "ENTERPRISE"})
case("an invalid project ID", "POST", "/emulator/v1/projects/P5/databases/(default):setDatabaseEdition", {"edition": "ENTERPRISE"})
case("an invalid project ID, a named database", "POST", "/emulator/v1/projects/P5/databases/named:setDatabaseEdition", {"edition": "ENTERPRISE"})
case("an invalid project ID, a bad body", "POST", "/emulator/v1/projects/P5/databases/named:setDatabaseEdition", {"edition": "FOO"})

# Last, as it clears everything: whether `/reset` forgets the editions.
setting("enterprise before a reset", {"edition": "ENTERPRISE"}, database="reset")
case("reset", "POST", "/reset")
asking("after a reset", "reset")


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
    "generated_by": "tools/oracle/database_edition.py",
    **({"edition": EDITION} if EDITION != "standard" else {}),
    "cases": run(),
}, sys.stdout, ensure_ascii=False, indent=1)
print()
