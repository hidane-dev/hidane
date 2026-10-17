#!/usr/bin/env python3
"""Which flags does the official emulator start with, and which does it refuse?

Usage (from the repository root):
    python3 -I tools/oracle/startup_flags.py <cloud-firestore-emulator.jar> [java] [port] \\
        > crates/hidane/tests/fixtures/startup_flags.json

The script starts the jar once per entry of `STARTS`, on `port` (default 8230), from a temporary
directory and under an English locale, each time with `--host 127.0.0.1 --port <port>` and the
entry's flags. A start is recorded as `started` with the banner's `Database Edition` and `Database
Mode` lines, or as the exit code and the message of the error it exits with: the line after
`SEVERE: Exiting due to unexpected exception.`, from its first `: ` on (without the exception's
class), the stack trace left out.

`--require_indexes` in Firestore Native mode starts; `REQUESTS` are then sent to it over REST, as
`datastore_queries.json` records them: a query is refused whatever it holds, other requests are
not. `crates/hidane/tests/launch.rs` and `crates/hidane/tests/rest.rs` replay them. Stdlib only.
"""

import http.client
import json
import os
import subprocess
import sys
import tempfile
import time

JAR = sys.argv[1]
JAVA = sys.argv[2] if len(sys.argv) > 2 else "java"
PORT = int(sys.argv[3]) if len(sys.argv) > 3 else 8230

STARTS = [
    [],
    ["--database-mode", "firestore-native"],
    ["--database-mode", "datastore-mode"],
    ["--database-mode", "CLOUD_FIRESTORE"],
    ["--database-mode", "CLOUD_FIRESTORE_IN_DATASTORE_MODE"],
    ["--database-mode", "CLOUD_DATASTORE"],
    ["--database-mode", "Datastore-Mode"],
    ["--database-mode", "cloud-datastore"],
    ["--database-mode", "bogus"],
    ["--database-edition", "standard"],
    ["--database-edition", "enterprise"],
    ["--database-edition", "STANDARD"],
    ["--database-edition", "ENTERPRISE"],
    ["--database-edition", "bogus"],
    ["--database-mode", "datastore-mode", "--database-edition", "enterprise"],
    ["--database-mode", "bogus", "--database-edition", "bogus"],
    ["--database-mode", "bogus", "--single_project_mode", "true"],
    ["--require_indexes"],
    ["--require_indexes", "--database-mode", "firestore-native"],
    ["--single_project_mode", "true"],
    ["--single_project_mode_error", "true"],
    ["--export-name", "x"],
    ["--single_project_mode", "true", "--export-name", "x"],
]

PARENT = "/v1/projects/demo-flags/databases/(default)/documents"
QUERY = {"from": [{"collectionId": "c"}]}
REQUESTS = [
    ("commit", "POST", f"{PARENT}:commit",
     {"writes": [{"update": {"name": "projects/demo-flags/databases/(default)/documents/c/d", "fields": {}}}]}),
    ("get the document", "GET", f"{PARENT}/c/d", None),
    ("runQuery", "POST", f"{PARENT}:runQuery", {"structuredQuery": QUERY}),
    ("runQuery without from", "POST", f"{PARENT}:runQuery", {"structuredQuery": {}}),
    ("runQuery with a bad field path", "POST", f"{PARENT}:runQuery",
     {"structuredQuery": {**QUERY, "orderBy": [{"field": {"fieldPath": "a..b"}}]}}),
    ("runQuery of a collection group", "POST", f"{PARENT}:runQuery",
     {"structuredQuery": {"from": [{"collectionId": "c", "allDescendants": True}]}}),
    ("runQuery in a new transaction", "POST", f"{PARENT}:runQuery", {"structuredQuery": QUERY, "newTransaction": {}}),
    ("runQuery under a document", "POST", f"{PARENT}/c/d:runQuery", {"structuredQuery": QUERY}),
    ("runQuery with no body", "POST", f"{PARENT}:runQuery", None),
    ("runQuery with an unknown field", "POST", f"{PARENT}:runQuery", {"x": 1}),
    ("runQuery in another database", "POST", "/v1/projects/demo-flags/databases/db2/documents:runQuery",
     {"structuredQuery": QUERY}),
    ("runAggregationQuery", "POST", f"{PARENT}:runAggregationQuery",
     {"structuredAggregationQuery": {"structuredQuery": QUERY, "aggregations": [{"count": {}}]}}),
    ("listCollectionIds", "POST", f"{PARENT}:listCollectionIds", {}),
    ("beginTransaction", "POST", f"{PARENT}:beginTransaction", {}),
]


def wait(proc, log):
    """`started` once the emulator answers, or the exit code and what it printed."""
    for _ in range(240):
        if proc.poll() is not None:
            return proc.returncode
        try:
            conn = http.client.HTTPConnection("127.0.0.1", PORT, timeout=5)
            conn.request("GET", "/")
            if conn.getresponse().status == 200:
                return None
        except OSError:
            time.sleep(0.25)
    proc.kill()
    raise SystemExit(f"the emulator neither started nor stopped: {open(log).read()[-500:]}")


def stop(proc):
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
        proc.wait()


def start(work, flags, requests=()):
    log = os.path.join(work, f"jar-{len(os.listdir(work))}.out")
    with open(log, "wb") as out:
        proc = subprocess.Popen([JAVA, "-Duser.language=en", "-Duser.country=US", "-jar", JAR,
                                 "--host", "127.0.0.1", "--port", str(PORT), *flags],
                                cwd=work, stdout=out, stderr=subprocess.STDOUT)
    code = wait(proc, log)
    lines = open(log, encoding="utf-8", errors="replace").read().splitlines()
    if code is not None:
        message = None
        for i, line in enumerate(lines):
            if line == "SEVERE: Exiting due to unexpected exception." and i + 1 < len(lines):
                message = lines[i + 1].split(": ", 1)[1]
                break
        return {"flags": flags, "exit": code, "message": message}
    try:
        banner = [line for line in lines if line.startswith(("Database Edition: ", "Database Mode: "))]
        answers = []
        for name, method, path, body in requests:
            conn = http.client.HTTPConnection("127.0.0.1", PORT, timeout=30)
            data = None if body is None else json.dumps(body)
            conn.request(method, path, body=data, headers={"Content-Type": "application/json",
                                                          "Authorization": "Bearer owner"})
            r = conn.getresponse()
            text = r.read().decode()
            answers.append({"name": name, "method": method, "path": path, "body": body,
                            "status": r.status, "message": json.loads(text)["error"].get("message")
                            if r.status >= 400 else None})
    finally:
        stop(proc)
    out = {"flags": flags, "started": banner}
    if requests:
        out["requests"] = answers
    return out


def main():
    with tempfile.TemporaryDirectory() as work:
        starts = [start(work, flags) for flags in STARTS]
        enforced = start(work, ["--require_indexes"], REQUESTS)
    json.dump({
        "oracle": "cloud-firestore-emulator v1.22.0 (sha256 9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)",
        "generated_by": "tools/oracle/startup_flags.py",
        "starts": starts,
        "index_enforcement_in_firestore_mode": enforced,
    }, sys.stdout, ensure_ascii=False, indent=1)
    print()


if __name__ == "__main__":
    main()
