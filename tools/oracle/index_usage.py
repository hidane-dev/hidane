#!/usr/bin/env python3
"""Which composite indexes does the official emulator report a query would need?
(`GET /emulator/v1/projects/{p}:indexUsage?database=…`, #36)

Usage (from the repository root), with a freshly started jar under an English locale:
    python3 -I tools/oracle/index_usage.py 127.0.0.1:8086 \\
        > crates/hidane/tests/fixtures/index_usage.json
    python3 -I tools/oracle/index_usage.py 127.0.0.1:8087 enterprise \\
        > crates/hidane/tests/fixtures/index_usage_enterprise.json

The official emulator keeps, per database, the composite indexes the queries run there would
need in production, with how many queries used each: what `:indexUsage` answers. This runs
each query of `crates/hidane/tests/fixtures/queries.json` (RunQuery) and of `aggregations.json`
(RunAggregationQuery) once, each in a database of its own seeded with the fixture's dataset, and
records what `:indexUsage` answers for that database; then a few sequences of queries in one
database (counts, order of the reports), and how the endpoint answers bad requests. The second
records the sequences alone, against a jar started with `--database-edition enterprise`.
`crates/hidane/tests/index_usage.rs` replays both. Stdlib only.
"""

import http.client
import json
import sys
import time
import urllib.parse
from pathlib import Path

HOST = sys.argv[1]
EDITION = sys.argv[2] if len(sys.argv) > 2 else "standard"
if EDITION not in ("standard", "enterprise"):
    raise SystemExit(f"the edition is standard or enterprise, not {EDITION}")
FIXTURES = Path(__file__).resolve().parents[2] / "crates/hidane/tests/fixtures"
PROJECT = f"index-{time.strftime('%H%M%S')}"
OWNER = {"Authorization": "Bearer owner"}
ORACLE = ("cloud-firestore-emulator v1.22.0 "
          "(sha256 9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)")


def call(method, path, body=None):
    c = http.client.HTTPConnection(*HOST.split(":"), timeout=30)
    c.request(method, path, body=None if body is None else json.dumps(body),
              headers={**OWNER, "Content-Type": "application/json"})
    r = c.getresponse()
    text = r.read().decode()
    return r.status, json.loads(text) if text.strip() else None


def db(n):
    return f"projects/{PROJECT}/databases/d{n}"


def fill(value, database):
    return json.loads(json.dumps(value).replace("{documents}", f"{database}/documents")
                      .replace("{database}", database))


def seed(database, dataset):
    writes = [{"update": {"name": f"{database}/documents/{d['name']}", "fields": fill(d["fields"], database)}}
              for d in dataset]
    for i in range(0, len(writes), 400):
        status, body = call("POST", f"/v1/{database}/documents:commit", {"writes": writes[i:i + 400]})
        assert status == 200, body


def usage(database):
    status, body = call("GET", f"/emulator/v1/projects/{PROJECT}:indexUsage?database="
                        + urllib.parse.quote(database, safe="/()"))
    text = json.dumps(body).replace(PROJECT, "{project}").replace(database, "{database}")
    return {"status": status, "body": json.loads(text)}


def reset():
    c = http.client.HTTPConnection(*HOST.split(":"), timeout=30)
    c.request("POST", "/reset")
    return c.getresponse().status


def listen(database, structured):
    """Adds `structured` as a Listen target over WebChannel and reads until it is current."""
    path = "/google.firestore.v1.Firestore/Listen/channel"
    message = {"database": database, "addTarget": {"targetId": 2, "query": {
        "parent": f"{database}/documents", "structuredQuery": structured}}}
    body = {"headers": "Authorization:Bearer owner\r\n", "count": 1, "ofs": 0, "req0___data__": json.dumps(message)}
    q = lambda params: urllib.parse.urlencode(params, safe="()")
    c = http.client.HTTPConnection(*HOST.split(":"), timeout=30)
    c.request("POST", path + "?" + q({"VER": 8, "database": database, "RID": 1000, "CVER": 22,
                                       "X-HTTP-Session-Id": "gsessionid", "zx": "x", "t": 1}),
              body=urllib.parse.urlencode(body), headers={"Content-Type": "application/x-www-form-urlencoded"})
    r = c.getresponse()
    first = r.read1(4096).decode()
    sid = json.loads(first.split("\n", 1)[1])[0][1][1]
    c = http.client.HTTPConnection(*HOST.split(":"), timeout=30)
    c.request("GET", path + "?" + q({"VER": 8, "database": database, "RID": "rpc", "SID": sid, "AID": 0,
                                      "CI": 0, "TYPE": "xmlhttp", "zx": "x", "t": 1}))
    r = c.getresponse()
    seen, deadline = "", time.monotonic() + 10
    while "CURRENT" not in seen and time.monotonic() < deadline:
        data = r.read1(65536)
        if not data:
            break
        seen += data.decode()
    c = http.client.HTTPConnection(*HOST.split(":"), timeout=30)
    c.request("POST", path + "?" + q({"VER": 8, "database": database, "SID": sid, "RID": 1001, "TYPE": "terminate", "zx": "x"}))
    c.getresponse().read()
    return "CURRENT" in seen


def main():
    queries = json.loads((FIXTURES / "queries.json").read_text())
    aggregations = json.loads((FIXTURES / "aggregations.json").read_text())
    n = 0
    cases = []
    for kind, fixture in (("query", queries), ("aggregation", aggregations)) if EDITION == "standard" else ():
        for case in fixture["cases"]:
            if case["outcome"]["status"] != "OK":
                continue
            n += 1
            database = db(n)
            seed(database, fixture["dataset"])
            parent = f"{database}/documents" + ("/" + case["parent"] if case["parent"] else "")
            if kind == "query":
                body = {"structuredQuery": fill(case["query"], database)}
                status, _ = call("POST", f"/v1/{parent}:runQuery", body)
            else:
                body = {"structuredAggregationQuery": {"structuredQuery": fill(case["query"], database),
                                                       "aggregations": case["aggregations"]}}
                status, _ = call("POST", f"/v1/{parent}:runAggregationQuery", body)
            assert status == 200, (case["name"], status)
            cases.append({"kind": kind, "name": case["name"], "usage": usage(database)})
    sequences = []
    iv = lambda i: {"integerValue": str(i)}
    ff = lambda f, op, v: {"fieldFilter": {"field": {"fieldPath": f}, "op": op, "value": v}}
    AND = lambda *fs: {"compositeFilter": {"op": "AND", "filters": list(fs)}}
    eq_range = {"from": [{"collectionId": "c"}], "where": AND(ff("a", "EQUAL", iv(1)), ff("b", "GREATER_THAN", iv(0)))}
    eq_desc = {"from": [{"collectionId": "c"}], "where": ff("a", "EQUAL", iv(1)),
               "orderBy": [{"field": {"fieldPath": "b"}, "direction": "DESCENDING"}]}
    contains = {"from": [{"collectionId": "d"}], "where": ff("tags", "ARRAY_CONTAINS", iv(1)),
                "orderBy": [{"field": {"fieldPath": "a"}}]}
    plain = {"from": [{"collectionId": "c"}], "where": ff("a", "EQUAL", iv(1))}
    group = {"from": [{"collectionId": "c", "allDescendants": True}]}
    group_eq = {"from": [{"collectionId": "c", "allDescendants": True}], "where": ff("a", "EQUAL", iv(1))}

    def steps(name, *requests):
        """Runs `requests` in a database of their own holding one document: each a REST call
        (`{"rpc": …, "body": …}`, `{commitTime}` for the time the document was written), a
        listener's query (`{"listen": …}`) or a reset."""
        nonlocal n
        n += 1
        database = db(n)
        _, committed = call("POST", f"/v1/{database}/documents:commit", {"writes": [
            {"update": {"name": f"{database}/documents/c/x", "fields": {"a": iv(1), "b": iv(2)}}}]})
        answers = []
        for request in requests:
            if "rpc" in request:
                body = json.loads(json.dumps(request["body"]).replace("{commitTime}", committed["commitTime"]))
                answers.append(call("POST", f"/v1/{database}/documents:{request['rpc']}", body)[0])
            elif "listen" in request:
                answers.append(listen(database, request["listen"]))
            else:
                answers.append(reset())
        sequences.append({"name": name, "requests": list(requests), "answers": answers, "usage": usage(database)})

    def query(q, **more):
        return {"rpc": "runQuery", "body": {"structuredQuery": q, **more}}

    steps("three queries, the first again", query(eq_range), query(eq_desc), query(contains),
          query(eq_range), query(plain))
    steps("the second first", query(contains), query(eq_desc), query(eq_range))
    steps("a collection group without a filter, and with one", query(group), query(group_eq))
    steps("a query that fails a later check", query({**eq_range, "limit": -1}))
    steps("a query in a transaction it begins", query(eq_range, newTransaction={"readWrite": {}}))
    steps("a query at a read time", query(eq_range, readTime="{commitTime}"))
    steps("an aggregation of the query", {"rpc": "runAggregationQuery", "body": {
        "structuredAggregationQuery": {"structuredQuery": eq_range, "aggregations": [{"count": {}}]}}})
    steps("a listener's query", {"listen": eq_range})
    steps("cleared by a reset", query(eq_range), {"reset": True})

    if EDITION != "standard":
        json.dump({"oracle": ORACLE, "generated_by": "tools/oracle/index_usage.py", "edition": EDITION,
                   "sequences": sequences}, sys.stdout, ensure_ascii=False, indent=1)
        print()
        return

    def endpoint(name, path, method="GET"):
        c = http.client.HTTPConnection(*HOST.split(":"), timeout=30)
        c.request(method, path.replace("{project}", PROJECT), headers=OWNER)
        r = c.getresponse()
        endpoints.append({"name": name, "method": method, "path": path, "status": r.status,
                          "content_type": r.getheader("Content-Type"),
                          "body": r.read().decode().replace(PROJECT, "{project}")})
    endpoints = []
    # One report, printed as the endpoint prints it: a field path in backquotes, an
    # array-contains field and a descending order.
    used = "projects/{project}/databases/used"
    printed = {"from": [{"collectionId": "c"}],
               "where": AND(ff("`a b`", "EQUAL", iv(1)), ff("tags", "ARRAY_CONTAINS", iv(1))),
               "orderBy": [{"field": {"fieldPath": "c"}, "direction": "DESCENDING"}]}
    assert call("POST", f"/v1/{used.replace('{project}', PROJECT)}/documents:runQuery",
                {"structuredQuery": printed})[0] == 200
    endpoint("without a database", "/emulator/v1/projects/{project}:indexUsage")
    endpoint("an empty database", "/emulator/v1/projects/{project}:indexUsage?database=")
    endpoint("a database that lacks", "/emulator/v1/projects/{project}:indexUsage?database=projects/x")
    endpoint("an invalid project ID", "/emulator/v1/projects/{project}:indexUsage?database=projects/P1/databases/(default)")
    endpoint("a database never used", "/emulator/v1/projects/{project}:indexUsage?database=projects/{project}/databases/never")
    endpoint("a report", f"/emulator/v1/projects/{{project}}:indexUsage?database={used}")
    endpoint("another project in the path", f"/emulator/v1/projects/other:indexUsage?database={used}")
    endpoint("an encoded database", "/emulator/v1/projects/{project}:indexUsage?database="
             + urllib.parse.quote(used, safe="{}"))
    endpoint("the documents of a database", f"/emulator/v1/projects/{{project}}:indexUsage?database={used}/documents")
    endpoint("an empty project", f"/emulator/v1/projects/:indexUsage?database={used}")
    endpoint("a database path", "/emulator/v1/projects/{project}/databases/d1:indexUsage")
    endpoint("POST", "/emulator/v1/projects/{project}:indexUsage?database=projects/{project}/databases/d1", "POST")

    json.dump({
        "oracle": ORACLE,
        "generated_by": "tools/oracle/index_usage.py",
        "cases": cases,
        "sequences": sequences,
        "endpoints": endpoints,
    }, sys.stdout, ensure_ascii=False, indent=1)
    print()


main()
