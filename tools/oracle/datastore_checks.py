#!/usr/bin/env python3
"""Which problem of a Datastore query does the official emulator report first? (#81)

Usage (from the repository root), with a jar freshly started with `--database-mode
datastore-mode` under an English locale:
    python3 -I tools/oracle/datastore_checks.py 127.0.0.1:8086 \\
        > crates/hidane/tests/fixtures/datastore_checks.json

The differential fuzzer (tools/fuzz/, `--mode datastore-mode`) found queries with two problems
refused for another one than hidane reported. Over REST, this sends each problem of `QUERY`
alone (`:runQuery`) and every ordered pair of them that fits in one query (two problems that
set the same part of a query do not: the second would replace the first; their filters are
joined with `AND`, the first's first); then the same for `AGGREGATION` (`:runAggregationQuery`,
problems of the aggregations and of the query they run over); then the queries of `PARTS` and
`AGGREGATION_PARTS`, several problems in one part (a filter, a projection, a query of the
kinds or namespaces). The database is empty: these are refused before anything is read.

What the recording shows. A query's filters are walked three times: first each filter's own
parts (an operator, a name, a value, no array or entity operand but in `IN`, `HAS_ANCESTOR`'s
too); later the operands of `IN`, `NOT_IN` and `HAS_ANCESTOR`; last, once the query's index is
found, the filter in disjunctive normal form (without its ancestor, an `IN` an equality per
value), conjunction by conjunction: keys of `__key__` filters, a property filter without a kind,
an equality on a projected property, then on one the query is distinct on. Between them come,
in order: one ancestor, the same for every disjunction (each filter of a conjunction holds to
it, each of a disjunction alike, or none); the kind's name; empty names; limit, offset; one
`NOT_EQUAL` or `NOT_IN`, and no `NOT_IN` with `IN` or `OR`; one kind; a disjunction of ancestors
alone (an internal error); cursors; at most 30 disjunctions; orders; then, after the index, the
ancestor's namespace, orders without a kind (the explicit ones, then the distinct, inequality
and projected properties), distinct on the key, a reserved name projected (`__`, at least one
character, `__`), a property projected twice (the key may be), distinct on a reserved name, the
walk above, distinct properties ordered first, the cursors' kind, and the queries of kinds and
namespaces (refused only a first order descending). (With `--require_indexes`,
`datastore_indexes.json`, looking up the index of a query without a kind refuses its orders and
property filters already.) An aggregation's query goes through other checks, worded otherwise
(`Missing filter operator.`, `limit is negative`): its aggregations' operators first, a second
ancestor that is another one is a duplicate, and an ancestor in a disjunction, `NOT_IN` with
`IN`, several `NOT_EQUAL` and entity operands pass, while an array in an operand is an internal
error (`UNKNOWN`, no message), after the key's operands and the equalities on projected and
distinct properties and before a property filter without a kind.

Each case is a request and its answer in the format of `datastore_entities.json` (without what
`every_case` gives them all), on one line. `crates/hidane/tests/datastore.rs` replays it.
Stdlib only.
"""

import http.client
import itertools
import json
import re
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from harness import mask  # noqa: E402

HOST = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1:8086"
PROJECT = f"checks-{time.strftime('%H%M%S')}"
JSON = {"Content-Type": "application/json"}
# What every case has, written once.
EVERY_CASE = {"method": "POST", "headers": JSON, "compare": "exact"}

ONE = {"integerValue": "1"}
VERSION = re.compile(r'"(version|commitVersion|snapshotVersion)": "\d+"')
CURSOR = re.compile(r'"(cursor|endCursor|skippedCursor|startCursor)": "[^"]*"')


def masked(text):
    """Server times and versions, and cursors, as `datastore_entities.json` masks them."""
    text = VERSION.sub(lambda m: f'"{m.group(1)}": "<version>"', mask(text, PROJECT))
    return CURSOR.sub(lambda m: f'"{m.group(1)}": "<cursor>"', text)


def key(*path, ns=None):
    k = {"path": [{"kind": kind, "name": name} for kind, name in zip(path[::2], path[1::2])]}
    if ns is not None:
        k["partitionId"] = {"namespaceId": ns}
    return k


def pf(name, op, value=None):
    f = {"property": {"name": name}}
    if op is not None:
        f["op"] = op
    if value is not None:
        f["value"] = value
    return {"propertyFilter": f}


def composite(op, *filters):
    c = {"filters": list(filters)}
    if op is not None:
        c["op"] = op
    return {"compositeFilter": c}


def array(*values):
    return {"arrayValue": {"values": list(values)} if values else {}}


def numbers(n):
    return array(*({"integerValue": str(i)} for i in range(n)))


def prop(name):
    return {"property": {"name": name}}


ANCESTOR = pf("__key__", "HAS_ANCESTOR", {"keyValue": key("K", "a")})


# A problem of a query: the parts of the query it sets (`filter` is a filter joined with the
# other problem's), by name.
QUERY = {
    "two kinds": {"kind": [{"name": "K"}, {"name": "L"}]},
    "an empty kind": {"kind": [{"name": ""}]},
    "a reserved kind": {"kind": [{"name": "__x__"}]},
    "a kind query with a property filter": {"kind": [{"name": "__kind__"}], "filter": pf("a", "EQUAL", ONE)},
    "a kind query ordered descending": {"kind": [{"name": "__kind__"}],
                                        "order": [{**prop("__key__"), "direction": "DESCENDING"}]},
    "a kind query with a projection": {"kind": [{"name": "__kind__"}], "projection": [prop("a")]},
    "a kind query with an ancestor": {"kind": [{"name": "__kind__"}],
                                      "filter": pf("__key__", "HAS_ANCESTOR", {"keyValue": key("__kind__", "K")})},
    "a kind query with a key of another kind": {"kind": [{"name": "__kind__"}],
                                                "filter": pf("__key__", "EQUAL", {"keyValue": key("K", "a")})},
    "a property projected twice": {"projection": [prop("a"), prop("a")]},
    "an empty projected name": {"projection": [prop("")]},
    "an empty filter": {"filter": {}},
    "an empty composite filter": {"filter": composite("AND")},
    "a composite filter without an operator": {"filter": composite(None, pf("a", "EQUAL", ONE))},
    "a filter without an operator": {"filter": pf("a", None, ONE)},
    "a filter on an empty name": {"filter": pf("", "EQUAL", ONE)},
    "a filter without a value": {"filter": pf("a", "EQUAL")},
    "IN without an array": {"filter": pf("a", "IN", ONE)},
    "IN an empty array": {"filter": pf("a", "IN", array())},
    "IN 31 values": {"filter": pf("a", "IN", numbers(31))},
    "NOT_IN 11 values": {"filter": pf("a", "NOT_IN", numbers(11))},
    "a key filter on a string": {"filter": pf("__key__", "EQUAL", {"stringValue": "a"})},
    "a key IN strings": {"filter": pf("__key__", "IN", array({"stringValue": "a"}))},
    "an ancestor on a property": {"filter": pf("a", "HAS_ANCESTOR", {"keyValue": key("K", "a")})},
    "an ancestor that is not a key": {"filter": pf("__key__", "HAS_ANCESTOR", {"stringValue": "a"})},
    "an ancestor in another namespace": {"filter": pf("__key__", "HAS_ANCESTOR", {"keyValue": key("K", "a", ns="x")})},
    "an ancestor in a disjunction": {"filter": composite("OR", pf("__key__", "HAS_ANCESTOR", {"keyValue": key("K", "a")}),
                                                         pf("a", "EQUAL", ONE))},
    "an array operand": {"filter": pf("a", "EQUAL", array(ONE))},
    "an entity operand": {"filter": pf("a", "EQUAL", {"entityValue": {"properties": {"x": ONE}}})},
    "NOT_IN with IN": {"filter": composite("AND", pf("a", "NOT_IN", array(ONE)), pf("b", "IN", array(ONE)))},
    "NOT_IN in a disjunction": {"filter": composite("OR", pf("a", "NOT_IN", array(ONE)), pf("b", "EQUAL", ONE))},
    "two NOT_EQUAL": {"filter": composite("AND", pf("a", "NOT_EQUAL", ONE), pf("b", "NOT_EQUAL", ONE))},
    "NOT_EQUAL with NOT_IN": {"filter": composite("AND", pf("a", "NOT_EQUAL", ONE), pf("b", "NOT_IN", array(ONE)))},
    "a filter without a kind": {"kind": [], "filter": pf("a", "EQUAL", ONE)},
    "an order without a kind": {"kind": [], "order": [prop("a")]},
    "a descending key without a kind": {"kind": [], "order": [{**prop("__key__"), "direction": "DESCENDING"}]},
    "a property ordered twice": {"order": [prop("a"), prop("a")]},
    "an order on an empty name": {"order": [prop("")]},
    "an order without a property": {"order": [{}]},
    "a projection of an equality": {"projection": [prop("a")], "filter": pf("a", "EQUAL", ONE)},
    "distinct on the key": {"projection": [prop("__key__")], "distinctOn": [{"name": "__key__"}]},
    "distinct on what is not projected": {"projection": [prop("a")], "distinctOn": [{"name": "b"}]},
    "distinct after another order": {"projection": [prop("a"), prop("b")], "distinctOn": [{"name": "b"}],
                                     "order": [prop("a")]},
    "a negative limit": {"limit": -1},
    "a negative offset": {"offset": -1},
    "a cursor that does not parse": {"startCursor": "AAAA"},
    "inequalities on two properties": {"filter": composite("AND", pf("a", "LESS_THAN", ONE), pf("b", "GREATER_THAN", ONE))},
    "an inequality not ordered first": {"filter": pf("a", "LESS_THAN", ONE), "order": [prop("b")]},
    "IN arrays": {"filter": pf("a", "IN", array(array(ONE)))},
    "IN entities": {"filter": pf("a", "IN", array({"entityValue": {}}))},
    "a key in another namespace": {"filter": pf("__key__", "EQUAL", {"keyValue": key("K", "a", ns="x")})},
    "a distinct on an empty name": {"projection": [prop("a")], "distinctOn": [{"name": ""}]},
    "an ancestor that is a list": {"filter": pf("__key__", "HAS_ANCESTOR", array(ONE))},
    "an ancestor that is an entity": {"filter": pf("__key__", "HAS_ANCESTOR", {"entityValue": {}})},
    "a reserved name projected": {"projection": [prop("__x__")]},
    "distinct on a reserved name": {"distinctOn": [{"name": "__x__"}]},
    "distinct on an equality": {"distinctOn": [{"name": "a"}], "filter": pf("a", "EQUAL", ONE)},
    "a kind query ordered descending by a property": {"kind": [{"name": "__kind__"}],
                                                      "order": [{**prop("a"), "direction": "DESCENDING"}]},
    "36 disjunctions": {"filter": composite("AND", pf("d", "IN", numbers(6)), pf("e", "IN", numbers(6)))},
}

# A problem of an aggregation query: aggregations it adds, and parts of the nested query it sets.
AGGREGATION = {
    "an aggregation without an operator": {"aggregations": [{"alias": "op"}]},
    "a negative up_to": {"aggregations": [{"count": {"upTo": "-1"}, "alias": "upto"}]},
    "a sum without a property": {"aggregations": [{"sum": {}, "alias": "empty"}]},
    "an average of an empty name": {"aggregations": [{"avg": {"property": {"name": ""}}, "alias": "empty2"}]},
    "a sum of the key": {"aggregations": [{"sum": {"property": {"name": "__key__"}}, "alias": "key"}]},
    "six aggregations": {"aggregations": [{"count": {}, "alias": f"m{n}"} for n in range(6)]},
    "an alias twice": {"aggregations": [{"count": {}, "alias": "d"}, {"count": {}, "alias": "d"}]},
    "a reserved alias": {"aggregations": [{"count": {}, "alias": "__r__"}]},
    "a sum not in the projection": {"projection": [prop("b")], "aggregations": [{"sum": {"property": {"name": "z"}},
                                                                                "alias": "proj"}]},
    "a reserved kind": {"kind": [{"name": "__kind__"}]},
    "two kinds": {"kind": [{"name": "K"}, {"name": "L"}]},
    "a property ordered twice": {"order": [prop("a"), prop("a")]},
    "a negative limit": {"limit": -1},
    "a negative offset": {"offset": -1},
    "an array operand": {"filter": pf("a", "EQUAL", array(ONE))},
    "a filter without a kind": {"kind": [], "filter": pf("a", "EQUAL", ONE)},
    "two NOT_EQUAL": {"filter": composite("AND", pf("a", "NOT_EQUAL", ONE), pf("b", "NOT_EQUAL", ONE))},
    "a projection of an equality": {"projection": [prop("a")], "filter": pf("a", "EQUAL", ONE)},
    "distinct on the key": {"projection": [prop("__key__")], "distinctOn": [{"name": "__key__"}]},
    "an empty kind": {"kind": [{"name": ""}]},
    "an empty projected name": {"projection": [prop("")]},
    "an empty filter": {"filter": {}},
    "a filter without an operator": {"filter": pf("a", None, ONE)},
    "a filter without a value": {"filter": pf("a", "EQUAL")},
    "IN without an array": {"filter": pf("a", "IN", ONE)},
    "an entity operand": {"filter": pf("a", "EQUAL", {"entityValue": {"properties": {"x": ONE}}})},
    "an ancestor on a property": {"filter": pf("a", "HAS_ANCESTOR", {"keyValue": key("K", "a")})},
    "an ancestor in a disjunction": {"filter": composite("OR", pf("__key__", "HAS_ANCESTOR", {"keyValue": key("K", "a")}),
                                                         pf("a", "EQUAL", ONE))},
    "an ancestor in another namespace": {"filter": pf("__key__", "HAS_ANCESTOR", {"keyValue": key("K", "a", ns="x")})},
    "NOT_IN with IN": {"filter": composite("AND", pf("a", "NOT_IN", array(ONE)), pf("b", "IN", array(ONE)))},
    "a key filter on a string": {"filter": pf("__key__", "EQUAL", {"stringValue": "a"})},
    "a cursor that does not parse": {"startCursor": "AAAA"},
    "an order without a kind": {"kind": [], "order": [prop("a")]},
    "distinct after another order": {"projection": [prop("a"), prop("b")], "distinctOn": [{"name": "b"}],
                                     "order": [prop("a")]},
    "a property projected twice": {"projection": [prop("a"), prop("a")]},
    "no aggregations": {"aggregations": []},
    "a filter on an empty name": {"filter": pf("", "EQUAL", ONE)},
    "an empty composite filter": {"filter": composite("AND")},
    "a composite filter without an operator": {"filter": composite(None, pf("a", "EQUAL", ONE))},
    "two ancestors": {"filter": composite("AND", pf("__key__", "HAS_ANCESTOR", {"keyValue": key("K", "a")}),
                                          pf("__key__", "HAS_ANCESTOR", {"keyValue": key("K", "b")}))},
    "IN an empty array": {"filter": pf("a", "IN", array())},
    "IN arrays": {"filter": pf("a", "IN", array(array(ONE)))},
    "a key in another namespace": {"filter": pf("__key__", "EQUAL", {"keyValue": key("K", "a", ns="x")})},
    "an order on an empty name": {"order": [prop("")]},
    "a sum of a reserved name": {"aggregations": [{"sum": {"property": {"name": "__x__"}}, "alias": "rn"}]},
    "a reserved name projected": {"projection": [prop("__x__")]},
    "distinct on a reserved name": {"distinctOn": [{"name": "__x__"}]},
    "distinct on an equality": {"distinctOn": [{"name": "a"}], "filter": pf("a", "EQUAL", ONE)},
    "36 disjunctions": {"filter": composite("AND", pf("d", "IN", numbers(6)), pf("e", "IN", numbers(6)))},
}

# Problems of an aggregation's query in one part, where the pairs cannot put two.
AGGREGATION_PARTS = {
    "a composite filter without an operator": {"filter": composite(None, pf("a", "EQUAL", ONE))},
    "an empty composite filter": {"filter": composite("AND")},
    "a composite filter without an operator or filters": {"filter": {"compositeFilter": {}}},
    "a filter on an empty name": {"filter": pf("", "EQUAL", ONE)},
    "a filter without an operator, on an empty name": {"filter": pf("", None, ONE)},
    "a filter on an empty name without a value": {"filter": pf("", "EQUAL")},
    "an ancestor that is not a key": {"filter": pf("__key__", "HAS_ANCESTOR", {"stringValue": "a"})},
    "an ancestor without a value": {"filter": pf("__key__", "HAS_ANCESTOR")},
    "an ancestor on a property, not a key": {"filter": pf("a", "HAS_ANCESTOR", ONE)},
    "IN an empty array": {"filter": pf("a", "IN", array())},
    "IN 31 values": {"filter": pf("a", "IN", numbers(31))},
    "NOT_IN 11 values": {"filter": pf("a", "NOT_IN", numbers(11))},
    "NOT_IN without an array": {"filter": pf("a", "NOT_IN", ONE)},
    "IN arrays": {"filter": pf("a", "IN", array(array(ONE)))},
    "IN entities": {"filter": pf("a", "IN", array({"entityValue": {}}))},
    "two ancestors": {"filter": composite("AND", pf("__key__", "HAS_ANCESTOR", {"keyValue": key("K", "a")}),
                                          pf("__key__", "HAS_ANCESTOR", {"keyValue": key("K", "b")}))},
    "two ancestors in a disjunction": {"filter": composite("OR", pf("__key__", "HAS_ANCESTOR", {"keyValue": key("K", "a")}),
                                                           pf("__key__", "HAS_ANCESTOR", {"keyValue": key("K", "b")}))},
    "an order on an empty name": {"order": [prop("")]},
    "an order without a property": {"order": [{}]},
    "distinct on an empty name": {"projection": [prop("a")], "distinctOn": [{"name": ""}]},
    "a key IN strings": {"filter": pf("__key__", "IN", array({"stringValue": "a"}))},
    "a key in another namespace": {"filter": pf("__key__", "EQUAL", {"keyValue": key("K", "a", ns="x")})},
    "a kindless projection": {"kind": [], "projection": [prop("a")]},
    "an empty kind and two kinds": {"kind": [{"name": "K"}, {"name": ""}]},
    "a namespace kind": {"kind": [{"name": "__namespace__"}]},
    "a property kind": {"kind": [{"name": "__property__"}]},
    "a reserved kind with a negative up_to": {"kind": [{"name": "__x__"}], "aggregations": [{"count": {"upTo": "-1"}}]},
    "an empty alias": {"aggregations": [{"count": {}, "alias": ""}, {"count": {}, "alias": ""}]},
    "an alias like a property": {"aggregations": [{"count": {}, "alias": "a.b"}]},
    "an alias with a space": {"aggregations": [{"count": {}, "alias": "a b"}]},
    "a sum of a dotted name": {"aggregations": [{"sum": {"property": {"name": "e.x"}}}]},
    "a sum of a reserved name": {"aggregations": [{"sum": {"property": {"name": "__x__"}}}]},
    "the same ancestor twice": {"filter": composite("AND", ANCESTOR, ANCESTOR)},
    "an ancestor and a disjunction without it": {"filter": composite("AND", ANCESTOR, composite(
        "OR", ANCESTOR, pf("a", "EQUAL", ONE)))},
    "a disjunction of the same ancestor alone": {"filter": composite("OR", ANCESTOR, ANCESTOR)},
    "the key projected twice": {"projection": [prop("__key__"), prop("__key__")]},
    "a kind of four underscores": {"kind": [{"name": "____"}]},
    "a sum of four underscores": {"aggregations": [{"sum": {"property": {"name": "____"}}}]},
    "an alias of four underscores": {"aggregations": [{"count": {}, "alias": "____"}]},
}


def kind_query(kind="__kind__", **parts):
    return {"kind": [{"name": kind}], **parts}


KIND_KEY = pf("__key__", "EQUAL", {"keyValue": key("__kind__", "K")})
# Problems in one part of a query, where the pairs above cannot put two: inside one filter, one
# projection, and the queries of kinds and namespaces.
PARTS = {
    "a composite filter without an operator or filters": {"filter": {"compositeFilter": {}}},
    "a filter without an operator, on an empty name": {"filter": pf("", None, ONE)},
    "a filter without an operator or a value": {"filter": pf("a", None)},
    "a filter on an empty name without a value": {"filter": pf("", "EQUAL")},
    "an array operand on an empty name": {"filter": pf("", "EQUAL", array(ONE))},
    "an array operand without an operator": {"filter": pf("a", None, array(ONE))},
    "IN without an array, on an empty name": {"filter": pf("", "IN", ONE)},
    "IN without a value": {"filter": pf("a", "IN")},
    "HAS_ANCESTOR without a value": {"filter": pf("__key__", "HAS_ANCESTOR")},
    "HAS_ANCESTOR on an empty name": {"filter": pf("", "HAS_ANCESTOR", {"keyValue": key("K", "a")})},
    "HAS_ANCESTOR on a property, not a key": {"filter": pf("a", "HAS_ANCESTOR", ONE)},
    "two ancestors": {"filter": composite("AND", pf("__key__", "HAS_ANCESTOR", {"keyValue": key("K", "a")}),
                                          pf("__key__", "HAS_ANCESTOR", {"keyValue": key("K", "b")}))},
    "a key filter on a string, without a kind": {"kind": [], "filter": pf("__key__", "EQUAL", {"stringValue": "a"})},
    "a property projected twice, with an equality": {"projection": [prop("a"), prop("a")],
                                                     "filter": pf("a", "EQUAL", ONE)},
    "the key projected twice, distinct on it": {"projection": [prop("__key__"), prop("__key__")],
                                                "distinctOn": [{"name": "__key__"}]},
    "distinct on the key, with an equality": {"projection": [prop("a"), prop("__key__")],
                                              "distinctOn": [{"name": "__key__"}], "filter": pf("a", "EQUAL", ONE)},
    "distinct on the key after another order": {"projection": [prop("a"), prop("__key__")],
                                                "distinctOn": [{"name": "__key__"}], "order": [prop("a")]},
    "a kind query ordered descending, with a projection": kind_query(
        order=[{**prop("__key__"), "direction": "DESCENDING"}], projection=[prop("a")]),
    "a kind query ordered descending, with a property filter": kind_query(
        order=[{**prop("__key__"), "direction": "DESCENDING"}], filter=pf("a", "EQUAL", ONE)),
    "a kind query with a projection and a property filter": kind_query(projection=[prop("a")],
                                                                       filter=pf("a", "LESS_THAN", ONE)),
    "a kind query with an ancestor and a property filter": kind_query(filter=composite(
        "AND", pf("__key__", "HAS_ANCESTOR", {"keyValue": key("__kind__", "K")}), pf("a", "EQUAL", ONE))),
    "a kind query with a key of another kind and a property filter": kind_query(filter=composite(
        "AND", pf("__key__", "EQUAL", {"keyValue": key("K", "a")}), pf("a", "EQUAL", ONE))),
    "a kind query with an ancestor and a key of another kind": kind_query(filter=composite(
        "AND", pf("__key__", "HAS_ANCESTOR", {"keyValue": key("__kind__", "K")}),
        pf("__key__", "EQUAL", {"keyValue": key("K", "a")}))),
    "a kind query ordered by a property": kind_query(order=[prop("a")]),
    "a kind query with distinct": kind_query(projection=[prop("__key__")], distinctOn=[{"name": "__key__"}]),
    "a kind query with a projection, distinct after another order": kind_query(
        projection=[prop("a"), prop("b")], distinctOn=[{"name": "b"}], order=[prop("a")]),
    "a kind query with a key IN": kind_query(filter=pf("__key__", "IN", array({"keyValue": key("__kind__", "K")}))),
    "a kind query with a key NOT_EQUAL": kind_query(filter=pf("__key__", "NOT_EQUAL", {"keyValue": key("__kind__", "K")})),
    "a kind query with a key NOT_IN": kind_query(filter=pf("__key__", "NOT_IN", array({"keyValue": key("__kind__", "K")}))),
    "a kind query with a key in a namespace": kind_query(filter=pf("__key__", "EQUAL", {"keyValue": key("__kind__", "K", ns="x")})),
    "a kind query with a key below another": kind_query(filter=pf("__key__", "EQUAL", {"keyValue": key("P", "p", "__kind__", "K")})),
    "a kind query with a key by ID": kind_query(filter=pf("__key__", "EQUAL", {"keyValue": {"path": [{"kind": "__kind__", "id": "1"}]}})),
    "a namespace query with a property filter": kind_query("__namespace__", filter=pf("a", "EQUAL", ONE)),
    "a namespace query with a key of a kind": kind_query("__namespace__", filter=pf("__key__", "EQUAL", {"keyValue": key("__kind__", "K")})),
    "a namespace query ordered descending": kind_query("__namespace__", order=[{**prop("__key__"), "direction": "DESCENDING"}]),
    "a namespace query with a projection": kind_query("__namespace__", projection=[prop("__key__")]),
    "a property query with a property filter": kind_query("__property__", filter=pf("a", "EQUAL", ONE)),
    "a property query ordered descending": kind_query("__property__", order=[{**prop("__key__"), "direction": "DESCENDING"}]),
    "a kindless query with an ancestor ordered descending": {"kind": [], "filter": pf(
        "__key__", "HAS_ANCESTOR", {"keyValue": key("K", "a")}), "order": [{**prop("__key__"), "direction": "DESCENDING"}]},
    "a kindless query with a projection": {"kind": [], "projection": [prop("a")]},
    "a kindless query with a key projection": {"kind": [], "projection": [prop("__key__")]},
    "a kindless query distinct on the key": {"kind": [], "projection": [prop("__key__")], "distinctOn": [{"name": "__key__"}]},
    "a kindless query with an inequality on the key": {"kind": [], "filter": pf("__key__", "GREATER_THAN", {"keyValue": key("K", "a")})},
    "the same ancestor twice": {"filter": composite("AND", ANCESTOR, ANCESTOR)},
    "the same ancestor in each disjunction": {"filter": composite(
        "OR", composite("AND", ANCESTOR, pf("a", "EQUAL", ONE)), composite("AND", ANCESTOR, pf("b", "EQUAL", ONE)))},
    "an ancestor and a disjunction without it": {"filter": composite("AND", ANCESTOR, composite(
        "OR", ANCESTOR, pf("a", "EQUAL", ONE)))},
    "a disjunction of the same ancestor alone": {"filter": composite("OR", ANCESTOR, ANCESTOR)},
    "a disjunction of the same ancestor, alone and with more": {"filter": composite(
        "OR", ANCESTOR, ANCESTOR, composite("AND", ANCESTOR, pf("a", "EQUAL", ONE)))},
    "a disjunction of the same ancestor alone, with two NOT_EQUAL": {"filter": composite(
        "AND", composite("OR", ANCESTOR, ANCESTOR), pf("a", "NOT_EQUAL", ONE), pf("b", "NOT_EQUAL", ONE))},
    "a disjunction of the same ancestor alone, IN an empty array": {"filter": composite(
        "AND", composite("OR", ANCESTOR, ANCESTOR), pf("a", "IN", array()))},
    "the key projected twice": {"projection": [prop("__key__"), prop("__key__")]},
    "a reserved name projected twice": {"projection": [prop("__x__"), prop("__x__")]},
    "a reserved name projected, distinct on another": {"projection": [prop("__x__")],
                                                        "distinctOn": [{"name": "__y__"}]},
    "distinct on a projected equality": {"projection": [prop("a")], "distinctOn": [{"name": "a"}],
                                         "filter": pf("a", "EQUAL", ONE)},
    "a namespace query ordered descending by a property": kind_query(
        "__namespace__", order=[{**prop("a"), "direction": "DESCENDING"}]),
    "a property query ordered descending by a property": kind_query(
        "__property__", order=[{**prop("a"), "direction": "DESCENDING"}]),
    "a kind query ordered by a property, then descending": kind_query(
        order=[prop("a"), {**prop("b"), "direction": "DESCENDING"}]),
    "four underscores projected": {"projection": [prop("____")]},
    "distinct on four underscores": {"distinctOn": [{"name": "____"}]},
    "a property projected twice, then a reserved name": {"projection": [prop("a"), prop("a"), prop("__x__")]},
    "a reserved name projected, then a property twice": {"projection": [prop("__x__"), prop("a"), prop("a")]},
}


def fits(*problems):
    """Whether problems set different parts of a query (filters and aggregations add up)."""
    parts = [set(p) - {"filter", "aggregations"} for p in problems]
    return not (parts[0] & parts[1])


def combined(problems, base):
    q = dict(base)
    filters = []
    aggregations = []
    for p in problems:
        for part, value in p.items():
            if part == "filter":
                filters.append(value)
            elif part == "aggregations":
                aggregations.extend(value)
            else:
                q[part] = value
    if len(filters) == 1:
        q["filter"] = filters[0]
    elif filters:
        q["filter"] = composite("AND", *filters)
    return q, aggregations


def cases(catalogue, build):
    out = []
    for name, problem in catalogue.items():
        out.append((name, build([problem])))
    for (a, pa), (b, pb) in itertools.permutations(catalogue.items(), 2):
        if fits(pa, pb):
            out.append((f"{a}, then {b}", build([pa, pb])))
    return out


def query_body(problems):
    q, _ = combined(problems, {"kind": [{"name": "K"}]})
    return "runQuery", {"query": q}


def aggregation_body(problems):
    q, aggregations = combined(problems, {"kind": [{"name": "K"}]})
    if not aggregations and not any(p.get("aggregations") == [] for p in problems):
        aggregations = [{"count": {}, "alias": "c"}]
    return "runAggregationQuery", {"aggregationQuery": {"nestedQuery": q, "aggregations": aggregations}}


def call(path, body):
    conn = http.client.HTTPConnection(*HOST.split(":"), timeout=30)
    conn.request("POST", path, body=json.dumps(body).encode(), headers=JSON)
    r = conn.getresponse()
    return r.status, r.getheader("content-type"), r.read().decode()


def record(section, built):
    out = []
    for name, (verb, body) in built:
        path = f"/v1/projects/{{project}}:{verb}"
        status, ctype, text = call(path.replace("{project}", PROJECT), body)
        out.append({"name": f"{section}: {name}", "path": path, "body": body,
                    "outcome": {"status": status, "contentType": ctype, "body": masked(text)}})
    return out


def main():
    recorded = (record("query", cases(QUERY, query_body))
                + record("query", [(name, query_body([p])) for name, p in PARTS.items()])
                + record("aggregation", cases(AGGREGATION, aggregation_body))
                + record("aggregation", [(name, aggregation_body([p])) for name, p in AGGREGATION_PARTS.items()]))
    sys.stdout.write('{\n "oracle": "cloud-firestore-emulator v1.22.0 (sha256 '
                     '9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)",\n'
                     ' "generated_by": "tools/oracle/datastore_checks.py",\n'
                     f' "every_case": {json.dumps(EVERY_CASE)},\n "cases": [\n')
    sys.stdout.write(",\n".join("  " + json.dumps(c, ensure_ascii=False, separators=(",", ":")) for c in recorded))
    sys.stdout.write("\n ]\n}\n")


if __name__ == "__main__":
    main()
