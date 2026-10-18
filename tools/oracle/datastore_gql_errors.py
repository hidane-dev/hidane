#!/usr/bin/env python3
"""How does the official emulator's GQL parser fail, at each point of its grammar? (#81)

Usage (from the repository root), with a jar freshly started with `--database-mode
datastore-mode` under an English locale:
    python3 -I tools/oracle/datastore_gql_errors.py 127.0.0.1:8086 \\
        > crates/hidane/tests/fixtures/datastore_gql_errors.json

The differential fuzzer (tools/fuzz/, `--mode datastore-mode`) found GQL refused with other
tokens expected, or for another reason, than hidane reported. This cuts each query of `CORPUS`
(the queries `datastore_gql.json` records as read, and a few more) after each of its tokens and
sends what is left, as it is (the parser then meets its end) and with a keyword that fits almost
nowhere after it (`ancestor`), over REST to the RPC the query is for; the first three cuts go to
the other RPC too. `EXTRA` adds texts about how the query's kind (entities or an aggregation) is
told and about lexical errors. Literals are allowed, and each binding a text names is given (an
integer), so that what is left to tell is the syntax. `SENT` adds texts sent with what they
need of their own (bindings, literals not allowed, the request's namespace) or nothing: the
differential fuzzer's findings and what was asked to explain them (when the parser lexes the
next token, `KEY()`'s partition, `DATETIME`, `CONTAINS`, binding names) and the checks of the
queries they read (ancestors, disjunctions, the queries of kinds, the projects of keys and what
comes before them). Each case is a request and its
answer, in the format of `datastore_entities.json` (without what `every_case` gives them all), on
one line; the database is empty. `crates/hidane/tests/datastore.rs` replays it. Stdlib only.
"""

import http.client
import json
import re
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from harness import mask  # noqa: E402

HOST = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1:8086"
PROJECT = f"gql-errors-{time.strftime('%H%M%S')}"
JSON = {"Content-Type": "application/json"}
EVERY_CASE = {"method": "POST", "headers": JSON, "compare": "exact"}
VERSION = re.compile(r'"(version|commitVersion|snapshotVersion)": "\d+"')
CURSOR = re.compile(r'"(cursor|endCursor|skippedCursor|startCursor)": "[^"]*"')

Q, A = "runQuery", "runAggregationQuery"
CORPUS = [(Q, q) for q in [
    "SELECT * FROM P", "SELECT * FROM `P`", "SELECT *", "SELECT __key__ FROM P", "SELECT a, b FROM P",
    "SELECT DISTINCT b FROM P", "SELECT DISTINCT ON (b) * FROM P", "SELECT DISTINCT ON (b, c) a, b FROM P",
    "SELECT * FROM P WHERE m.x = 1", "SELECT * FROM P WHERE `a` = 2", "SELECT * FROM P WHERE a != 2",
    "SELECT * FROM P WHERE a < 2", "SELECT * FROM P WHERE a <= 2", "SELECT * FROM P WHERE a > 2",
    "SELECT * FROM P WHERE a >= 2", "SELECT * FROM P WHERE 2 < a", "SELECT * FROM T WHERE v IS NULL",
    "SELECT * FROM P WHERE a IN ARRAY(1, 3)", "SELECT * FROM P WHERE a NOT IN ARRAY(1, 3)",
    "SELECT * FROM P WHERE 'red' IN tags", "SELECT * FROM P WHERE tags CONTAINS 'red'",
    "SELECT * FROM P WHERE a > 1 AND b = 'y'", "SELECT * FROM P WHERE a = 2 OR b = 'z'",
    "SELECT * FROM P WHERE (a = 3 OR a = 1) AND b = 'x'", "SELECT * FROM C WHERE __key__ HAS ANCESTOR KEY(Q, 'q1')",
    "SELECT * FROM C WHERE KEY(Q, 'q1') HAS DESCENDANT __key__", "SELECT * FROM T WHERE __key__ = KEY(T, 3)",
    "SELECT * WHERE __key__ = KEY(Q, 'q1', C, 'c1')",
    "SELECT * FROM T WHERE v = KEY(PROJECT('p'), NAMESPACE('ns1'), A, 'x')",
    "SELECT * FROM T WHERE v = -1", "SELECT * FROM T WHERE v = +.5", "SELECT * FROM T WHERE v = 15e-1",
    "SELECT * FROM T WHERE v = \"a\"", "SELECT * FROM T WHERE v = TRUE", "SELECT * FROM T WHERE v = false",
    "SELECT * FROM T WHERE v = NULL", "SELECT * FROM T WHERE v = DATETIME('2020-01-01T00:00:00Z')",
    "SELECT * FROM T WHERE v = BLOB('YQ')", "SELECT * FROM P WHERE a = @a AND b = @1",
    "SELECT * FROM P WHERE a IN @a", "SELECT * FROM P ORDER BY b DESC, a ASC", "SELECT * FROM P ORDER BY __key__",
    "SELECT * FROM P WHERE a > 1 ORDER BY b", "SELECT * FROM P LIMIT 3", "SELECT * FROM P LIMIT @n",
    "SELECT * FROM P LIMIT 2 OFFSET 2", "SELECT * FROM P OFFSET @c", "SELECT * FROM P LIMIT @c, 2",
    "SELECT * FROM P OFFSET @c + 2", "SELECT * FROM P LIMIT FIRST(2, @c)", "SELECT * FROM P LIMIT FIRST(2, 3)",
    "SELECT * FROM P OFFSET 2 + 3", "SELECT * FROM P GROUP BY a", "SELECT * FROM P GROUP BY a, b ORDER BY a",
    "SELECT * FROM P WHERE a = 1 GROUP BY a LIMIT 1", "SELECT a.b FROM P", "SELECT a, b, c FROM P", "SELECT `a` FROM P",
    "SELECT DISTINCT ON (a.b) a FROM P", "SELECT * FROM P WHERE __key__ HAS ANCESTOR @a",
    "SELECT * FROM P WHERE a IN ARRAY(@a, @1)", "SELECT * FROM P WHERE a IS NULL AND b = 1 OR c = 2",
    "SELECT * FROM P WHERE KEY(P, 1) = __key__", "SELECT * FROM P WHERE ARRAY(1) CONTAINS a",
    "SELECT * FROM P WHERE __key__ HAS PARENT KEY(Q, 'q')", "SELECT * FROM P WHERE KEY(Q, 'q') HAS CHILD __key__",
]] + [(A, q) for q in [
    "SELECT COUNT(*) FROM P", "SELECT COUNT(*) AS n FROM P WHERE b = 'x'",
    "SELECT COUNT(*), SUM(a) AS s, AVG(a) AS m FROM P", "SELECT COUNT_UP_TO(3) FROM P",
    "AGGREGATE COUNT(*) OVER (SELECT * FROM P WHERE b = 'x')", "AGGREGATE COUNT(*) AS n OVER (SELECT * FROM P LIMIT 2)",
    "AGGREGATE SUM(a), AVG(b) AS m OVER (SELECT a, b FROM P)", "AGGREGATE COUNT_UP_TO(5) OVER (SELECT * FROM P)",
    "AGGREGATE COUNT(*) OVER (SELECT * FROM P ORDER BY a OFFSET 1)",
    "AGGREGATE COUNT(*) OVER (SELECT DISTINCT ON (a) a FROM P WHERE a > @1 GROUP BY a)",
    "SELECT COUNT(*) FROM P WHERE a = 1 AND b = @a",
]]

# How a query's kind is told, and lexical errors.
EXTRA = [(verb, q) for verb in (Q, A) for q in [
    "", "SELECT", "AGGREGATE", "aggregate x", "AGGREGATE #", "AGGREGATE COUNT(*)", "AGGREGATEX",
    "SELECT COUNT", "SELECT COUNT FROM K", "SELECT COUNT(", "SELECT COUNT(*)", "SELECT count(*) FROM K",
    "SELECT SUM(a) FROM K", "SELECT sum", "SELECT sum(", "SELECT *", "SELECT DISTINCT", "SELECT a,", "SELECT #",
    "# SELECT", "SELECT 'x'", "SELECT COUNT(*), a FROM K", "SELECT a, COUNT(*) FROM K", "FROM K", "x",
    "SELECT * FROM K WHERE a = @1", "SELECT * FROM K WHERE", "select x y", "SELECT * FROM K WHERE a = 'x",
    "SELECT * FROM K WHERE a = \"x", "SELECT * FROM `K", "SELECT * FROM K WHERE a = @", "SELECT * FROM K WHERE a = 1e",
    "SELECT * FROM K WHERE a = .", "SELECT * FROM K WHERE a = 1.5.5", "SELECT * FROM K WHERE a == 1",
    "SELECT * FROM K WHERE a <> 1", "SELECT * FROM K WHERE a = !1", "SELECT * FROM K;",
]]

# Texts sent with what they need of their own (bindings, literals not allowed, the request's
# namespace), or nothing: (verb, text, what the request adds).
SENT = [
    # Lexical errors: the prefix is written unless at most one character came before the end.
    (Q, "SELECT * FROM K WHERE a = @ x"),
    (Q, "SELECT * FROM K WHERE a = @"),
    (Q, "SELECT * FROM K ORDER BY a'"),
    (Q, "SELECT * FROM K ORDER BY a\""),
    (Q, "SELECT * FROM K ORDER BY a`"),
    (Q, "SELECT * FROM K ORDER BY a'\\"),
    (Q, "SELECT * FROM K ORDER BY a !"),
    (Q, "SELECT * FROM K ORDER BY a !x"),
    (Q, "SELECT * FROM K ORDER BY a #"),
    (Q, "SELECT * FROM K ORDER BY a #x"),
    # The parser lexes the token after one before it acts on it: bindings, literals, counts.
    (Q, "SELECT * FROM K LIMIT @1`", {'positionalBindings': [{'value': {}}]}),
    (Q, "SELECT * FROM K LIMIT @a`", {'namedBindings': {'a': {'value': {}}}}),
    (Q, "SELECT * FROM K LIMIT @a x`", {'namedBindings': {'a': {'value': {}}}}),
    (Q, "SELECT * FROM K LIMIT @b`", {'namedBindings': {'a': {'value': {}}}}),
    (Q, "SELECT * FROM K LIMIT 1`"),
    (Q, "SELECT * FROM K LIMIT -1`"),
    (Q, "SELECT * FROM K LIMIT -1 x`"),
    (Q, "SELECT * FROM K LIMIT 1`", {'allowLiterals': False}),
    (Q, "SELECT * FROM K OFFSET @a`", {'namedBindings': {'a': {'value': {}}}}),
    (Q, "SELECT * FROM K OFFSET @b`", {'namedBindings': {'a': {'value': {}}}}),
    (Q, "SELECT * FROM K OFFSET 1`", {'allowLiterals': False}),
    (Q, "SELECT * FROM K LIMIT FIRST(@a`", {'namedBindings': {'a': {'value': {}}}}),
    (Q, "SELECT * FROM K LIMIT FIRST(@a, @b`", {'namedBindings': {'a': {'value': {}}, 'b': {'value': {}}}}),
    (Q, "SELECT * FROM K LIMIT FIRST(1, 2`"),
    (Q, "SELECT * FROM K LIMIT 2147483648"),
    (Q, "SELECT * FROM K LIMIT 2147483647"),
    (Q, "SELECT * FROM K LIMIT 2147483648 x x"),
    (Q, "SELECT * FROM K LIMIT 2147483648`"),
    (Q, "SELECT * FROM K LIMIT @a", {'namedBindings': {'a': {'value': {'integerValue': '2147483648'}}}}),
    (Q, "SELECT * FROM K LIMIT @a x", {'namedBindings': {'a': {'value': {'integerValue': '2147483648'}}}}),
    (Q, "SELECT * FROM K LIMIT FIRST(2147483648, @c)", {'namedBindings': {'c': {'cursor': ''}}}),
    (Q, "SELECT * FROM K OFFSET 2147483648"),
    (Q, "SELECT * FROM K OFFSET 2147483648 x"),
    (Q, "SELECT * FROM K OFFSET -1"),
    (Q, "SELECT * FROM K OFFSET -1 x"),
    (Q, "SELECT * FROM K WHERE a = @a`", {'namedBindings': {'a': {'cursor': ''}}}),
    (Q, "SELECT * FROM K WHERE a = @b`", {'namedBindings': {'a': {'cursor': ''}}}),
    (Q, "SELECT * FROM K WHERE a = 1`", {'allowLiterals': False}),
    (Q, "SELECT * FROM K WHERE a = 1 x`", {'allowLiterals': False}),
    (Q, "SELECT * FROM K WHERE a = 'x'`", {'allowLiterals': False}),
    # Functions, GROUP BY, OFFSET `+`, literals not allowed, values.
    (Q, "AGGREGATE`"),
    (Q, "AGGREGATE x"),
    (A, "SELECT a`"),
    (A, "SELECT *`"),
    (A, "SELECT DISTINCT`"),
    (Q, "SELECT COUNT(*)`"),
    (Q, "SELECT COUNT(*) x"),
    (A, "SELECT COUNT(a)`"),
    (A, "SELECT foo(a)`"),
    (A, "SELECT foo(`"),
    (A, "SELECT foo`"),
    (Q, "SELECT * FROM K GROUP BY`"),
    (Q, "SELECT * FROM K GROUP BY a"),
    (Q, "SELECT * FROM K OFFSET 1 +`"),
    (Q, "SELECT * FROM K OFFSET 2147483648 + 1"),
    (Q, "SELECT * FROM K OFFSET @c + 2147483648", {'namedBindings': {'c': {'cursor': ''}}}),
    (Q, "SELECT * FROM K OFFSET @c + 2147483648`", {'namedBindings': {'c': {'cursor': ''}}}),
    (Q, "SELECT * FROM K OFFSET @c + -1", {'namedBindings': {'c': {'cursor': ''}}}),
    (Q, "SELECT * FROM K LIMIT FIRST(2147483648, 5)"),
    (Q, "SELECT * FROM K LIMIT FIRST(5, 2147483648)"),
    (Q, "SELECT * FROM K LIMIT FIRST(-1, @c)", {'namedBindings': {'c': {'cursor': ''}}}),
    (Q, "SELECT * FROM K LIMIT FIRST(@c, -1)", {'namedBindings': {'c': {'cursor': ''}}}),
    (Q, "SELECT * FROM K LIMIT FIRST(@c, -1`)", {'namedBindings': {'c': {'cursor': ''}}}),
    (Q, "SELECT * FROM K LIMIT @c, 5", {'namedBindings': {'c': {'cursor': ''}}}),
    (Q, "SELECT * FROM K LIMIT @a", {'namedBindings': {'a': {'value': {'integerValue': '-1'}}}}),
    (Q, "SELECT * FROM K OFFSET @a", {'namedBindings': {'a': {'value': {'integerValue': '-1'}}}}),
    (Q, "SELECT * FROM K OFFSET @a", {'namedBindings': {'a': {'value': {'integerValue': '2147483648'}}}}),
    (Q, "SELECT * FROM K WHERE a = DATETIME('x'`"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('x')`"),
    (Q, "SELECT * FROM K WHERE a = BLOB('=')`"),
    (Q, "SELECT * FROM K WHERE a = BLOB('='`"),
    (Q, "SELECT * FROM K WHERE a = KEY(K, 1)`", {'allowLiterals': False}),
    (Q, "SELECT * FROM K WHERE a = KEY(K, 1`", {'allowLiterals': False}),
    (Q, "SELECT * FROM K WHERE a = ARRAY(1`", {'allowLiterals': False}),
    (Q, "SELECT * FROM K WHERE a IS NULL`", {'allowLiterals': False}),
    (Q, "SELECT * FROM K WHERE a = NULL`", {'allowLiterals': False}),
    (Q, "SELECT * FROM K WHERE a = TRUE`", {'allowLiterals': False}),
    (Q, "SELECT * FROM K WHERE a = foo(`"),
    (Q, "SELECT * FROM K WHERE a = foo`"),
    (Q, "SELECT * FROM K WHERE a = foo x"),
    (Q, "SELECT * FROM K WHERE a = foo"),
    (Q, "SELECT * FROM K WHERE a = key"),
    (Q, "SELECT * FROM K WHERE a = key x"),
    (Q, "SELECT * FROM K WHERE a = `foo`"),
    (Q, "SELECT * FROM K WHERE foo = a"),
    (Q, "SELECT * FROM K WHERE a HAS PARENT KEY(K, 1)`"),
    (Q, "SELECT * FROM K WHERE a HAS PARENT`"),
    (Q, "SELECT * FROM K WHERE a HAS ANCESTOR`"),
    (Q, "SELECT * FROM K WHERE 1.5e = a"),
    (Q, "SELECT * FROM K WHERE a = 1.5e`"),
    (Q, "SELECT * FROM K WHERE a = 1.5e x"),
    (Q, "SELECT * FROM K WHERE a = 99999999999999999999 x"),
    (Q, "SELECT * FROM K WHERE a = 99999999999999999999`"),
    (Q, "SELECT * FROM K WHERE a = KEY", {'allowLiterals': False}),
    (Q, "SELECT * FROM K WHERE a = KEY`", {'allowLiterals': False}),
    (Q, "SELECT * FROM K WHERE a = KEY x", {'allowLiterals': False}),
    (Q, "SELECT * FROM K WHERE a = KEY(`", {'allowLiterals': False}),
    (Q, "SELECT * FROM K WHERE a = KEY(x", {'allowLiterals': False}),
    (Q, "SELECT * FROM K WHERE a = ARRAY", {'allowLiterals': False}),
    (Q, "SELECT * FROM K WHERE a = ARRAY(", {'allowLiterals': False}),
    (Q, "SELECT * FROM K WHERE a = ARRAY(@a)", {'allowLiterals': False, 'namedBindings': {'a': {'value': {}}}}),
    (Q, "SELECT * FROM K WHERE a = ARRAY()", {'allowLiterals': False, 'namedBindings': {'a': {'value': {}}}}),
    (Q, "SELECT * FROM K WHERE a = BLOB(", {'allowLiterals': False}),
    (Q, "SELECT * FROM K WHERE a = BLOB(x", {'allowLiterals': False}),
    (Q, "SELECT * FROM K WHERE a = DATETIME(x", {'allowLiterals': False}),
    (Q, "SELECT * FROM K WHERE a = foo(x", {'allowLiterals': False}),
    (Q, "SELECT * FROM K WHERE a = foo", {'allowLiterals': False}),
    (Q, "SELECT * FROM K WHERE KEY(K, 1) = a", {'allowLiterals': False}),
    (Q, "SELECT * FROM K WHERE KEY = a", {'allowLiterals': False}),
    (Q, "SELECT * FROM K WHERE a = PROJECT('x')"),
    (Q, "SELECT * FROM K WHERE a = NAMESPACE('x')"),
    (Q, "SELECT * FROM K WHERE a = first"),
    (Q, "SELECT * FROM K WHERE a = Key(K, 1)"),
    (Q, "SELECT * FROM K WHERE a = array(1)"),
    (Q, "SELECT * FROM K WHERE a = blob('YQ')"),
    (Q, "SELECT * FROM K WHERE a = datetime('2020-01-01T00:00:00Z')"),
    (Q, "SELECT * FROM K WHERE a = KEY(project('p'), K, 1)"),
    (Q, "SELECT * FROM K WHERE a = KEY(PROJECT, K, 1)"),
    (Q, "SELECT * FROM K WHERE a = KEY(PROJECT x, K, 1)"),
    (Q, "SELECT * FROM K WHERE a = KEY(NAMESPACE, K, 1)"),
    # KEY(): PROJECT() then NAMESPACE(), at most once each; without them, the request's namespace.
    (Q, "SELECT * FROM K WHERE a = KEY(namespace('x'), project('p'), K, 1)"),
    (Q, "SELECT * FROM K WHERE a = key", {'allowLiterals': False}),
    (Q, "SELECT * FROM K WHERE a = Array", {'allowLiterals': False}),
    (Q, "SELECT * FROM K WHERE a = KEY(namespace('x'), project('p'), K, 1)`"),
    (Q, "SELECT * FROM K WHERE a = KEY(namespace('x'), project("),
    (Q, "SELECT * FROM K WHERE a = KEY(namespace('x'), project"),
    (Q, "SELECT * FROM K WHERE a = KEY(namespace('x'), project x"),
    (Q, "SELECT * FROM K WHERE a = KEY(namespace('x'), namespace('y'), K, 1)"),
    (Q, "SELECT * FROM K WHERE a = KEY(project('x'), project('y'), K, 1)"),
    (Q, "SELECT * FROM K WHERE a = KEY(project('x'), namespace('y'), project('z'), K, 1)"),
    (Q, "SELECT * FROM K WHERE a = KEY(namespace('x'), K, 1)"),
    (Q, "SELECT * FROM K WHERE a = KEY(namespace(\"x\"), K, 1)"),
    (Q, "SELECT * FROM K WHERE a = KEY(K, 1, namespace('x'), 2)"),
    (Q, "SELECT * FROM K WHERE a = KEY(namespace(@a), K, 1)", {'namedBindings': {'a': {'value': {'stringValue': 'x'}}}}),
    (Q, "SELECT * FROM K WHERE a = KEY(namespace('x') K, 1)"),
    (Q, "SELECT * FROM K WHERE a = KEY(project('x') K, 1)"),
    (Q, "SELECT * FROM K WHERE a = KEY(namespace('x'),"),
    (Q, "SELECT * FROM K WHERE a = KEY(project('x'),"),
    (Q, "SELECT * FROM K WHERE a = KEY("),
    (Q, "SELECT * FROM K WHERE a = KEY(namespace"),
    (Q, "SELECT * FROM K WHERE a = KEY(namespace("),
    (Q, "SELECT * FROM K WHERE a = KEY(namespace('x'"),
    (Q, "SELECT * FROM K WHERE a = KEY(project"),
    (Q, "SELECT * FROM K WHERE a = KEY(K, 1)", {'partitionId': {'namespaceId': 'ns'}}),
    (Q, "SELECT * FROM K WHERE a = KEY(namespace(''), K, 1)", {'partitionId': {'namespaceId': 'ns'}}),
    (Q, "SELECT * FROM K WHERE a = KEY(project('x'), K, 1)", {'partitionId': {'namespaceId': 'ns'}}),
    (Q, "SELECT * FROM K WHERE a = KEY(project('gq'), K, 1)"),
    (Q, "SELECT * FROM K WHERE a = KEY(K, 1)"),
    (Q, "SELECT * FROM K WHERE a = KEY(namespace('x'), project(`"),
    (Q, "SELECT * FROM K WHERE a = KEY(namespace('x'), project`"),
    (Q, "SELECT * FROM K WHERE a = KEY(namespace('x'), namespace(`"),
    (Q, "SELECT * FROM K WHERE a = KEY(namespace('x'), namespace('y'`"),
    (Q, "SELECT * FROM K WHERE a = KEY(project('x'), project(`"),
    (Q, "SELECT * FROM K WHERE a = KEY(project('x'), namespace('y'), K, 1)"),
    (Q, "SELECT * FROM K WHERE a = KEY(project('x'), namespace('y'), K, 1)", {'allowLiterals': False}),
    (Q, "SELECT * FROM K WHERE __key__ HAS ANCESTOR KEY(K, 1)", {'partitionId': {'namespaceId': 'ns'}}),
    (Q, "SELECT * FROM K WHERE a IN ARRAY(KEY(K, 1))", {'partitionId': {'namespaceId': 'ns'}}),
    (Q, "SELECT * FROM K WHERE a = @a", {'namedBindings': {'a': {'value': {'keyValue': {'path': [{'kind': 'K', 'id': '1'}]}}}}, 'partitionId': {'namespaceId': 'ns'}}),
    # Selects of functions: the query's kind, told as the select is read.
    (Q, "SELECT a("),
    (Q, "SELECT a(`"),
    (Q, "SELECT a`"),
    (Q, "SELECT a(b"),
    (Q, "SELECT a(b)"),
    (Q, "SELECT a(*) FROM K"),
    (Q, "SELECT DISTINCT a("),
    (Q, "SELECT DISTINCT ON (a) a("),
    (Q, "SELECT DISTINCT ON (a) COUNT(`"),
    (Q, "SELECT DISTINCT ON (a) COUNT(*)"),
    (Q, "SELECT DISTINCT COUNT(*)"),
    (Q, "SELECT COUNT(`"),
    (Q, "SELECT COUNT`"),
    (Q, "SELECT COUNT x"),
    (Q, "SELECT COUNT(*) AS"),
    (Q, "SELECT COUNT(*) AS x`"),
    (Q, "SELECT COUNT(*),`"),
    (Q, "SELECT COUNT(*), a"),
    (Q, "SELECT COUNT(*), a("),
    (Q, "SELECT COUNT(*), a(b) x"),
    (Q, "SELECT a, b("),
    (Q, "SELECT a, COUNT(*)"),
    (Q, "SELECT `a`("),
    (A, "SELECT a(`"),
    (A, "SELECT a(b) x"),
    (A, "SELECT `a`("),
    (A, "SELECT COUNT(*), a"),
    (A, "SELECT COUNT(*), a("),
    (Q, "SELECT COUNT(a)"),
    (Q, "SELECT COUNT(a) x"),
    (Q, "SELECT COUNT(a), b("),
    (Q, "SELECT sum(a), b("),
    (Q, "SELECT sum(a) x"),
    (Q, "SELECT count_up_to(`"),
    (Q, "SELECT count_up_to(1)"),
    (Q, "SELECT a(b), COUNT(*)"),
    (Q, "SELECT COUNT(*) AS x, SUM(`"),
    (Q, "SELECT COUNT(*) AS x, a(`"),
    (A, "SELECT a(b), COUNT(*)"),
    (A, "SELECT COUNT(*) FROM K WHERE a = @1`", {'positionalBindings': [{'value': {}}]}),
    (A, "SELECT COUNT(*) FROM K x"),
    (A, "SELECT COUNT(*) x"),
    (A, "AGGREGATE a("),
    (A, "AGGREGATE a(`"),
    (A, "AGGREGATE COUNT(a)`"),
    (A, "AGGREGATE COUNT(*) OVER (SELECT a("),
    (Q, "SELECT * FROM K WHERE a("),
    # CONTAINS and IN: an array makes IN, anything else an equality.
    (Q, "SELECT * FROM K WHERE a CONTAINS 1"),
    (Q, "SELECT * FROM K WHERE a CONTAINS ARRAY(1)"),
    (Q, "SELECT * FROM K WHERE a CONTAINS @a", {'namedBindings': {'a': {'value': {'arrayValue': {'values': [{'integerValue': '1'}]}}}}}),
    (Q, "SELECT * FROM K WHERE a NOT IN 1"),
    (Q, "SELECT * FROM K WHERE a NOT CONTAINS 1"),
    (Q, "SELECT * FROM K WHERE a NOT CONTAINS ARRAY(1)"),
    (Q, "SELECT * FROM K WHERE a NOT IN ARRAY(1)"),
    (Q, "SELECT * FROM K WHERE 1 IN a"),
    (Q, "SELECT * FROM K WHERE ARRAY(1) IN a"),
    (Q, "SELECT * FROM K WHERE a IN 1"),
    (Q, "SELECT * FROM K WHERE a = ARRAY(1)"),
    (Q, "SELECT * FROM K WHERE a != ARRAY(1)"),
    # COUNT of a property; functions after other items; JavaCC's longest match.
    (A, "SELECT COUNT(a`"),
    (A, "SELECT COUNT(a x"),
    (A, "SELECT COUNT(a.b`"),
    (A, "SELECT COUNT(a.b)`"),
    (A, "SELECT COUNT(a.`"),
    (A, "SELECT COUNT(`a``"),
    (A, "SELECT COUNT(`a`)`"),
    (A, "SELECT COUNT(a"),
    (Q, "SELECT a, b(`"),
    (Q, "SELECT a.b("),
    (Q, "SELECT a, b.c("),
    (Q, "SELECT a, `b`("),
    (Q, "SELECT a, b, c("),
    (Q, "SELECT a, b(c) FROM K"),
    (A, "SELECT a, b("),
    (Q, "SELECT DISTINCT ON (a("),
    (Q, "SELECT DISTINCT ON (`a`("),
    (Q, "SELECT * FROM K ORDER BY a("),
    (Q, "SELECT * FROM K WHERE a = 'x''"),
    (Q, "SELECT * FROM K WHERE a = 'x'' y"),
    (Q, "SELECT * FROM K WHERE a = 'x''\\q'"),
    (Q, "SELECT * FROM K WHERE a = \"x\"\""),
    (Q, "SELECT * FROM `K``"),
    (Q, "SELECT * FROM `K`` x"),
    # DATETIME, read as Java's GregorianCalendar reads it.
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-01-01t00:00:00z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-01-01T00:00:00z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-01-01t00:00:00Z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-01-01 00:00:00Z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-01-01T00:00:00')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-01-01T00:00Z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-01-01')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-1-1T00:00:00Z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-01-01T00:00:00.5Z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-01-01T00:00:00.123456789Z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-01-01T00:00:00.1234567891Z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-01-01T00:00:00+00:00')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-01-01T00:00:00+01:00')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-01-01T00:00:00-00:00')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-01-01T00:00:00+0100')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-01-01T00:00:00+01')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-01-01T24:00:00Z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-02-30T00:00:00Z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-13-01T00:00:00Z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-01-01T00:60:00Z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-01-01T00:00:60Z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('20200-01-01T00:00:00Z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('0000-01-01T00:00:00Z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('0001-01-01T00:00:00Z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('9999-12-31T23:59:59.999999Z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME(' 2020-01-01T00:00:00Z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-01-01T00:00:00Zx')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-01-01T00:00:00.Z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('+2020-01-01T00:00:00Z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-01-01T00:00:00.000001Z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-01-01T00:00:00.0000001Z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-01-01T00:00:00+14:00')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-01-01T00:00:00+23:59')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-01-01T00:00:00+24:00')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-01-01T00:00:00+01:60')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('0001-01-01T00:00:00.000001Z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('0001-01-02T00:00:00Z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('0001-01-03T00:00:00Z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('0001-02-01T00:00:00Z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('0100-01-01T00:00:00Z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('1000-01-01T00:00:00Z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('1582-10-10T00:00:00Z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('1582-10-15T00:00:00Z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('1900-02-29T00:00:00Z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('1500-02-29T00:00:00Z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2019-02-29T00:00:00Z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-02-29T00:00:00Z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-04-31T00:00:00Z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-00-01T00:00:00Z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-01-00T00:00:00Z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-13-01T00:00:00+00:00')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-01-01T00:00:00+18:00')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-01-01T00:00:00-18:00')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-01-01T00:00:00+18:01')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-01-01T00:00:00+00:01')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-01-01T00:00:00-00:30')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('9999-12-31T23:59:59.999999-01:00')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('0001-01-01T00:00:00+01:00')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('0001-01-01T05:00:00+01:00')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-01-01T00:00:00.1z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME(\"2020-01-01T00:00:00Z\")"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('２０２０-01-01T00:00:00Z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-01-01T00:00:00-13:00')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-01-01T00:00:00-13:01')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-01-01T00:00:00+14:01')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-01-01T00:00:00+13:59')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-01-01T00:00:00-12:59')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-01-01T00:00:00+00:60')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-01-01T00:00:00+01:59')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('0001-01-03T00:00:00.000001Z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('0001-01-02T23:59:59.999999Z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('1582-10-04T00:00:00Z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('1582-10-05T00:00:00Z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('1582-10-14T23:59:59Z')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('1582-10-15T00:00:00+01:00')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('1582-10-04T23:30:00-01:00')"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('0001-01-01T00:00:00Z') x x"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('0001-01-01T00:00:00Z'"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('0001-01-01T00:00:00Z')`"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('0001-01-01T00:00:00Z') AND b = @nope"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-13-01T00:00:00Z' x"),
    (Q, "SELECT * FROM K WHERE a = DATETIME('2020-13-01T00:00:00Z'"),
    # A syntax error: the token after the one met is lexed first.
    (Q, "SELECT * FROM `a` b`"),
    (Q, "SELECT * FROM a b`"),
    (Q, "SELECT b, a FROM `a` b`"),
    (Q, "SELECT b FROM a b c`"),
    (Q, "SELECT b.c FROM a b`"),
    (Q, "SELECT b, c FROM a WHERE x = 1 ORDER BY x b`"),
    (Q, "SELECT b, c b`"),
    (Q, "SELECT b, c FROM a ORDER BY x LIMIT 1 b`"),
    (Q, "SELECT COUNT x`"),
    (Q, "SELECT * FROM K WHERE a = = `"),
    (Q, "SELECT * FROM 1 `"),
    (Q, "SELECT * FROM K WHERE a = KEY(1 `"),
    (Q, "SELECT * FROM K WHERE a = KEY(K, x `"),
    (Q, "SELECT * FROM K WHERE a IS x `"),
    (Q, "SELECT * FROM K WHERE a HAS x `"),
    (Q, "SELECT * FROM K WHERE 1 x `"),
    (Q, "SELECT * FROM K WHERE ( x `"),
    (Q, "SELECT * FROM K WHERE ) `"),
    (Q, "SELECT * FROM K ORDER x `"),
    (Q, "x `"),
    (A, "AGGREGATE COUNT(*) OVER x `"),
    (A, "AGGREGATE COUNT(*) x `"),
    (A, "SELECT COUNT(*) FROM K WHERE a = 1 x `"),
    (Q, "SELECT DISTINCT ON a `"),
    (Q, "SELECT * FROM K LIMIT FIRST x `"),
    (Q, "SELECT * FROM K LIMIT x `"),
    (Q, "SELECT * FROM K OFFSET x `"),
    # Ancestors in conjunctions and disjunctions; a disjunction of ancestors alone.
    (Q, "SELECT * FROM K WHERE (__key__ HAS ANCESTOR KEY(K, 'a') AND x = 1) OR (__key__ HAS ANCESTOR KEY(K, 'a') AND y = 2)"),
    (Q, "SELECT * FROM K WHERE __key__ HAS ANCESTOR KEY(K, 'a') AND (x = 1 OR y = 2)"),
    (Q, "SELECT * FROM K WHERE (__key__ HAS ANCESTOR KEY(K, 'a') AND x = 1) OR y = 2"),
    (Q, "SELECT * FROM K WHERE (__key__ HAS ANCESTOR KEY(K, 'a') AND x = 1) OR (__key__ HAS ANCESTOR KEY(K, 'b') AND y = 2)"),
    (Q, "SELECT * FROM K WHERE __key__ HAS ANCESTOR KEY(K, 'a') OR __key__ HAS ANCESTOR KEY(K, 'a')"),
    (Q, "SELECT * FROM K WHERE __key__ HAS ANCESTOR KEY(K, 'a') AND (x = 1 OR (y = 2 AND __key__ HAS ANCESTOR KEY(K, 'a')))"),
    (Q, "SELECT * FROM K WHERE __key__ HAS ANCESTOR KEY(K, 'a') AND (x = 1 OR (y = 2 AND __key__ HAS ANCESTOR KEY(K, 'b')))"),
    (Q, "SELECT * FROM K WHERE __key__ HAS ANCESTOR KEY(K, 'a') AND __key__ HAS ANCESTOR KEY(K, 'b')"),
    (Q, "SELECT * FROM K WHERE __key__ HAS ANCESTOR KEY(K, 'a') AND __key__ HAS ANCESTOR KEY(K, 'a') AND __key__ HAS ANCESTOR KEY(K, 'b')"),
    (A, "AGGREGATE COUNT(*) OVER (SELECT * FROM K WHERE (__key__ HAS ANCESTOR KEY(K, 'a') AND x = 1) OR (__key__ HAS ANCESTOR KEY(K, 'a') AND y = 2))"),
    (A, "AGGREGATE COUNT(*) OVER (SELECT * FROM K WHERE (__key__ HAS ANCESTOR KEY(K, 'a') AND x = 1) OR y = 2)"),
    (A, "AGGREGATE COUNT(*) OVER (SELECT * FROM K WHERE __key__ HAS ANCESTOR KEY(K, 'a') OR __key__ HAS ANCESTOR KEY(K, 'b'))"),
    (A, "AGGREGATE COUNT(*) OVER (SELECT * FROM K WHERE __key__ HAS ANCESTOR KEY(K, 'a') AND __key__ HAS ANCESTOR KEY(K, 'a'))"),
    (A, "AGGREGATE COUNT(*) OVER (SELECT * FROM K WHERE __key__ HAS ANCESTOR KEY(K, 'a') AND __key__ HAS ANCESTOR KEY(K, 'b'))"),
    (Q, "SELECT * FROM K WHERE __key__ HAS ANCESTOR KEY(K, 'a') AND __key__ HAS ANCESTOR KEY(K, 'a')"),
    (Q, "SELECT * FROM K WHERE __key__ HAS ANCESTOR KEY(K, 'a') OR (__key__ HAS ANCESTOR KEY(K, 'a') AND x = 1)"),
    (Q, "SELECT * FROM K WHERE (__key__ HAS ANCESTOR KEY(K, 'a') AND x = 1 AND __key__ HAS ANCESTOR KEY(K, 'a')) OR (__key__ HAS ANCESTOR KEY(K, 'a') AND x = 2)"),
    (Q, "SELECT * FROM K WHERE __key__ HAS ANCESTOR KEY(K, 'a') AND (__key__ HAS ANCESTOR KEY(K, 'a') OR x = 2)"),
    (Q, "SELECT * FROM K WHERE __key__ HAS ANCESTOR KEY(K, 'a') AND __key__ HAS ANCESTOR KEY(K, 'a') AND (x = 1 OR x = 2)"),
    (Q, "SELECT * FROM K WHERE (x = 1 OR x = 2) AND (__key__ HAS ANCESTOR KEY(K, 'a') OR __key__ HAS ANCESTOR KEY(K, 'a'))"),
    (Q, "SELECT * FROM K WHERE __key__ HAS ANCESTOR KEY(K, 'a') OR (__key__ HAS ANCESTOR KEY(K, 'a') AND __key__ HAS ANCESTOR KEY(K, 'a'))"),
    (Q, "SELECT * FROM K WHERE (__key__ HAS ANCESTOR KEY(K, 'a') OR __key__ HAS ANCESTOR KEY(K, 'a')) AND x = 1"),
    (Q, "SELECT a, a FROM K WHERE __key__ HAS ANCESTOR KEY(K, 'a') OR __key__ HAS ANCESTOR KEY(K, 'a')"),
    (Q, "SELECT * FROM __kind__ WHERE __key__ HAS ANCESTOR KEY(K, 'a') OR __key__ HAS ANCESTOR KEY(K, 'a')"),
    (A, "AGGREGATE COUNT(*) OVER (SELECT * FROM K WHERE __key__ HAS ANCESTOR KEY(K, 'a') OR __key__ HAS ANCESTOR KEY(K, 'a'))"),
    (A, "AGGREGATE COUNT(*) OVER (SELECT * FROM K WHERE __key__ HAS ANCESTOR KEY(K, 'a') AND (__key__ HAS ANCESTOR KEY(K, 'a') OR x = 2))"),
    (A, "AGGREGATE COUNT(*) OVER (SELECT * FROM K WHERE x = 1 OR (__key__ HAS ANCESTOR KEY(K, 'a') AND __key__ HAS ANCESTOR KEY(K, 'b')))"),
    (Q, "SELECT * FROM K WHERE __key__ HAS ANCESTOR KEY(K, 'a') OR __key__ HAS ANCESTOR KEY(K, 'a') OR (__key__ HAS ANCESTOR KEY(K, 'a') AND x = 1)"),
    (Q, "SELECT * FROM K WHERE (__key__ HAS ANCESTOR KEY(K, 'a') OR __key__ HAS ANCESTOR KEY(K, 'a')) AND b = ARRAY(1)"),
    (Q, "SELECT * FROM K WHERE (__key__ HAS ANCESTOR KEY(K, 'a') OR __key__ HAS ANCESTOR KEY(K, 'a')) AND b IN ARRAY()"),
    (Q, "SELECT * FROM K WHERE (__key__ HAS ANCESTOR KEY(K, 'a') OR __key__ HAS ANCESTOR KEY(K, 'a')) AND (a NOT IN ARRAY(1) AND b NOT IN ARRAY(1))"),
    (Q, "SELECT * FROM K WHERE (__key__ HAS ANCESTOR KEY(K, 'a') OR __key__ HAS ANCESTOR KEY(K, 'a')) LIMIT @a", {'namedBindings': {'a': {'value': {'integerValue': '-1'}}}}),
    (Q, "SELECT * FROM K WHERE (__key__ HAS ANCESTOR KEY(K, 'a') OR __key__ HAS ANCESTOR KEY(K, 'b')) AND b = ARRAY(1)"),
    # Equalities in disjunctions; checks in disjunctive normal form; at most 30 disjunctions.
    (Q, "SELECT a FROM K WHERE a = 1 OR b = 2"),
    (Q, "SELECT a FROM K WHERE (a IN ARRAY(1, 2) OR b = 1)"),
    (Q, "SELECT DISTINCT ON (a) * FROM K WHERE a = 1 OR b = 2"),
    (Q, "SELECT DISTINCT ON (a) * FROM K WHERE (a = 1 AND c = 3) OR b = 2"),
    (Q, "SELECT a, b FROM Other WHERE (e.x = 1 OR a IS NULL) AND (__key__ > @1)", {'positionalBindings': [{'value': {'booleanValue': False}}]}),
    (Q, "SELECT a, b FROM K WHERE (b = 1 OR a IS NULL) AND (__key__ > 1)"),
    (Q, "SELECT d FROM K WHERE (c = 1 OR d = 2) AND (e = 1 OR __key__ > false)"),
    (Q, "SELECT * FROM K WHERE " + " OR ".join(f"a = {n}" for n in range(31))),
    (Q, "SELECT * FROM K WHERE " + " AND ".join(f"(b{n} = 1 OR c{n} = 2)" for n in range(6))),
    (Q, "SELECT * FROM K WHERE a IN ARRAY(1,2,3,4,5,6) AND (b = 1 OR b = 2 OR b = 3 OR b = 4 OR b = 5 OR b = 6)"),
    (Q, "SELECT a FROM K WHERE (c = 1 OR c = 1) AND (__key__ > false OR a = 1)"),
    (Q, "SELECT a FROM K WHERE (c = 1 OR c = 2) AND (a = 1 OR __key__ > false)"),
    (Q, "SELECT a FROM K WHERE c IN ARRAY(1, 2) AND (a = 1 OR __key__ > false)"),
    # The queries of kinds and namespaces; reserved names; ancestors that are not keys.
    (Q, "SELECT * FROM __kind__ WHERE __key__ IN ARRAY(KEY(__kind__, 'a'), KEY(K, 'x')) AND a = 1"),
    (Q, "SELECT * FROM __kind__ WHERE (__key__ = KEY(__kind__, 'a') OR __key__ = KEY(K, 'x')) AND __key__ = KEY(P, 'p', __kind__, 'b')"),
    (Q, "SELECT a FROM K WHERE __key__ IN ARRAY(KEY(K, 'x'), KEY(namespace('n'), K, 'y')) AND a = 1"),
    (Q, "SELECT * FROM K WHERE __key__ IN ARRAY(KEY(K, 'x'), 's') AND kindless = 1"),
    (Q, "SELECT * FROM __kind__ ORDER BY a, b DESC"),
    (Q, "SELECT * FROM __kind__ ORDER BY a DESC, b"),
    (Q, "SELECT * FROM __namespace__ WHERE a = @v ORDER BY a ASC, b DESC", {'namedBindings': {'v': {'value': {'booleanValue': True}}}}),
    (Q, "SELECT DISTINCT ON (a) * FROM __namespace__ ORDER BY a, tags DESC"),
    (Q, "SELECT __KEY__ FROM K"),
    (Q, "SELECT * FROM K WHERE a HAS ANCESTOR ARRAY(1)"),
    (Q, "SELECT * FROM K WHERE a HAS ANCESTOR @a", {'namedBindings': {'a': {'value': {'entityValue': {}}}}}),
    (Q, "SELECT * FROM K WHERE ARRAY(KEY(K, 1)) HAS DESCENDANT a"),
    # GQL is read before the request's partition and read options are checked.
    (Q, "SELECT x x", {'partitionId': {'namespaceId': 'é'}}),
    (Q, "SELECT x x", {'readOptions': {'readConsistency': 'READ_CONSISTENCY_UNSPECIFIED'}}),
    (A, "SELECT x x", {'readOptions': {'transaction': ''}}),
    (A, "SELECT COUNT(*) FROM K", {'partitionId': {'namespaceId': 'é'}}),
    (Q, "SELECT * FROM K WHERE a = KEY(K, 1)", {'partitionId': {'namespaceId': 'é'}}),
    # Binding names.
    (Q, "SELECT * FROM K", {'namedBindings': {'': {'value': {}}}}),
    (Q, "SELECT * FROM K", {'namedBindings': {'': {'cursor': ''}}}),
    (Q, "SELECT * FROM K", {'namedBindings': {'': {}}}),
    (Q, "SELECT * FROM K", {'namedBindings': {'a': {}}}),
    (Q, "SELECT * FROM K", {'namedBindings': {'1a': {'value': {}}}, 'positionalBindings': [{}]}),
    (Q, "SELECT * FROM K", {'namedBindings': {'1a': {}}}),
    (Q, "SELECT * FROM K", {'namedBindings': {'__x__': {'value': {}}}}),
    (Q, "SELECT * FROM K", {'namedBindings': {'a b': {'value': {}}}}),
    (Q, "SELECT * FROM K", {'namedBindings': {'é': {'value': {}}}}),
    (Q, "SELECT * FROM K", {'namedBindings': {'a-b': {'value': {}}}}),
    (Q, "SELECT * FROM K", {'namedBindings': {'a$b': {'value': {}}}}),
    (Q, "SELECT * FROM K", {'namedBindings': {'____': {'value': {}}}}),
    (Q, "SELECT * FROM K", {'namedBindings': {'_____': {'value': {}}}}),
    (Q, "SELECT * FROM K", {'namedBindings': {'__1__': {'value': {}}}}),
    (Q, "SELECT * FROM K", {'namedBindings': {'0a': {'value': {}}}}),
    (Q, "SELECT * FROM K", {'namedBindings': {'1a-': {'value': {}}}}),
    (Q, "SELECT * FROM K", {'namedBindings': {'__a-__': {'value': {}}}}),
    (Q, "SELECT * FROM K", {'namedBindings': {'@a': {'value': {}}}}),
    (A, "SELECT COUNT(*) FROM K", {'namedBindings': {'': {'value': {}}}}),
    # A key's project: an app ID, or an entity query refuses it (once it told of an unused
    # positional binding, before it tells of its other keys in turn and of HAS PARENT) and an
    # aggregation fails (unless HAS PARENT came first). An aggregation never tells of an unused
    # positional binding.
    (Q, "SELECT * FROM K WHERE A > KEY(PROJECT('Bad'), K, 'a')"),
    (A, "AGGREGATE COUNT(*) OVER (SELECT * FROM K WHERE A > KEY(PROJECT('Bad'), K, 'a'))"),
    (Q, "SELECT A FROM K WHERE B HAS ANCESTOR KEY(PROJECT('{PROJECT}'), K, 'D2')"),
    (A, "AGGREGATE COUNT_UP_TO(2) OVER (SELECT DISTINCT ON (A) A, B FROM __NAMESPACE__ WHERE A > KEY(PROJECT('{PROJECT}'), K, 'D2'))"),
    (Q, "SELECT * FROM K WHERE A > KEY(PROJECT('example.com:p'), K, 'a')"),
    (Q, "SELECT * FROM K WHERE A > KEY(PROJECT('a.b'), K, 'a')"),
    (Q, "SELECT * FROM K WHERE A > KEY(PROJECT('x:y:z'), K, 'a')"),
    (Q, "SELECT * FROM K WHERE A > KEY(PROJECT('UPPER:p'), K, 'a')"),
    (Q, "SELECT * FROM K WHERE A > KEY(PROJECT('s~p'), K, 'a')"),
    (Q, "SELECT * FROM K WHERE A > KEY(PROJECT('-a'), K, 'a')"),
    (Q, "SELECT * FROM K WHERE A > KEY(PROJECT('" + "a" * 101 + "'), K, 'a')"),
    (Q, "SELECT * FROM K WHERE A IN ARRAY(KEY(PROJECT('Bad'), K, 'a'))"),
    (Q, "SELECT * FROM K WHERE A > @k", {'namedBindings': {'k': {'value': {'keyValue': {'partitionId': {'projectId': 'Bad'}, 'path': [{'kind': 'K', 'name': 'a'}]}}}}}),
    (A, "AGGREGATE COUNT(*) OVER (SELECT * FROM K WHERE A > @1)", {'positionalBindings': [{'value': {'keyValue': {'partitionId': {'projectId': 'Bad'}, 'path': [{'kind': 'K', 'name': 'a'}]}}}]}),
    (Q, "SELECT * FROM K WHERE A > KEY(PROJECT('Bad'), K, 'a')", {'positionalBindings': [{'value': {'integerValue': '1'}}]}),
    (A, "AGGREGATE COUNT(*) OVER (SELECT * FROM K WHERE A > KEY(PROJECT('Bad'), K, 'a'))", {'positionalBindings': [{'value': {'integerValue': '1'}}]}),
    (Q, "SELECT * FROM K WHERE B HAS PARENT KEY(K, 'b') AND A > KEY(PROJECT('Bad'), K, 'a')"),
    (A, "AGGREGATE COUNT(*) OVER (SELECT * FROM K WHERE B HAS PARENT KEY(K, 'b') AND A > KEY(PROJECT('Bad'), K, 'a'))"),
    (A, "AGGREGATE COUNT(*) OVER (SELECT * FROM K WHERE A > KEY(PROJECT('Bad'), K, 'a') AND B HAS PARENT KEY(K, 'b'))"),
    (Q, "SELECT * FROM K WHERE B HAS PARENT KEY(K, 'b') AND A > KEY(K, 0)"),
    (Q, "SELECT * FROM K WHERE B HAS PARENT KEY(K, 'b') AND A > KEY(NAMESPACE('é'), K, 'a')"),
    (Q, "SELECT * FROM K WHERE B HAS PARENT KEY(K, 'b')", {'positionalBindings': [{'value': {'integerValue': '1'}}]}),
    (Q, "SELECT * FROM K WHERE B > KEY(NAMESPACE('é'), K, 'a') AND A > KEY(PROJECT('Bad'), K, 'a')"),
    (A, "AGGREGATE COUNT(*) OVER (SELECT * FROM K WHERE B > KEY(NAMESPACE('é'), K, 'a') AND A > KEY(PROJECT('Bad'), K, 'a'))"),
    (Q, "SELECT * FROM K WHERE A > KEY(PROJECT('Bad'), K, 0)"),
    (Q, "SELECT * FROM K WHERE A > KEY(PROJECT('Bad'), K, 'a') AND B != 1 AND C != 2"),
    (A, "AGGREGATE COUNT(*) OVER (SELECT * FROM K)", {'positionalBindings': [{'value': {'integerValue': '1'}}]}),
    (A, "AGGREGATE COUNT(*) OVER (SELECT * FROM K WHERE A > KEY(K, 0))", {'positionalBindings': [{'value': {'integerValue': '1'}}]}),
    (A, "AGGREGATE COUNT(*) OVER (SELECT * FROM __kind__)", {'positionalBindings': [{'value': {'integerValue': '1'}}]}),
    # A character the lexer refuses that ends the text: the end met after it, the character in
    # the prefix.
    (Q, "SELECT * FROM K@)"),
    (Q, "SELECT * FROM K@ "),
    (Q, "SELECT * FROM K@)x"),
    (Q, "SELECT * FROM K WHERE a = @)"),
    (A, "AGGREGATE COUNT(*) OVER (SELECT * FROM K@)"),
    (A, "AGGREGATE AVG(b) AS m, COUNT(*) OVER (SELECT * FROM __namespace__@)"),
]

TOKEN = re.compile(r"""\s*(`(?:[^`\\]|\\.|``)*`|'(?:[^'\\]|\\.|'')*'|"(?:[^"\\]|\\.|"")*"|@\w+|[+-]?(?:\d+\.?\d*|\.\d+)(?:[eE][+-]?\d+)?|\w+|!=|<=|>=|.)""")


def tokens(text):
    out, at = [], 0
    while at < len(text):
        m = TOKEN.match(text, at)
        if not m or not m.group(1):
            break
        out.append(m.end())
        at = m.end()
    return out


def probes():
    seen = set()
    out = []

    def add(verb, text):
        if (verb, text) not in seen:
            seen.add((verb, text))
            out.append((verb, text))

    for verb, query in CORPUS:
        ends = tokens(query)
        for n, end in enumerate(ends):
            prefix = query[:end]
            for text in (prefix, prefix + " ancestor"):
                add(verb, text)
                if n < 3:
                    add(A if verb == Q else Q, text)
    for verb, text in EXTRA:
        add(verb, text)
    return out


def bindings(text):
    """The bindings `text` names: each named one and as many positional ones as its highest."""
    named = {m: {"value": {"integerValue": "1"}} for m in re.findall(r"@([A-Za-z_]\w*)", text)}
    highest = max((int(m) for m in re.findall(r"@(\d+)", text)), default=0)
    out = {}
    if named:
        out["namedBindings"] = named
    if highest:
        out["positionalBindings"] = [{"value": {"integerValue": "1"}} for _ in range(highest)]
    return out


def masked(text):
    text = VERSION.sub(lambda m: f'"{m.group(1)}": "<version>"', mask(text, PROJECT))
    return CURSOR.sub(lambda m: f'"{m.group(1)}": "<cursor>"', text)


def call(verb, body):
    conn = http.client.HTTPConnection(*HOST.split(":"), timeout=30)
    conn.request("POST", f"/v1/projects/{PROJECT}:{verb}", body=json.dumps(body).encode(), headers=JSON)
    r = conn.getresponse()
    return r.status, r.getheader("content-type"), r.read().decode()


def main():
    recorded = []
    bodies = [(verb, text, {"gqlQuery": {"queryString": text, "allowLiterals": True, **bindings(text)}})
              for verb, text in probes()]
    for verb, text, *own in SENT:
        own = dict(own[0]) if own else {}
        request = {part: own.pop(part) for part in ("partitionId", "readOptions") if part in own}
        body = {"gqlQuery": {"queryString": text, "allowLiterals": True, **own}, **request}
        bodies.append((verb, text, body))
    for verb, text, body in bodies:
        status, ctype, answer = call(verb, body)
        recorded.append({"name": f"{verb}: {text}", "path": f"/v1/projects/{{project}}:{verb}", "body": body,
                         "outcome": {"status": status, "contentType": ctype, "body": masked(answer)}})
    sys.stdout.write('{\n "oracle": "cloud-firestore-emulator v1.22.0 (sha256 '
                     '9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)",\n'
                     ' "generated_by": "tools/oracle/datastore_gql_errors.py",\n'
                     f' "every_case": {json.dumps(EVERY_CASE)},\n "cases": [\n')
    sys.stdout.write(",\n".join("  " + json.dumps(c, ensure_ascii=False, separators=(",", ":")) for c in recorded))
    sys.stdout.write("\n ]\n}\n")


if __name__ == "__main__":
    main()
