#!/usr/bin/env python3
"""Which queries does the official emulator in Datastore mode serve with `--require_indexes`?
(#81)

Usage (from the repository root):
    python3 -I tools/oracle/datastore_indexes.py <cloud-firestore-emulator.jar> [java] [port] \\
        > crates/hidane/tests/fixtures/datastore_indexes.json

The script starts the jar once per run, on `port` (default 8210), from a temporary directory and
under an English locale, always with `--database-mode datastore-mode`. Each run names its flags,
with `{dir}` for that directory, and the `index.yaml` it writes there first (`index_file`, in the
format of https://cloud.google.com/datastore/docs/tools/indexconfig; `null` writes none):

- `enforced`: `--require_indexes` with an index file (`INDEXES`). Over a small dataset, runQuery
  and runAggregationQuery, structured and GQL, with queries that need no index, an index the file
  defines, or one it does not: kinds and ancestors, the key, equalities in their order,
  inequalities, orders and their directions, projections, `distinctOn`, `OR`, sums and averages,
  namespaces, other projects and transactions; and queries an index serves that are refused
  after the index is found.
- `refused`: `--require_indexes` with a file that defines no index, against queries refused for
  another reason: which are refused before the index is looked for, which after.
- `not enforced`: `--index_file` alone, with a file the emulator cannot read: it is not read.
- `no index file`: `--require_indexes` alone.
- `file read late`: `--require_indexes` with a file that does not exist yet; steps write it
  (`write_index_file`) between requests, showing when the emulator reads it.
- one run per `VARIANTS` entry: spellings and mistakes in the file, each answered by the same few
  requests (`PROBES`).

Each request is recorded with its answer as in `datastore_queries.json` (server times, versions,
transaction IDs and cursors masked). `crates/hidane/tests/datastore.rs` replays every run against
hidane started with the same flags. Stdlib only.
"""

import http.client
import json
import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import datastore_queries as base  # noqa: E402

JAR = sys.argv[1]
JAVA = sys.argv[2] if len(sys.argv) > 2 else "java"
PORT = int(sys.argv[3]) if len(sys.argv) > 3 else 8210
base.HOST = f"127.0.0.1:{PORT}"
base.PROJECT = f"dsindex-{time.strftime('%H%M%S')}"
P = base.P
key, I, S, ENT = base.key, base.I, base.S, base.ENT
kind, pf, AND, OR, asc, desc = base.kind, base.pf, base.AND, base.OR, base.asc, base.desc

DATASET = [
    (key(("P", f"p{i}")), {"a": I(i % 3), "b": S("xyz"[i % 3]), "c": I(i), "d": I(i % 2), "B": I(i % 2)})
    for i in range(6)
]
DATASET += [
    (key(("P", "m1")), {"a": I(1), "m": ENT(x=I(1))}),
    (key(("P", "p0"), ("C", "c1")), {"a": I(1), "b": S("y")}),
    (key(("C", "top")), {"a": I(2)}),
    (key(("P", "n1"), ns="ns1"), {"a": I(1), "b": S("y")}),
    (key(("Q", "q1")), {"a": I(1)}),
]

INDEXES = """\
indexes:
# Kind P with no property: what a query on the kind alone needs.
- kind: P
- kind: C
  ancestor: yes
- kind: Q
  properties:
  - name: __key__
- kind: __kind__
- kind: P
  properties:
  - name: a
  - name: b
- kind: P
  properties:
  - name: b
  - name: a
    direction: asc
  - name: c
    direction: desc
- kind: P
  properties:
  - name: c
  - name: a
- kind: P
  properties:
  - name: a
  - name: a
- kind: P
  properties:
  - name: B
  - name: a
- kind: P
  properties:
  - name: __key__
  - name: d
- kind: P
  properties:
  - name: a
  - name: c
    direction: desc
- kind: P
  properties:
  - name: d
  - name: __key__
    direction: desc
- kind: P
  properties:
  - name: m.x
  - name: a
- kind: P
  ancestor: yes
  properties:
  - name: a
  - name: b
- kind: P
  properties:
  - name: b
  - name: d
  - name: a
# Kind R: what a query does not order by follows its last order's direction.
- kind: R
  properties:
  - name: t
    direction: desc
  - name: a
    direction: desc
- kind: R
  properties:
  - name: u
    direction: desc
  - name: a
"""

ANCESTOR = pf("__key__", "HAS_ANCESTOR", {"keyValue": key(("P", "p0"))})
KEY = {"keyValue": key(("P", "p1"))}
EQ = lambda p, v: pf(p, "EQUAL", v)
GT = lambda p, v: pf(p, "GREATER_THAN", v)
SUM = lambda p: {"sum": {"property": {"name": p}}}
AVG = lambda p: {"avg": {"property": {"name": p}}}
COUNT = {"count": {}}
PROJ = lambda *names: [{"property": {"name": n}} for n in names]
DISTINCT = lambda *names: [{"name": n} for n in names]
PK = {"kind": kind("P")}

CASES = []


def case(name, verb, body, project=P, defines=None):
    c = {"name": name, "method": "POST", "path": f"/v1/projects/{project}:{verb}", "headers": base.JSON,
         "body": body, "compare": "exact"}
    if defines:
        c["defines"] = defines
    CASES.append(c)


def query(name, q, **more):
    case(name, "runQuery", {"query": q, **more})


def agg(name, nested, *aggregations, **more):
    case(name, "runAggregationQuery", {"aggregationQuery": {"nestedQuery": nested, "aggregations": list(aggregations)}, **more})


def gql(name, text, verb="runQuery"):
    case(name, verb, {"gqlQuery": {"queryString": text, "allowLiterals": True}})


def enforced():
    # --- kinds, ancestors and the key -------------------------------------------------------
    query("kind P, defined without properties", PK)
    query("kind Q, defined as its key ascending", {"kind": kind("Q")})
    query("kind C, defined with an ancestor only", {"kind": kind("C")})
    query("kind C with an ancestor", {"kind": kind("C"), "filter": ANCESTOR})
    query("kind P with an ancestor", {**PK, "filter": ANCESTOR})
    query("kind p, another case", {"kind": kind("p")})
    query("kind T, not defined", {"kind": kind("T")})
    query("no kind", {})
    query("no kind, an ancestor", {"filter": ANCESTOR})
    query("no kind, the key above p1", {"filter": GT("__key__", KEY)})
    query("__kind__, defined", {"kind": kind("__kind__")})
    query("__namespace__, not defined", {"kind": kind("__namespace__")})
    query("order by __key__", {**PK, "order": [asc("__key__")]})
    query("order by __key__ descending", {**PK, "order": [desc("__key__")]})
    query("__key__ above p1", {**PK, "filter": GT("__key__", KEY)})
    query("__key__ = p1", {**PK, "filter": EQ("__key__", KEY)})
    query("__key__ IN [p1]", {**PK, "filter": pf("__key__", "IN", {"arrayValue": {"values": [KEY]}})})
    query("__key__ != p1", {**PK, "filter": pf("__key__", "NOT_EQUAL", KEY)})
    query("__key__ = p1, d = 1", {**PK, "filter": AND(EQ("__key__", KEY), EQ("d", I(1)))})
    query("d = 1, __key__ = p1", {**PK, "filter": AND(EQ("d", I(1)), EQ("__key__", KEY))})
    # --- one property -------------------------------------------------------------------------
    query("a = 1", {**PK, "filter": EQ("a", I(1))})
    query("order by c", {**PK, "order": [asc("c")]})
    query("order by c descending", {**PK, "order": [desc("c")]})
    query("c > 2", {**PK, "filter": GT("c", I(2))})
    query("a IN [1, 2]", {**PK, "filter": pf("a", "IN", {"arrayValue": {"values": [I(1), I(2)]}})})
    query("a != 1", {**PK, "filter": pf("a", "NOT_EQUAL", I(1))})
    query("a NOT_IN [1]", {**PK, "filter": pf("a", "NOT_IN", {"arrayValue": {"values": [I(1)]}})})
    query("c > 0 and c < 4", {**PK, "filter": AND(GT("c", I(0)), pf("c", "LESS_THAN", I(4)))})
    query("m.x = 1", {**PK, "filter": EQ("m.x", I(1))})
    query("a = 1 with an ancestor", {**PK, "filter": AND(ANCESTOR, EQ("a", I(1)))})
    query("order by a, then __key__", {**PK, "order": [asc("a"), asc("__key__")]})
    # --- equalities ---------------------------------------------------------------------------
    query("a = 1, b = y", {**PK, "filter": AND(EQ("a", I(1)), EQ("b", S("y")))})
    query("b = y, a = 1", {**PK, "filter": AND(EQ("b", S("y")), EQ("a", I(1)))})
    query("a = 1, a = 2", {**PK, "filter": AND(EQ("a", I(1)), EQ("a", I(2)))})
    query("a = 1 order by a", {**PK, "filter": EQ("a", I(1)), "order": [asc("a")]})
    query("c = 1, a = 1", {**PK, "filter": AND(EQ("c", I(1)), EQ("a", I(1)))})
    query("a = 1, c = 1", {**PK, "filter": AND(EQ("a", I(1)), EQ("c", I(1)))})
    query("B = 1, a = 1", {**PK, "filter": AND(EQ("B", I(1)), EQ("a", I(1)))})
    query("m.x = 1, a = 1", {**PK, "filter": AND(EQ("m.x", I(1)), EQ("a", I(1)))})
    query("a = 1, d = 1", {**PK, "filter": AND(EQ("a", I(1)), EQ("d", I(1)))})
    query("a IN [1, 2], b = y", {**PK, "filter": AND(pf("a", "IN", {"arrayValue": {"values": [I(1), I(2)]}}), EQ("b", S("y")))})
    query("b = y, a IN [1, 2]", {**PK, "filter": AND(EQ("b", S("y")), pf("a", "IN", {"arrayValue": {"values": [I(1), I(2)]}}))})
    query("an ancestor, a = 1, b = y", {**PK, "filter": AND(ANCESTOR, EQ("a", I(1)), EQ("b", S("y")))})
    query("a = 1, an ancestor, b = y", {**PK, "filter": AND(EQ("a", I(1)), ANCESTOR, EQ("b", S("y")))})
    query("a = 1, b = y in nested ands", {**PK, "filter": AND(EQ("a", I(1)), AND(EQ("b", S("y"))))})
    # --- orders -------------------------------------------------------------------------------
    query("order by a, b", {**PK, "order": [asc("a"), asc("b")]})
    query("order by b, a", {**PK, "order": [asc("b"), asc("a")]})
    query("order by b, a, c descending", {**PK, "order": [asc("b"), asc("a"), desc("c")]})
    query("order by c descending, a", {**PK, "order": [desc("c"), asc("a")]})
    query("order by c, a", {**PK, "order": [asc("c"), asc("a")]})
    query("order by a, c descending", {**PK, "order": [asc("a"), desc("c")]})
    query("order by a, __key__ descending", {**PK, "order": [asc("a"), desc("__key__")]})
    query("order by d, __key__ descending", {**PK, "order": [asc("d"), desc("__key__")]})
    query("order by __key__, d", {**PK, "order": [asc("__key__"), asc("d")]})
    query("order by a, __key__, c", {**PK, "order": [asc("a"), asc("__key__"), asc("c")]})
    query("b = y order by a, c descending", {**PK, "filter": EQ("b", S("y")), "order": [asc("a"), desc("c")]})
    query("b = y, a = 1 order by c descending", {**PK, "filter": AND(EQ("b", S("y")), EQ("a", I(1))), "order": [desc("c")]})
    query("a = 1, b = y order by c descending", {**PK, "filter": AND(EQ("a", I(1)), EQ("b", S("y"))), "order": [desc("c")]})
    query("a = 1 order by __key__ descending", {**PK, "filter": EQ("a", I(1)), "order": [desc("__key__")]})
    query("d = 1 order by __key__ descending", {**PK, "filter": EQ("d", I(1)), "order": [desc("__key__")]})
    query("d = 1, __key__ above p1, order by __key__ descending", {**PK, "filter": AND(EQ("d", I(1)), GT("__key__", KEY)), "order": [desc("__key__")]})
    # --- inequalities -------------------------------------------------------------------------
    query("b > a, a > 0", {**PK, "filter": AND(GT("b", S("a")), GT("a", I(0)))})
    query("c > 0, a > 0", {**PK, "filter": AND(GT("c", I(0)), GT("a", I(0)))})
    query("a > 0, B > 0", {**PK, "filter": AND(GT("a", I(0)), GT("B", I(0)))})
    query("a > 0 order by c", {**PK, "filter": GT("a", I(0)), "order": [asc("c")]})
    query("a != 1 order by c", {**PK, "filter": pf("a", "NOT_EQUAL", I(1)), "order": [asc("c")]})
    query("a > 0 order by a, b", {**PK, "filter": GT("a", I(0)), "order": [asc("a"), asc("b")]})
    query("a = 1, c > 0", {**PK, "filter": AND(EQ("a", I(1)), GT("c", I(0)))})
    query("a = 1, c > 0 order by c descending", {**PK, "filter": AND(EQ("a", I(1)), GT("c", I(0))), "order": [desc("c")]})
    query("a = 1, a > 0", {**PK, "filter": AND(EQ("a", I(1)), GT("a", I(0)))})
    query("a = 1, b != y", {**PK, "filter": AND(EQ("a", I(1)), pf("b", "NOT_EQUAL", S("y")))})
    query("b = y, a != 1", {**PK, "filter": AND(EQ("b", S("y")), pf("a", "NOT_EQUAL", I(1)))})
    query("__key__ above p1, a > 0", {**PK, "filter": AND(GT("__key__", KEY), GT("a", I(0)))})
    query("a > 0, __key__ above p1", {**PK, "filter": AND(GT("a", I(0)), GT("__key__", KEY))})
    query("__key__ above p1 order by c", {**PK, "filter": GT("__key__", KEY), "order": [asc("c")]})
    query("__key__ above p1, project a", {**PK, "filter": GT("__key__", KEY), "projection": PROJ("a")})
    # --- what a query does not order by, after a descending order -----------------------------
    RK = {"kind": kind("R")}
    query("R: project a, order by t descending", {**RK, "projection": PROJ("a"), "order": [desc("t")]})
    query("R: project a, order by u descending", {**RK, "projection": PROJ("a"), "order": [desc("u")]})
    query("R: a > 0 order by t descending", {**RK, "filter": GT("a", I(0)), "order": [desc("t")]})
    query("R: a > 0 order by u descending", {**RK, "filter": GT("a", I(0)), "order": [desc("u")]})
    query("R: distinct on t, a, order by t descending", {**RK, "distinctOn": DISTINCT("t", "a"), "order": [desc("t")]})
    agg("R: sum of a, order by t descending", {**RK, "order": [desc("t")]}, SUM("a"))
    agg("R: sum of a, order by u descending", {**RK, "order": [desc("u")]}, SUM("a"))
    query("R: __key__ above, order by t descending", {**RK, "filter": GT("__key__", {"keyValue": key(("R", "r"))}), "order": [desc("t")]})
    # --- OR -----------------------------------------------------------------------------------
    query("OR(a = 1 and b = y, c = 1)", {**PK, "filter": OR(AND(EQ("a", I(1)), EQ("b", S("y"))), EQ("c", I(1)))})
    query("OR(a = 1 and b = y, b = y and a = 1)", {**PK, "filter": OR(AND(EQ("a", I(1)), EQ("b", S("y"))), AND(EQ("b", S("y")), EQ("a", I(1))))})
    query("OR(a = 1 and b = y, b = z and a = 2)", {**PK, "filter": OR(AND(EQ("a", I(1)), EQ("b", S("y"))), AND(EQ("b", S("z")), EQ("a", I(2))))})
    query("OR(c = 1 and a = 1, a = 1)", {**PK, "filter": OR(AND(EQ("c", I(1)), EQ("a", I(1))), EQ("a", I(1)))})
    query("OR(a = 1 and c = 1, a = 1)", {**PK, "filter": OR(AND(EQ("a", I(1)), EQ("c", I(1))), EQ("a", I(1)))})
    query("OR(b = y and a = 1)", {**PK, "filter": OR(AND(EQ("b", S("y")), EQ("a", I(1))))})
    query("OR(a = 1, a = 1)", {**PK, "filter": OR(EQ("a", I(1)), EQ("a", I(1)))})
    query("AND(OR(c = 1, c = 2), a = 1)", {**PK, "filter": AND(OR(EQ("c", I(1)), EQ("c", I(2))), EQ("a", I(1)))})
    query("AND(a = 1, OR(c = 1, c = 2))", {**PK, "filter": AND(EQ("a", I(1)), OR(EQ("c", I(1)), EQ("c", I(2))))})
    query("AND(OR(b = y, c = 1), a = 1)", {**PK, "filter": AND(OR(EQ("b", S("y")), EQ("c", I(1))), EQ("a", I(1)))})
    query("OR(a = 1, b = y) order by c", {**PK, "filter": OR(EQ("a", I(1)), EQ("b", S("y"))), "order": [asc("c")]})
    query("OR(a = 1, c = 1)", {**PK, "filter": OR(EQ("a", I(1)), EQ("c", I(1)))})
    # --- projections and distinctOn -----------------------------------------------------------
    query("project a, b", {**PK, "projection": PROJ("a", "b")})
    query("project b, a", {**PK, "projection": PROJ("b", "a")})
    query("project a, c", {**PK, "projection": PROJ("a", "c")})
    query("project a, B", {**PK, "projection": PROJ("a", "B")})
    query("project __key__", {**PK, "projection": PROJ("__key__")})
    query("project a, __key__", {**PK, "projection": PROJ("a", "__key__")})
    query("a = 1, project b", {**PK, "filter": EQ("a", I(1)), "projection": PROJ("b")})
    query("b = y, project a", {**PK, "filter": EQ("b", S("y")), "projection": PROJ("a")})
    query("order by c, project a", {**PK, "order": [asc("c")], "projection": PROJ("a")})
    query("c > 0, project c, a", {**PK, "filter": GT("c", I(0)), "projection": PROJ("c", "a")})
    query("distinct on c, project a, c", {**PK, "projection": PROJ("a", "c"), "distinctOn": DISTINCT("c")})
    query("distinct on a, project a, c", {**PK, "projection": PROJ("a", "c"), "distinctOn": DISTINCT("a")})
    query("distinct on b, project a, b", {**PK, "projection": PROJ("a", "b"), "distinctOn": DISTINCT("b")})
    query("distinct on c, a, project a, c", {**PK, "projection": PROJ("a", "c"), "distinctOn": DISTINCT("c", "a")})
    query("a > 0, distinct on c, project a, c", {**PK, "filter": GT("a", I(0)), "projection": PROJ("a", "c"), "distinctOn": DISTINCT("c")})
    query("c > 0, distinct on a, project a, c", {**PK, "filter": GT("c", I(0)), "projection": PROJ("a", "c"), "distinctOn": DISTINCT("a")})
    query("b = y, d > 0, project a", {**PK, "filter": AND(EQ("b", S("y")), GT("d", I(0))), "projection": PROJ("a")})
    query("b = y, distinct on d, project a, d", {**PK, "filter": EQ("b", S("y")), "projection": PROJ("a", "d"), "distinctOn": DISTINCT("d")})
    query("a = 1, b = y, project a", {**PK, "filter": AND(EQ("a", I(1)), EQ("b", S("y"))), "projection": PROJ("a")})
    # --- aggregations -------------------------------------------------------------------------
    agg("count P", PK, COUNT)
    agg("count a = 1, b = y", {**PK, "filter": AND(EQ("a", I(1)), EQ("b", S("y")))}, COUNT)
    agg("count a = 1, d = 1", {**PK, "filter": AND(EQ("a", I(1)), EQ("d", I(1)))}, COUNT)
    agg("count up to 2, c = 1", {**PK, "filter": EQ("c", I(1))}, {"count": {"upTo": "2"}})
    agg("sum of b, a = 1", {**PK, "filter": EQ("a", I(1))}, SUM("b"))
    agg("sum of a, b = y", {**PK, "filter": EQ("b", S("y"))}, SUM("a"))
    agg("sum of a", PK, SUM("a"))
    agg("sum of c, sum of a", PK, SUM("c"), SUM("a"))
    agg("avg of a, sum of c", PK, AVG("a"), SUM("c"))
    agg("sum of a twice", PK, SUM("a"), SUM("a"))
    agg("sum of a order by c", {**PK, "order": [asc("c")]}, SUM("a"))
    agg("sum of c order by c", {**PK, "order": [asc("c")]}, SUM("c"))
    agg("sum of a, c > 0", {**PK, "filter": GT("c", I(0))}, SUM("a"))
    agg("count and sum of a, c = 1", {**PK, "filter": EQ("c", I(1))}, COUNT, SUM("a"))
    # --- GQL ----------------------------------------------------------------------------------
    gql("GQL c = 1 AND a = 1", "SELECT * FROM P WHERE c = 1 AND a = 1")
    gql("GQL a = 1 AND c = 1", "SELECT * FROM P WHERE a = 1 AND c = 1")
    gql("GQL kind P", "SELECT * FROM P")
    gql("GQL no kind", "SELECT *")
    gql("GQL order by c, a", "SELECT * FROM P ORDER BY c, a")
    gql("GQL project a, c", "SELECT a, c FROM P")
    gql("GQL DISTINCT c, a", "SELECT DISTINCT c, a FROM P")
    gql("GQL DISTINCT ON (c) a, c", "SELECT DISTINCT ON (c) a, c FROM P")
    gql("GQL count a = 1 AND c = 1", "AGGREGATE COUNT(*) OVER (SELECT * FROM P WHERE a = 1 AND c = 1)", "runAggregationQuery")
    gql("GQL sum of a, c = 1", "AGGREGATE SUM(a) OVER (SELECT * FROM P WHERE c = 1)", "runAggregationQuery")
    # --- namespaces, projects, transactions -------------------------------------------------
    query("in ns1, a = 1, b = y", {**PK, "filter": AND(EQ("a", I(1)), EQ("b", S("y")))}, partitionId={"namespaceId": "ns1"})
    query("in ns1, b = y, a = 1", {**PK, "filter": AND(EQ("b", S("y")), EQ("a", I(1)))}, partitionId={"namespaceId": "ns1"})
    case("another project, kind P", "runQuery", {"query": PK}, project=f"{P}-other")
    case("another project, b = y, a = 1", "runQuery", {"query": {**PK, "filter": AND(EQ("b", S("y")), EQ("a", I(1)))}}, project=f"{P}-other")
    case("begin a transaction", "beginTransaction", {}, defines={"t1": "transaction"})
    query("in a transaction, b = y, a = 1", {**PK, "filter": AND(EQ("b", S("y")), EQ("a", I(1)))}, readOptions={"transaction": "<t1>"})
    query("in a transaction, a = 1, b = y", {**PK, "filter": AND(EQ("a", I(1)), EQ("b", S("y")))}, readOptions={"transaction": "<t1>"})
    query("in a new transaction, b = y, a = 1", {**PK, "filter": AND(EQ("b", S("y")), EQ("a", I(1)))}, readOptions={"newTransaction": {}})
    agg("in a transaction, count b = y, a = 1", {**PK, "filter": AND(EQ("b", S("y")), EQ("a", I(1)))}, COUNT, readOptions={"transaction": "<t1>"})
    case("lookup", "lookup", {"keys": [key(("P", "p1"))]})
    # --- refused once the index is found ----------------------------------------------------
    query("a = 1, project a", {**PK, "filter": EQ("a", I(1)), "projection": PROJ("a")})
    query("__key__ = a string, d = 1", {**PK, "filter": AND(EQ("__key__", S("p1")), EQ("d", I(1)))})
    query("kind C, an ancestor in ns1", {"kind": kind("C"), "filter": pf("__key__", "HAS_ANCESTOR", {"keyValue": key(("P", "p0"), ns="ns1")})})
    query("distinct on __key__", {**PK, "projection": PROJ("__key__"), "distinctOn": DISTINCT("__key__")})
    query("order by c, distinct on a, project a, c", {**PK, "order": [asc("c")], "projection": PROJ("a", "c"), "distinctOn": DISTINCT("a")})
    query("kind P, a read time too old", PK, readOptions={"readTime": "2020-01-01T00:00:00Z"})
    agg("an alias __x__", PK, {"count": {}, "alias": "__x__"})
    agg("no aggregations", PK)
    agg("six aggregations", PK, *[{"count": {}, "alias": f"n{i}"} for i in range(6)])
    agg("two aliases alike", PK, {"count": {}, "alias": "n"}, {"count": {}, "alias": "n"})
    agg("sum of __key__", PK, SUM("__key__"))
    agg("sum of a, project c", {**PK, "projection": PROJ("c")}, SUM("a"))


def refused():
    # Refused before the index is looked for.
    query("two kinds", {"kind": kind("P") + kind("Q"), "filter": AND(EQ("b", S("y")), EQ("a", I(1)))})
    query("an empty kind name", {"kind": kind(""), "filter": AND(EQ("b", S("y")), EQ("a", I(1)))})
    query("order twice by a", {**PK, "order": [asc("a"), desc("a")]})
    query("order without a kind", {"order": [asc("a")]})
    query("project a twice", {**PK, "projection": PROJ("a", "a")})
    query("a negative limit", {**PK, "filter": AND(EQ("b", S("y")), EQ("a", I(1))), "limit": -1})
    query("a negative offset", {**PK, "filter": AND(EQ("b", S("y")), EQ("a", I(1))), "offset": -1})
    query("a cursor that is not ours", {**PK, "filter": AND(EQ("b", S("y")), EQ("a", I(1))), "startCursor": "AAAA"})
    query("a filter without a property", {**PK, "filter": AND(EQ("b", S("y")), pf("", "EQUAL", I(1)))})
    query("a filter without a value", {**PK, "filter": AND(EQ("b", S("y")), {"propertyFilter": {"property": {"name": "a"}, "op": "EQUAL"}})})
    query("an empty and", {**PK, "filter": {"compositeFilter": {"op": "AND", "filters": []}}})
    query("an unknown operator", {**PK, "filter": AND(EQ("b", S("y")), {"propertyFilter": {"property": {"name": "a"}, "value": I(1)}})})
    query("an ancestor in an or", {**PK, "filter": OR(ANCESTOR, EQ("a", I(1)))})
    query("an ancestor of a string", {**PK, "filter": pf("__key__", "HAS_ANCESTOR", S("p0"))})
    query("an ancestor on a property", {**PK, "filter": pf("a", "HAS_ANCESTOR", {"keyValue": key(("P", "p0"))})})
    query("b = y, a = an array", {**PK, "filter": AND(EQ("b", S("y")), EQ("a", base.ARR(I(1))))})
    query("b = y, a = an entity", {**PK, "filter": AND(EQ("b", S("y")), EQ("a", ENT(x=I(1))))})
    query("b = y, a IN []", {**PK, "filter": AND(EQ("b", S("y")), pf("a", "IN", {"arrayValue": {}}))})
    query("b = y, a NOT_IN 11 values", {**PK, "filter": AND(EQ("b", S("y")), pf("a", "NOT_IN", {"arrayValue": {"values": [I(i) for i in range(11)]}}))})
    query("a = 1 without a kind", {"filter": EQ("a", I(1))})
    case("an unknown field", "runQuery", {"query": {**PK, "x": 1}})
    case("neither query", "runQuery", {})
    case("a GQL syntax error", "runQuery", {"gqlQuery": {"queryString": "SELECT * FROM P WHERE", "allowLiterals": True}})
    case("a GQL binding missing", "runQuery", {"gqlQuery": {"queryString": "SELECT * FROM P WHERE b = @b AND a = @a", "namedBindings": {"a": {"value": I(1)}}}})
    query("an unknown transaction", {**PK, "filter": AND(EQ("b", S("y")), EQ("a", I(1)))}, readOptions={"transaction": "AAAA"})
    # Refused once the index is found: here, as none is, the index first.
    query("b = y, a = 1, project a", {**PK, "filter": AND(EQ("b", S("y")), EQ("a", I(1))), "projection": PROJ("a")})
    query("__key__ = a string", {**PK, "filter": EQ("__key__", S("p1"))})
    query("kind P, an ancestor in ns1", {**PK, "filter": pf("__key__", "HAS_ANCESTOR", {"keyValue": key(("P", "p0"), ns="ns1")})})
    query("distinct on __key__", {**PK, "projection": PROJ("__key__"), "distinctOn": DISTINCT("__key__")})
    query("order by a, distinct on c, project a, c", {**PK, "order": [asc("a")], "projection": PROJ("a", "c"), "distinctOn": DISTINCT("c")})
    query("kind P, a read time too old", PK, readOptions={"readTime": "2020-01-01T00:00:00Z"})
    agg("an alias __x__", PK, {"count": {}, "alias": "__x__"})
    agg("no aggregations", PK)
    agg("six aggregations", PK, *[{"count": {}, "alias": f"n{i}"} for i in range(6)])
    agg("two aliases alike", PK, {"count": {}, "alias": "n"}, {"count": {}, "alias": "n"})
    agg("sum of __key__", PK, SUM("__key__"))
    agg("sum of a, project c", {**PK, "projection": PROJ("c")}, SUM("a"))


def probes():
    case("lookup", "lookup", {"keys": [key(("P", "x"))]})
    query("kind P", PK)
    query("a = 1, b = y", {**PK, "filter": AND(EQ("a", I(1)), EQ("b", S("y")))})
    query("order by a, b descending", {**PK, "order": [asc("a"), desc("b")]})
    query("order by a descending, b", {**PK, "order": [desc("a"), asc("b")]})
    query("kind P with an ancestor", {**PK, "filter": ANCESTOR})
    query("kind Q", {"kind": kind("Q")})


def every_request():
    case("lookup", "lookup", {"keys": [key(("P", "x"))]})
    case("commit", "commit", {"mode": "NON_TRANSACTIONAL", "mutations": [{"upsert": {"key": key(("P", "x")), "properties": {}}}]})
    case("begin a transaction", "beginTransaction", {})
    case("allocate IDs", "allocateIds", {"keys": [{"path": [{"kind": "P"}]}]})
    case("reserve IDs", "reserveIds", {"keys": [key(("P", 5))]})
    case("rollback", "rollback", {"transaction": "AAAA"})
    query("kind P", PK)
    agg("count P", PK, COUNT)
    case("an unknown field", "runQuery", {"query": {**PK, "x": 1}})
    case("lookup in another project", "lookup", {"keys": [key(("P", "x"))]}, project=f"{P}-other")


def collect(build):
    CASES.clear()
    build()
    return list(CASES)


def a_property(direction):
    return f"indexes:\n- kind: P\n  properties:\n  - name: a\n  - name: b\n    direction: {direction}\n"


ONE = "indexes:\n- kind: P\n"
VARIANTS = [
    ("ancestor: yes", "indexes:\n- kind: P\n  ancestor: yes\n"),
    ("ancestor: no", "indexes:\n- kind: P\n  ancestor: no\n"),
    ("ancestor: true", "indexes:\n- kind: P\n  ancestor: true\n"),
    ("ancestor: false", "indexes:\n- kind: P\n  ancestor: false\n"),
    ("ancestor: Yes", "indexes:\n- kind: P\n  ancestor: Yes\n"),
    ("ancestor: YES", "indexes:\n- kind: P\n  ancestor: YES\n"),
    ("ancestor: TRUE", "indexes:\n- kind: P\n  ancestor: TRUE\n"),
    ("ancestor: on", "indexes:\n- kind: P\n  ancestor: on\n"),
    ("ancestor: Off", "indexes:\n- kind: P\n  ancestor: Off\n"),
    ("ancestor: y", "indexes:\n- kind: P\n  ancestor: y\n"),
    ("ancestor: N", "indexes:\n- kind: P\n  ancestor: N\n"),
    ("ancestor: quoted yes", "indexes:\n- kind: P\n  ancestor: \"yes\"\n"),
    ("ancestor: yEs", "indexes:\n- kind: P\n  ancestor: yEs\n"),
    ("ancestor: 1", "indexes:\n- kind: P\n  ancestor: 1\n"),
    ("ancestor: maybe", "indexes:\n- kind: P\n  ancestor: maybe\n"),
    ("ancestor: nothing", "indexes:\n- kind: P\n  ancestor:\n"),
    ("direction: asc", a_property("asc")),
    ("direction: desc", a_property("desc")),
    ("direction: quoted desc", a_property("\"desc\"")),
    ("direction: ASC", a_property("ASC")),
    ("direction: DESC", a_property("DESC")),
    ("direction: descending", a_property("descending")),
    ("direction: foo", a_property("foo")),
    ("direction: nothing", a_property("")),
    ("an empty file", ""),
    ("only a comment", "# no index\n"),
    ("indexes: nothing", "indexes:\n"),
    ("indexes: []", "indexes: []\n"),
    ("flow style", "indexes: [{kind: P, properties: [{name: a}, {name: b, direction: desc}]}]\n"),
    ("comments and quotes", "# indexes\nindexes:  # all of them\n- kind: 'P'  # the kind\n  properties:\n  - name: \"a\"\n  - name: b\n    direction: desc\n"),
    ("an explicit document", "---\nindexes:\n- kind: P\n"),
    ("two documents", "indexes:\n- kind: P\n---\nindexes:\n- kind: Q\n"),
    ("an anchor and an alias", "indexes:\n- &p\n  kind: P\n- *p\n"),
    ("not YAML", "{{{ not yaml\n"),
    ("a tab", "indexes:\n\t- kind: P\n"),
    ("a list at the top", "- kind: P\n"),
    ("a scalar at the top", "indexes\n"),
    ("indexes: a scalar", "indexes: P\n"),
    ("an index that is a scalar", "indexes:\n- P\n"),
    ("properties: a scalar", "indexes:\n- kind: P\n  properties: a\n"),
    ("a property that is a scalar", "indexes:\n- kind: P\n  properties:\n  - a\n"),
    ("an unknown key at the top", "foo: bar\nindexes:\n- kind: P\n"),
    ("an unknown key in an index", "indexes:\n- kind: P\n  foo: bar\n"),
    ("an unknown key in a property", "indexes:\n- kind: P\n  properties:\n  - name: a\n    foo: bar\n  - name: b\n"),
    ("a key twice", "indexes:\n- kind: Q\n  kind: P\n"),
    ("an index without a kind", "indexes:\n- ancestor: yes\n"),
    ("kind: nothing", "indexes:\n- kind:\n"),
    ("an empty kind", "indexes:\n- kind: \"\"\n"),
    ("a numeric kind", "indexes:\n- kind: 5\n"),
    ("properties: nothing", "indexes:\n- kind: P\n  properties:\n"),
    ("properties: []", "indexes:\n- kind: P\n  properties: []\n"),
    ("a property without a name", "indexes:\n- kind: P\n  properties:\n  - direction: asc\n  - name: b\n"),
    ("an empty property name", "indexes:\n- kind: P\n  properties:\n  - name: \"\"\n"),
    ("the same index twice", ONE + "- kind: P\n"),
    ("a file that does not exist", None),
]


def start(work, name, flags):
    log = open(os.path.join(work, f"{name}.out"), "wb")
    proc = subprocess.Popen([JAVA, "-Duser.language=en", "-Duser.country=US", "-jar", JAR, "--host", "127.0.0.1",
                             "--port", str(PORT), "--database-mode", "datastore-mode", *flags],
                            cwd=work, stdout=log, stderr=subprocess.STDOUT)
    for _ in range(240):
        try:
            conn = http.client.HTTPConnection("127.0.0.1", PORT, timeout=5)
            conn.request("GET", "/")
            if conn.getresponse().status == 200:
                return proc
        except OSError:
            time.sleep(0.25)
    proc.kill()
    raise SystemExit(f"the emulator did not start for {name}")


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


def replay(cases, work):
    out = []
    labels = {}
    for c in cases:
        if "write_index_file" in c:
            path = os.path.join(work, "index.yaml")
            if c["write_index_file"] is None:
                os.remove(path)
            else:
                Path(path).write_text(c["write_index_file"])
            out.append(c)
            continue
        text = json.dumps(c["body"]).replace("{project}", base.PROJECT) if "body" in c else ""
        for label, value in labels.items():
            text = text.replace(f'"<{label}>"', json.dumps(value))
        status, ctype, answer = base.call(c["method"], c["path"].replace("{project}", base.PROJECT), text)
        for label, path in (c.get("defines") or {}).items():
            labels[label] = base.at(json.loads(answer), path)
        out.append(dict(c, outcome={"status": status, "contentType": ctype, "body": base.masked(answer)}))
    return out


def run(work, name, index_file, flags, cases, seeded):
    """One fresh emulator in `work/<n>` with `flags` (`{dir}` that directory) and `index_file`
    written to `{dir}/index.yaml` first (none if `None`)."""
    here = os.path.join(work, str(len(os.listdir(work))))
    os.makedirs(here)
    if index_file is not None:
        Path(here, "index.yaml").write_text(index_file)
    proc = start(here, "jar", [f.replace("{dir}", here) for f in flags])
    try:
        if seeded:
            base.DATASET = DATASET
            base.seed()
        cases = replay(cases, here)
    finally:
        stop(proc)
    return {"name": name, "flags": flags, "index_file": index_file, "seeded": seeded, "cases": cases}


REQUIRE = ["--require_indexes", "--index_file", "{dir}/index.yaml"]


def main():
    lazy = [
        {"name": "lookup, no file yet"},
        {"name": "write the file", "write_index_file": ONE},
        {"name": "lookup, the file written"},
        {"name": "kind P"},
        {"name": "write an empty file", "write_index_file": "indexes:\n"},
        {"name": "kind P, the file emptied"},
        {"name": "kind P in another project"},
        {"name": "reset"},
        {"name": "kind P after a reset"},
    ]
    requests = {
        "lookup, no file yet": ("lookup", {"keys": [key(("P", "x"))]}, P),
        "lookup, the file written": ("lookup", {"keys": [key(("P", "x"))]}, P),
        "kind P": ("runQuery", {"query": PK}, P),
        "kind P, the file emptied": ("runQuery", {"query": PK}, P),
        "kind P in another project": ("runQuery", {"query": PK}, f"{P}-other"),
        "kind P after a reset": ("runQuery", {"query": PK}, P),
    }
    late = []
    for step in lazy:
        if "write_index_file" in step:
            late.append(step)
        elif step["name"] == "reset":
            late.append({"name": "reset", "method": "POST", "path": "/reset", "headers": {}, "compare": "exact"})
        else:
            verb, body, project = requests[step["name"]]
            late.append({"name": step["name"], "method": "POST", "path": f"/v1/projects/{project}:{verb}",
                         "headers": base.JSON, "body": body, "compare": "exact"})
    with tempfile.TemporaryDirectory() as work:
        work = os.path.realpath(work)
        runs = [
            run(work, "enforced", INDEXES, REQUIRE, collect(enforced), True),
            run(work, "refused", "indexes:\n", REQUIRE, collect(refused), True),
            run(work, "not enforced", "indexes:\n- kind: P\n  ancestor: maybe\n", ["--index_file", "{dir}/index.yaml"],
                collect(probes), True),
            run(work, "no index file", None, ["--require_indexes"], collect(every_request), False),
            run(work, "file read late", None, REQUIRE, late, False),
        ]
        for name, content in VARIANTS:
            runs.append(run(work, name, content, REQUIRE, collect(probes), False))
    fixture = {
        "oracle": "cloud-firestore-emulator v1.22.0 (sha256 9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)",
        "generated_by": "tools/oracle/datastore_indexes.py",
        "mode": "datastore-mode",
        "dataset": [{"key": k, "properties": props} for k, props in DATASET],
        "runs": runs,
    }
    json.dump(fixture, sys.stdout, ensure_ascii=False, indent=1)
    print()


if __name__ == "__main__":
    main()
