#!/usr/bin/env python3
"""How does the official emulator in Datastore mode read GQL? (#81)

Usage (from the repository root), with a jar freshly started with `--database-mode
datastore-mode` under an English locale:
    python3 -I tools/oracle/datastore_gql.py 127.0.0.1:8086 \\
        > crates/hidane/tests/fixtures/datastore_gql.json

Seeds the dataset of `datastore_queries.py`, then sends `runQuery` and `runAggregationQuery`
with `gqlQuery`: each clause of the grammar of the GQL reference
(https://cloud.google.com/datastore/docs/reference/gql_reference), each kind of literal and
binding, and the queries it refuses. A GQL query's answer holds the structured query it read
(`query`), recorded too. Cursors are named as in `datastore_queries.py`. The fixture has its
format; `crates/hidane/tests/datastore.rs` replays it. Stdlib only.
"""

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import datastore_queries as base  # noqa: E402

base.PROJECT = base.PROJECT.replace("dsquery", "dsgql")
CASES = []


def gql(name, text, verb="runQuery", allow=True, named=None, positional=None, defines=None, **more):
    q = {"queryString": text}
    if allow:
        q["allowLiterals"] = True
    if named is not None:
        q["namedBindings"] = named
    if positional is not None:
        q["positionalBindings"] = positional
    c = {"name": name, "method": "POST", "path": f"/v1/projects/{base.P}:{verb}", "headers": base.JSON,
         "body": {"gqlQuery": q, **more}, "compare": "exact"}
    if defines:
        c["defines"] = defines
    CASES.append(c)


V = lambda value: {"value": value}
I, S, D = base.I, base.S, base.D

# --- SELECT and FROM ----------------------------------------------------------------------------
gql("select all from P", "SELECT * FROM P")
gql("lowercase keywords", "select * from P")
gql("a quoted kind", "SELECT * FROM `P`")
gql("no FROM", "SELECT *")
gql("keys only", "SELECT __key__ FROM P")
gql("a projection", "SELECT a FROM P")
gql("two properties", "SELECT a, b FROM P")
gql("DISTINCT", "SELECT DISTINCT b FROM P")
gql("DISTINCT ON, every property", "SELECT DISTINCT ON (b) * FROM P")
gql("DISTINCT ON, a projection", "SELECT DISTINCT ON (b) a, b FROM P")
gql("a property of an entity value", "SELECT * FROM P WHERE m.x = 1")
gql("a quoted property", "SELECT * FROM P WHERE `a` = 2")
gql("a keyword quoted as a property", "SELECT * FROM P WHERE `order` = 2")
gql("a kind that is a keyword", "SELECT * FROM `SELECT`")
gql("a reserved kind", "SELECT * FROM __kind__")

# --- WHERE -------------------------------------------------------------------------------------
gql("=", "SELECT * FROM P WHERE a = 2")
gql("!=", "SELECT * FROM P WHERE a != 2")
gql("<", "SELECT * FROM P WHERE a < 2")
gql("<=", "SELECT * FROM P WHERE a <= 2")
gql(">", "SELECT * FROM P WHERE a > 2")
gql(">=", "SELECT * FROM P WHERE a >= 2")
gql("a value first", "SELECT * FROM P WHERE 2 < a")
gql("a value first, =", "SELECT * FROM P WHERE 'x' = b")
gql("IS NULL", "SELECT * FROM T WHERE v IS NULL")
gql("= NULL", "SELECT * FROM T WHERE v = NULL")
gql("!= NULL", "SELECT * FROM T WHERE v != NULL")
gql("IN", "SELECT * FROM P WHERE a IN ARRAY(1, 3)")
gql("NOT IN", "SELECT * FROM P WHERE a NOT IN ARRAY(1, 3)")
gql("a value IN a property", "SELECT * FROM P WHERE 'red' IN tags")
gql("CONTAINS", "SELECT * FROM P WHERE tags CONTAINS 'red'")
gql("AND", "SELECT * FROM P WHERE a > 1 AND b = 'y'")
gql("OR", "SELECT * FROM P WHERE a = 2 OR b = 'z'")
gql("AND before OR", "SELECT * FROM P WHERE a = 3 OR a = 1 AND b = 'x'")
gql("parentheses", "SELECT * FROM P WHERE (a = 3 OR a = 1) AND b = 'x'")
gql("HAS ANCESTOR", "SELECT * FROM C WHERE __key__ HAS ANCESTOR KEY(Q, 'q1')")
gql("HAS DESCENDANT", "SELECT * FROM C WHERE KEY(Q, 'q1') HAS DESCENDANT __key__")
gql("HAS ANCESTOR without a kind", "SELECT * WHERE __key__ HAS ANCESTOR KEY(Q, 'q1')")
gql("__key__ =", "SELECT * FROM T WHERE __key__ = KEY(T, 'str')")
gql("__key__ >", "SELECT * FROM T WHERE __key__ > KEY(T, 'str')")
gql("a key with an ID", "SELECT * FROM T WHERE __key__ = KEY(T, 3)")
gql("a key of two elements", "SELECT * WHERE __key__ = KEY(Q, 'q1', C, 'c1')")
gql("a key with PROJECT and NAMESPACE", "SELECT * FROM T WHERE v = KEY(PROJECT('{project}'), NAMESPACE('ns1'), A, 'x')")
gql("a key with NAMESPACE", "SELECT * FROM T WHERE v = KEY(NAMESPACE('ns1'), A, 'x')")
gql("a key with another PROJECT", "SELECT * FROM T WHERE v = KEY(PROJECT('elsewhere'), A, 'x')")
gql("two inequalities", "SELECT * FROM P WHERE a > 1 AND b > 'w'")

# --- literals ---------------------------------------------------------------------------------
for label, literal in [
    ("an integer", "1"), ("a positive integer", "+1"), ("a negative integer", "-1"),
    ("an integer too large", "9223372036854775808"),
    ("a double", "1.5"), ("a double ending in a point", "-3."), ("a double starting with a point", "+.5"),
    ("an exponent", "15e-1"), ("NaN as text", "'NaN'"),
    ("a single-quoted string", "'a'"), ("a double-quoted string", '"a"'), ("a doubled quote", "'it''s'"),
    ("an escaped quote", "'it\\'s'"), ("an escaped newline", "'a\\nb'"), ("an escaped percent", "'a\\%b'"),
    ("an unknown escape", "'a\\qb'"), ("an empty string", "''"),
    ("TRUE", "TRUE"), ("lowercase false", "false"), ("NULL", "NULL"),
    ("DATETIME", "DATETIME('2020-01-01T00:00:00Z')"),
    ("DATETIME with microseconds and an offset", "DATETIME('2021-06-01T21:00:00.500000+09:00')"),
    ("DATETIME badly formed", "DATETIME('2020-01-01')"),
    ("BLOB", "BLOB('YQ')"), ("BLOB with padding", "BLOB('YQ==')"), ("BLOB with a URL alphabet", "BLOB('-_8')"),
    ("ARRAY", "ARRAY(1, 'a')"),
]:
    gql(f"v = {label}", f"SELECT * FROM T WHERE v = {literal}")
gql("literals refused", "SELECT * FROM T WHERE v = 1", allow=False)
gql("a string refused", "SELECT * FROM T WHERE v = 'a'", allow=False)
gql("a key refused", "SELECT * FROM T WHERE __key__ = KEY(T, 'str')", allow=False)

# --- bindings ----------------------------------------------------------------------------------
gql("a named binding", "SELECT * FROM P WHERE a = @a", allow=False, named={"a": V(I(2))})
gql("a positional binding", "SELECT * FROM P WHERE a = @1", allow=False, positional=[V(I(2))])
gql("two positional bindings", "SELECT * FROM P WHERE a = @1 AND b = @2", allow=False, positional=[V(I(2)), V(S("x"))])
gql("a missing named binding", "SELECT * FROM P WHERE a = @a", allow=False, named={})
gql("a missing positional binding", "SELECT * FROM P WHERE a = @2", allow=False, positional=[V(I(2))])
gql("an unused named binding", "SELECT * FROM P WHERE a = 2", named={"x": V(I(2))})
gql("an unused positional binding", "SELECT * FROM P WHERE a = @1", allow=False, positional=[V(I(2)), V(I(3))])
gql("a binding of an array", "SELECT * FROM P WHERE a IN @a", allow=False, named={"a": V(base.ARR(I(1), I(3)))})
gql("a binding of a key", "SELECT * FROM T WHERE __key__ = @k", allow=False, named={"k": V(base.KEY(("T", "str")))})
gql("a binding name starting with a digit", "SELECT * FROM P WHERE a = @1a", allow=False, named={"1a": V(I(2))})
gql("a binding with neither", "SELECT * FROM P WHERE a = @a", allow=False, named={"a": {}})
gql("named and positional", "SELECT * FROM P WHERE a = @a AND b = @1", allow=False, named={"a": V(I(2))}, positional=[V(S("x"))])

# --- ORDER BY, LIMIT, OFFSET, cursors --------------------------------------------------------
gql("ORDER BY", "SELECT * FROM P ORDER BY a")
gql("ORDER BY DESC and ASC", "SELECT * FROM P ORDER BY b DESC, a ASC")
gql("ORDER BY __key__ DESC", "SELECT * FROM P ORDER BY __key__ DESC")
gql("ORDER BY with an inequality on another property", "SELECT * FROM P WHERE a > 1 ORDER BY b")
gql("LIMIT", "SELECT * FROM P LIMIT 3", defines={"c1": "batch.endCursor"})
gql("LIMIT a binding", "SELECT * FROM P LIMIT @n", allow=False, named={"n": V(I(2))})
gql("OFFSET", "SELECT * FROM P OFFSET 2")
gql("LIMIT and OFFSET", "SELECT * FROM P LIMIT 2 OFFSET 2")
gql("LIMIT from a cursor", "SELECT * FROM P LIMIT @c, 2", allow=False, named={"c": {"cursor": "<c1>"}})
gql("LIMIT FIRST", "SELECT * FROM P LIMIT FIRST(@c, 2)", allow=False, named={"c": {"cursor": "<c1>"}})
gql("OFFSET a cursor", "SELECT * FROM P OFFSET @c", allow=False, named={"c": {"cursor": "<c1>"}})
gql("OFFSET a cursor plus 1", "SELECT * FROM P OFFSET @c + 1", allow=False, named={"c": {"cursor": "<c1>"}})
gql("a cursor in WHERE", "SELECT * FROM P WHERE a = @c", allow=False, named={"c": {"cursor": "<c1>"}})
gql("a value as a cursor", "SELECT * FROM P OFFSET @c", allow=False, named={"c": V(I(2))})
gql("a negative LIMIT", "SELECT * FROM P LIMIT -1")
gql("LIMIT a double", "SELECT * FROM P LIMIT 1.5")

# --- refused -----------------------------------------------------------------------------------
for label, text in [
    ("empty", ""), ("not SELECT", "UPDATE P SET a = 1"), ("a stray token", "SELECT * FROM P WHERE"),
    ("an unknown operator", "SELECT * FROM P WHERE a == 2"), ("an unterminated string", "SELECT * FROM P WHERE b = 'x"),
    ("trailing text", "SELECT * FROM P extra"), ("an unquoted keyword as a property", "SELECT * FROM P WHERE order = 2"),
    ("a property condition without a kind", "SELECT * WHERE a = 2"), ("HAS ANCESTOR on a property", "SELECT * FROM C WHERE n HAS ANCESTOR KEY(Q, 'q1')"),
    ("DISTINCT ON a property not projected", "SELECT DISTINCT ON (a) b FROM P"), ("a projection and an equality", "SELECT a FROM P WHERE a = 2"),
    ("an unknown function", "SELECT * FROM P WHERE a = FOO(1)"), ("a key of an odd path", "SELECT * FROM T WHERE __key__ = KEY(T)"),
]:
    gql(f"refused: {label}", text)
CASES.append({"name": "no query string", "method": "POST", "path": f"/v1/projects/{base.P}:runQuery",
              "headers": base.JSON, "body": {"gqlQuery": {}}, "compare": "exact"})

# --- aggregations ------------------------------------------------------------------------------
A = "runAggregationQuery"
gql("SELECT COUNT(*)", "SELECT COUNT(*) FROM P", verb=A)
gql("COUNT AS an alias", "SELECT COUNT(*) AS n FROM P WHERE b = 'x'", verb=A)
gql("SUM and AVG", "SELECT COUNT(*), SUM(a) AS s, AVG(a) AS m FROM P", verb=A)
gql("COUNT_UP_TO", "SELECT COUNT_UP_TO(3) FROM P", verb=A)
gql("AGGREGATE OVER", "AGGREGATE COUNT(*) OVER (SELECT * FROM P WHERE b = 'x')", verb=A)
gql("AGGREGATE OVER a limit", "AGGREGATE COUNT(*) AS n OVER (SELECT * FROM P LIMIT 2)", verb=A)
gql("a limit without OVER", "SELECT COUNT(*) FROM P LIMIT 2", verb=A)
gql("an aggregation with runQuery", "SELECT COUNT(*) FROM P")
gql("a query with runAggregationQuery", "SELECT * FROM P", verb=A)
gql("lowercase aggregations", "select count(*) as n from P", verb=A)


if __name__ == "__main__":
    base.CASES[:] = CASES
    json.dump({
        "oracle": "cloud-firestore-emulator v1.22.0 (sha256 9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)",
        "generated_by": "tools/oracle/datastore_gql.py",
        "mode": "datastore-mode",
        "dataset": [{"key": k, "properties": props} for k, props in base.DATASET],
        "cases": base.run(),
    }, sys.stdout, ensure_ascii=False, indent=1)
    print()
