"""Seeded generators of fuzz cases for Datastore mode (#81): REST requests to the Datastore API
(`POST /v1/projects/{project}:<verb>`), sent to both emulators started with `--database-mode
datastore-mode`.

Cases have gen.py's format and placeholders, and two more: `{cursor}`, the last cursor an answer
held (a batch's `endCursor`), and `{version}`, the version of the first mutation result of the
last commit. Each area is a function of a `gen.Gen`, whose choices and values it draws on (the
values Firestore and Datastore share are written the same way in JSON). Stdlib only.
"""

AREAS = ("datastore-entities", "datastore-queries", "datastore-transactions", "datastore-gql",
         "datastore-malformed")

INTS = ["0", "1", "-1", "2", "3", "42", "2147483648", "9007199254740993", "9223372036854775807",
        "-9223372036854775808"]
DOUBLES = [0.0, -0.0, 1.0, -1.5, 0.1, 2.0, 1e21, 5e-324, 1.7976931348623157e308, "NaN", "Infinity", "-Infinity"]
KINDS = ["K", "K", "K", "Task", "k", "\u00e9", "a b", "K2"]
NAMES = ["a", "b", "c", "x", "\u00e9", "a b", "\U0001F600", "1", "a/b", " ", "A"]
IDS = ["1", "2", "5", "42", "9223372036854775807", "4503599627370496", "5629499534213120"]
NAMESPACES = ["", "ns", "ns", "\u00e9"]
PROPERTIES = ["a", "b", "n", "s", "tags", "x", "A", "a.b", "x y", "\u00e9", "_", "e"]
OPS = ["EQUAL", "EQUAL", "NOT_EQUAL", "LESS_THAN", "LESS_THAN_OR_EQUAL", "GREATER_THAN", "GREATER_THAN_OR_EQUAL",
       "IN", "NOT_IN", "HAS_ANCESTOR"]


def rpc(verb, body=None, raw=None, headers=None):
    out = {"method": "POST", "path": "/v1/projects/{project}:" + verb, "headers": dict(headers or {})}
    if raw is not None:
        out["raw"] = raw
    elif body is not None:
        out["body"] = body
    return out


def commit(mutations, transaction=None):
    body = {"mode": "TRANSACTIONAL" if transaction else "NON_TRANSACTIONAL", "mutations": mutations}
    if transaction:
        body["transaction"] = transaction
    return rpc("commit", body)


def lookup(keys, **more):
    return rpc("lookup", {"keys": keys, **more})


def complete(key):
    return all("name" in e or "id" in e for e in key["path"])


# --- keys and values ---------------------------------------------------------------------------

def element(g, kind, named=None):
    e = {"kind": kind}
    if named is None:
        named = g.chance(0.5)
    if named:
        e["name"] = g.pick(NAMES)
    else:
        e["id"] = g.pick(IDS)
    return e


def partition(g):
    p = {}
    if g.chance(0.85):
        p["namespaceId"] = g.pick(NAMESPACES)
    if g.chance(0.3):
        p["projectId"] = "{project}"
    return p


def key(g, kind=None, incomplete=False):
    path = []
    if g.chance(0.2):
        path.append(element(g, g.pick(KINDS)))
    last = element(g, kind or g.pick(KINDS))
    if incomplete:
        del last["name" if "name" in last else "id"]
    path.append(last)
    k = {"path": path}
    if g.chance(0.15):
        k["partitionId"] = partition(g)
    return k


def value(g, depth=0):
    r = g.rng.random()
    if r < 0.07:
        v = {"nullValue": None}
    elif r < 0.14:
        v = {"booleanValue": g.chance(0.5)}
    elif r < 0.28:
        v = {"integerValue": g.pick(INTS)}
    elif r < 0.40:
        v = {"doubleValue": g.pick(DOUBLES)}
    elif r < 0.46:
        v = {"timestampValue": g.timestamp()}
    elif r < 0.60:
        long = g.chance(0.04)
        v = {"stringValue": g.long_text() if long else g.text()}
        if long and g.chance(0.6):
            v["excludeFromIndexes"] = True
    elif r < 0.65:
        v = {"blobValue": g.bytes_value()["bytesValue"]}
    elif r < 0.70:
        v = g.geo()
    elif r < 0.76:
        v = {"keyValue": key(g)}
    elif r < 0.86 and depth < 2:
        values = [x for x in (value(g, depth + 1) for _ in range(g.rng.randint(0, 4))) if "arrayValue" not in x]
        return {"arrayValue": {"values": values} if values else {}}
    elif depth < 2:
        v = {"entityValue": entity(g, depth + 1, key(g) if g.chance(0.1) else None)}
    else:
        v = {"stringValue": g.text()}
    if g.chance(0.1):
        v["excludeFromIndexes"] = True
    return v


def properties(g, depth=0, lo=0, hi=5):
    return {g.pick(PROPERTIES): value(g, depth) for _ in range(g.rng.randint(lo, hi))}


def entity(g, depth=0, k=None):
    out = {}
    if k is not None:
        out["key"] = k
    props = properties(g, depth)
    if props or g.chance(0.5):
        out["properties"] = props
    return out


def mask(g, props):
    names = list(props) + ["a", "e.x", "missing"]
    return {"paths": [g.pick(names) for _ in range(g.rng.randint(0, 3))]}


def mutation(g, k, props=None):
    op = g.pick(["insert", "upsert", "upsert", "update", "delete"])
    if op == "delete":
        m = {"delete": k}
    else:
        e = {"key": k}
        props = properties(g) if props is None else props
        if props or g.chance(0.5):
            e["properties"] = props
        m = {op: e}
        if g.chance(0.1):
            m["propertyMask"] = mask(g, props)
    if g.chance(0.1):
        m["baseVersion"] = g.pick(["{version}", "{version}", "1", "0"])
    elif g.chance(0.04):
        m["updateTime"] = g.pick(["{commitTime}", "2001-01-01T00:00:00Z"])
    return m


def read_options(g, transaction=None, begin=True):
    """Read options: in `transaction`, at a consistency or a read time, or beginning a
    transaction unless not to `begin` one (a later write to what it read would wait for its
    lock, which the official emulator holds for a minute)."""
    r = g.rng.random()
    if transaction is not None and r < 0.8:
        return {"transaction": transaction}
    if r < 0.4 or (r >= 0.7 and not begin):
        return {"readConsistency": g.pick(["STRONG", "EVENTUAL", "READ_CONSISTENCY_UNSPECIFIED"])}
    if r < 0.7:
        return {"readTime": "{commitTime}"}
    return {"newTransaction": g.pick([{}, {"readOnly": {}}, {"readWrite": {}}])}


# --- the entities queries read ----------------------------------------------------------------

def domain_value(g):
    return g.pick([{"integerValue": "0"}, {"integerValue": "1"}, {"integerValue": "2"}, {"integerValue": "3"},
                   {"doubleValue": 0.5}, {"doubleValue": 1.0}, {"doubleValue": "NaN"}, {"doubleValue": -0.0},
                   {"stringValue": ""}, {"stringValue": "a"}, {"stringValue": "b"}, {"nullValue": None},
                   {"booleanValue": False}, {"booleanValue": True}, {"timestampValue": "2020-01-01T00:00:00Z"},
                   {"blobValue": "AQ=="}, {"keyValue": domain_key(g)},
                   {"geoPointValue": {"latitude": 1, "longitude": 2}},
                   {"arrayValue": {"values": [{"integerValue": "1"}, {"stringValue": "a"}]}}, {"arrayValue": {}},
                   {"entityValue": {"properties": {"x": {"integerValue": "1"}}}}])


def domain_key(g, namespace=None):
    path = []
    if g.chance(0.25):
        path.append({"kind": "P", "name": g.pick(["p", "q"])})
    path.append({"kind": "K", "name": g.pick(["d0", "d1", "d2", "d3"])} if g.chance(0.7)
                else {"kind": "K", "id": g.pick(["1", "2", "3"])})
    k = {"path": path}
    if namespace:
        k["partitionId"] = {"namespaceId": namespace}
    return k


def domain_entity(g, namespace=None):
    props = {}
    for name in ("a", "b", "tags", "e"):
        if g.chance(0.75):
            if name == "tags":
                values = [g.pick([{"stringValue": "red"}, {"stringValue": "blue"}, {"integerValue": "1"},
                                  {"nullValue": None}]) for _ in range(g.rng.randint(0, 3))]
                props[name] = {"arrayValue": {"values": values} if values else {}}
            elif name == "e":
                props[name] = {"entityValue": {"properties": {"x": domain_value(g)}}}
            else:
                props[name] = domain_value(g)
                if g.chance(0.08):
                    props[name]["excludeFromIndexes"] = True
    return {"key": domain_key(g, namespace), "properties": props}


def seed(g):
    """A commit of a few entities of kind `K` (some below a `P`, some in namespace `ns`), and
    the namespace the queries read."""
    namespace = "ns" if g.chance(0.15) else None
    entities = [domain_entity(g, namespace if g.chance(0.8) else None) for _ in range(g.rng.randint(2, 7))]
    seen, mutations = set(), []
    for e in entities:
        k = repr(e["key"])
        if k not in seen:
            seen.add(k)
            mutations.append({"upsert": e})
    return commit(mutations), namespace


def query_property(g):
    return g.pick(["a", "a", "b", "b", "tags", "__key__", "e.x", "e", "missing"])


def property_filter(g):
    name = query_property(g)
    op = g.pick(OPS)
    if op == "HAS_ANCESTOR" and g.chance(0.8):
        name = "__key__"
    if name == "__key__" and g.chance(0.85):
        k = lambda: {"keyValue": domain_key(g)}
        v = {"arrayValue": {"values": [k() for _ in range(g.rng.randint(1, 3))]}} if op in ("IN", "NOT_IN") else k()
    elif op in ("IN", "NOT_IN"):
        n = g.rng.randint(1, 4) if g.chance(0.9) else g.pick([0, 10, 11, 30, 31])
        v = {"arrayValue": {"values": [domain_value(g) for _ in range(n)]} if n else {}}
    else:
        v = domain_value(g)
    return {"propertyFilter": {"property": {"name": name}, "op": op, "value": v}}


def query_filter(g, depth=0):
    if depth < 2 and g.chance(0.25):
        return {"compositeFilter": {"op": g.pick(["AND", "AND", "OR"]),
                                    "filters": [query_filter(g, depth + 1) for _ in range(g.rng.randint(1, 3))]}}
    return property_filter(g)


def query(g, namespace=None, cursor=False):
    r = g.rng.random()
    q = {"kind": [{"name": "K"}] if r < 0.8 else [] if r < 0.9 else
         [{"name": g.pick(["__kind__", "__namespace__", "P", "Other", "__property__"])}]}
    if g.chance(0.65):
        q["filter"] = query_filter(g)
    orders = [{"property": {"name": g.pick(["a", "b", "tags", "__key__", "e.x", "missing"])},
               "direction": g.pick(["ASCENDING", "DESCENDING"])} for _ in range(g.pick([0, 0, 1, 1, 2]))]
    if orders:
        q["order"] = orders
    if g.chance(0.15):
        q["projection"] = [{"property": {"name": g.pick(["a", "b", "__key__", "tags", "e.x"])}}
                           for _ in range(g.rng.randint(1, 2))]
        if g.chance(0.4):
            q["distinctOn"] = [p["property"] for p in q["projection"][:g.rng.randint(1, len(q["projection"]))]]
    elif g.chance(0.05):
        q["distinctOn"] = [{"name": g.pick(["a", "b"])}]
    if g.chance(0.3):
        q["limit"] = g.pick([0, 1, 2, 3, 5])
    if g.chance(0.15):
        q["offset"] = g.pick([0, 1, 2, 5])
    if cursor and g.chance(0.6):
        q[g.pick(["startCursor", "startCursor", "endCursor"])] = "{cursor}"
    body = {"query": q}
    if namespace and g.chance(0.85):
        body["partitionId"] = {"namespaceId": namespace}
    return body


def aggregation(g, namespace=None):
    aggregations = []
    for n in range(g.rng.randint(1, 3)):
        kind = g.pick(["count", "count", "sum", "avg"])
        if kind == "count":
            agg = {"count": {"upTo": g.pick(["1", "2", "5"])} if g.chance(0.25) else {}}
        else:
            agg = {kind: {"property": {"name": g.pick(["a", "b", "e.x", "missing"])}}}
        if g.chance(0.9):
            agg["alias"] = f"x{n}"
        aggregations.append(agg)
    body = query(g, namespace)
    return dict({k: v for k, v in body.items() if k != "query"},
                aggregationQuery={"nestedQuery": body["query"], "aggregations": aggregations})


# --- areas -------------------------------------------------------------------------------------

def entities(g):
    """A commit of mutations of every kind to a few keys, then lookups, more commits (over the
    versions and times the first gave), ID allocation and the queries that list kinds."""
    keys = [key(g, incomplete=g.chance(0.15)) for _ in range(g.rng.randint(1, 3))]
    mutations = []
    for n in range(g.rng.randint(1, 4)):
        mutations.append(mutation(g, keys[n % len(keys)] if g.chance(0.8) else g.pick(keys)))
    steps = [commit(mutations)]
    reads = [k for k in keys if complete(k)]
    for _ in range(g.rng.randint(1, 3)):
        what = g.pick(["lookup", "lookup", "commit", "allocate", "reserve", "kinds", "query"])
        if what == "lookup" and reads:
            extra = {}
            if g.chance(0.2):
                extra["propertyMask"] = mask(g, {})
            if g.chance(0.2):
                extra["readOptions"] = read_options(g, begin=False)
            more = [key(g)] if g.chance(0.3) else []
            steps.append(lookup(reads + more, **extra))
        elif what == "commit":
            steps.append(commit([mutation(g, k) for k in (reads or keys)[:g.rng.randint(1, 2)]]))
        elif what == "allocate":
            steps.append(rpc("allocateIds", {"keys": [key(g, incomplete=True) for _ in range(g.rng.randint(1, 3))]}))
        elif what == "reserve":
            steps.append(rpc("reserveIds", {"keys": [key(g) for _ in range(g.rng.randint(1, 2))]}))
        elif what == "kinds":
            steps.append(rpc("runQuery", {"query": {"kind": [{"name": g.pick(["__kind__", "__namespace__"])}]}}))
        else:
            k = g.pick(keys)
            body = {"query": {"kind": [{"name": k["path"][-1]["kind"]}]}}
            if "partitionId" in k and "namespaceId" in k["partitionId"]:
                body["partitionId"] = {"namespaceId": k["partitionId"]["namespaceId"]}
            steps.append(rpc("runQuery", body))
    if reads:
        steps.append(lookup(reads))
    return steps


def queries(g):
    """A few entities of kind `K`, then queries and aggregations over them: filters (every
    operator, `__key__`, ancestors, nested `AND` and `OR`), orders, projections, `distinctOn`,
    limits, offsets, a page's cursor, kindless queries and the kinds and namespaces."""
    first, namespace = seed(g)
    steps = [first]
    cursor = False
    for _ in range(g.rng.randint(1, 3)):
        if g.chance(0.3):
            steps.append(rpc("runAggregationQuery", aggregation(g, namespace)))
            continue
        body = query(g, namespace, cursor)
        if g.chance(0.1):
            body["readOptions"] = read_options(g)
        steps.append(rpc("runQuery", body))
        cursor = True
    return steps


def transactions(g):
    """A transaction (read-write, read-only, at a read time, or begun by a read), its reads and
    queries, then a commit or rollback, a second use, a write outside it to what it read, or one
    that does not exist."""
    first, namespace = seed(g)
    steps = [first]
    k = domain_key(g, namespace)
    how = g.pick(["begin", "begin", "read-only", "read-time", "new"])
    if how == "begin":
        steps.append(rpc("beginTransaction", {"transactionOptions": {"readWrite": {}}} if g.chance(0.5) else {}))
    elif how == "read-only":
        steps.append(rpc("beginTransaction", {"transactionOptions": {"readOnly": {}}}))
    elif how == "read-time":
        steps.append(rpc("beginTransaction", {"transactionOptions": {"readOnly": {"readTime": "{commitTime}"}}}))
    else:
        steps.append(lookup([k], readOptions={"newTransaction": g.pick([{}, {"readWrite": {}}, {"readOnly": {}}])}))
    tx = "{tx}" if g.chance(0.95) else g.pick(["AAAA", "AAAAAAAAAAAAAAAAAAAAAAAA", "not base64", ""])
    for _ in range(g.rng.randint(0, 2)):
        # A write outside the transaction to what it read waits a minute for its lock on the
        # official emulator, longer than a step's timeout: rarely.
        what = "outside" if g.chance(0.05) else g.pick(["lookup", "query", "aggregate"])
        if what == "lookup":
            steps.append(lookup([k] + ([domain_key(g, namespace)] if g.chance(0.4) else []),
                                readOptions=read_options(g, tx)))
        elif what == "query":
            body = query(g, namespace)
            if g.chance(0.5):
                body["query"]["filter"] = {"propertyFilter": {"property": {"name": "__key__"}, "op": "HAS_ANCESTOR",
                                                              "value": {"keyValue": domain_key(g, namespace)}}}
            body["readOptions"] = read_options(g, tx)
            steps.append(rpc("runQuery", body))
        elif what == "aggregate":
            body = aggregation(g, namespace)
            body["readOptions"] = read_options(g, tx)
            steps.append(rpc("runAggregationQuery", body))
        else:
            steps.append(commit([{"upsert": domain_entity(g, namespace)}]))
    end = g.pick(["commit", "commit", "rollback", "none"])
    if end == "commit":
        mutations = [{"upsert": dict(domain_entity(g, namespace), key=k)}] if g.chance(0.8) else []
        if g.chance(0.2):
            mutations.append({"delete": domain_key(g, namespace)})
        steps.append(commit(mutations, tx))
    elif end == "rollback":
        steps.append(rpc("rollback", {"transaction": tx}))
    if end != "none" and g.chance(0.4):
        steps.append(g.pick([commit([], tx), rpc("rollback", {"transaction": tx}),
                             lookup([k], readOptions={"transaction": tx})]))
    steps.append(lookup([k]))
    return steps


# GQL: the parts of a query, and literals and bindings for its values.
GQL_SELECT = ["*", "*", "__key__", "a", "a, b", "DISTINCT a", "DISTINCT ON (a) a, b", "DISTINCT ON (a) *", "e.x",
              "b, a", "COUNT(*)"]
GQL_FROM = ["K", "K", "K", "`K`", "P", "__kind__", "__namespace__", "Other", "`a b`"]
GQL_PROPERTIES = ["a", "a", "b", "tags", "__key__", "e.x", "`e`.`x`", "missing", "`a`"]
GQL_OPS = ["=", "=", "!=", "<", "<=", ">", ">=", "IN", "NOT IN", "CONTAINS", "HAS ANCESTOR", "HAS DESCENDANT", "IS"]
GQL_LITERALS = ["0", "1", "-1", "2", "3", "1.5", "0.5", "-0.0", "'a'", '"b"', "''", "TRUE", "false", "NULL",
                "KEY(K, 'd0')", "KEY(K, 1)", "KEY(P, 'p', K, 'd1')", "KEY(PROJECT('{project}'), K, 'd2')",
                "KEY(NAMESPACE('ns'), K, 'd0')", "DATETIME('2020-01-01T00:00:00Z')", "ARRAY(1, 2)", "ARRAY('a', 'b')",
                "ARRAY()", "BLOB('AQ==')", "9223372036854775807", "9223372036854775808", "1e400", "'\\''"]


def gql_operand(g, bindings):
    if bindings["allow"] and g.chance(0.5):
        return g.pick(GQL_LITERALS)
    if g.chance(0.5):
        name = g.pick(["v", "w", "x1", "_v"])
        bindings["named"].setdefault(name, {"value": domain_value(g)})
        return "@" + name
    bindings["positional"].append({"value": domain_value(g)})
    return "@" + str(len(bindings["positional"]))


def gql_condition(g, bindings):
    prop, op = g.pick(GQL_PROPERTIES), g.pick(GQL_OPS)
    if op == "IS":
        return f"{prop} IS NULL"
    if op in ("HAS ANCESTOR", "HAS DESCENDANT") and g.chance(0.8):
        return f"__key__ {op} " + (g.pick(["KEY(P, 'p')", "KEY(K, 'd0')"]) if bindings["allow"] and g.chance(0.6)
                                   else gql_operand(g, bindings))
    return f"{prop} {op} {gql_operand(g, bindings)}"


def gql_where(g, bindings, depth=0):
    parts = [gql_condition(g, bindings) if depth > 0 or g.chance(0.8) else f"({gql_where(g, bindings, depth + 1)})"
             for _ in range(g.rng.randint(1, 3))]
    return f" {g.pick(['AND', 'AND', 'OR'])} ".join(parts)


def gql_string(g, bindings):
    text = f"SELECT {g.pick(GQL_SELECT)}"
    if g.chance(0.93):
        text += f" FROM {g.pick(GQL_FROM)}"
    if g.chance(0.65):
        text += f" WHERE {gql_where(g, bindings)}"
    if g.chance(0.35):
        text += " ORDER BY " + ", ".join(f"{g.pick(['a', 'b', '__key__', 'tags'])}{g.pick(['', ' ASC', ' DESC'])}"
                                         for _ in range(g.rng.randint(1, 2)))
    if g.chance(0.3):
        text += " LIMIT " + (g.pick(["0", "1", "2", "5", "-1"]) if g.chance(0.7) else gql_operand(g, bindings))
    if g.chance(0.2):
        text += " OFFSET " + (g.pick(["0", "1", "2"]) if g.chance(0.7) else gql_operand(g, bindings))
    if g.chance(0.12):
        # A typo: a token dropped or doubled, or a character in a random place.
        words = text.split(" ")
        n = g.rng.randrange(len(words))
        r = g.rng.random()
        if r < 0.4:
            del words[n]
        elif r < 0.7:
            words.insert(n, words[n])
        else:
            words[n] += g.pick([",", "(", ")", "'", "\"", "`", "@", "=", "!", ".", "#", "\u00e9"])
        text = " ".join(words)
    if g.chance(0.1):
        text = text.lower() if g.chance(0.5) else text.upper()
    return text


def gql(g):
    """A few entities of kind `K`, then GQL queries (`runQuery` and, wrapped in `AGGREGATE \u2026
    OVER (\u2026)`, `runAggregationQuery`): selections, `DISTINCT ON`, conditions over every
    operator, literals where allowed, named and positional bindings (some missing or unused, a
    cursor), orders, limits, offsets, and typos."""
    first, namespace = seed(g)
    steps = [first]
    cursor = False
    for _ in range(g.rng.randint(1, 3)):
        bindings = {"allow": g.chance(0.6), "named": {}, "positional": []}
        text = gql_string(g, bindings)
        if cursor and g.chance(0.3):
            bindings["named"]["c"] = {"cursor": "{cursor}"}
            text += " OFFSET @c" if "OFFSET" not in text else ""
        if g.chance(0.05):
            bindings["named"]["unused"] = {"value": {"integerValue": "1"}}
        if bindings["positional"] and g.chance(0.05):
            bindings["positional"].pop()
        aggregate = g.chance(0.25)
        if aggregate:
            text = g.pick(["AGGREGATE COUNT(*) OVER (", "AGGREGATE COUNT_UP_TO(2) OVER (", "AGGREGATE SUM(a) OVER (",
                           "AGGREGATE AVG(b) AS m, COUNT(*) OVER (", "AGGREGATE COUNT(*) AS n OVER ("]) + text + ")"
        q = {"queryString": text}
        if bindings["allow"]:
            q["allowLiterals"] = True
        if bindings["named"]:
            q["namedBindings"] = bindings["named"]
        if bindings["positional"]:
            q["positionalBindings"] = bindings["positional"]
        body = {"gqlQuery": q}
        if namespace and g.chance(0.8):
            body["partitionId"] = {"namespaceId": namespace}
        steps.append(rpc("runAggregationQuery" if aggregate else "runQuery", body))
        cursor = cursor or not aggregate
    return steps


def named(name="a", kind="K", **more):
    return {"path": [{"kind": kind, "name": name}], **more}


def by_id(id_, kind="K"):
    return {"path": [{"kind": kind, "id": id_}]}


def where(name, op, value=None, kind="K", **more):
    f = {"property": {"name": name}, "op": op}
    if value is not None:
        f["value"] = value
    return {"kind": [{"name": kind}], "filter": {"propertyFilter": f}, **more}


def both(*filters, op="AND"):
    return {"kind": [{"name": "K"}], "filter": {"compositeFilter": {"op": op, "filters": [
        where(*f)["filter"] for f in filters]}}}


ONE, TWO = {"integerValue": "1"}, {"integerValue": "2"}
BAD_KEYS = [
    {"path": []}, {}, {"path": [{}]}, {"path": [{"kind": ""}]}, named(kind="__x__"), named(""), named("__x__"),
    by_id("0"), by_id("-1"), by_id("x"), by_id("9223372036854775808"),
    {"path": [{"kind": "K", "id": "1", "name": "a"}]},
    {"path": [{"kind": "K"}, {"kind": "K", "name": "a"}]}, named(partitionId={"projectId": "other"}),
    named(partitionId={"namespaceId": "__x__"}), named(partitionId={"namespaceId": "a b"}),
    named(partitionId={"databaseId": "other"}), named("a" * 1501), named(kind="k" * 1501),
    {"path": named()["path"] * 101}, {"path": named()["path"] * 100}, named("\ud800"), named(1), {"path": "K/a"},
]
BAD_VALUES = [
    {}, {"fooValue": 1}, {"integerValue": "1", "stringValue": "a"}, {"integerValue": "x"},
    {"integerValue": "9223372036854775808"}, {"integerValue": 1.5}, {"doubleValue": "nan"}, {"doubleValue": "x"},
    {"timestampValue": "2020-13-01T00:00:00Z"}, {"timestampValue": "10000-01-01T00:00:00Z"}, {"timestampValue": 1},
    {"blobValue": "*"}, {"blobValue": "AQ"}, {"geoPointValue": {"latitude": 91, "longitude": 0}},
    {"geoPointValue": {"latitude": 0, "longitude": 181}}, {"geoPointValue": {"latitude": "NaN", "longitude": 0}},
    {"arrayValue": {"values": [{"arrayValue": {}}]}}, {"arrayValue": {"values": [ONE]}, "excludeFromIndexes": True},
    {"stringValue": "a" * 1501}, {"stringValue": "\u00e9" * 751}, {"blobValue": "A" * 2004},
    {"stringValue": "a", "meaning": 22}, {"integerValue": "1", "meaning": 14}, {"stringValue": "a", "meaning": -1},
    {"entityValue": {"properties": {"": ONE}}}, {"entityValue": {"properties": {"__x__": {"nullValue": None}}}},
    {"keyValue": {"path": []}}, {"keyValue": {"path": [{"kind": "K"}]}}, {"nullValue": 1}, {"booleanValue": "true"},
    {"stringValue": None}, {"arrayValue": []}, {"entityValue": []},
]
BAD_QUERIES = [
    {"kind": [{"name": "K"}, {"name": "L"}]}, {"kind": [{"name": ""}]}, {"kind": [{}]}, {"limit": -1}, {"offset": -1},
    {"kind": [{"name": "K"}], "filter": {}}, {"kind": [{"name": "K"}], "filter": {"propertyFilter": {}}},
    where("a", "LIKE", ONE), where("a", "EQUAL"), {"kind": [{"name": "K"}], "filter": {"compositeFilter": {
        "op": "AND", "filters": []}}}, both(("a", "EQUAL", {"nullValue": None}), op="XOR"),
    {"kind": [{"name": "K"}], "order": [{"property": {"name": "a"}, "direction": "SIDEWAYS"}]},
    {"kind": [{"name": "K"}], "order": [{"property": {"name": ""}}]}, {"kind": [{"name": "K"}], "order": [{}]},
    {"kind": [{"name": "K"}], "projection": [{"property": {"name": "a"}}, {"property": {"name": "a"}}]},
    {"kind": [{"name": "K"}], "projection": [{"property": {"name": "e"}}]},
    {"kind": [{"name": "K"}], "distinctOn": [{"name": "a"}], "projection": [{"property": {"name": "b"}}]},
    {"kind": [{"name": "K"}], "startCursor": "*"}, {"kind": [{"name": "K"}], "startCursor": "AAAA"},
    where("__key__", "EQUAL", {"stringValue": "a"}), where("a", "HAS_ANCESTOR", {"keyValue": named()}),
    where("a", "IN", ONE), where("a", "IN", {"arrayValue": {"values": [{"integerValue": str(n)} for n in range(31)]}}),
    both(("a", "LESS_THAN", ONE), ("b", "GREATER_THAN", ONE)),
    both(("a", "NOT_EQUAL", ONE), ("a", "NOT_IN", {"arrayValue": {"values": [TWO]}})),
    where("a", "EQUAL", ONE, kind="__kind__"),
]


def malformed(g):
    """Bad keys, values, mutations, bodies, read options, queries, GQL and aggregations, and
    entities and commits over the size limits."""
    what = g.pick(["key", "key", "value", "value", "mutation", "body", "options", "query", "query", "gql",
                   "aggregation", "size"])
    good, incomplete = named(), {"path": [{"kind": "K"}]}
    steps = []
    if what == "key":
        bad = g.pick(BAD_KEYS)
        how = g.pick(["upsert", "insert", "update", "delete", "lookup", "allocate", "reserve", "value", "ancestor"])
        if how in ("upsert", "insert", "update"):
            steps.append(commit([{how: {"key": bad, "properties": {"a": ONE}}}]))
        elif how == "delete":
            steps.append(commit([{"delete": bad}]))
        elif how == "lookup":
            steps.append(lookup([bad]))
        elif how == "allocate":
            steps.append(rpc("allocateIds", {"keys": [bad]}))
        elif how == "reserve":
            steps.append(rpc("reserveIds", {"keys": [bad]}))
        elif how == "value":
            steps.append(commit([{"upsert": {"key": good, "properties": {"k": {"keyValue": bad}}}}]))
        else:
            steps.append(rpc("runQuery", {"query": where("__key__", "HAS_ANCESTOR", {"keyValue": bad})}))
    elif what == "value":
        bad = g.pick(BAD_VALUES)
        name = g.pick(["a", "a", "", "__x__", "a" * 1501, "\u00e9" * 751, "a.b"])
        how = g.pick(["property", "property", "array", "entity", "filter"])
        if how == "filter":
            steps.append(rpc("runQuery", {"query": where(name or "a", g.pick(OPS), bad)}))
            return steps
        props = {"property": {name: bad}, "array": {name: {"arrayValue": {"values": [ONE, bad]}}},
                 "entity": {"e": {"entityValue": {"properties": {name: bad}}}}}[how]
        steps.append(commit([{"upsert": {"key": good, "properties": props}}]))
        steps.append(lookup([good]))
    elif what == "mutation":
        bad = g.pick([{}, {"upsert": {"key": good}, "delete": good}, {"upsert": {}}, {"upsert": {"properties": {}}},
                      {"insert": {"key": incomplete}, "baseVersion": "1"}, {"delete": good, "baseVersion": "x"},
                      {"upsert": {"key": good}, "baseVersion": "1", "updateTime": "2001-01-01T00:00:00Z"},
                      {"upsert": {"key": good}, "propertyMask": {"paths": [""]}},
                      {"upsert": {"key": good}, "propertyMask": {"paths": ["a..b"]}}, {"update": {"key": incomplete}},
                      {"delete": incomplete}, {"upsert": "x"},
                      {"upsert": {"key": good}, "conflictResolutionStrategy": "SERVER_VALUE"}])
        steps.append(commit([bad]))
        if g.chance(0.3):
            steps.append(commit([{"upsert": {"key": good}}, {"delete": good}]))
    elif what == "body":
        verb = g.pick(["commit", "lookup", "runQuery", "runAggregationQuery", "beginTransaction", "rollback",
                       "allocateIds", "reserveIds"])
        raw = g.pick(['{not json', '[]', '', 'null', '{"unknownField": 1}', '{"mode": "SOMETIMES"}', '{"keys": "x"}',
                      '{"mutations": {}}', '{"mode": "TRANSACTIONAL"}',
                      '{"mode": "NON_TRANSACTIONAL", "transaction": "AAAA"}',
                      '{"query": {}, "gqlQuery": {"queryString": "SELECT *"}}', '{"transaction": "*"}',
                      '{"keys": [],}', '\ufeff{}', '{} trailing', "{'keys': []}", '{"databaseId": "other"}',
                      '{"databaseId": "(default)"}', '{"transactionOptions": {"readOnly": {}, "readWrite": {}}}',
                      '{"projectId": "other"}'])
        steps.append(rpc(verb, raw=raw))
    elif what == "options":
        bad = g.pick([{"readConsistency": "STRONG", "transaction": "AAAA"}, {"readConsistency": "SOMETIMES"},
                      {"readTime": "2020-13-01T00:00:00Z"}, {"readTime": "2001-01-01T00:00:00Z"},
                      {"readTime": "2999-01-01T00:00:00Z"}, {"transaction": "*"},
                      {"newTransaction": {"readOnly": {"readTime": "2999-01-01T00:00:00Z"}}},
                      {"newTransaction": {}, "transaction": "AAAA"},
                      {"readConsistency": "EVENTUAL", "newTransaction": {}}])
        verb = g.pick(["lookup", "runQuery", "runAggregationQuery"])
        body = {"lookup": {"keys": [good]}, "runQuery": {"query": {"kind": [{"name": "K"}]}},
                "runAggregationQuery": {"aggregationQuery": {"nestedQuery": {"kind": [{"name": "K"}]},
                                                             "aggregations": [{"count": {}}]}}}[verb]
        steps.append(rpc(verb, dict(body, readOptions=bad)))
    elif what == "query":
        steps.append(rpc("runQuery", {"query": g.pick(BAD_QUERIES)}))
    elif what == "gql":
        select = "SELECT * FROM K"
        q = g.pick([{"queryString": ""}, {"queryString": select + " WHERE a = 1"}, {},
                    {"queryString": select + " WHERE a = @1"}, {"queryString": select + " WHERE a = @x"},
                    {"queryString": select + " WHERE a = @1", "positionalBindings": [{}]},
                    {"queryString": select + " WHERE a = @x", "namedBindings": {"x": {"cursor": "AAAA"}}},
                    {"queryString": select + " WHERE a = @1", "namedBindings": {"1": {"value": ONE}}},
                    {"queryString": select, "namedBindings": {"": {"value": ONE}}},
                    {"queryString": select + " LIMIT @1", "positionalBindings": [{"value": {"stringValue": "a"}}]},
                    {"queryString": select + " WHERE __key__ = KEY(K, 'a', L)", "allowLiterals": True},
                    {"queryString": select + " WHERE a = DATETIME('x')", "allowLiterals": True},
                    {"queryString": select + " WHERE a = BLOB('*')", "allowLiterals": True},
                    {"queryString": select + " WHERE a IN ARRAY(" + ", ".join(str(n) for n in range(31)) + ")",
                     "allowLiterals": True},
                    {"queryString": select + " ORDER BY a LIMIT 1 OFFSET 1 LIMIT 2"},
                    {"queryString": "SELECT a, a FROM K"}, {"queryString": select + " GROUP BY a"},
                    {"queryString": select + ";"}, {"queryString": "AGGREGATE COUNT(*) OVER (" + select + ")"},
                    {"queryString": select + " " + select}, {"queryString": "x" * 5000}])
        verb = g.pick(["runQuery", "runQuery", "runAggregationQuery"])
        steps.append(rpc(verb, {"gqlQuery": q}))
    elif what == "aggregation":
        aggregations = g.pick([[], [{"count": {}, "alias": "a"}, {"count": {}, "alias": "a"}], [{"sum": {}}],
                               [{"count": {}, "alias": "__x__"}], [{"count": {"upTo": "-1"}}],
                               [{"count": {"upTo": "0"}}], [{"avg": {"property": {"name": "__key__"}}}],
                               [{"count": {}} for _ in range(6)], [{"count": {}, "sum": {"property": {"name": "a"}}}],
                               [{"alias": "x"}], [{"count": {}, "alias": ""}], [{"count": {}, "alias": "1a"}]])
        nested = g.pick([{"kind": [{"name": "K"}]}, {}, {"kind": [{"name": "K"}], "limit": 1},
                         {"kind": [{"name": "K"}], "projection": [{"property": {"name": "a"}}]},
                         {"kind": [{"name": "K"}], "order": [{"property": {"name": "a"}}]}])
        steps.append(rpc("runAggregationQuery", {"aggregationQuery": {"nestedQuery": nested,
                                                                      "aggregations": aggregations}}))
    else:
        how = g.pick(["many", "long name", "longer name", "long string", "longer string", "long array"])
        many = {"many": lambda: {f"p{n}": {"stringValue": "x" * 1000, "excludeFromIndexes": True} for n in range(1050)},
                "long name": lambda: {"a" * 1500: ONE}, "longer name": lambda: {"a" * 1501: ONE},
                "long string": lambda: {"s": {"stringValue": "x" * 1048487, "excludeFromIndexes": True}},
                "longer string": lambda: {"s": {"stringValue": "x" * 1048488, "excludeFromIndexes": True}},
                "long array": lambda: {"t": {"arrayValue": {"values": [{"integerValue": str(n)}
                                                                       for n in range(20001)]}}}}[how]()
        steps.append(commit([{"upsert": {"key": good, "properties": many}}]))
        if g.chance(0.5):
            steps.append(commit([{"upsert": {"key": by_id(str(n + 1))}} for n in range(g.pick([500, 501]))]))
    return steps


GENERATORS = {"datastore-entities": entities, "datastore-queries": queries, "datastore-transactions": transactions,
              "datastore-gql": gql, "datastore-malformed": malformed}
