"""How does the official emulator run transactions? (#17)

Usage (from the repository root, `grpcurl` on PATH):
    python3 -I tools/oracle/transactions.py 127.0.0.1:8086 > crates/hidane/tests/fixtures/transactions.json

The scenarios are data: the fixture keeps each scenario's steps next to the official outcome, and
`crates/hidane/tests/transactions.rs` runs the same steps against hidane and compares. Each
scenario uses its own project, so transaction IDs (a per-database counter) start at 1.

Step ops (documents are relative to the project's database, e.g. "c/d"):
    begin       {as, options?}                    BeginTransaction (REST options JSON)
    read        {txn?, new?, documents}           BatchGetDocuments with transaction / newTransaction
    get         {txn?, document}                  GetDocument (with txn over gRPC through grpcurl:
                                                  REST GET ?transaction= hangs)
    list        {txn?, collection, pageSize?}     ListDocuments (same)
    query       {txn?, new?, collection, group?}  RunQuery over a whole collection (or group)
    commit      {txn?, writes}                    Commit; writes are {update | delete | verify,
                                                  n?, exists?, reserved?, mask?, increment?}
    batchWrite  {writes}                          BatchWrite
    rollback    {txn?, raw?}                      Rollback (raw: literal base64 transaction bytes)
    reset                                         POST /reset (clears every project)
    sleep       {seconds}
    wait        {for: [labels]}                   joins background steps
Any step may set `background: true` (runs on a thread) and `compare` ("status" compares the
status only, "codes" everything but messages, "none" records without comparing: see
docs/parity-exceptions.md). A scenario with
`slow: true` is recorded here but not replayed by the test (it takes over a minute).

Outcomes record the status, the message (with the project ID replaced by `{project}`), the
elapsed time rounded to 0.5 s (lock waits show up as 2.0) and a small result, never absolute
timestamps. Transaction IDs are recorded but not compared: the official emulator also spends an
ID on every non-transactional request, so the values differ (docs/parity-exceptions.md).
"""

import json
import math
import re
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request

HOST = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1:8080"
RUN = time.strftime("%H%M%S")


def scenario(name, steps, slow=False):
    s = {"name": name, "steps": steps}
    if slow:
        s["slow"] = True
    return s


def step(op, label, **fields):
    return {"op": op, "label": label, **fields}


def up(name, n=None, **extra):
    w = {"update": name}
    if n is not None:
        w["n"] = n
    w.update(extra)
    return w


SEED_ONE = step("commit", "seed c/d", writes=[up("c/d", 1)])
SEED_AB = step("commit", "seed c/a and c/b", writes=[up("c/a", 1), up("c/b", 1)])

SCENARIOS = [
    scenario("an outside write waits for a read lock and proceeds when the transaction commits", [
        SEED_ONE,
        step("begin", "begin T", **{"as": "T"}),
        step("read", "T reads c/d", txn="T", documents=["c/d"]),
        step("commit", "outside write to c/d", writes=[up("c/d", 2)], background=True),
        step("sleep", "sleep", seconds=0.5),
        step("commit", "T writes c/d", txn="T", writes=[up("c/d", 3)]),
        step("wait", "wait", **{"for": ["outside write to c/d"]}),
        step("get", "final c/d", document="c/d"),
    ]),
    scenario("an outside write gives up after the lock timeout", [
        SEED_ONE,
        step("begin", "begin T", **{"as": "T"}),
        step("read", "T reads c/d", txn="T", documents=["c/d"]),
        step("commit", "outside write to c/d", writes=[up("c/d", 2)]),
        step("get", "plain get of c/d does not wait", document="c/d"),
        step("commit", "T writes c/d", txn="T", writes=[up("c/d", 3)]),
        step("get", "final c/d", document="c/d"),
    ]),
    scenario("updates, deletes and creates all wait for read locks", [
        SEED_ONE,
        step("begin", "begin T", **{"as": "T"}),
        step("read", "T reads c/d and missing c/m", txn="T", documents=["c/d", "c/m"]),
        step("commit", "outside delete of c/d", writes=[{"delete": "c/d"}]),
        step("commit", "outside create of c/m", writes=[up("c/m", 1, exists=False)]),
        step("commit", "outside write to unread c/x", writes=[up("c/x", 1)]),
        step("commit", "T writes c/m", txn="T", writes=[up("c/m", 2)]),
    ]),
    scenario("rollback releases the locks and ends the transaction", [
        SEED_ONE,
        step("begin", "begin T", **{"as": "T"}),
        step("read", "T reads c/d", txn="T", documents=["c/d"]),
        step("commit", "outside write to c/d", writes=[up("c/d", 2)], background=True),
        step("sleep", "sleep", seconds=0.5),
        step("rollback", "rollback T", txn="T"),
        step("wait", "wait", **{"for": ["outside write to c/d"]}),
        step("rollback", "rollback T again", txn="T"),
        step("read", "T reads after rollback", txn="T", documents=["c/d"]),
        step("commit", "T commits after rollback", txn="T", writes=[up("c/d", 3)]),
        step("get", "final c/d", document="c/d"),
    ]),
    scenario("two transactions that read and then write the same document", [
        SEED_ONE,
        step("begin", "begin T1", **{"as": "T1"}),
        step("begin", "begin T2", **{"as": "T2"}),
        step("read", "T1 reads c/d", txn="T1", documents=["c/d"]),
        step("read", "T2 reads c/d", txn="T2", documents=["c/d"]),
        step("commit", "T1 writes c/d", txn="T1", writes=[up("c/d", 10)], background=True),
        step("sleep", "sleep", seconds=0.5),
        step("commit", "T2 writes c/d", txn="T2", writes=[up("c/d", 20)], background=True),
        step("wait", "wait", **{"for": ["T1 writes c/d", "T2 writes c/d"]}),
        step("read", "T1 reads after its lock timeout", txn="T1", documents=["c/d"]),
        step("rollback", "rollback T1 after its lock timeout", txn="T1"),
        step("get", "final c/d", document="c/d"),
    ]),
    scenario("a transaction does not wait for its own locks", [
        SEED_AB,
        step("begin", "begin T", **{"as": "T"}),
        step("read", "T reads c/a", txn="T", documents=["c/a"]),
        step("list", "T lists c", txn="T", collection="c"),
        step("commit", "T writes c/a, c/b and c/new", txn="T", writes=[up("c/a", 2), up("c/b", 2), up("c/new", 2)]),
    ]),
    scenario("a read-only transaction takes no locks and reads one snapshot", [
        SEED_ONE,
        step("begin", "begin R", **{"as": "R", "options": {"readOnly": {}}}),
        step("read", "R reads c/d", txn="R", documents=["c/d"]),
        step("commit", "outside write to c/d", writes=[up("c/d", 2)]),
        step("read", "R reads c/d again", txn="R", documents=["c/d"]),
        step("get", "R gets c/d", txn="R", document="c/d"),
        step("commit", "R tries to write", txn="R", writes=[up("c/d", 3)]),
        step("read", "R is still open", txn="R", documents=["c/d"]),
        step("commit", "R commits nothing", txn="R", writes=[]),
        step("read", "R reads after its commit", txn="R", documents=["c/d"]),
    ]),
    scenario("a read-write transaction reads the latest version of each document", [
        SEED_AB,
        step("begin", "begin T", **{"as": "T"}),
        step("read", "T reads c/a", txn="T", documents=["c/a"]),
        step("commit", "outside write to c/b", writes=[up("c/b", 2)]),
        step("read", "T reads c/b", txn="T", documents=["c/b"]),
    ]),
    scenario("a transaction is single-use", [
        step("begin", "begin T", **{"as": "T"}),
        step("commit", "T commits", txn="T", writes=[up("c/d", 1)]),
        step("commit", "T commits again", txn="T", writes=[up("c/d", 2)]),
        step("read", "T reads after its commit", txn="T", documents=["c/d"]),
        step("rollback", "rollback after commit", txn="T"),
        step("begin", "begin E", **{"as": "E"}),
        step("commit", "E commits nothing", txn="E", writes=[]),
        step("read", "E reads after its empty commit", txn="E", documents=["c/d"]),
    ]),
    scenario("a failed precondition ends the transaction, an invalid write does not", [
        SEED_AB,
        step("begin", "begin T", **{"as": "T"}),
        step("commit", "T writes a reserved field name", txn="T", writes=[up("c/a", 2, reserved=True)]),
        step("read", "T is still open", txn="T", documents=["c/a"]),
        step("commit", "T creates existing c/a", txn="T", writes=[up("c/a", 2, exists=False)], compare="status"),
        step("read", "T reads after the failed precondition", txn="T", documents=["c/a"]),
        step("begin", "begin L", **{"as": "L"}),
        step("read", "L reads c/b", txn="L", documents=["c/b"]),
        step("commit", "invalid outside write to locked c/b does not wait", writes=[up("c/b", 2, reserved=True)]),
        step("commit", "outside create of locked existing c/b waits", writes=[up("c/b", 2, exists=False)]),
    ]),
    scenario("a commit waits for every target and stays atomic", [
        SEED_ONE,
        step("begin", "begin T", **{"as": "T"}),
        step("read", "T reads c/d", txn="T", documents=["c/d"]),
        step("commit", "outside write to c/d and c/free", writes=[up("c/d", 2), up("c/free", 2)]),
        step("get", "c/free was not written", document="c/free"),
    ]),
    scenario("BatchWrite waits for each locked write in turn", [
        SEED_AB,
        step("begin", "begin T", **{"as": "T"}),
        step("read", "T reads c/a and c/b", txn="T", documents=["c/a", "c/b"]),
        step("batchWrite", "batch write c/a, c/b and c/c", writes=[up("c/a", 2), up("c/b", 2), up("c/c", 2)]),
        step("get", "c/c was written", document="c/c"),
    ]),
    scenario("GetDocument in a transaction locks the document", [
        SEED_ONE,
        step("begin", "begin T", **{"as": "T"}),
        step("get", "T gets c/d", txn="T", document="c/d"),
        step("get", "T gets missing c/m", txn="T", document="c/m"),
        step("commit", "outside write to c/d", writes=[up("c/d", 2)]),
        step("commit", "outside create of c/m", writes=[up("c/m", 2)]),
    ]),
    scenario("ListDocuments in a transaction locks every collection with that ID", [
        SEED_AB,
        step("begin", "begin T", **{"as": "T"}),
        step("list", "T lists one document of c", txn="T", collection="c", pageSize=1),
        step("commit", "outside write to listed c/a", writes=[up("c/a", 2)]),
        step("commit", "outside write to c/b on the next page", writes=[up("c/b", 2)]),
        step("commit", "outside create of c/new", writes=[up("c/new", 2)]),
        step("commit", "outside create of x/y/c/z", writes=[up("x/y/c/z", 2)]),
        step("commit", "outside create of c/a/sub/x", writes=[up("c/a/sub/x", 2)]),
        step("commit", "outside create of other/x", writes=[up("other/x", 2)]),
    ]),
    scenario("RunQuery in a transaction locks every collection with that ID", [
        SEED_AB,
        step("begin", "begin T", **{"as": "T"}),
        step("query", "T queries c", txn="T", collection="c"),
        step("commit", "outside write to c/a", writes=[up("c/a", 2)]),
        step("commit", "outside create of c/new", writes=[up("c/new", 2)]),
        step("commit", "outside create of x/y/c/z", writes=[up("x/y/c/z", 2)]),
        step("commit", "outside create of c/a/sub/x", writes=[up("c/a/sub/x", 2)]),
        step("commit", "T creates c/mine", txn="T", writes=[up("c/mine", 2)]),
    ]),
    scenario("a collection-group RunQuery in a transaction locks the collection ID", [
        SEED_AB,
        step("begin", "begin T", **{"as": "T"}),
        step("query", "T queries group c", txn="T", collection="c", group=True),
        step("commit", "outside create of x/y/c/z", writes=[up("x/y/c/z", 2)]),
        step("commit", "outside create of d/q", writes=[up("d/q", 2)]),
        step("rollback", "rollback T", txn="T"),
        step("commit", "outside create of x/y/c/z after the rollback", writes=[up("x/y/c/z", 2)]),
    ]),
    scenario("RunQuery can start a transaction", [
        SEED_AB,
        step("query", "new read-write transaction queries c", new={"readWrite": {}}, collection="c", **{"as": "T"}),
        step("commit", "outside write to c/a", writes=[up("c/a", 2)]),
        step("query", "new transaction queries an empty collection", new={"readWrite": {}}, collection="zz", **{"as": "E"}),
        step("begin", "begin R", **{"as": "R", "options": {"readOnly": {}}}),
        step("commit", "T writes c/a", txn="T", writes=[up("c/a", 3)]),
        step("query", "read-only R queries c and sees its snapshot", txn="R", collection="c"),
    ]),
    scenario("BatchGetDocuments can start a transaction", [
        SEED_ONE,
        step("read", "new read-write transaction reads c/d", new={"readWrite": {}}, documents=["c/d"], **{"as": "T"}),
        step("commit", "outside write to c/d", writes=[up("c/d", 2)]),
        step("read", "new read-only transaction reads c/d", new={"readOnly": {}}, documents=["c/d"], **{"as": "R"}),
        step("read", "new transaction reads nothing", new={"readWrite": {}}, documents=[], **{"as": "E"}),
        step("commit", "T writes c/d", txn="T", writes=[up("c/d", 3)]),
    ]),
    scenario("a verify checks a precondition without writing", [
        SEED_AB,
        step("commit", "verify existing c/a", writes=[{"verify": "c/a", "exists": True}]),
        step("commit", "verify missing c/m exists", writes=[{"verify": "c/m", "exists": True}], compare="status"),
        step("commit", "verify missing c/m does not exist", writes=[{"verify": "c/m", "exists": False}]),
        step("get", "c/m was not created", document="c/m"),
        step("commit", "verify without a precondition", writes=[{"verify": "c/a"}]),
        step("commit", "verify with a mask", writes=[{"verify": "c/a", "exists": True, "mask": ["x"]}]),
        step("commit", "verify with a transform", writes=[{"verify": "c/a", "exists": True, "increment": "n"}]),
        step("commit", "verify then write c/a", writes=[{"verify": "c/a", "exists": True}, up("c/a", 2)]),
        step("commit", "write then verify c/a", writes=[up("c/a", 2), {"verify": "c/a", "exists": True}]),
        step("commit", "verify c/a twice", writes=[{"verify": "c/a", "exists": True}, {"verify": "c/a", "exists": True}]),
        step("commit", "failing verify and a write", writes=[{"verify": "c/m", "exists": True}, up("c/x", 1)], compare="status"),
        step("get", "c/x was not written", document="c/x"),
        step("begin", "begin T", **{"as": "T"}),
        step("read", "T reads c/b", txn="T", documents=["c/b"]),
        step("commit", "outside verify of locked c/b", writes=[{"verify": "c/b", "exists": True}]),
        step("commit", "T verifies c/b", txn="T", writes=[{"verify": "c/b", "exists": True}]),
        step("batchWrite", "batch write of two verifies", writes=[{"verify": "c/a", "exists": True}, {"verify": "c/m", "exists": True}], compare="codes"),
        step("batchWrite", "batch write with an invalid write", writes=[up("c/r", 1, reserved=True), up("c/ok", 1)]),
        step("get", "c/ok was not written", document="c/ok"),
    ]),
    scenario("the optimistic concurrency mode still locks", [
        SEED_ONE,
        step("begin", "begin T optimistic", **{"as": "T", "options": {"readWrite": {"concurrencyMode": "OPTIMISTIC"}}}),
        step("read", "T reads c/d", txn="T", documents=["c/d"]),
        step("commit", "outside write to c/d", writes=[up("c/d", 2)]),
        step("commit", "T writes c/d", txn="T", writes=[up("c/d", 3)]),
    ]),
    scenario("BeginTransaction and Rollback reject bad input", [
        step("begin", "begin with no options", **{"as": "T"}),
        step("begin", "read-only at a time before retention", **{"as": "X", "options": {"readOnly": {"readTime": "2020-01-01T00:00:00Z"}}}),
        step("begin", "read-only at a future time", **{"as": "Y", "options": {"readOnly": {"readTime": "2999-01-01T00:00:00Z"}}}),
        step("begin", "retry of T", **{"as": "T2", "options": {"readWrite": {"retryTransaction": "{T}"}}}),
        step("begin", "retry of malformed bytes", **{"as": "Z", "options": {"readWrite": {"retryTransaction": "AAAA"}}}),
        step("rollback", "rollback of malformed bytes", raw="AAAA"),
        step("rollback", "rollback without a transaction", raw=""),
        step("commit", "commit with malformed bytes", raw="AAAA", writes=[]),
    ]),
    scenario("reset forgets open transactions and their locks", [
        SEED_ONE,
        step("begin", "begin T", **{"as": "T"}),
        step("read", "T reads c/d", txn="T", documents=["c/d"]),
        step("reset", "reset"),
        step("commit", "outside write to c/d after reset", writes=[up("c/d", 2)]),
        # The official emulator fails with UNKNOWN and no message here (docs/parity-exceptions.md).
        step("commit", "T commits after reset", txn="T", writes=[up("c/d", 3)], compare="none"),
        step("begin", "begin after reset", **{"as": "U"}),
    ]),
    scenario("an idle transaction expires after 60 seconds", [
        SEED_ONE,
        step("begin", "begin T", **{"as": "T"}),
        step("read", "T reads c/d", txn="T", documents=["c/d"]),
        step("sleep", "sleep 45 s", seconds=45),
        step("begin", "begin K", **{"as": "K"}),
        step("read", "K reads c/k", txn="K", documents=["c/k"]),
        step("sleep", "sleep 17 s", seconds=17),
        step("commit", "outside write to c/d after 62 s", writes=[up("c/d", 2)]),
        step("commit", "outside write to c/k 17 s after K's read", writes=[up("c/k", 2)]),
        step("commit", "T commits after expiring", txn="T", writes=[up("c/d", 3)]),
    ], slow=True),
]


# --- running steps against the official emulator ---------------------------------------------

def rest(method, path, body=None, root=False):
    url = f"http://{HOST}/{path}" if root else f"http://{HOST}/v1/{path}"
    req = urllib.request.Request(url, method=method, data=None if body is None else json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json", "Authorization": "Bearer owner"})
    try:
        with urllib.request.urlopen(req, timeout=120) as res:
            raw = res.read()
            try:
                return "OK", "", json.loads(raw) if raw else None
            except ValueError:
                return "OK", "", raw.decode()
    except urllib.error.HTTPError as err:
        raw = err.read()
        try:
            e = json.loads(raw)["error"]
            return e.get("status", str(err.code)), e.get("message", ""), None
        except (ValueError, KeyError):
            return str(err.code), raw.decode(errors="replace"), None


def grpc_overhead():
    """How long starting grpcurl takes, subtracted from the elapsed time of gRPC steps."""
    times = []
    for _ in range(3):
        start = time.monotonic()
        grpc("GetDocument", {"name": "projects/overhead/databases/(default)/documents/c/d"})
        times.append(time.monotonic() - start)
    return min(times)


def grpc(method, body):
    out = subprocess.run(
        ["grpcurl", "-plaintext", "-import-path", "proto", "-proto", "google/firestore/v1/firestore.proto",
         "-H", "authorization: Bearer owner", "-d", json.dumps(body), HOST, f"google.firestore.v1.Firestore/{method}"],
        capture_output=True, text=True, timeout=120)
    text = out.stdout + out.stderr
    m = re.search(r"Code: (\w+)\n\s*Message: (.*)", text)
    if m:
        return re.sub(r"(?<!^)(?=[A-Z])", "_", m.group(1)).upper(), m.group(2).strip(), None
    return "OK", "", json.loads(out.stdout) if out.stdout.strip() else None


def rest_write(base, w):
    if "delete" in w:
        out = {"delete": f"{base}/{w['delete']}"}
    elif "verify" in w:
        out = {"verify": f"{base}/{w['verify']}"}
    else:
        fields = {}
        if "n" in w:
            fields["n"] = {"integerValue": str(w["n"])}
        if w.get("reserved"):
            fields["__x__"] = {"nullValue": None}
        out = {"update": {"name": f"{base}/{w['update']}", "fields": fields}}
    if "exists" in w:
        out["currentDocument"] = {"exists": w["exists"]}
    if "mask" in w:
        out["updateMask"] = {"fieldPaths": w["mask"]}
    if "increment" in w:
        out["updateTransforms"] = [{"fieldPath": w["increment"], "increment": {"integerValue": "1"}}]
    return out


def doc_result(base, doc):
    n = doc.get("fields", {}).get("n", {}).get("integerValue")
    return {"name": doc["name"][len(base) + 1:], "n": None if n is None else int(n)}


def run(sc, project):
    db = f"projects/{project}/databases/(default)"
    base = f"{db}/documents"
    txns = {}
    outcomes = {}
    threads = {}

    def fill(text):
        return re.sub(r"\{(\w+)\}", lambda m: txns.get(m.group(1), m.group(0)), text)

    def execute(st):
        op = st["op"]
        txn = txns.get(st["txn"]) if "txn" in st else None
        if op == "begin":
            body = {"options": json.loads(fill(json.dumps(st["options"])))} if "options" in st else {}
            status, message, res = rest("POST", f"{base}:beginTransaction", body)
            if res:
                txns[st["as"]] = res["transaction"]
            return status, message, res
        if op == "read":
            body = {"documents": [f"{base}/{d}" for d in st["documents"]]}
            if txn:
                body["transaction"] = txn
            if "new" in st:
                body["newTransaction"] = st["new"]
            status, message, res = rest("POST", f"{base}:batchGet", body)
            if res is None:
                return status, message, None
            out = []
            for r in res:
                if "transaction" in r:
                    txns[st["as"]] = r["transaction"]
                    out.append({"transaction": r["transaction"]})
                elif "found" in r:
                    out.append({"found": doc_result(base, r["found"])})
                elif "missing" in r:
                    out.append({"missing": r["missing"][len(base) + 1:]})
            return status, message, out
        if op == "get":
            if not txn:
                status, message, res = rest("GET", f"{base}/{st['document']}")
            else:
                status, message, res = grpc("GetDocument", {"name": f"{base}/{st['document']}", "transaction": txn})
            return status, message, doc_result(base, res) if res else None
        if op == "list":
            if not txn:
                query = f"?pageSize={st['pageSize']}" if "pageSize" in st else ""
                status, message, res = rest("GET", f"{base}/{st['collection']}{query}")
            else:
                body = {"parent": base, "collectionId": st["collection"], "transaction": txn}
                if "pageSize" in st:
                    body["pageSize"] = st["pageSize"]
                status, message, res = grpc("ListDocuments", body)
            return status, message, [doc_result(base, d) for d in res.get("documents", [])] if res is not None else None
        if op == "query":
            body = {"structuredQuery": {"from": [{"collectionId": st["collection"], "allDescendants": st.get("group", False)}]}}
            if txn:
                body["transaction"] = txn
            if "new" in st:
                body["newTransaction"] = st["new"]
            status, message, res = rest("POST", f"{base}:runQuery", body)
            if res is None:
                return status, message, None
            out = []
            for r in res:
                if "transaction" in r:
                    txns[st["as"]] = r["transaction"]
                    out.append({"transaction": r["transaction"]})
                if "document" in r:
                    out.append({"document": doc_result(base, r["document"])})
            return status, message, out
        if op == "commit":
            body = {"writes": [rest_write(base, w) for w in st["writes"]]}
            if "raw" in st:
                body["transaction"] = st["raw"]
            elif txn:
                body["transaction"] = txn
            status, message, res = rest("POST", f"{base}:commit", body)
            return status, message, None
        if op == "batchWrite":
            body = {"writes": [rest_write(base, w) for w in st["writes"]]}
            status, message, res = rest("POST", f"{base}:batchWrite", body)
            if res is None:
                return status, message, None
            return status, message, [
                {"status": re.sub(r"(?<!^)(?=[A-Z])", "_", CODES[s.get("code", 0)]).upper(), "message": s.get("message", "")}
                for s in res["status"]]
        if op == "rollback":
            body = {"transaction": st["raw"]} if "raw" in st else {"transaction": txn}
            status, message, res = rest("POST", f"{base}:rollback", body)
            return status, message, None
        if op == "reset":
            status, message, res = rest("POST", "reset", root=True)
            return status, message, None
        raise ValueError(op)

    def timed(st):
        start = time.monotonic()
        status, message, result = execute(st)
        elapsed = time.monotonic() - start
        if st["op"] in ("get", "list") and "txn" in st:
            # grpcurl's start-up time varies by a few hundred milliseconds; round down so the
            # leftover never reaches the next half second (these reads never wait for locks).
            rounded = math.floor(max(0.0, elapsed - GRPCURL_OVERHEAD) * 2) / 2
        else:
            rounded = round(elapsed * 2) / 2
        message = message.replace(project, "{project}")
        outcome = {"status": status, "message": message, "elapsed": rounded}
        if result is not None:
            outcome["result"] = json.loads(json.dumps(result).replace(project, "{project}"))
        outcomes[st["label"]] = outcome

    for st in sc["steps"]:
        if st["op"] == "sleep":
            time.sleep(st["seconds"])
        elif st["op"] == "wait":
            for label in st["for"]:
                threads.pop(label).join()
        elif st.get("background"):
            t = threading.Thread(target=timed, args=(st,))
            t.start()
            threads[st["label"]] = t
        else:
            timed(st)
    for t in threads.values():
        t.join()
    return [dict(st, outcome=outcomes[st["label"]]) if st["label"] in outcomes else st for st in sc["steps"]]


CODES = ["Ok", "Cancelled", "Unknown", "InvalidArgument", "DeadlineExceeded", "NotFound", "AlreadyExists",
         "PermissionDenied", "ResourceExhausted", "FailedPrecondition", "Aborted", "OutOfRange",
         "Unimplemented", "Internal", "Unavailable", "DataLoss", "Unauthenticated"]

GRPCURL_OVERHEAD = grpc_overhead()
recorded = []
for i, sc in enumerate(SCENARIOS):
    print(f"{sc['name']}", file=sys.stderr, flush=True)
    recorded.append(dict(sc, steps=run(sc, f"txn-{RUN}-{i}")))

json.dump({
    "oracle": "cloud-firestore-emulator v1.22.0 (sha256 9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)",
    "generated_by": "tools/oracle/transactions.py",
    "scenarios": recorded,
}, sys.stdout, ensure_ascii=False, indent=1)
print()
