"""How large may requests be, and which gRPC services answer? (#25)

Usage (from the repository root, `grpcurl` on PATH):
    python3 -I tools/oracle/grpc_settings.py 127.0.0.1:8086 > crates/hidane/tests/fixtures/grpc_settings.json

Records:

- `services`: what `grpcurl list` shows through server reflection.
- `v1beta1`: `google.firestore.v1beta1.Firestore` is the same service as v1 over the same data
  (a commit through one is read through the other) and names itself in PartitionQuery's
  message. Its messages are wire-compatible with v1's, so the calls are made with a scratch
  proto that declares the v1beta1 service over the v1 messages (the official reflection cannot
  describe v1beta1).
- `grpc_sizes`: Commit requests of growing size; the limit is 100 MiB.
- `rest_sizes`: REST bodies at 16 MiB and one byte more (`413` with an empty body).

`crates/hidane/tests/grpc_settings.rs` replays the v1beta1 and REST cases, and the gRPC sizes
up to 20 MB (larger ones are slow to send twice).
"""

import http.client
import json
import os
import re
import subprocess
import sys
import tempfile

HOST = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1:8080"
DB = "projects/grpc-settings/databases/(default)"
DOCS = f"{DB}/documents"
BETA = """syntax = "proto3";
package google.firestore.v1beta1;
import "google/firestore/v1/firestore.proto";
import "google/firestore/v1/document.proto";
service Firestore {
  rpc GetDocument(google.firestore.v1.GetDocumentRequest) returns (google.firestore.v1.Document);
  rpc Commit(google.firestore.v1.CommitRequest) returns (google.firestore.v1.CommitResponse);
  rpc ListCollectionIds(google.firestore.v1.ListCollectionIdsRequest) returns (google.firestore.v1.ListCollectionIdsResponse);
  rpc PartitionQuery(google.firestore.v1.PartitionQueryRequest) returns (google.firestore.v1.PartitionQueryResponse);
}
"""


def grpcurl(service, method, request, owner=True, extra=()):
    with tempfile.TemporaryDirectory() as beta:
        with open(os.path.join(beta, "beta.proto"), "w") as f:
            f.write(BETA)
        proto = ["-import-path", beta, "-proto", "beta.proto"] if service.endswith("v1beta1.Firestore") else ["-proto", "google/firestore/v1/firestore.proto"]
        args = ["grpcurl", "-plaintext", "-max-msg-sz", "1000000000", "-import-path", "proto", *proto, *extra]
        if owner:
            args += ["-H", "authorization: Bearer owner"]
        with tempfile.NamedTemporaryFile("w", suffix=".json") as body:
            json.dump(request, body)
            body.flush()
            out = subprocess.run(args + ["-d", "@", HOST, f"{service}/{method}"], stdin=open(body.name), capture_output=True, text=True, timeout=600)
    text = out.stdout + out.stderr
    m = re.search(r"Code: (\w+)\n\s*Message: (.*)", text)
    if m:
        return {"code": re.sub(r"(?<!^)(?=[A-Z])", "_", m.group(1)).upper(), "message": m.group(2).strip()}
    return {"code": "OK", "response": json.loads(out.stdout) if out.stdout.strip() else None}


def without_times(value):
    if isinstance(value, dict):
        return {k: "<time>" if k.endswith("Time") else without_times(v) for k, v in value.items()}
    if isinstance(value, list):
        return [without_times(v) for v in value]
    return value


def commit_of(total_mb):
    """A Commit of `total_mb` megabytes in writes of 1,000,000-byte strings."""
    return {"database": DB, "writes": [
        {"update": {"name": f"{DOCS}/sizes/{i}", "fields": {"s": {"string_value": "x" * 1_000_000}}}}
        for i in range(total_mb)]}


def rest_body(total):
    """A `:commit` body of exactly `total` bytes, in writes of at most 900,000-byte strings."""
    writes = []
    while True:
        writes.append({"update": {"name": f"{DOCS}/rest/{len(writes)}", "fields": {"s": {"stringValue": ""}}}})
        if len(json.dumps({"writes": writes})) + 900_000 > total:
            break
        writes[-1]["update"]["fields"]["s"]["stringValue"] = "x" * 900_000
    writes[-1]["update"]["fields"]["s"]["stringValue"] = "x" * (total - len(json.dumps({"writes": writes})))
    body = json.dumps({"writes": writes})
    assert len(body) == total
    return body


def rest(total):
    c = http.client.HTTPConnection(*HOST.split(":"), timeout=600)
    c.request("POST", f"/v1/{DOCS}:commit", body=rest_body(total),
              headers={"Content-Type": "application/json", "Authorization": "Bearer owner"})
    r = c.getresponse()
    body = r.read().decode()
    return {"status": r.status, "content_type": r.getheader("content-type"),
            "body": body if r.status != 200 else ""}


V1, BETA_SERVICE = "google.firestore.v1.Firestore", "google.firestore.v1beta1.Firestore"
c = http.client.HTTPConnection(*HOST.split(":"), timeout=10)
c.request("POST", "/reset")
c.getresponse().read()
listed = subprocess.run(["grpcurl", "-plaintext", HOST, "list"], capture_output=True, text=True, timeout=60).stdout.split()
written = {"database": DB, "writes": [{"update": {"name": f"{DOCS}/c/a", "fields": {"n": {"integer_value": "1"}}}}]}
fixture = {
    "oracle": "cloud-firestore-emulator v1.22.0 (sha256 9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)",
    "generated_by": "tools/oracle/grpc_settings.py",
    "services": listed,
    "v1beta1": [],
    "grpc_sizes": [],
    "rest_sizes": [],
}
for name, service, method, request, owner in [
    ("commit through v1beta1", BETA_SERVICE, "Commit", written, True),
    ("read through v1", V1, "GetDocument", {"name": f"{DOCS}/c/a"}, True),
    ("read through v1beta1", BETA_SERVICE, "GetDocument", {"name": f"{DOCS}/c/a"}, True),
    ("a missing document through v1beta1", BETA_SERVICE, "GetDocument", {"name": f"{DOCS}/c/zz"}, True),
    ("collection IDs through v1beta1 without a token", BETA_SERVICE, "ListCollectionIds", {"parent": DOCS}, False),
    ("PartitionQuery through v1beta1", BETA_SERVICE, "PartitionQuery", {"parent": DOCS}, True),
    ("PartitionQuery through v1", V1, "PartitionQuery", {"parent": DOCS}, True),
]:
    out = grpcurl(service, method, request, owner)
    fixture["v1beta1"].append({"name": name, "service": service, "method": method, "request": request,
                               "owner": owner, "outcome": without_times(out)})
for mb in [5, 20, 64, 105]:
    out = grpcurl(V1, "Commit", commit_of(mb))
    fixture["grpc_sizes"].append({"megabytes": mb, "outcome": {"code": out["code"], "message": out.get("message", "")}})
for total in [16 * 1024 * 1024, 16 * 1024 * 1024 + 1]:
    fixture["rest_sizes"].append({"bytes": total, "outcome": rest(total)})
json.dump(fixture, sys.stdout, ensure_ascii=False, indent=1)
print()
