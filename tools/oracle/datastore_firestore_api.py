#!/usr/bin/env python3
"""How does the official emulator in Datastore mode answer the Firestore API? (#81)

Usage (from the repository root, `grpcurl` on PATH), with a jar freshly started with
`--database-mode datastore-mode` under an English locale:
    python3 -I tools/oracle/datastore_firestore_api.py 127.0.0.1:8086 \\
        > crates/hidane/tests/fixtures/datastore_firestore_api.json

The Firestore API is refused in that mode ("Database mode not set to Firestore Mode. …"), but
not first: a REST request is routed, its body read and the names it holds checked as in
Firestore Native mode, and only then refused; a route, a method or a `Content-Type` that mode
does not serve is not found. This sends, over REST, each Firestore route with each method, valid
and invalid bodies and names and several content types, and the emulator's own endpoints (Security
Rules, coverage, clearing a database, index usage, triggers), which fail (`UNKNOWN`); over gRPC,
calls with a valid or an invalid name, and the RPC that is not implemented. Cases have the
format of `datastore_entities.json` (`raw` is a body sent as written); the gRPC calls are in
`grpc`, each with its code and message. `crates/hidane/tests/datastore.rs` replays it.
Stdlib and grpcurl.
"""

import json
import re
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import datastore_queries as base  # noqa: E402

base.PROJECT = "demo-firestore-api"
P = base.P
DB = f"/v1/projects/{P}/databases/(default)"
D = f"{DB}/documents"
JSON = base.JSON

CASES = []


def case(name, method, path, body=None, headers=JSON, raw=None):
    c = {"name": name, "method": method, "path": path, "headers": headers, "compare": "exact"}
    if raw is not None:
        c["raw"] = raw
    elif body is not None:
        c["body"] = body
    CASES.append(c)


DOC = f"projects/{P}/databases/(default)/documents/c/d"
# Routes, methods and bodies.
case("GET a document", "GET", f"{D}/c/d")
case("GET a collection's documents", "GET", f"{D}/c")
case("GET a document with a mask", "GET", f"{D}/c/d?mask.fieldPaths=a")
case("GET the documents root", "GET", D)
case("GET a document of another database", "GET", f"/v1/projects/{P}/databases/db2/documents/c/d")
case("GET a document over v1beta1", "GET", f"/v1beta1/projects/{P}/databases/(default)/documents/c/d")
case("GET the database", "GET", DB)
case("PATCH a document", "PATCH", f"{D}/c/d", {"fields": {}})
case("PATCH a document, an unknown field", "PATCH", f"{D}/c/d", {"x": 1})
case("PUT a document", "PUT", f"{D}/c/d", {"fields": {}})
case("DELETE a document", "DELETE", f"{D}/c/d")
case("createDocument", "POST", f"{D}/c", {"fields": {}})
case("createDocument with an ID", "POST", f"{D}/c?documentId=x", {"fields": {}})
case("createDocument under a document's name", "POST", f"{D}/c/d", {"fields": {}})
case("createDocument, an unknown field", "POST", f"{D}/c", {"x": 1})
case("commit", "POST", f"{D}:commit", {})
case("commit, an unknown field", "POST", f"{D}:commit", {"x": 1})
case("commit, a write with a bad name", "POST", f"{D}:commit", {"writes": [{"update": {"name": "bad"}}]})
case("commit, a write", "POST", f"{D}:commit", {"writes": [{"update": {"name": DOC, "fields": {}}}]})
case("batchGet", "POST", f"{D}:batchGet", {"documents": [DOC]})
case("beginTransaction", "POST", f"{D}:beginTransaction", {})
case("rollback", "POST", f"{D}:rollback", {"transaction": "AAAA"})
case("runQuery", "POST", f"{D}:runQuery", {"structuredQuery": {}})
case("runQuery under a document", "POST", f"{D}/c/d:runQuery", {"structuredQuery": {}})
case("runAggregationQuery", "POST", f"{D}:runAggregationQuery", {})
case("listCollectionIds", "POST", f"{D}:listCollectionIds", {})
case("partitionQuery", "POST", f"{D}:partitionQuery", {})
case("batchWrite", "POST", f"{D}:batchWrite", {})
case("executePipeline", "POST", f"{D}:executePipeline", {})
case("write over REST", "POST", f"{D}:write", {})
case("listen over REST", "POST", f"{D}:listen", {})
case("GET runQuery", "GET", f"{D}:runQuery")
# Content types.
case("commit in protobuf", "POST", f"{D}:commit", headers={"Content-Type": "application/x-protobuf"}, raw="")
case("createDocument in protobuf", "POST", f"{D}/c", headers={"Content-Type": "application/x-protobuf"}, raw="")
case("commit as text/html", "POST", f"{D}:commit", headers={"Content-Type": "text/html"}, raw="{}")
case("commit as text/plain", "POST", f"{D}:commit", headers={"Content-Type": "text/plain"}, raw="{}")
case("commit without a content type", "POST", f"{D}:commit", headers={}, raw="{}")
case("GET a document as protobuf", "GET", f"{D}/c/d", headers={"Content-Type": "application/x-protobuf"})
case("GET a document as text/html", "GET", f"{D}/c/d", headers={"Content-Type": "text/html"})
# The emulator's own endpoints.
E = f"/emulator/v1/projects/{P}"
case("GET the Security Rules", "GET", f"{E}:securityRules")
case("PUT Security Rules", "PUT", f"{E}:securityRules",
     {"rules": {"files": [{"content": "rules_version = '2';\nservice cloud.firestore {}"}]}})
case("GET the rule coverage", "GET", f"{E}:ruleCoverage")
case("GET the rule coverage page", "GET", f"{E}:ruleCoverage.html")
case("clear the default database", "DELETE", f"{E}/databases/(default)/documents")
case("clear another database", "DELETE", f"{E}/databases/db2/documents")
case("index usage", "GET", f"{E}:indexUsage")
case("index usage of the default database", "GET", f"{E}:indexUsage?database=projects/{P}/databases/(default)")
case("register a trigger", "PUT", f"{E}/triggers/t", {})
case("reset", "POST", "/reset", headers={})
case("the root", "GET", "/", headers={})

PB = "crates/hidane-proto/proto"
GRPC = [
    ("GetDocument, a bad name", "GetDocument", {"name": "bad"}),
    ("GetDocument", "GetDocument", {"name": DOC}),
    ("Commit, a bad database", "Commit", {"database": "bad"}),
    ("Commit", "Commit", {"database": f"projects/{P}/databases/(default)"}),
    ("ListCollectionIds, a bad parent", "ListCollectionIds", {"parent": "x"}),
    ("ListCollectionIds", "ListCollectionIds", {"parent": f"projects/{P}/databases/(default)/documents"}),
    ("RunQuery, a bad parent", "RunQuery", {"parent": "x"}),
    ("PartitionQuery", "PartitionQuery", {"parent": f"projects/{P}/databases/(default)/documents"}),
    ("BeginTransaction, a bad database", "BeginTransaction", {"database": "x"}),
]


def grpcurl(rpc, request):
    args = ["grpcurl", "-plaintext", "-max-time", "3", "-import-path", PB, "-proto",
            "google/firestore/v1/firestore.proto", "-d", json.dumps(request).replace("{project}", base.PROJECT),
            base.HOST, f"google.firestore.v1.Firestore/{rpc}"]
    out = subprocess.run(args, capture_output=True, text=True, timeout=60)
    text = out.stdout + out.stderr
    m = re.search(r"Code: (\w+)\n\s*Message: (.*)", text)
    if m:
        code = re.sub(r"(?<!^)(?=[A-Z])", "_", m.group(1)).upper()
        return {"code": code, "message": m.group(2).strip().replace(base.PROJECT, "{project}")}
    return {"code": "OK"}


def run():
    out = []
    for c in CASES:
        text = c.get("raw")
        if text is None:
            text = json.dumps(c["body"]) if "body" in c else ""
        text = text.replace("{project}", base.PROJECT)
        conn = base.http.client.HTTPConnection(*base.HOST.split(":"), timeout=60)
        conn.request(c["method"], c["path"].replace("{project}", base.PROJECT), body=text or None, headers=c["headers"])
        r = conn.getresponse()
        answer = r.read().decode()
        out.append(dict(c, outcome={"status": r.status, "contentType": r.getheader("content-type"),
                                    "body": base.masked(answer)}))
    return out


if __name__ == "__main__":
    cases = run()
    grpc = [{"name": n, "rpc": rpc, "request": request, "outcome": grpcurl(rpc, request)} for n, rpc, request in GRPC]
    json.dump({
        "oracle": "cloud-firestore-emulator v1.22.0 (sha256 9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)",
        "generated_by": "tools/oracle/datastore_firestore_api.py",
        "mode": "datastore-mode",
        "cases": cases,
        "grpc": grpc,
    }, sys.stdout, ensure_ascii=False, indent=1)
    print()
