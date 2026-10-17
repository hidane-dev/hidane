#!/usr/bin/env python3
"""How does the official emulator read a Datastore request's project, in each mode? (#81)

Usage (from the repository root, `grpcurl` on PATH):
    python3 -I tools/oracle/datastore_projects.py <cloud-firestore-emulator.jar> [java] [port] \\
        > crates/hidane/tests/fixtures/datastore_projects.json

The script starts the jar twice on `port` (default 8240), from a temporary directory and under an
English locale: in Firestore Native mode (no `--database-mode`) and with `--database-mode
datastore-mode`. Against each it sends Datastore requests: over gRPC, `Lookup` with each project
ID of `PROJECTS` (empty, valid, invalid: the app IDs of App Engine, a domain and a partition) and
every RPC with an empty one; over REST, every verb, projects in the path, and bodies it cannot
read. Firestore Native mode serves the Datastore API too, and refuses it ("Database mode not set
to Datastore Mode. …") once the project is read. It also records the services server reflection
lists. A gRPC answer is its code and message (`OK` alone, whatever it holds); a REST answer is
recorded as in `datastore_entities.json`. `crates/hidane/tests/datastore.rs` replays it.
Stdlib and grpcurl.
"""

import http.client
import json
import os
import re
import subprocess
import sys
import tempfile
import time

JAR = sys.argv[1]
JAVA = sys.argv[2] if len(sys.argv) > 2 else "java"
PORT = int(sys.argv[3]) if len(sys.argv) > 3 else 8240
HOST = f"127.0.0.1:{PORT}"
PB = "crates/hidane-proto/proto"

A100 = "a" * 100
PROJECTS = [
    "", "a", "x", "1", "0", "a-b", "a-", "p--", "1a", A100, A100 + "a", "a" * 99 + "-",
    "A", "aB", "-a", "-", "--", "a_b", "a.b", "a b", "a/b", "é",
    "a:b", "google.com:proj", ":", ":a", "a:", "1:2", "a.b:c", "a-:b", "a.:b", "a..b:c", "a:" + "a" * 99,
    "a:-b", "-a:b", "A:b", "a:B", ".a:b", "a::b", "domain.com:a:b",
    "dev~p", "s~p", "b~a", "aa~p", "-~p", "a-b~p", "dev~a:b",
    "dev~", "~", "~a", "a~", "A~p", "dev~A", "dev~a b", "a~b~c",
]
RPCS = ["Lookup", "RunQuery", "RunAggregationQuery", "BeginTransaction", "Commit", "Rollback",
        "AllocateIds", "ReserveIds"]


def grpcurl(rpc, request):
    args = ["grpcurl", "-plaintext", "-max-time", "3", "-import-path", PB, "-proto",
            "google/datastore/v1/datastore.proto", "-d", json.dumps(request), HOST,
            f"google.datastore.v1.Datastore/{rpc}"]
    out = subprocess.run(args, capture_output=True, text=True, timeout=60)
    m = re.search(r"Code: (\w+)\n\s*Message: (.*)", out.stdout + out.stderr)
    if m:
        return {"code": re.sub(r"(?<!^)(?=[A-Z])", "_", m.group(1)).upper(), "message": m.group(2).strip()}
    return {"code": "OK"}


def reflection():
    out = subprocess.run(["grpcurl", "-plaintext", HOST, "list"], capture_output=True, text=True, timeout=60)
    return sorted(line for line in out.stdout.splitlines() if line)


def rest_cases():
    lookup = {"keys": [{"path": [{"kind": "T", "name": "a"}]}]}
    cases = []
    for verb, body in [("lookup", lookup), ("beginTransaction", {}), ("commit", {"mode": "NON_TRANSACTIONAL"}),
                       ("rollback", {"transaction": "AAAA"}), ("allocateIds", {"keys": [{"path": [{"kind": "T"}]}]}),
                       ("reserveIds", {"keys": [{"path": [{"kind": "T", "id": "5"}]}]}),
                       ("runQuery", {"query": {}}), ("runAggregationQuery", {"aggregationQuery": {"aggregations": [{"count": {}}]}})]:
        cases.append((verb, "POST", f"/v1/projects/demo-projects:{verb}", body))
    cases += [
        ("lookup, an unknown field", "POST", "/v1/projects/demo-projects:lookup", {"x": 1}),
        ("lookup, a project with capitals", "POST", "/v1/projects/Demo:lookup", lookup),
        ("lookup, a project with a space", "POST", "/v1/projects/a%20b:lookup", lookup),
        ("lookup, a partitioned project", "POST", "/v1/projects/dev~p:lookup", lookup),
        ("lookup, a domain project", "POST", "/v1/projects/google.com:p:lookup", lookup),
        ("lookup, an empty project", "POST", "/v1/projects/:lookup", lookup),
        ("an unknown verb", "POST", "/v1/projects/demo-projects:frobnicate", {}),
        ("GET lookup", "GET", "/v1/projects/demo-projects:lookup", None),
    ]
    out = []
    for name, method, path, body in cases:
        conn = http.client.HTTPConnection("127.0.0.1", PORT, timeout=30)
        conn.request(method, path, body=None if body is None else json.dumps(body),
                     headers={"Content-Type": "application/json"} if body is not None else {})
        r = conn.getresponse()
        text = r.read().decode()
        text = re.sub(r'"(readTime|commitTime|updateTime|createTime)": "[^"]*"', r'"\1": "<time>"', text)
        text = re.sub(r'"transaction": "[^"]*"', '"transaction": "<id>"', text)
        text = re.sub(r'"(version|commitVersion|snapshotVersion)": "\d+"', r'"\1": "<version>"', text)
        c = {"name": name, "method": method, "path": path,
             "headers": {"Content-Type": "application/json"} if body is not None else {}, "compare": "exact"}
        if body is not None:
            c["body"] = body
        c["outcome"] = {"status": r.status, "contentType": r.getheader("content-type"), "body": text}
        out.append(c)
    return out


def run(work, mode, flags):
    log = open(os.path.join(work, f"{mode}.out"), "wb")
    proc = subprocess.Popen([JAVA, "-Duser.language=en", "-Duser.country=US", "-jar", JAR, "--host", "127.0.0.1",
                             "--port", str(PORT), *flags], cwd=work, stdout=log, stderr=subprocess.STDOUT)
    for _ in range(240):
        try:
            conn = http.client.HTTPConnection("127.0.0.1", PORT, timeout=5)
            conn.request("GET", "/")
            if conn.getresponse().status == 200:
                break
        except OSError:
            time.sleep(0.25)
    else:
        proc.kill()
        raise SystemExit("the emulator did not start")
    try:
        grpc = [{"name": f"Lookup, project {p!r}", "rpc": "Lookup", "request": {"project_id": p},
                 "outcome": grpcurl("Lookup", {"project_id": p})} for p in PROJECTS]
        grpc += [{"name": f"{rpc}, an empty project", "rpc": rpc, "request": {"project_id": ""},
                  "outcome": grpcurl(rpc, {"project_id": ""})} for rpc in RPCS]
        grpc += [{"name": f"{rpc}, a project", "rpc": rpc, "request": {"project_id": "demo-projects"},
                  "outcome": grpcurl(rpc, {"project_id": "demo-projects"})} for rpc in RPCS]
        return {"mode": mode, "flags": flags, "reflection": reflection(), "grpc": grpc, "cases": rest_cases()}
    finally:
        try:
            conn = http.client.HTTPConnection("127.0.0.1", PORT, timeout=5)
            conn.request("POST", "/shutdown")
            conn.getresponse().read()
        except OSError:
            pass
        try:
            proc.wait(timeout=30)
        except subprocess.TimeoutExpired:
            proc.kill()


def main():
    with tempfile.TemporaryDirectory() as work:
        runs = [run(work, "firestore-native", []), run(work, "datastore-mode", ["--database-mode", "datastore-mode"])]
    json.dump({
        "oracle": "cloud-firestore-emulator v1.22.0 (sha256 9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)",
        "generated_by": "tools/oracle/datastore_projects.py",
        "runs": runs,
    }, sys.stdout, ensure_ascii=False, indent=1)
    print()


if __name__ == "__main__":
    main()
