#!/usr/bin/env python3
"""What do `--single_project_mode` and `--single_project_mode_error` do to a request for
another project? (#14)

Usage (from the repository root, `grpcurl` on PATH):
    python3 -I tools/oracle/single_project_mode.py <jar> [java] [port] \\
        > crates/hidane/tests/fixtures/single_project_mode.json

Starts the jar twice under an English locale, with `--project_id configured
--single_project_mode true` (as firebase-tools does unless `emulators.singleProjectMode` is
`false`), then with `--single_project_mode_error true` too, and sends each a request per
surface: every REST binding, the `/emulator/v1` endpoints, gRPC calls and streams, and
WebChannel handshakes, for the configured project and for another. Each step records its
answer and the lines the jar printed on its standard output meanwhile (its log records go to
standard error and are left out).

The project is the one the request is addressed to: a REST path's; a gRPC call's
`google-cloud-resource-prefix` header, else the database of its `x-goog-request-params`
header, else its name; a WebChannel URL's `database` parameter; an export's, import's or bulk delete's body.
`crates/hidane/tests/single_project_mode.rs` replays the steps.
"""

import http.client
import json
import os
import re
import subprocess
import sys
import tempfile
import time
import urllib.parse

JAR = sys.argv[1]
JAVA = sys.argv[2] if len(sys.argv) > 2 else "java"
PORT = int(sys.argv[3]) if len(sys.argv) > 3 else 8210
HOST = f"127.0.0.1:{PORT}"
PROTOS = "crates/hidane-proto/proto"
OWNER = {"Authorization": "Bearer owner"}
RULES = ("rules_version = '2';\nservice cloud.firestore {\n  match /databases/{database}/documents {\n"
         "    match /{document=**} {\n      allow read, write: if true;\n    }\n  }\n}\n")


def db(project, database="(default)"):
    return f"projects/{project}/databases/{database}"


def docs(project, database="(default)"):
    return f"{db(project, database)}/documents"


def rest_step(name, method, path, body=None, headers=None):
    return {"name": name, "kind": "rest", "method": method, "path": path,
            "headers": OWNER if headers is None else headers, **({} if body is None else {"body": body})}


def grpc_step(name, rpc, request, prefix=None, params=None):
    """`prefix`: a `google-cloud-resource-prefix` header; `params`: the database of an
    `x-goog-request-params` header, as SDKs send it."""
    step = {"name": name, "kind": "grpc", "rpc": rpc, "prefix": prefix, "request": request}
    if params:
        step["params"] = "database=" + urllib.parse.quote(params, safe="()")
    return step


def webchannel_step(name, rpc, database, message):
    return {"name": name, "kind": "webchannel", "rpc": rpc, "database": database, "message": message}


def steps():
    out = []
    for project in ("configured", "other"):
        d, base = db(project), f"/v1/{docs(project)}"
        out += [
            rest_step(f"{project}: get", "GET", f"{base}/c/d"),
            rest_step(f"{project}: list", "GET", f"{base}/c"),
            rest_step(f"{project}: create", "POST", f"{base}/c?documentId=x", {"fields": {}}),
            rest_step(f"{project}: patch", "PATCH", f"{base}/c/d", {"fields": {}}),
            rest_step(f"{project}: delete", "DELETE", f"{base}/c/x"),
            rest_step(f"{project}: commit", "POST", f"{base}:commit",
                      {"writes": [{"update": {"name": f"{docs(project)}/c/d", "fields": {}}}]}),
            rest_step(f"{project}: batchGet", "POST", f"{base}:batchGet", {"documents": [f"{docs(project)}/c/d"]}),
            rest_step(f"{project}: runQuery", "POST", f"{base}:runQuery",
                      {"structuredQuery": {"from": [{"collectionId": "c"}]}}),
            rest_step(f"{project}: runAggregationQuery", "POST", f"{base}:runAggregationQuery",
                      {"structuredAggregationQuery": {"structuredQuery": {"from": [{"collectionId": "c"}]},
                                                      "aggregations": [{"count": {}}]}}),
            rest_step(f"{project}: listCollectionIds", "POST", f"{base}:listCollectionIds", {}),
            rest_step(f"{project}: beginTransaction", "POST", f"{base}:beginTransaction", {}),
            rest_step(f"{project}: rollback", "POST", f"{base}:rollback", {"transaction": "eA=="}),
            rest_step(f"{project}: batchWrite", "POST", f"{base}:batchWrite",
                      {"writes": [{"update": {"name": f"{docs(project)}/c/e", "fields": {}}}]}),
            rest_step(f"{project}: named database", "GET", f"/v1/{docs(project, 'other-db')}/c/d"),
            rest_step(f"{project}: rules, put", "PUT", f"/emulator/v1/projects/{project}:securityRules",
                      {"rules": {"files": [{"name": "firestore.rules", "content": RULES}]}}),
            rest_step(f"{project}: rules, get", "GET", f"/emulator/v1/projects/{project}:securityRules"),
            rest_step(f"{project}: rules of a database, get", "GET", f"/emulator/v1/{d}:securityRules"),
            rest_step(f"{project}: rule coverage", "GET", f"/emulator/v1/{d}:ruleCoverage"),
            rest_step(f"{project}: clear a database", "DELETE", f"/emulator/v1/{docs(project)}"),
            rest_step(f"{project}: delete a tree", "DELETE", f"/emulator/v1/{docs(project)}/c/d"),
            rest_step(f"{project}: bulk delete", "POST", f"/emulator/v1/projects/{project}:bulkDelete",
                      {"database": d, "collection_ids": ["zz"]}),
            grpc_step(f"{project}: gRPC GetDocument", "GetDocument", {"name": f"{docs(project)}/c/d"}),
            grpc_step(f"{project}: gRPC Commit", "Commit", {"database": d}),
            grpc_step(f"{project}: gRPC RunQuery", "RunQuery",
                      {"parent": docs(project), "structured_query": {"from": [{"collection_id": "c"}]}}),
            grpc_step(f"{project}: gRPC Write", "Write", {"database": d}, prefix=d),
            grpc_step(f"{project}: gRPC Listen", "Listen",
                      {"database": d, "add_target": {"target_id": 1, "documents": {"documents": [f"{docs(project)}/c/a"]}}},
                      prefix=d),
            webchannel_step(f"{project}: WebChannel Write", "Write", d, {"database": d}),
            webchannel_step(f"{project}: WebChannel Listen", "Listen", d,
                            {"database": d, "addTarget": {"targetId": 2, "documents": {"documents": [f"{docs(project)}/c/a"]}}}),
        ]
    other, configured = db("other"), db("configured")
    out += [
        # Which project counts: the request's own, not the names inside it. (A write to another
        # database's document is an intentional difference, docs/parity-exceptions.md.)
        rest_step("configured path, a reference to another project", "POST", f"/v1/{docs('configured')}:commit",
                  {"writes": [{"update": {"name": f"{docs('configured')}/c/r",
                                          "fields": {"r": {"referenceValue": f"{docs('other')}/c/d"}}}}]}),
        rest_step("another project's path, a reference to the configured project", "POST",
                  f"/v1/{docs('other')}:commit",
                  {"writes": [{"update": {"name": f"{docs('other')}/c/r",
                                          "fields": {"r": {"referenceValue": f"{docs('configured')}/c/d"}}}}]}),
        grpc_step("gRPC: a name of another project, the configured prefix", "GetDocument",
                  {"name": f"{docs('other')}/c/d"}, prefix=configured),
        grpc_step("gRPC: the configured name, another project's prefix", "GetDocument",
                  {"name": f"{docs('configured')}/c/d"}, prefix=other),
        grpc_step("gRPC Write: the configured prefix, another project's database", "Write",
                  {"database": other}, prefix=configured),
        grpc_step("gRPC Write: another project's prefix, the configured database", "Write",
                  {"database": configured}, prefix=other),
        grpc_step("gRPC: the configured name, another project's request params", "GetDocument",
                  {"name": f"{docs('configured')}/c/d"}, params=other),
        grpc_step("gRPC: the configured prefix, another project's request params", "GetDocument",
                  {"name": f"{docs('configured')}/c/d"}, prefix=configured, params=other),
        grpc_step("gRPC Write: another project's request params, the configured database", "Write",
                  {"database": configured}, params=other),
        webchannel_step("WebChannel Write: the configured URL, another project's database", "Write",
                        configured, {"database": other}),
        webchannel_step("WebChannel Write: another project's URL, the configured database", "Write",
                        other, {"database": configured}),
        rest_step("export: another project's path, the configured database", "POST",
                  "/emulator/v1/projects/other:export",
                  {"database": configured, "export_directory": "/nonexistent-hidane-dir"}),
        rest_step("export: the configured path, another project's database", "POST",
                  "/emulator/v1/projects/configured:export",
                  {"database": other, "export_directory": "/nonexistent-hidane-dir"}),
        rest_step("bulk delete: the configured path, another project's database", "POST",
                  "/emulator/v1/projects/configured:bulkDelete", {"database": other, "collection_ids": ["zz"]}),
        rest_step("import: the configured path, another project's database", "POST",
                  "/emulator/v1/projects/configured:import",
                  {"database": other, "export_directory": "/nonexistent-hidane-dir"}),
        # Against the other checks of a request.
        rest_step("another project: an invalid Authorization header", "GET", f"/v1/{docs('other')}/c/d",
                  headers={"Authorization": "Bearer not-a-jwt"}),
        rest_step("another project: a reserved document ID", "GET", f"/v1/{docs('other')}/c/__x__"),
        rest_step("another project: an invalid query", "POST", f"/v1/{docs('other')}:runQuery",
                  {"structuredQuery": {"limit": -1}}),
        rest_step("another project: an invalid write", "POST", f"/v1/{docs('other')}:commit",
                  {"writes": [{"update": {"name": "bad", "fields": {}}}]}),
        rest_step("an invalid project ID", "GET", f"/v1/{docs('Other')}/c/d"),
        rest_step("another project, case aside", "GET", f"/v1/{docs('configured-2')}/c/d"),
    ]
    return out


# --- Running ----------------------------------------------------------------------------------

def norm(text):
    text = re.sub(r'"(createTime|updateTime|readTime|commitTime)": "[^"]*"', r'"\1": "<time>"', text)
    return re.sub(r'"transaction": "[^"]*"', '"transaction": "<id>"', text)


def rest(step):
    conn = http.client.HTTPConnection("127.0.0.1", PORT, timeout=30)
    body = json.dumps(step["body"]) if "body" in step else None
    headers = dict(step["headers"])
    if body is not None:
        headers["Content-Type"] = "application/json"
    conn.request(step["method"], step["path"], body=body, headers=headers)
    r = conn.getresponse()
    return {"status": r.status, "body": norm(r.read().decode())}


def grpc(step):
    args = ["grpcurl", "-plaintext", "-max-time", "3", "-import-path", PROTOS,
            "-proto", "google/firestore/v1/firestore.proto", "-H", "authorization: Bearer owner"]
    if step["prefix"]:
        args += ["-H", f"google-cloud-resource-prefix: {step['prefix']}"]
    if "params" in step:
        args += ["-H", f"x-goog-request-params: {step['params']}"]
    args += ["-d", json.dumps(step["request"]), HOST, f"google.firestore.v1.Firestore/{step['rpc']}"]
    out = subprocess.run(args, capture_output=True, text=True, timeout=60)
    text = out.stdout + out.stderr
    m = re.search(r"Code: (\w+)\n\s*Message: (.*)", text)
    stream = step["rpc"] in ("Listen", "Write")
    # A stream that answered before the deadline counts as accepted (`auth.py`).
    if m and not (stream and out.stdout.strip()):
        return {"code": re.sub(r"(?<!^)(?=[A-Z])", "_", m.group(1)).upper(), "message": m.group(2).strip()}
    return {"code": "OK"}


def webchannel(step):
    """The handshake's first frame, or that none came within five seconds."""
    query = urllib.parse.urlencode({"VER": 8, "database": step["database"], "RID": 1000, "CVER": 22,
                                    "X-HTTP-Session-Id": "gsessionid", "zx": "x", "t": 1}, safe="()")
    body = urllib.parse.urlencode({"headers": "Authorization:Bearer owner\r\n", "count": 1, "ofs": 0,
                                   "req0___data__": json.dumps(step["message"])})
    conn = http.client.HTTPConnection("127.0.0.1", PORT, timeout=5)
    try:
        conn.request("POST", f"/google.firestore.v1.Firestore/{step['rpc']}/channel?{query}", body=body,
                     headers={"Content-Type": "application/x-www-form-urlencoded"})
        r = conn.getresponse()
        first = r.read1(4096).decode()
    except TimeoutError:
        return {"answered": False}
    frame = json.loads(first.split("\n", 1)[1])
    if frame and frame[0][1] and frame[0][1][0] == "c":
        frame[0][1][1] = "<sid>"
    return {"answered": True, "status": r.status, "frame": frame}


def run(name, flags, work):
    out_path, err_path = os.path.join(work, f"{name}.out"), os.path.join(work, f"{name}.err")
    with open(out_path, "wb") as out, open(err_path, "wb") as err:
        proc = subprocess.Popen(
            [JAVA, "-Duser.language=en", "-Duser.country=US", "-jar", JAR, "--host", "127.0.0.1",
             "--port", str(PORT), *flags], cwd=work, stdout=out, stderr=err)
    try:
        for _ in range(240):
            try:
                conn = http.client.HTTPConnection("127.0.0.1", PORT, timeout=1)
                conn.request("GET", "/")
                if conn.getresponse().status == 200:
                    break
            except OSError:
                time.sleep(0.25)
        else:
            raise SystemExit("the emulator did not start")
        recorded = []
        for step in steps():
            at = os.path.getsize(out_path)
            outcome = {"rest": rest, "grpc": grpc, "webchannel": webchannel}[step["kind"]](step)
            time.sleep(0.3)
            with open(out_path, "rb") as f:
                f.seek(at)
                printed = [line for line in f.read().decode().splitlines() if line]
            recorded.append({**step, "outcome": outcome, "stdout": printed})
        return {"name": name, "flags": flags, "steps": recorded}
    finally:
        proc.kill()
        proc.wait(timeout=30)


def main():
    with tempfile.TemporaryDirectory() as work:
        common = ["--project_id", "configured", "--single_project_mode", "true"]
        runs = [run("warning", common, work),
                run("error", common + ["--single_project_mode_error", "true"], work)]
    json.dump({
        "oracle": "cloud-firestore-emulator v1.22.0 (sha256 9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)",
        "generated_by": "tools/oracle/single_project_mode.py",
        "runs": runs,
    }, sys.stdout, ensure_ascii=False, indent=1)
    print()


main()
