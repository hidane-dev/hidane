"""Seeded generators of fuzz cases: REST requests that the fuzzer sends to both emulators.

A case is plain JSON, so it can be written out, read back and replayed:

    {"name": "seed 7 case 3", "area": "queries", "seed": 7, "index": 3, "steps": [step, ...]}

and a step is one request: `method`, `path` (from its `/`), `headers`, and either `body` (sent as
JSON) or `raw` (sent as it is). `{project}` in a path, a body or a header stands for the project
the case runs in, a fresh one each time; `{tx}`, `{token}`, `{commitTime}` and `{updateTime}` for
the last transaction ID, page token, commit time and update time an answer held (each emulator
gets its own). Case `index` of seed `seed` is always the same case: each case draws from its own
`random.Random`, seeded with both. Stdlib only.
"""

import base64
import json
import random
import string
from urllib.parse import quote

import gen_datastore

DB = "projects/{project}/databases/(default)"
NAME = DB + "/documents"
BASE = "/v1/" + NAME
OWNER = {"Authorization": "Bearer owner"}
AREAS = ("documents", "queries", "transactions", "batches", "transforms", "rules", "malformed")
# The areas of each mode the emulators can be started in (`--database-mode`): a run fuzzes one.
MODES = {"firestore-native": AREAS, "datastore-mode": gen_datastore.AREAS}


def jwt(claims):
    """An unsigned (`alg: none`) token, as rules-unit-testing builds them."""
    part = lambda obj: base64.urlsafe_b64encode(json.dumps(obj, separators=(",", ":")).encode()).decode().rstrip("=")
    return "Bearer " + part({"alg": "none", "kid": "fakekid", "typ": "JWT"}) + "." + part(claims) + "."


# Fixed times, so that a case is the same on every run; nothing checks them.
USERS = {
    "none": {},
    "owner": OWNER,
    "u1": {"Authorization": jwt({"sub": "u1", "user_id": "u1", "iat": 1700000000, "exp": 4102444800})},
    "u2": {"Authorization": jwt({"sub": "u2", "user_id": "u2", "iat": 1700000000, "exp": 4102444800})},
    "admin": {"Authorization": jwt({"sub": "u3", "user_id": "u3", "admin": True, "iat": 1700000000, "exp": 4102444800})},
}

INTS = [0, 1, -1, 2, 3, 7, 42, 255, 2**31 - 1, -2**31, 2**32, 2**53, 2**53 + 1, -2**53 - 1, 10**15,
        2**63 - 1, -2**63, -2**63 + 1, 2**63 - 2]
DOUBLES = [0.0, -0.0, 1.0, -1.0, 0.5, 1.5, 2.0, 3.25, 0.1, 1e-7, 0.001, 1e21, 1e22, 1e16, 12345.6789,
           123456789.0, 9007199254740992.0, 1.7976931348623157e308, -1.7976931348623157e308, 5e-324,
           2.2250738585072014e-308, 4.9e-324, "NaN", "Infinity", "-Infinity"]
STRINGS = ["", "a", "b", "aa", "A", "z", "x", "\u00e9", "e\u0301", "\u3042", "\u65e5\u672c\u8a9e", "\U0001F600",
           "\U0001F468\u200d\U0001F469", "\u0000", "a\u0000b", "\t\n\r", "\u2028\u2029", "\ufeff",
           "\uffff", "\u202e", "<>&='\"\\/", "\u007f\u0080\u009f", " ", "x/y", "__x__", "null", "0", "NaN"]
CHARS = (string.ascii_letters + string.digits + string.punctuation + " \u00e9\u00fc\u00df\u3042\u3044\u3046\u65e5\u672c\u8a9e\u0301\u200b\u2028"
         + "\U0001F600\U0001F680\u0000\u0001\u001f\u007f\uffff")
KEYS = ["a", "b", "c", "n", "s", "v", "m", "tags", "x", "y", "z", "A", "_", "x y", "\u00e9", "1", "a-b"]
ODD_KEYS = ["\U0001F600", "a.b", "`a`", "__a", "a__", "\u0000", "\\", "x" * 300, "\u65e5\u672c"]
IDS = ["a", "b", "c", "d", "x", "y", "doc", "007", "\u00e9", "a b", "\U0001F600", "__id5__", "a.b", "-"]
COLLECTIONS = ["c", "d", "users", "\u00e9"]
OPS = ["EQUAL", "NOT_EQUAL", "LESS_THAN", "LESS_THAN_OR_EQUAL", "GREATER_THAN", "GREATER_THAN_OR_EQUAL",
       "ARRAY_CONTAINS", "IN", "NOT_IN", "ARRAY_CONTAINS_ANY"]
UNARY = ["IS_NULL", "IS_NAN", "IS_NOT_NULL", "IS_NOT_NAN"]


def i(n): return {"integerValue": str(n)}
def d(x): return {"doubleValue": x}
def s(x): return {"stringValue": x}
def arr(*xs): return {"arrayValue": {"values": list(xs)} if xs else {}}
def mp(fields): return {"mapValue": {"fields": fields} if fields else {}}
NULL = {"nullValue": None}


def segment(name):
    """A field path segment, quoted with backticks unless it is a plain identifier."""
    if name and (name[0].isalpha() or name[0] == "_") and all(c.isalnum() or c == "_" for c in name) and name.isascii():
        return name
    return "`" + name.replace("\\", "\\\\").replace("`", "\\`") + "`"


def field_path(*names):
    return ".".join(segment(n) for n in names)


def url(path):
    """The REST path of a document or collection path, each ID percent-encoded."""
    return BASE + "/" + "/".join(quote(p, safe="") for p in path.split("/"))


def name(path):
    return NAME + "/" + path


class Gen:
    def __init__(self, rng):
        self.rng = rng

    # --- choices ---------------------------------------------------------------------------
    def chance(self, p):
        return self.rng.random() < p

    def pick(self, seq):
        return self.rng.choice(seq)

    def some(self, seq, lo, hi):
        return [self.pick(seq) for _ in range(self.rng.randint(lo, hi))]

    # --- values ------------------------------------------------------------------------------
    def text(self, lo=0, hi=12):
        if self.chance(0.4):
            return self.pick(STRINGS)
        return "".join(self.pick(CHARS) for _ in range(self.rng.randint(lo, hi)))

    def long_text(self):
        n = self.rng.choice([100, 1000, 1500, 1501, 4096, 20000]) + self.rng.randint(0, 3)
        return "".join(self.pick(CHARS) for _ in range(n)) if self.chance(0.5) else self.pick("a\u00e9\U0001f600") * n

    def timestamp(self):
        if self.chance(0.4):
            return self.pick(["0001-01-01T00:00:00Z", "9999-12-31T23:59:59.999999999Z", "1970-01-01T00:00:00Z",
                              "1969-12-31T23:59:59.999999999Z", "2020-01-01T00:00:00.123456789Z",
                              "2001-01-01T00:00:00Z", "2020-02-29T12:00:00.100Z", "2100-01-01T00:00:00.000001Z"])
        # Never between 2021 and 2099: the fuzzer masks times in those years as the server's.
        year = self.pick([self.rng.randint(1, 2019), self.rng.randint(2101, 9998)])
        text = f"{year:04}-{self.rng.randint(1, 12):02}-{self.rng.randint(1, 28):02}T{self.rng.randint(0, 23):02}:" \
               f"{self.rng.randint(0, 59):02}:{self.rng.randint(0, 59):02}"
        digits = self.pick([0, 0, 1, 3, 3, 6, 6, 9, 9, 2, 7])
        if digits:
            text += "." + "".join(self.pick(string.digits) for _ in range(digits))
        return text + self.pick(["Z", "Z", "Z", "+09:00", "-05:30", "+00:00"])

    def number(self):
        return i(self.pick(INTS)) if self.chance(0.5) else d(self.pick(DOUBLES))

    def bytes_value(self):
        n = self.pick([0, 1, 2, 3, 16, 33, 255]) if self.chance(0.8) else self.rng.randint(0, 3000)
        return {"bytesValue": base64.b64encode(bytes(self.rng.randrange(256) for _ in range(n))).decode()}

    def reference(self):
        return {"referenceValue": name(self.doc_path())}

    def geo(self):
        if self.chance(0.1):
            return {"geoPointValue": {}}
        lat = self.pick([0, 1.5, -90, 90, -0.0, 45.123456789, self.rng.uniform(-90, 90)])
        lng = self.pick([0, -180, 180, 179.999999, -0.0, 2, self.rng.uniform(-180, 180)])
        return {"geoPointValue": {"latitude": lat, "longitude": lng}}

    def vector(self):
        n = self.pick([1, 2, 3, 4, 8]) if self.chance(0.9) else self.rng.randint(0, 64)
        values = [d(self.pick([0.0, 1.0, -1.0, 0.5, 2.0, 1e-3, 3.25, -0.0, 1e300])) for _ in range(n)]
        return mp({"__type__": s("__vector__"), "value": arr(*values)})

    def value(self, depth=0):
        kind = self.rng.random()
        if kind < 0.06:
            return NULL
        if kind < 0.12:
            return {"booleanValue": self.chance(0.5)}
        if kind < 0.27:
            return i(self.pick(INTS))
        if kind < 0.42:
            return d(self.pick(DOUBLES))
        if kind < 0.47:
            return {"timestampValue": self.timestamp()}
        if kind < 0.62:
            return s(self.long_text() if self.chance(0.05) else self.text())
        if kind < 0.67:
            return self.bytes_value()
        if kind < 0.71:
            return self.reference()
        if kind < 0.75:
            return self.geo()
        if kind < 0.79:
            return self.vector()
        if kind < 0.9 and depth < 3:
            # Arrays hold no arrays.
            return arr(*[v for v in (self.value(depth + 1) for _ in range(self.rng.randint(0, 4)))
                         if "arrayValue" not in v])
        if depth < 3:
            return mp(self.fields(depth + 1, 0, 4))
        return s(self.text())

    def key(self):
        return self.pick(ODD_KEYS) if self.chance(0.05) else self.pick(KEYS)

    def fields(self, depth=0, lo=1, hi=6):
        return {self.key(): self.value(depth) for _ in range(self.rng.randint(lo, hi))}

    def doc_path(self):
        path = self.pick(COLLECTIONS) + "/" + self.pick(IDS)
        if self.chance(0.15):
            path += "/sub/" + self.pick(IDS)
        return path

    def mask_paths(self, fields):
        names = list(fields) + KEYS[:4]
        paths = []
        for _ in range(self.rng.randint(1, 3)):
            n = self.pick(names)
            value = fields.get(n, {})
            if "mapValue" in value and value["mapValue"].get("fields") and self.chance(0.5):
                paths.append(field_path(n, self.pick(list(value["mapValue"]["fields"]))))
            else:
                paths.append(field_path(n))
        return paths

    # --- writes ------------------------------------------------------------------------------
    def precondition(self):
        return self.pick([{"exists": True}, {"exists": False}, {"updateTime": "{updateTime}"},
                          {"updateTime": "2001-01-01T00:00:00Z"}])

    def transform(self, fields):
        path = field_path(self.pick(list(fields) + ["n", "t", "arr"]))
        if self.chance(0.15):
            path = field_path("m", self.pick(["x", "y"]))
        kind = self.pick(["setToServerValue", "increment", "maximum", "minimum", "appendMissingElements",
                          "removeAllFromArray"])
        if kind == "setToServerValue":
            return {"fieldPath": path, kind: "REQUEST_TIME"}
        if kind in ("increment", "maximum", "minimum"):
            return {"fieldPath": path, kind: self.number()}
        return {"fieldPath": path, kind: {"values": [self.small_value() for _ in range(self.rng.randint(0, 3))]}}

    def small_value(self):
        return self.pick([i(1), i(2), d(1.0), d("NaN"), d(-0.0), s("a"), s("b"), NULL, {"booleanValue": True},
                          mp({"x": i(1)}), i(2**63 - 1)])

    def write(self, path, fields=None):
        """A commit's write to `path`: a set, a masked update, a delete or a transform."""
        kind = self.rng.random()
        if kind < 0.15:
            w = {"delete": name(path)}
        elif kind < 0.25:
            w = {"transform": {"document": name(path), "fieldTransforms": [self.transform(fields or {"n": i(1)})
                                                                           for _ in range(self.rng.randint(1, 3))]}}
        else:
            fields = fields if fields is not None else self.fields()
            w = {"update": {"name": name(path), "fields": fields}}
            if self.chance(0.3):
                w["updateMask"] = {"fieldPaths": self.mask_paths(fields)}
            if self.chance(0.2):
                w["updateTransforms"] = [self.transform(fields) for _ in range(self.rng.randint(1, 2))]
        if self.chance(0.25):
            w["currentDocument"] = self.precondition()
        return w

    # --- queries -----------------------------------------------------------------------------
    def domain_value(self):
        return self.pick([i(-1), i(0), i(1), i(2), i(3), d(0.5), d(1.0), d(-0.0), d("NaN"), d("Infinity"), NULL,
                          s(""), s("a"), s("b"), s("x"), {"booleanValue": False}, {"booleanValue": True},
                          arr(s("red")), arr(i(1), i(2)), arr(), mp({"x": i(1)}), mp({}),
                          {"timestampValue": "2020-01-01T00:00:00Z"}, {"bytesValue": "AQ=="}])

    def domain_doc(self):
        fields = {}
        for key in ("a", "b", "tags", "m"):
            if self.chance(0.75):
                if key == "tags":
                    fields[key] = arr(*[self.pick([s("red"), s("blue"), s("green"), i(1), NULL])
                                        for _ in range(self.rng.randint(0, 3))])
                elif key == "m":
                    fields[key] = mp({"x": self.domain_value()} if self.chance(0.8) else {})
                else:
                    fields[key] = self.domain_value()
        return fields

    def query_field(self):
        return self.pick(["a", "a", "b", "b", "tags", "m.x", "m", "__name__", "missing"])

    def query_value(self, field, op):
        if field == "__name__" and self.chance(0.85):
            ref = lambda: {"referenceValue": name("q/" + self.pick(["d0", "d1", "d2", "d3", "zz"]))}
            if op in ("IN", "NOT_IN", "ARRAY_CONTAINS_ANY"):
                return arr(*[ref() for _ in range(self.rng.randint(1, 3))])
            return ref()
        if op in ("IN", "NOT_IN", "ARRAY_CONTAINS_ANY"):
            n = self.rng.randint(1, 4) if self.chance(0.9) else self.pick([0, 11, 30, 31])
            return arr(*[self.domain_value() for _ in range(n)])
        return self.domain_value()

    def filter(self, depth=0):
        r = self.rng.random()
        if depth < 2 and r < 0.25:
            op = self.pick(["AND", "OR"])
            return {"compositeFilter": {"op": op, "filters": [self.filter(depth + 1)
                                                              for _ in range(self.rng.randint(1, 3))]}}
        field = self.query_field()
        if r < 0.38:
            return {"unaryFilter": {"field": {"fieldPath": field}, "op": self.pick(UNARY)}}
        op = self.pick(OPS)
        return {"fieldFilter": {"field": {"fieldPath": field}, "op": op, "value": self.query_value(field, op)}}

    def structured_query(self, group=False):
        q = {"from": [{"collectionId": "q", "allDescendants": True} if group else {"collectionId": "q"}]}
        if self.chance(0.7):
            q["where"] = self.filter()
        orders = []
        for _ in range(self.pick([0, 0, 1, 1, 2])):
            orders.append({"field": {"fieldPath": self.pick(["a", "b", "m.x", "__name__", "tags"])},
                           "direction": self.pick(["ASCENDING", "DESCENDING"])})
        if orders:
            q["orderBy"] = orders
        for key in ("startAt", "endAt"):
            if orders and self.chance(0.2):
                values = []
                for o in orders[:self.rng.randint(1, len(orders))]:
                    if o["field"]["fieldPath"] == "__name__":
                        values.append({"referenceValue": name("q/" + self.pick(["d0", "d1", "d2", "zz"]))})
                    else:
                        values.append(self.domain_value())
                q[key] = {"values": values, "before": self.chance(0.5)}
        if self.chance(0.3):
            q["limit"] = self.pick([0, 1, 2, 3, 5]) if self.chance(0.95) else -1
        if self.chance(0.15):
            q["offset"] = self.pick([0, 1, 2]) if self.chance(0.95) else -1
        if self.chance(0.15):
            q["select"] = {"fields": [{"fieldPath": self.pick(["a", "b", "m.x", "__name__", "missing"])}
                                      for _ in range(self.rng.randint(0, 2))]}
        return q

    def aggregation_query(self, group=False):
        aggregations = []
        for n in range(self.rng.randint(1, 3)):
            kind = self.pick(["count", "sum", "avg"])
            if kind == "count":
                agg = {"count": {"upTo": str(self.pick([1, 2, 5]))} if self.chance(0.2) else {}}
            else:
                agg = {kind: {"field": {"fieldPath": self.pick(["a", "b", "m.x", "missing"])}}}
            if self.chance(0.9):
                agg["alias"] = f"x{n}"
            aggregations.append(agg)
        return {"structuredAggregationQuery": {"structuredQuery": self.structured_query(group),
                                               "aggregations": aggregations}}


def step(method, path, body=None, headers=None, raw=None):
    out = {"method": method, "path": path, "headers": dict(OWNER if headers is None else headers)}
    if raw is not None:
        out["raw"] = raw
    elif body is not None:
        out["body"] = body
    return out


def commit(writes, headers=None, transaction=None):
    body = {"writes": writes}
    if transaction:
        body["transaction"] = transaction
    return step("POST", BASE + ":commit", body, headers)


def batch_get(paths, headers=None, **extra):
    return step("POST", BASE + ":batchGet", {"documents": [name(p) for p in paths], **extra}, headers)


# --- areas ---------------------------------------------------------------------------------------

def documents(g):
    """A document written one of the REST ways, then read, listed, updated and deleted."""
    path = g.doc_path()
    collection, _, doc_id = path.rpartition("/")
    fields = g.fields()
    steps = []
    how = g.pick(["patch", "create", "commit", "auto"])
    if how == "patch":
        query = []
        if g.chance(0.2):
            query += ["updateMask.fieldPaths=" + quote(p, safe="") for p in g.mask_paths(fields)]
        if g.chance(0.15):
            query.append("currentDocument.exists=" + g.pick(["true", "false"]))
        if g.chance(0.15):
            query.append("mask.fieldPaths=" + quote(g.pick(list(fields)), safe=""))
        steps.append(step("PATCH", url(path) + ("?" + "&".join(query) if query else ""), {"fields": fields}))
    elif how == "create":
        steps.append(step("POST", url(collection) + "?documentId=" + quote(doc_id, safe=""), {"fields": fields}))
    elif how == "auto":
        steps.append(step("POST", url("gen"), {"fields": fields}))
        path = None
    else:
        steps.append(commit([g.write(path, fields)]))
    for _ in range(g.rng.randint(1, 4)):
        target = path or "gen/x"
        what = g.pick(["get", "get", "mask", "list", "batchGet", "update", "delete", "collections", "create"])
        if what == "get":
            steps.append(step("GET", url(target)))
        elif what == "mask":
            paths = g.mask_paths(fields)
            steps.append(step("GET", url(target) + "?" + "&".join("mask.fieldPaths=" + quote(p, safe="") for p in paths)))
        elif what == "list":
            query = [f"pageSize={g.pick([1, 2, 10])}"] if g.chance(0.5) else []
            if g.chance(0.3):
                query.append("showMissing=true")
            if g.chance(0.2):
                query.append("orderBy=" + quote(g.pick(["a", "b desc", "__name__ desc", "n"]), safe=""))
            col = target.rpartition("/")[0]
            steps.append(step("GET", url(col) + ("?" + "&".join(query) if query else "")))
        elif what == "batchGet":
            steps.append(batch_get([target, g.doc_path()]))
        elif what == "update":
            more = g.fields(0, 1, 3)
            paths = g.mask_paths({**fields, **more})
            steps.append(step("PATCH", url(target) + "?" + "&".join("updateMask.fieldPaths=" + quote(p, safe="")
                                                                    for p in paths), {"fields": more}))
        elif what == "delete":
            q = "?currentDocument.exists=" + g.pick(["true", "false"]) if g.chance(0.3) else ""
            steps.append(step("DELETE", url(target) + q))
        elif what == "collections":
            steps.append(step("POST", BASE + ":listCollectionIds", {"pageSize": g.pick([1, 10])} if g.chance(0.3) else {}))
        else:
            col, _, doc_id = target.rpartition("/")
            steps.append(step("POST", url(col) + "?documentId=" + quote(doc_id, safe=""), {"fields": g.fields(0, 0, 2)}))
    return steps


def seed_queries(g):
    docs = {f"q/d{n}": g.domain_doc() for n in range(g.rng.randint(2, 8))}
    if g.chance(0.4):
        docs["q/d0/q/s0"] = g.domain_doc()
        docs["x/y/q/s1"] = g.domain_doc()
    return [commit([{"update": {"name": name(p), "fields": f}} for p, f in docs.items()])]


def queries(g):
    """A small collection, then queries and aggregations over it."""
    steps = seed_queries(g)
    for _ in range(g.rng.randint(1, 3)):
        group = g.chance(0.15)
        parent = BASE + ("/q/d0" if g.chance(0.08) else "")
        if g.chance(0.3):
            steps.append(step("POST", parent + ":runAggregationQuery", g.aggregation_query(group)))
        else:
            steps.append(step("POST", parent + ":runQuery", {"structuredQuery": g.structured_query(group)}))
    return steps


def transactions(g):
    """Transactions: begun, read in, written in, rolled back, reused, or made up."""
    steps = seed_queries(g)
    docs = ["q/d0", "q/d1", "q/zz"]
    if g.chance(0.2):
        steps.append(batch_get(docs[:2], newTransaction={"readWrite": {}} if g.chance(0.7) else {"readOnly": {}}))
    elif g.chance(0.1):
        steps.append(step("POST", BASE + ":runQuery", {"structuredQuery": g.structured_query(),
                                                        "newTransaction": {"readWrite": {}}}))
    else:
        options = g.pick([{}, {"options": {"readWrite": {}}}, {"options": {"readOnly": {}}},
                          {"options": {"readWrite": {"retryTransaction": "AAAA"}}},
                          {"options": {"readOnly": {"readTime": "{commitTime}"}}}])
        steps.append(step("POST", BASE + ":beginTransaction", options))
    tx = "{tx}"
    if g.chance(0.08):
        tx = g.pick(["AAAA", "EWQAAAAAAAAA", "not base64!", ""])
    ended = False
    for _ in range(g.rng.randint(1, 4)):
        what = g.pick(["batchGet", "batchGet", "query", "aggregate", "commit", "commit", "rollback", "outside"])
        if what == "batchGet":
            steps.append(batch_get(g.some(docs, 1, 3), transaction=tx))
        elif what == "query":
            steps.append(step("POST", BASE + ":runQuery", {"structuredQuery": g.structured_query(), "transaction": tx}))
        elif what == "aggregate":
            body = g.aggregation_query()
            body["transaction"] = tx
            steps.append(step("POST", BASE + ":runAggregationQuery", body))
        elif what == "commit":
            steps.append(commit([g.write(g.pick(docs)) for _ in range(g.rng.randint(0, 3))], transaction=tx))
            ended = True
        elif what == "rollback":
            steps.append(step("POST", BASE + ":rollback", {"transaction": tx}))
            ended = True
        elif not ended and g.chance(0.3):
            # A write outside the transaction to a document it may have read: waits for its lock.
            steps.append(commit([{"update": {"name": name(g.pick(docs)), "fields": {"o": i(1)}}}]))
    return steps


def batches(g):
    """Commits and batchWrites of several writes, some to one document, then the documents."""
    paths = [f"b/{n}" for n in g.some(["a", "b", "c", "d"], 1, 3)]
    steps = []
    if g.chance(0.6):
        steps.append(commit([{"update": {"name": name(p), "fields": g.fields(0, 1, 3)}} for p in paths]))
    for _ in range(g.rng.randint(1, 2)):
        writes = [g.write(g.pick(paths)) for _ in range(g.rng.randint(0 if g.chance(0.05) else 1, 6))]
        if g.chance(0.35):
            body = {"writes": writes}
            if g.chance(0.2):
                body["labels"] = {"k": "v"}
            steps.append(step("POST", BASE + ":batchWrite", body))
        else:
            steps.append(commit(writes))
    steps.append(batch_get(paths + ["b/zz"]))
    return steps


def transforms(g):
    """Field transforms on stored values of every kind, then the document."""
    fields = {"n": g.number(), "arr": arr(*[g.small_value() for _ in range(g.rng.randint(0, 3))])}
    for _ in range(g.rng.randint(0, 3)):
        fields[g.pick(["s", "t", "m", "x"])] = g.value(1)
    steps = [commit([{"update": {"name": name("t/d"), "fields": fields}}])]
    for _ in range(g.rng.randint(1, 3)):
        transforms = [g.transform(fields) for _ in range(g.rng.randint(1, 4))]
        if g.chance(0.5):
            w = {"transform": {"document": name("t/d"), "fieldTransforms": transforms}}
        else:
            more = g.fields(0, 0, 2)
            w = {"update": {"name": name(g.pick(["t/d", "t/e"])), "fields": more}, "updateTransforms": transforms}
            if g.chance(0.6):
                w["updateMask"] = {"fieldPaths": g.mask_paths(more) if more else []}
        if g.chance(0.15):
            w["currentDocument"] = {"exists": g.chance(0.5)}
        steps.append(commit([w]))
    steps.append(step("GET", url("t/d")))
    return steps


RULE_ATOMS = [
    "true", "false", "request.auth != null", "request.auth == null", "request.auth.uid == 'u1'",
    "request.auth.token.admin == true", "resource.data.n > 1", "resource.data.n == 1", "resource.data.s == 'a'",
    "'n' in resource.data", "resource.data.keys().hasAll(['n'])", "resource == null",
    "resource.data.tags.hasAny(['red'])", "request.resource.data.n is int", "request.resource.data.n < 10",
    "request.resource.data.keys().hasOnly(['n', 's', 'tags'])", "request.resource.data.s.size() < 5",
    "request.resource.data.diff(resource.data).affectedKeys().hasOnly(['n'])", "id == 'a'",
    "id.matches('[a-c]')", "id.size() == 1", "request.time < timestamp.date(3000, 1, 1)",
    "exists(/databases/$(database)/documents/r/a)", "get(/databases/$(database)/documents/r/a).data.n == 1",
    "request.query.limit <= 5", "request.method == 'get'", "request.resource.data.n == resource.data.n + 1",
    "resource.data.n is int && resource.data.n % 2 == 0", "request.auth.uid == resource.data.owner",
    "request.resource.data.owner == request.auth.uid", "debug(resource.data.n) > 0",
]


def rule_condition(g, depth=0):
    if depth < 2 and g.chance(0.35):
        op = g.pick([" && ", " || "])
        return "(" + rule_condition(g, depth + 1) + op + rule_condition(g, depth + 1) + ")"
    if depth < 2 and g.chance(0.08):
        return "!(" + rule_condition(g, depth + 1) + ")"
    return g.pick(RULE_ATOMS)


def ruleset(g):
    methods = g.pick([["read", "write"], ["read", "create", "update", "delete"], ["get", "list", "create", "update", "delete"],
                      ["read", "create", "update"]])
    lines = "\n".join(f"      allow {m}: if {rule_condition(g)};" for m in methods)
    sub = ""
    if g.chance(0.3):
        sub = f"\n      match /s/{{sid}} {{\n        allow read, write: if {rule_condition(g)};\n      }}"
    return ("rules_version = '2';\nservice cloud.firestore {\n  match /databases/{database}/documents {\n"
            f"    match /r/{{id}} {{\n{lines}{sub}\n    }}\n  }}\n}}\n")


def rules(g):
    """Generated Security Rules, documents written past them, then reads and writes as users."""
    seed = {p: {"n": i(g.pick([0, 1, 2, 3])), "s": s(g.pick(["a", "b"])), "tags": arr(s(g.pick(["red", "blue"]))),
                "owner": s(g.pick(["u1", "u2"]))} for p in ("r/a", "r/b", "r/c")}
    steps = [
        step("PUT", "/emulator/v1/projects/{project}:securityRules",
             {"rules": {"files": [{"name": "firestore.rules", "content": ruleset(g)}]}}),
        commit([{"update": {"name": name(p), "fields": f}} for p, f in seed.items()]),
    ]
    docs = ["r/a", "r/b", "r/d", "r/a/s/x"]
    for _ in range(g.rng.randint(1, 5)):
        who = USERS[g.pick(["none", "u1", "u1", "u2", "admin", "owner"])]
        what = g.pick(["get", "get", "list", "create", "update", "delete", "commit", "batchGet"])
        doc = g.pick(docs)
        data = {"n": i(g.pick([0, 1, 2, 5, 11])), "s": s(g.pick(["a", "abcdef"])), "owner": s(g.pick(["u1", "u2"]))}
        if g.chance(0.2):
            data["extra"] = i(1)
        if what == "get":
            steps.append(step("GET", url(doc), headers=who))
        elif what == "list":
            q = {"from": [{"collectionId": "r"}]}
            if g.chance(0.5):
                q["where"] = {"fieldFilter": {"field": {"fieldPath": g.pick(["n", "owner", "s"])}, "op": "EQUAL",
                                              "value": g.pick([i(1), s("u1"), s("a")])}}
            if g.chance(0.5):
                q["limit"] = g.pick([1, 5, 10])
            steps.append(step("POST", BASE + ":runQuery", {"structuredQuery": q}, who))
        elif what == "create":
            col, _, doc_id = doc.rpartition("/")
            steps.append(step("POST", url(col) + "?documentId=" + doc_id, {"fields": data}, who))
        elif what == "update":
            mask = "?updateMask.fieldPaths=n" if g.chance(0.5) else ""
            steps.append(step("PATCH", url(doc) + mask, {"fields": data}, who))
        elif what == "delete":
            steps.append(step("DELETE", url(doc), headers=who))
        elif what == "commit":
            steps.append(commit([g.write(g.pick(docs), dict(data)) for _ in range(g.rng.randint(1, 3))], who))
        else:
            steps.append(batch_get(g.some(docs, 1, 3), who))
    return steps


def deep(depth):
    value = i(1)
    for n in range(depth):
        value = mp({"m": value}) if n % 2 else arr(value) if n % 4 == 0 else mp({"a": value})
    return value


BAD_VALUES = [
    {"fooValue": 1}, {"integerValue": "1", "stringValue": "a"}, {"integerValue": "abc"}, {"integerValue": "1.5"},
    {"integerValue": "9223372036854775808"}, {"integerValue": "-9223372036854775809"}, {"integerValue": 5},
    {"integerValue": " 1"}, {"integerValue": "+1"}, {"integerValue": "0x10"}, {"doubleValue": "x"},
    {"doubleValue": "1.5"}, {"doubleValue": "1e400"}, {"doubleValue": "-1e-400"}, {"doubleValue": "nan"},
    {"doubleValue": True}, {"timestampValue": "not a time"}, {"timestampValue": "10000-01-01T00:00:00Z"},
    {"timestampValue": "0000-12-31T23:59:59Z"}, {"timestampValue": "2020-01-01T00:00:00.1234567891Z"},
    {"timestampValue": "2020-13-01T00:00:00Z"}, {"timestampValue": "2020-01-01T00:00:00"},
    {"timestampValue": "2020-01-01 00:00:00Z"}, {"timestampValue": 0}, {"bytesValue": "!!!"},
    {"bytesValue": "AQ"}, {"bytesValue": "_-8="}, {"geoPointValue": {"latitude": 91, "longitude": 0}},
    {"geoPointValue": {"latitude": 0, "longitude": 180.5}}, {"geoPointValue": {"latitude": "NaN", "longitude": 0}},
    {"geoPointValue": {"latitude": "1", "longitude": 0}}, arr(arr(i(1))), arr(mp({"a": arr(arr())})),
    mp({"__x__": i(1)}), mp({"": i(1)}), {"referenceValue": "projects/other/databases/(default)/documents/c/d"},
    {"referenceValue": NAME + "/c"}, {"referenceValue": "c/d"}, {"referenceValue": NAME + "/c//d"},
    {"referenceValue": "projects/{project}/databases/other/documents/c/d"}, {"nullValue": "x"},
    {"nullValue": 0}, {"nullValue": "NULL_VALUE"}, {"booleanValue": "yes"}, {"booleanValue": 1}, {"stringValue": 1},
    {"stringValue": "\ud800"}, {"mapValue": {"fields": {"a": {}}}}, {}, {"arrayValue": {"values": [{}]}},
    mp({"__type__": s("__vector__"), "value": arr(d("NaN"))}), mp({"__type__": s("__vector__"), "value": arr(i(1))}),
    mp({"__type__": s("__vector__"), "value": arr()}), mp({"__type__": s("__vector__"), "value": s("x")}),
    mp({"__type__": s("__vector__"), "value": arr(*[d(1.0)] * 2049)}), mp({"__type__": s("__other__")}),
    mp({"__type__": s("__vector__")}), deep(20), deep(21), deep(19), s("x" * 1048487), s("x" * 1048488),
    {"bytesValue": base64.b64encode(b"\0" * 1048488).decode()},
]

BAD_PATHS = ["c", "c/d/e", "c//d", "/c/d", "c/d/", "__x__/d", "c/__x__", "c/.", "c/..", "c/" + "x" * 1501,
             "x" * 1501 + "/d", "c/d%2Fe", "c/__id__", "c/__id-1__", "c/__id9223372036854775808__", "c/ ",
             "c/\u0000", "c/\U0001F600", "c/" + "\u00e9" * 751]


def malformed(g):
    """Requests that should be refused: bad names, values, bodies, sizes."""
    what = g.pick(["value", "value", "value", "path", "path", "body", "name", "mask", "query", "size", "transform"])
    steps = []
    if what == "value":
        bad = g.pick(BAD_VALUES)
        nest = g.pick(["top", "map", "array"])
        value = bad if nest == "top" else mp({"k": bad}) if nest == "map" else arr(i(1), bad)
        how = g.pick(["commit", "patch", "query", "batchWrite"])
        if how == "commit":
            steps.append(commit([{"update": {"name": name("c/d"), "fields": {"v": value}}}]))
        elif how == "patch":
            steps.append(step("PATCH", url("c/d"), {"fields": {"v": value}}))
        elif how == "batchWrite":
            steps.append(step("POST", BASE + ":batchWrite", {"writes": [{"update": {"name": name("c/d"), "fields": {"v": value}}}]}))
        else:
            steps.append(step("POST", BASE + ":runQuery", {"structuredQuery": {"from": [{"collectionId": "c"}], "where": {
                "fieldFilter": {"field": {"fieldPath": "v"}, "op": g.pick(OPS), "value": value}}}}))
        steps.append(step("GET", url("c/d")))
    elif what == "path":
        path = g.pick(BAD_PATHS)
        how = g.pick(["get", "patch", "delete", "commit", "batchGet", "list", "create"])
        if how == "get":
            steps.append(step("GET", url(path)))
        elif how == "patch":
            steps.append(step("PATCH", url(path), {"fields": {"a": i(1)}}))
        elif how == "delete":
            steps.append(step("DELETE", url(path)))
        elif how == "commit":
            steps.append(commit([{"update": {"name": name(path), "fields": {"a": i(1)}}}]))
        elif how == "batchGet":
            steps.append(batch_get([path]))
        elif how == "list":
            steps.append(step("GET", url(path) + "?pageSize=2"))
        else:
            col, _, doc_id = path.rpartition("/")
            steps.append(step("POST", url(col or "c") + "?documentId=" + quote(doc_id, safe=""), {"fields": {"a": i(1)}}))
    elif what == "name":
        bad = g.pick(["projects/{project}/databases/other/documents/c/d", "projects/other/databases/(default)/documents/c/d",
                      "projects/{project}/databases/(default)/c/d", "projects/{project}/databases/(default)/documents",
                      "c/d", "", "projects//databases/(default)/documents/c/d", NAME + "/c/d/"])
        how = g.pick(["commit", "batchGet", "delete", "transform"])
        if how == "commit":
            steps.append(commit([{"update": {"name": bad, "fields": {"a": i(1)}}}]))
        elif how == "batchGet":
            steps.append(step("POST", BASE + ":batchGet", {"documents": [bad]}))
        elif how == "delete":
            steps.append(commit([{"delete": bad}]))
        else:
            steps.append(commit([{"transform": {"document": bad, "fieldTransforms": [{"fieldPath": "n", "increment": i(1)}]}}]))
    elif what == "body":
        raw = g.pick(['{not json', '{"writes": "x"}', '[]', '', 'null', '{"writes": [{}]}', '{"writes": [{"update": {}}]}',
                      '{"unknownField": 1}', '{"writes": [],}', '{"writes": [{"update": {"name": 1}}]}',
                      '{"writes": [{"delete": "' + NAME + '/c/d", "update": {"name": "' + NAME + '/c/d"}}]}',
                      '\ufeff{"writes": []}', '{"writes": []} trailing', "{'writes': []}", '{"writes": null}'])
        verb = g.pick([":commit", ":batchGet", ":runQuery", ":beginTransaction", ":batchWrite", ":runAggregationQuery",
                       ":listCollectionIds", ":rollback"])
        headers = dict(OWNER, **{"Content-Type": g.pick(["application/json", "text/plain", "application/x-www-form-urlencoded"])})
        steps.append(step("POST", BASE + verb, headers=headers, raw=raw))
    elif what == "mask":
        paths = g.pick([["a..b"], [""], ["`a"], ["a.`b"], ["__name__"], ["a", "a"], ["a", "a.b"], ["a.b", "a"], ["`"],
                        ["a.__x__"], ["a b"], ["`a`.`b`"], ["*"], ["a[0]"]])
        how = g.pick(["commit", "patch", "get"])
        if how == "commit":
            steps.append(commit([{"update": {"name": name("c/d"), "fields": {"a": mp({"b": i(1)})}},
                                  "updateMask": {"fieldPaths": paths}}]))
        elif how == "patch":
            steps.append(step("PATCH", url("c/d") + "?" + "&".join("updateMask.fieldPaths=" + quote(p, safe="") for p in paths),
                              {"fields": {"a": i(1)}}))
        else:
            steps.append(step("GET", url("c/d") + "?" + "&".join("mask.fieldPaths=" + quote(p, safe="") for p in paths)))
    elif what == "query":
        q = g.pick([
            {"from": []}, {"from": [{"collectionId": "a"}, {"collectionId": "b"}]}, {"from": [{"collectionId": ""}]},
            {"from": [{"collectionId": "__x__"}]}, {"from": [{"collectionId": "c"}], "limit": 2**31},
            {"from": [{"collectionId": "c"}], "offset": 2**31}, {"from": [{"collectionId": "c"}], "where": {}},
            {"from": [{"collectionId": "c"}], "orderBy": [{"field": {"fieldPath": "a"}, "direction": "SIDEWAYS"}]},
            {"from": [{"collectionId": "c"}], "where": {"fieldFilter": {"field": {"fieldPath": "a"}, "op": "LIKE", "value": i(1)}}},
            {"from": [{"collectionId": "c"}], "startAt": {"values": [i(1)]}},
            {"from": [{"collectionId": "c"}], "orderBy": [{"field": {"fieldPath": "a"}}, {"field": {"fieldPath": "a"}}]},
            {"from": [{"collectionId": "c"}], "select": {"fields": [{"fieldPath": "a.."}]}},
            {"from": [{"collectionId": "c"}], "findNearest": {"vectorField": {"fieldPath": "v"}, "queryVector": arr(), "distanceMeasure": "EUCLIDEAN", "limit": 1}},
            {"from": [{"collectionId": "c"}], "where": {"unaryFilter": {"field": {"fieldPath": "a"}, "op": "IS_NAN"}},
             "orderBy": [{"field": {"fieldPath": "a"}, "direction": "ASCENDING"}]},
            {"from": [{"collectionId": "c", "allDescendants": True}], "orderBy": [{"field": {"fieldPath": "__name__"}, "direction": "DESCENDING"}]},
        ])
        if g.chance(0.3):
            steps.append(step("POST", BASE + ":runAggregationQuery", {"structuredAggregationQuery": {
                "structuredQuery": q, "aggregations": g.pick([[], [{"count": {}, "alias": "a"}, {"count": {}, "alias": "a"}],
                                                              [{"sum": {}}], [{"count": {}, "alias": "__x__"}],
                                                              [{"count": {"upTo": "-1"}}], [{"avg": {"field": {"fieldPath": "__name__"}}}],
                                                              [{"count": {}} for _ in range(6)]])}}))
        else:
            steps.append(step("POST", BASE + ":runQuery", {"structuredQuery": q}))
    elif what == "size":
        many = g.pick([{f"f{n}": s("x" * 1000) for n in range(1050)}, {"x" * 1500: i(1)}, {"x" * 1501: i(1)},
                       {"\u00e9" * 751: i(1)}])
        steps.append(commit([{"update": {"name": name("c/d"), "fields": many}}]))
        if g.chance(0.5):
            steps.append(commit([{"update": {"name": name(f"c/{n}"), "fields": {}}} for n in range(g.pick([500, 501]))]))
    else:
        bad = g.pick([{"fieldPath": "n", "increment": s("x")}, {"fieldPath": "n", "increment": d("NaN")},
                      {"fieldPath": "n", "setToServerValue": "SERVER_VALUE_UNSPECIFIED"}, {"fieldPath": "n"},
                      {"fieldPath": "", "increment": i(1)}, {"fieldPath": "__name__", "increment": i(1)},
                      {"fieldPath": "n", "increment": i(1), "maximum": i(1)},
                      {"fieldPath": "n", "appendMissingElements": {"values": [arr(i(1))]}},
                      {"fieldPath": "n", "appendMissingElements": {"values": [d("NaN")]}},
                      {"fieldPath": "n", "removeAllFromArray": {}}, {"fieldPath": "a.b", "increment": i(1)}])
        others = [{"fieldPath": "n", "increment": i(1)}] if g.chance(0.3) else []
        steps.append(commit([{"update": {"name": name("c/d"), "fields": {"a": i(1)}}, "updateTransforms": [bad] + others}]))
        steps.append(step("GET", url("c/d")))
    return steps


GENERATORS = {"documents": documents, "queries": queries, "transactions": transactions, "batches": batches,
              "transforms": transforms, "rules": rules, "malformed": malformed, **gen_datastore.GENERATORS}


def case(seed, index, areas=AREAS):
    """Case `index` of `seed`: its area by turns, its content from its own generator."""
    area = areas[index % len(areas)]
    rng = random.Random(f"hidane-fuzz:{seed}:{index}")
    return {"name": f"seed {seed} case {index}", "area": area, "seed": seed, "index": index,
            "steps": GENERATORS[area](Gen(rng))}
