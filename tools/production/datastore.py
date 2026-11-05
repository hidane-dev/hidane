"""Replays recordings of the official emulator in Datastore mode against a Datastore-mode database
of production (#81, #82).

`replay.py --datastore-database <id>` hands these recordings here: those of
`tools/oracle/datastore_*.py` that are REST requests sent in order (`datastore_entities.json`,
`datastore_queries.json`, `datastore_gql.json`, `datastore_firestore_api.json`,
`datastore_gql_errors.json`), the steps of `datastore_sequences.json`, REST in protobuf
(`datastore_protobuf.json`), how long a write waits for a lock (`datastore_locks.json`), and
which fault of a request is reported (`datastore_checks.json`, `datastore_commit_checks.json`). They go to that
database of the project, a named one in Datastore mode: a database has one mode, and the project's
`(default)` is in Firestore Native mode. The database is emptied before and after each recording,
and a recording's dataset written first, as its replaying test writes it.

What production does its own way, and how a request and its answer are put:

- A Datastore request names its database twice: `databaseId` in the body, and the routing header
  `x-goog-request-params`. Without the header production refuses the request (400, "Missing
  routing header"); without the body's, it reads the `(default)` database. A key whose
  `partitionId` names the project and no database is, to production, one of `(default)`, where
  the official emulator reads it as one of its default database: it gets the database's name
  (one that names neither is the request's, to both). Its answers name the
  database in each key's `partitionId` too, which the official emulator, serving its default
  database, leaves out: production's `databaseId` is taken out before comparing, as is the
  database its messages name in a key (`database_id: "<id>"` in one printed as protobuf text,
  `<project>#<id>` in "mismatched databases within request").
- A request of the Firestore API goes to the same database (`databases/(default)` becomes
  `databases/<id>`), and production's answer names `(default)` again.
- The IDs production allocates are not the official emulator's, which allocates them in a fixed
  order. Those of an answer to `allocateIds` or `commit` are carried, as transactions and cursors
  are (`defines`): production's take the official emulator's place in the requests that follow,
  and are read back as the official emulator's in what production answers.
- A query production refuses for want of a composite index, which the emulator never asks for,
  is counted apart (D070), the index it recommends kept in the report.

Answers are compared as in `replay.py`: the status, the media type and the body as JSON, a
recording's placeholders matching any value, and differences the parity exceptions record
counting as known (`tally`). Stdlib only.
"""

import base64
import json
import re
import threading
import time
from datetime import datetime, timezone

from replay import PROJECT_NAME, TRANSIENT, difference, log, mask, media, parsed, same_json, tally

DATASTORE_API = "https://datastore.googleapis.com"
# A request of the Datastore API in the recordings, and one of the Firestore API to the database.
DATASTORE_PATH = re.compile(r"/v1/projects/\{project\}:(\w+)")
FIRESTORE_PATH = re.compile(r"/v1(?:beta1)?/projects/\{project\}/databases/\(default\)(?=[/:?]|$)")
# The keys a query of the replay's own emptying reads at a time; a commit deletes at most 500.
EMPTYING_PAGE = 1000
MUTATIONS = 500
# How long a request of the replay's own waits before each try again: over three minutes in all.
CALL_WAITS = (5, 10, 20, 40, 60, 60, 0)
# Production's refusal of a query that needs a composite index, or several; the indexes follow,
# in the format of index.yaml, and a line of one.
NEEDS_INDEX = re.compile(r"^(?:no matching index found\. recommended index is:|One possible index to serve the query is:|"
                         r"The query requires multiple indexes\. The recommended indexes are:)")
# A kind with a reserved name, of which production makes no index (D132).
RESERVED_KIND = re.compile(r"__.*__")
INDEX_LINE = re.compile(r"\s*(-?)\s*(kind|ancestor|name|direction):\s*(.*?)\s*")

# In `datastore_sequences.json`: the labels of the times and versions of an answer, which the
# recorder gave in the order they first appear in the case's answers (`<t1>`, ...); a time or a
# version, as production gives it; cursors; and a request's references to the time of an earlier
# step's answer
# (`{time:<step>:<path>}`, moved by `:<microseconds>`) and to a transaction (`{tx}`, the last
# begun, or `{tx:<name>}`, the one a step began `as` that name).
LABEL = re.compile(r"<t\d+>")
TIME = re.compile(r"\d+|\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?Z")
CURSOR = re.compile(r'"(cursor|endCursor|skippedCursor|startCursor)": "[^"]*"')
STEP_TIME = re.compile(r"\{time:(\d+):([A-Za-z0-9.]+)(?::(-?\d+))?\}")
STEP_TRANSACTION = re.compile(r"\{tx(?::([a-z]+))?\}")
# A run of one character, as some recorders write 200 or more (`{repeat:<c>:<n>}`).
REPEAT = re.compile(r"\{repeat:(.*?):(\d+)\}")

# REST in protobuf (`datastore_protobuf.json`): its recorder, the project it names (fixed: an
# answer in protobuf cannot be masked without changing its length), the field of every request of
# the Datastore API that names the database (`database_id`), and the `google.rpc.Code` names an
# error in JSON gives.
PROTOBUF_RECORDER = "tools/oracle/datastore_protobuf.py"
PROTOBUF_PROJECT = "demo-protobuf"
DATABASE_FIELD = 9
CODES = {1: "CANCELLED", 2: "UNKNOWN", 3: "INVALID_ARGUMENT", 4: "DEADLINE_EXCEEDED", 5: "NOT_FOUND",
         6: "ALREADY_EXISTS", 7: "PERMISSION_DENIED", 8: "RESOURCE_EXHAUSTED", 9: "FAILED_PRECONDITION",
         10: "ABORTED", 11: "OUT_OF_RANGE", 12: "UNIMPLEMENTED", 13: "INTERNAL", 14: "UNAVAILABLE",
         15: "DATA_LOSS", 16: "UNAUTHENTICATED"}

# How long a write waits for another transaction's lock (`datastore_locks.json`): its recorder,
# the key the reader looks up to stay in use while the write waits, and how far, in seconds, the
# wait may be from the official emulator's (as hidane's test allows), and how long a write may
# wait for an answer: the official emulator gives up on one outside a transaction after 60 s.
LOCKS_RECORDER = "tools/oracle/datastore_locks.py"
KEPT_ALIVE_BY = {"keys": [{"path": [{"kind": "K", "name": "z"}]}]}
WAIT_TOLERANCE = 5
LOCK_TIMEOUT = 600

# Differences of Datastore mode that docs/parity-exceptions.md records (D119 to D133), set aside
# before an answer is compared, each counted as known by its entry (`set_aside`).
#
# Fields only one side's answers have, where in an answer: (entry, the path, the official
# emulator's own, production's own).
KNOWN_FIELDS = [
    ("D120", r"\$", {"commitTime", "readTime"}, {"indexUpdates"}),
    ("D120", r"\$\.found\[\d+\]", {"cursor"}, set()),
    ("D120", r"\$\.missing\[\d+\]", set(), {"version"}),
    ("D120", r"\$\.mutationResults\[\d+\]", set(), {"conflictDetected"}),
    ("D121", r"\$\.batch", set(), {"endCursor", "moreResults", "skippedCursor"}),
    ("D121", r"\$\.batch\.entityResults\[\d+\]", {"createTime", "updateTime", "version"}, set()),
    ("D121", r"\$", set(), {"query"}),
    ("D122", r"\$\.batch", set(), {"snapshotVersion"}),
    ("D122", r"\$\.batch\.entityResults\[\d+\]", set(), {"cursor"}),
]
# A value each side gives: (entry, the path, the official emulator's, production's).
KNOWN_VALUES = [
    ("D121", r"\$\.batch\.moreResults", "NO_MORE_RESULTS", "MORE_RESULTS_AFTER_CURSOR"),
    ("D121", r"\$\.batch\.moreResults", "NO_MORE_RESULTS", "MORE_RESULTS_AFTER_LIMIT"),
]
# Answers that differ as a whole, an error on one side or both: (entry, the official emulator's
# HTTP status, error status and message, then production's); None matches any, and a message
# pattern only an error that has one. The status of an answer that is no error is None.
TRANSACTION_ENDED = r"^The referenced transaction has expired or is no longer valid\.$"
FIRESTORE_API = r"^The Cloud Firestore API is not available for Firestore in Datastore Mode database "
# What production checks of a request before its other faults (D131): a new read-only
# transaction's read time, and a reserved name projected or grouped by.
READ_TIME = r"^(?:timestamp cannot have more than microseconds precision|Only timestamps past epoch are supported\.)$"
RESERVED = r"^(?:projections are|Group by is) not supported for the property: __"
KNOWN_ERRORS = [
    ("D124", 500, "UNKNOWN", None, 400, "INVALID_ARGUMENT", r"^Invalid transaction\.$"),
    ("D124", 400, "INVALID_ARGUMENT", r"^non-transaction commit cannot specify a transaction$",
     400, "INVALID_ARGUMENT", r"^Invalid transaction\.$"),
    ("D124", 409, "ABORTED", TRANSACTION_ENDED, 400, "INVALID_ARGUMENT", TRANSACTION_ENDED),
    ("D124", 200, None, None, 400, "INVALID_ARGUMENT", TRANSACTION_ENDED),
    ("D124", 409, "ABORTED", r"^Transaction lock timeout\.$",
     409, "ABORTED", r"^too much contention on these datastore entities\. please try again\.$"),
    ("D124", None, None, None, 400, "INVALID_ARGUMENT", r"^transaction required$"),
    # Production checks a transaction before the rest of a request.
    ("D124", None, None, r"", 400, "INVALID_ARGUMENT", r"^Invalid transaction\.$"),
    # Production stores arrays in arrays, and goes on to what else a request holds.
    ("D119", 400, "INVALID_ARGUMENT", r"^Nested arrays are not allowed$", None, None, None),
    ("D131", None, None, r"", 400, "INVALID_ARGUMENT", READ_TIME),
    ("D131", None, None, r"", 400, "INVALID_ARGUMENT", RESERVED),
    ("D131", 400, "INVALID_ARGUMENT", r"^Too many disjunctions after normalization\.", None, None, r""),
    ("D131", 404, "NOT_FOUND", r"^no entity to update: ", 400, "INVALID_ARGUMENT", r""),
    ("D131", 400, "INVALID_ARGUMENT", r"^Cannot update an entity with a base version set to zero: ",
     400, "INVALID_ARGUMENT", r"^Cannot insert an entity with a base version greater than zero: "),
    ("D131", 400, "INVALID_ARGUMENT", r"^Cannot (?:insert|update) an entity with a base version ",
     400, "INVALID_ARGUMENT", r"^cannot set a conflict resolution strategy if no detection strategy was set$"),
    ("D122", 400, "INVALID_ARGUMENT", r"^Key filter on __kind__ is invalid \(must be a key for __kind__\)", 200, None, None),
    ("D122", None, None, None, 400, "INVALID_ARGUMENT", r"^Unsupported query for kind __(?:kind|namespace|property)__$"),
    # Production refuses these whatever the official emulator answers, which takes a key by ID.
    ("D122", None, None, None, 400, "INVALID_ARGUMENT",
     r"^Projections are not supported for the kind: __|^Key filter on __kind__ is invalid \(must be a named key for __kind__\)"),
    ("D125", 400, "FAILED_PRECONDITION", r"^List of Ids to reserve cannot be empty\.$", 200, None, None),
    ("D125", 400, "INVALID_ARGUMENT", r"^Firestore emulator does not support eventual consistency options\.$",
     200, None, None),
    # Production takes an order on a property twice, and goes on to what else a query holds.
    ("D125", 400, "INVALID_ARGUMENT", r"^order by clause cannot contain duplicate fields ", None, None, None),
    ("D125", None, None, None, 400, "INVALID_ARGUMENT", r"^The project \S* does not exist\.$"),
    ("D125", 500, "UNKNOWN", None, 400, "INVALID_ARGUMENT",
     r"^(?:limit is negative|offset is negative|Unable to plan or invalidate query\.)$"),
    ("D125", 400, "INVALID_ARGUMENT", r"^kind is required for all orders except __key__ ascending$",
     400, "INVALID_ARGUMENT", r"^Unable to plan or invalidate query\.$"),
    # Production checks what a query of no kind holds before that it has none.
    ("D131", 400, "INVALID_ARGUMENT", r"^kind is required for ", None, None, r""),
    ("D125", 500, "UNKNOWN", None, 400, "INVALID_ARGUMENT", r"^Timestamp is not valid\. See proto definition for valid values\. "),
    ("D125", 400, "INVALID_ARGUMENT", r"^__key__ filter value must be a Key$",
     400, "INVALID_ARGUMENT", r"^Cannot use projection on a property with an equality filter\.$"),
    ("D125", 400, "INVALID_ARGUMENT", r"^Cannot use projection on a property with an equality filter\.$",
     400, "INVALID_ARGUMENT", r"^The query namespace is '[^']*' but __key__ filter namespace is '[^']*'\.$"),
    ("D125", 500, "UNKNOWN", None, 500, "INTERNAL", r"^An internal error occurred\.$"),
    ("D125", 400, "INVALID_ARGUMENT", r"", 500, "INTERNAL", r"^An internal error occurred\.$"),
    ("D125", 400, "INVALID_ARGUMENT", r"^Value type \w+ is not allowed$", 200, None, None),
    # Production refuses a point whose latitude or longitude is NaN or out of range in an
    # aggregation too, before the faults of its own the official emulator names, or fails on (500).
    ("D125", None, None, None, 400, "INVALID_ARGUMENT",
     r"^Geo point (?:latitude|longitude) (?:is not a number\.|outside permitted range )"),
    ("D125", 500, "UNKNOWN", None, 200, None, None),
    ("D125", 409, "ALREADY_EXISTS", r"^entity already exists: EntityRef\[",
     409, "ALREADY_EXISTS", r"^entity already exists: app: "),
    ("D126", None, None, None, 400, "FAILED_PRECONDITION", FIRESTORE_API),
    # Production asks for an index of a reserved kind, which it refuses to make.
    ("D132", 200, None, None, 400, "FAILED_PRECONDITION", NEEDS_INDEX.pattern + r"\s*- kind: __\S*__\n"),
    # An entity's limits: production words the refusal of its size its own way, and takes 5,001
    # indexed values that are all the same.
    ("D133", 400, "INVALID_ARGUMENT", r"^maximum entity size is 1048576 bytes$", 400, "INVALID_ARGUMENT", r"^entity is too big$"),
    ("D133", 400, "INVALID_ARGUMENT", r"^Too many indexed properties$", 200, None, None),
]


class Database:
    """A Datastore-mode database of the replay's project; its requests count in the run's budget."""

    def __init__(self, production, name, base=DATASTORE_API):
        self.production = production
        self.name = name
        self.base = base.rstrip("/")

    def send(self, method, body=None, raw=None, content_type="application/json", verb="POST", timeout=60, tries=None):
        """A request of the Datastore API (`method`, as `lookup`) to the database: a JSON `body`
        gets the database's `databaseId`, and so does each `partitionId` of the project that names
        none (`with_database`); a `raw` one is sent as it is. Answers (status, content type, body
        as text), or status 0 if none came in `timeout` seconds, `tries` times (by default as
        `Production.send`)."""
        project = self.production.project
        if body is not None:
            raw = json.dumps({**with_database(body, project, self.name), "databaseId": self.name}).encode()
        return self.production.send(verb, f"/v1/projects/{project}:{method}", raw, content_type, base=self.base,
                                    headers={"x-goog-request-params": f"project_id={project}&database_id={self.name}"},
                                    budgeted=True, timeout=timeout, **({"tries": tries} if tries else {}))

    def send_bytes(self, verb, path, data, content_type, headers):
        """A request as one of REST in protobuf is recorded: its own path (of the project), its
        `Content-Type` and other headers, its body sent as it is. Answers (status, content type,
        body as bytes)."""
        project = self.production.project
        return self.production.send(verb, path, data, content_type, base=self.base, budgeted=True, binary=True,
                                    headers={**headers, "x-goog-request-params": f"project_id={project}&database_id={self.name}"})

    def call(self, method, body):
        """A request of the replay's own, retried while the service is busy, or while a
        transaction a recording left open holds a lock on what it writes (409, "too much
        contention"), until the transaction expires. Its answer as JSON."""
        for wait in CALL_WAITS:
            status, _, text = self.send(method, body)
            if status not in TRANSIENT | {409}:
                break
            time.sleep(wait)
        if status != 200:
            raise RuntimeError(f"{method} answered {status}: {mask(text, self.production.project)[:300]}")
        return json.loads(text)

    def keys(self, query, namespace):
        """The keys a query finds in a namespace, a page at a time."""
        found, cursor = [], None
        while True:
            page = {**query, "projection": [{"property": {"name": "__key__"}}], "limit": EMPTYING_PAGE}
            if cursor:
                page["startCursor"] = cursor
            batch = self.call("runQuery", {"partitionId": {"namespaceId": namespace}, "query": page})["batch"]
            results = batch.get("entityResults", [])
            found += [r["entity"]["key"] for r in results]
            if len(results) < EMPTYING_PAGE:
                return found
            cursor = batch["endCursor"]

    def empty(self):
        """Deletes every entity of the database: in each namespace `__namespace__` lists, what a
        query of no kind finds, but the entities of kinds production keeps for itself
        (`__Stat_Total__` and the like)."""
        namespaces = [""] + [k["path"][-1]["name"] for k in self.keys({"kind": [{"name": "__namespace__"}]}, "")
                             if "name" in k["path"][-1]]
        keys = [k for namespace in namespaces for k in self.keys({}, namespace)
                if not k["path"][-1]["kind"].startswith("__")]
        log(f"emptying the Datastore-mode database: {len(keys)} entities")
        for start in range(0, len(keys), MUTATIONS):
            self.call("commit", {"mode": "NON_TRANSACTIONAL",
                                 "mutations": [{"delete": key} for key in keys[start:start + MUTATIONS]]})
        return len(keys)

    def seed(self, dataset):
        """Writes a recording's entities, as its replaying test does."""
        for start in range(0, len(dataset), MUTATIONS):
            self.call("commit", {"mode": "NON_TRANSACTIONAL", "mutations": [
                {"upsert": {"key": e["key"], "properties": e.get("properties", {})}} for e in dataset[start:start + MUTATIONS]]})


def sendable(case):
    """Why a case is not sent, or None: it must be a request of the Datastore API, or of the
    Firestore API to the `(default)` database, of the replay's project, to the default database."""
    path = case.get("path", "")
    if path == "/" or path.startswith(("/emulator/", "/reset")):
        return "the official emulator's own API"
    if re.fullmatch(r"/v1/projects/[^/:]+:\w+", path) and not DATASTORE_PATH.fullmatch(path):
        # The recorders send some requests to projects of their own, to allocate IDs from the
        # start (`{project}-ids1`).
        return "another project's"
    if not DATASTORE_PATH.fullmatch(path) and not FIRESTORE_PATH.match(path):
        return "not the project's Datastore API or (default) database"
    text = path + json.dumps(case.get("body")) + (case.get("raw") or "")
    if any(name != "{project}" for name in PROJECT_NAME.findall(text)):
        return "names another project"
    if isinstance(case.get("body"), dict) and case["body"].get("databaseId", ""):
        return "another database's"
    if "PROJECT('{project}')" in json.dumps(case.get("body")):
        # A key of GQL that names the project names no database: production reads it as one of
        # `(default)`, and GQL has no way to name another.
        return "a key of GQL that names the project"
    return None


def recommended(message):
    """The indexes production recommends in its refusal of a query, as `indexes.py` keeps them:
    `{"kind", "ancestor", "properties": [{"name", "direction"}]}`, the directions `asc` or
    `desc`."""
    indexes = []
    for line in message.splitlines():
        found = INDEX_LINE.fullmatch(line)
        if not found:
            continue
        dash, field, value = found.groups()
        value = json.loads(value) if value.startswith('"') else value
        if field == "kind" and dash:
            indexes.append({"kind": value, "ancestor": False, "properties": []})
        elif not indexes:
            continue
        elif field == "ancestor":
            indexes[-1]["ancestor"] = value == "yes"
        elif field == "name":
            indexes[-1]["properties"].append({"name": value, "direction": "asc"})
        elif field == "direction" and indexes[-1]["properties"]:
            indexes[-1]["properties"][-1]["direction"] = value
    return indexes


def at(value, path):
    """The value at a dotted path (list indexes as numbers), or None."""
    for part in path.split("."):
        if isinstance(value, list) and part.isdigit() and int(part) < len(value):
            value = value[int(part)]
        elif isinstance(value, dict) and part in value:
            value = value[part]
        else:
            return None
    return value


def allocated(official, production, ids):
    """Pairs the IDs in an answer of the official emulator with production's at the same places."""
    if isinstance(official, dict) and isinstance(production, dict):
        for key in official.keys() & production.keys():
            theirs, ours = official[key], production[key]
            if key == "id" and isinstance(theirs, str) and isinstance(ours, str) and theirs != ours:
                ids[theirs] = ours
            else:
                allocated(theirs, ours, ids)
    elif isinstance(official, list) and isinstance(production, list):
        for theirs, ours in zip(official, production):
            allocated(theirs, ours, ids)


def with_database(value, project, database):
    """A request's value with the database named in each `partitionId` that names the project and
    no database: the official emulator reads such a key as one of its default database, production
    as one of `(default)` ("mismatched databases within request"). One that names neither is the
    request's, to both; one that names a database and no project, production refuses."""
    if isinstance(value, dict):
        value = {k: with_database(v, project, database) for k, v in value.items()}
        partition = value.get("partitionId")
        if isinstance(partition, dict) and partition.get("projectId") == project and not partition.get("databaseId"):
            value["partitionId"] = {**partition, "databaseId": database}
        return value
    if isinstance(value, list):
        return [with_database(v, project, database) for v in value]
    return value


def without_database(value, project, database):
    """A value without the `databaseId` production puts in each key's `partitionId`, nor the
    database its messages name in a key."""
    if isinstance(value, dict):
        return {k: without_database(v, project, database) for k, v in value.items()
                if not (k == "databaseId" and v == database)}
    if isinstance(value, list):
        return [without_database(v, project, database) for v in value]
    if isinstance(value, str):
        return value.replace(f'database_id: "{database}"\n', "").replace(f"{project}#{database}", project)
    return value


def error_of(answer):
    """An answer's HTTP status, and its error's status and message: None where it has none."""
    body = answer.get("body")
    if isinstance(body, list) and body and isinstance(body[0], dict):
        body = body[0]
    error = body.get("error") if isinstance(body, dict) else None
    if not isinstance(error, dict):
        return answer["status"], None, None
    return answer["status"], error.get("status"), error.get("message")


def matches(error, status, name, message):
    """Whether an answer's `error_of` matches a side of a rule of `KNOWN_ERRORS`."""
    code, their_name, their_message = error
    return ((status is None or code == status) and (name is None or their_name == name)
            and (message is None or (their_message is not None and re.search(message, their_message) is not None)))


def set_aside(official, production, known):
    """Two answers to compare (`tally`'s: the status, media type and body) with the differences of
    `KNOWN_ERRORS`, `KNOWN_FIELDS` and `KNOWN_VALUES` set aside, and a batch of the official
    emulator's put as production gives it (`batched`); the entries are added to `known`."""
    if same_json(official, production):
        return official, production
    theirs, ours = error_of(official), error_of(production)
    for entry, *rule in KNOWN_ERRORS:
        if matches(theirs, *rule[:3]) and matches(ours, *rule[3:]):
            known.add(entry)
            return official, official
    if official["status"] != production["status"] or "body" not in official:
        return official, production
    theirs, ours = trimmed(official["body"], production["body"], "$", known)
    return {**official, "body": theirs}, {**production, "body": ours}


def trimmed(official, production, path, known):
    """Two JSON values, at `path` of an answer, without the fields and values `KNOWN_FIELDS` and
    `KNOWN_VALUES` record where they differ."""
    if isinstance(official, dict) and isinstance(production, dict):
        official, production = dict(official), dict(production)
        for entry, where, theirs, ours in KNOWN_FIELDS:
            if re.fullmatch(where, path):
                for side, other, own in ((official, production, theirs), (production, official, ours)):
                    for key in (side.keys() - other.keys()) & own:
                        del side[key]
                        known.add(entry)
        if path == "$.batch":
            official = batched(official, production, known)
        for key in official.keys() & production.keys():
            at_key = f"{path}.{key}"
            for entry, where, theirs, ours in KNOWN_VALUES:
                if re.fullmatch(where, at_key) and (official[key], production[key]) == (theirs, ours):
                    production[key] = theirs
                    known.add(entry)
            official[key], production[key] = trimmed(official[key], production[key], at_key, known)
        return official, production
    if isinstance(official, list) and isinstance(production, list) and len(official) == len(production):
        if path in ("$.found", "$.missing") and official != production:
            # Production gives the entities a lookup found, and those it did not, in an order of
            # its own (D120).
            def key(result):
                return json.dumps(result.get("entity", {}).get("key"), sort_keys=True)
            theirs, ours = [key(result) for result in official], [key(result) for result in production]
            place = {k: n for n, k in enumerate(ours)}
            if theirs != ours and sorted(theirs) == sorted(ours) and len(place) == len(ours):
                known.add("D120")
                official = sorted(official, key=lambda result: place[key(result)])
        if path.endswith(".arrayValue.values") and official != production:
            # Production gives an array's values that are indexed first, then those excluded (D121).
            indexed = [v for v in official if not v.get("excludeFromIndexes")] + \
                      [v for v in official if v.get("excludeFromIndexes")]
            if indexed == production:
                known.add("D121")
                official = indexed
        pairs = [trimmed(o, p, f"{path}[{n}]", known) for n, (o, p) in enumerate(zip(official, production))]
        return [o for o, _ in pairs], [p for _, p in pairs]
    return official, production


def batched(official, production, known):
    """A batch of query results of the official emulator's as production gives it (D121): an
    entity once, where the official emulator gives it once for each value of an array its query
    matched; and no more entities than production's batch holds when production has more to
    give (`NOT_FINISHED`), as it does past 300."""
    results, theirs = official.get("entityResults"), production.get("entityResults")
    if not isinstance(results, list) or not isinstance(theirs, list) or len(results) == len(theirs):
        return official
    seen, once = set(), []
    for result in results:
        key = json.dumps(result.get("entity", {}).get("key"), sort_keys=True)
        if key not in seen:
            seen.add(key)
            once.append(result)
    if len(once) < len(results) and len(once) >= len(theirs):
        known.add("D121")
        official = {**official, "entityResults": once}
    if production.get("moreResults") == "NOT_FINISHED" and len(theirs) < len(official["entityResults"]):
        known.add("D121")
        official = {**official, "entityResults": official["entityResults"][:len(theirs)], "moreResults": "NOT_FINISHED"}
    return official


def shown(text, production, database, ids, sequence=False):
    """Production's answer as the recording put the official emulator's: masked, the database
    left out of its keys (or, of the Firestore API, named `(default)`), its IDs read back. In a
    `sequence`, its times are kept, for `timed`."""
    value = parsed(text)
    if not isinstance(value, str):
        text = json.dumps(without_database(value, production.project, database.name), indent=2, ensure_ascii=False)
    text = text.replace(f"databases/{database.name}", "databases/(default)")
    for theirs, ours in ids.items():
        text = text.replace(f'"{ours}"', f'"{theirs}"')
    if sequence:
        return masked(text.replace(production.project, "{project}"))
    return mask(text, production.project)


def replay(production, recording, result):
    """Sends a recording's cases in order to the Datastore-mode database, after writing its
    dataset; compares each answer with the official emulator's. A recording whose recorder sent
    each case to a project of its own after a commit (`seed`, `datastore_commit_checks.json`)
    has that commit before its first case, and again, the database emptied, after each commit
    production did not refuse as a bad request (400): one it refused wrote nothing, nor does a
    read. A case of steps of its own goes as `replay_steps` sends it. Runs of one character are
    expanded (`expanded`). With a shard (`--datastore-shard i/n`), only the cases whose place is
    i modulo n are sent."""
    database = production.datastore
    # Each case with what the recording's `every_case` gives them all (`datastore_gql_errors.json`).
    cases = [{**recording.get("every_case", {}), **case} for case in recording["cases"]]
    if database is None:
        result["skipped"]["no Datastore-mode database (--datastore-database)"] = len(cases)
        return
    if recording.get("generated_by") == LOCKS_RECORDER:
        replay_locks(production, recording, result)
        return
    if "steps" in cases[0] and "seed" not in recording:
        # `datastore_sequences.json`; the sequences of a seeded recording go in `replay_steps`.
        replay_sequences(production, recording, result)
        return
    if recording.get("generated_by") == PROTOBUF_RECORDER:
        replay_protobuf(production, recording, result)
        return
    project = production.project
    seed = recording.get("seed")
    database.empty()
    database.seed(recording.get("dataset", []))
    if seed:
        database.call("commit", seed)
    # What production handed out, by the names the recording gives them, and the IDs it allocated
    # in place of the official emulator's.
    labels, ids = {}, {}
    for n, case in enumerate(cases, 1):
        if n % 50 == 0:
            log(f"{result['fixture']}: case {n}, {production.sent} requests so far")
        shard = production.datastore_shard
        why = "another part's" if shard and (n - 1) % shard[1] != shard[0] else None
        if why is None and "steps" in case:
            why = next((why for step in case["steps"] if (why := sendable(step))), None)
            if why is None:
                replay_steps(production, case, recording.get("every_case", {}), seed, result)
                continue
        why = why or sendable(case)
        if why:
            result["skipped"][why] = result["skipped"].get(why, 0) + 1
            continue
        if "raw" in case:
            text = case["raw"]
        elif "body" in case:
            text = json.dumps(case["body"])
        else:
            text = None
        if text is not None:
            text = expanded(text).replace("{project}", project)
            for label, value in labels.items():
                text = text.replace(f'"<{label}>"', json.dumps(value))
            for theirs, ours in ids.items():
                text = text.replace(f'"id": "{theirs}"', f'"id": "{ours}"')
        headers = {k.lower(): v for k, v in (case.get("headers") or {}).items()}
        content_type = headers.get("content-type", "application/json")
        method = DATASTORE_PATH.fullmatch(case["path"])
        if method:
            body = None if text is None or "raw" in case else parsed(text)
            if isinstance(body, dict):
                status, answer_type, answer = database.send(method.group(1), body, content_type=content_type,
                                                            verb=case["method"])
            else:
                status, answer_type, answer = database.send(method.group(1), raw=None if text is None else text.encode(),
                                                            content_type=content_type, verb=case["method"])
        else:
            path = FIRESTORE_PATH.sub(lambda m: m.group(0).replace("(default)", database.name), case["path"], 1)
            status, answer_type, answer = production.send(case["method"], path.replace("{project}", project),
                                                          None if text is None else text.encode(), content_type)
        result["sent"] += 1
        official = case["outcome"]
        value = parsed(answer)
        if status == 200 and not isinstance(value, str):
            defines = case.get("defines")
            if isinstance(defines, str) and isinstance(value.get("transaction"), str):
                labels[defines] = value["transaction"]
            elif isinstance(defines, dict):
                labels.update({label: found for label, path in defines.items()
                               if isinstance(found := at(value, path), str)})
            if method and method.group(1) in ("allocateIds", "commit") and official["status"] == 200:
                allocated(parsed(official.get("body") or ""), without_database(value, production.project, database.name), ids)
        answered = {"status": status, "contentType": answer_type, "body": shown(answer, production, database, ids)}
        message = str(at(value, "error.message") if not isinstance(value, str) else "")
        indexes = recommended(message)
        # An index of a reserved kind can never be made: that answer is compared (D132).
        if status != 200 and official["status"] == 200 and NEEDS_INDEX.search(message) and not (
                indexes and all(RESERVED_KIND.fullmatch(index["kind"]) for index in indexes)):
            result["needsIndex"].append(case["name"])
            for index in indexes:
                if index not in result.setdefault("datastoreIndexes", []):
                    result["datastoreIndexes"].append(index)
            continue
        theirs = {"status": official["status"], "media": media(official.get("contentType"))}
        ours = {"status": status, "media": media(answer_type)}
        if case.get("compare", "exact") == "exact":
            theirs["body"], ours["body"] = parsed(expanded(official.get("body") or "")), parsed(answered["body"])
        known = set()
        theirs, ours = set_aside(theirs, ours, known)
        tally(result, theirs, ours, lambda: {"name": case["name"], "method": case["method"], "path": case["path"],
                                             "difference": difference(official, answered), "official": official,
                                             "production": answered}, case["name"], known)
        if seed and method and method.group(1) == "commit" and status != 400:
            database.empty()
            database.call("commit", seed)
    database.empty()


def replay_steps(production, case, every, seed, result):
    """Sends a case's steps in turn (`datastore_commit_checks.json`'s sequences), the database
    emptied and the recording's `seed` written before them and after, as the recorder sent them
    to a project of their own: `{tx}` is the transaction the last step to begin one began. Each
    step's answer is compared as in `replay`, as `<case>, step <n>`."""
    database, project = production.datastore, production.project
    database.empty()
    if seed:
        database.call("commit", seed)
    transaction = None
    for i, step in enumerate(case["steps"], 1):
        step = {**every, **step}
        text = expanded(json.dumps(step["body"])).replace("{project}", project)
        if transaction:
            text = text.replace('"{tx}"', json.dumps(transaction))
        verb = DATASTORE_PATH.fullmatch(step["path"]).group(1)
        status, answer_type, answer = database.send(verb, json.loads(text))
        result["sent"] += 1
        value = parsed(answer)
        if verb == "beginTransaction" and status == 200 and isinstance(value, dict):
            transaction = value.get("transaction")
        official = step["outcome"]
        answered = {"status": status, "contentType": answer_type, "body": shown(answer, production, database, {})}
        theirs = {"status": official["status"], "media": media(official.get("contentType"))}
        ours = {"status": status, "media": media(answer_type)}
        if step.get("compare", "exact") == "exact":
            theirs["body"], ours["body"] = parsed(expanded(official.get("body") or "")), parsed(answered["body"])
        known = set()
        theirs, ours = set_aside(theirs, ours, known)
        name = f"{case['name']}, step {i}"
        tally(result, theirs, ours, lambda: {"name": name, "method": step.get("method", "POST"), "path": step["path"],
                                             "difference": difference(official, answered), "official": official,
                                             "production": answered}, name, known)
    database.empty()
    if seed:
        database.call("commit", seed)


def expanded(text):
    """A recording's text with each run of one character it wrote as `{repeat:<c>:<n>}`
    written out."""
    return REPEAT.sub(lambda m: m.group(1) * int(m.group(2)), text)


def varint(data, at):
    """The varint of protobuf `data` at `at`, and where it ends."""
    value = shift = 0
    while True:
        if at >= len(data) or shift > 63:
            raise ValueError("a varint that does not end")
        byte = data[at]
        value, shift, at = value | (byte & 0x7F) << shift, shift + 7, at + 1
        if not byte & 0x80:
            return value, at


def encoded_varint(value):
    out = bytearray()
    while value > 0x7F:
        out.append(value & 0x7F | 0x80)
        value >>= 7
    return bytes(out + bytes([value]))


def fields(data):
    """The fields of a protobuf message, in order, as (number, value): a varint as an int, any
    other as bytes. ValueError if `data` is not one (a group, which proto3 has not, among them)."""
    found, at = [], 0
    while at < len(data):
        tag, at = varint(data, at)
        number, wire = tag >> 3, tag & 7
        if number == 0 or wire not in (0, 1, 2, 5):
            raise ValueError(f"field {number}, wire type {wire}")
        if wire == 0:
            value, at = varint(data, at)
        else:
            size, at = (varint(data, at) if wire == 2 else ({1: 8, 5: 4}[wire], at))
            value, at = data[at:at + size], at + size
            if at > len(data):
                raise ValueError("a field past the end")
        found.append((number, value))
    return found


def key_of(data):
    """A `google.datastore.v1.Key` in protobuf as JSON gives it."""
    key = {}
    for number, value in fields(data):
        if number == 1:
            names = {2: "projectId", 3: "databaseId", 4: "namespaceId"}
            key["partitionId"] = {names[n]: v.decode() for n, v in fields(value) if n in names}
        elif number == 2:
            element = {}
            for n, v in fields(value):
                if n == 1:
                    element["kind"] = v.decode()
                elif n == 2:
                    element["id"] = str(v)
                elif n == 3:
                    element["name"] = v.decode()
            key.setdefault("path", []).append(element)
    return key


def decoded(verb, status, content_type, data):
    """An answer's body as it is compared: one in protobuf as JSON gives it (an error's
    `google.rpc.Status`, with the name of its code; the keys `allocateIds` answers; nothing), any
    other as `{"protobuf": <base64>}`; one of another type parsed as JSON if it is, else as
    text."""
    if media(content_type) != "application/x-protobuf":
        return parsed(data.decode(errors="replace"))
    try:
        if status != 200:
            error = {}
            for number, value in fields(data):
                if number == 1:
                    error.update(code=value, status=CODES.get(value, str(value)))
                elif number == 2:
                    error["message"] = value.decode()
                elif number == 3:
                    error.setdefault("details", []).append(
                        {"@type": next((v.decode() for n, v in fields(value) if n == 1), "")})
            return {"error": error}
        if verb == "allocateIds":
            return {"keys": [key_of(value) for number, value in fields(data) if number == 1]}
        if not data:
            return {}
    except (ValueError, UnicodeDecodeError, KeyError):
        pass
    return {"protobuf": base64.b64encode(data).decode()}


def named(data, content_type, database):
    """A request's body with the database named: a JSON object's `databaseId`, or, sent as
    protobuf, a message's `database_id`. Any other is sent as it is."""
    try:
        value = json.loads(data)
    except ValueError:
        value = None
    if isinstance(value, dict):
        return json.dumps({**value, "databaseId": database}).encode()
    if media(content_type) != "application/x-protobuf":
        return data
    try:
        fields(data)
    except ValueError:
        return data
    name = database.encode()
    return data + encoded_varint(DATABASE_FIELD << 3 | 2) + encoded_varint(len(name)) + name


def replay_protobuf(production, recording, result):
    """Sends the requests of REST in protobuf (`datastore_protobuf.json`) in order, each with its
    own method, path, headers and body, to the Datastore-mode database: the body names the
    database (`named`), and the path the project. Answers are compared as the recorder compared
    them (`compare`: the status and media type, and the body, `decoded`), production's project put
    as the recording's, its database left out of its keys and the IDs it allocates read back as
    the official emulator's. A request without a `Content-Type` is not sent: urllib would send a
    form's."""
    database = production.datastore
    project = production.project
    database.empty()
    ids = {}
    for case in recording["cases"]:
        headers = case.get("headers") or {}
        content_type = next((v for k, v in headers.items() if k.lower() == "content-type"), None)
        if content_type is None:
            why = "without a Content-Type"
            result["skipped"][why] = result["skipped"].get(why, 0) + 1
            continue
        path = case["path"].replace(f"/projects/{PROTOBUF_PROJECT}", f"/projects/{project}", 1)
        verb = re.search(r":(\w+)(?:\?|$)", path)
        verb = verb and verb.group(1)
        data = named(base64.b64decode(case["body"]), content_type, database.name) if case["method"] == "POST" else None
        status, answer_type, answer = database.send_bytes(case["method"], path, data, content_type,
                                                          {k: v for k, v in headers.items() if k.lower() != "content-type"})
        result["sent"] += 1
        official = case["outcome"]
        theirs_body = (None if official.get("body") is None else
                       decoded(verb, official["status"], official.get("contentType"), base64.b64decode(official["body"])))
        ours_body = without_database(decoded(verb, status, answer_type, answer), project, database.name)
        if verb == "allocateIds" and status == 200 == official["status"]:
            allocated(theirs_body, ours_body, ids)
        text = json.dumps(ours_body, indent=2, ensure_ascii=False).replace(project, PROTOBUF_PROJECT)
        for theirs, ours in ids.items():
            text = text.replace(f'"{ours}"', f'"{theirs}"')
        ours_body = json.loads(text)
        shown_official = {"status": official["status"], "contentType": official.get("contentType"),
                          "body": None if theirs_body is None else json.dumps(theirs_body, indent=2, ensure_ascii=False)}
        answered = {"status": status, "contentType": answer_type, "body": mask(text, project)}
        theirs = {"status": official["status"], "media": media(official.get("contentType"))}
        ours = {"status": status, "media": media(answer_type)}
        if case.get("compare", "exact") == "exact":
            theirs["body"], ours["body"] = theirs_body, ours_body
        known = set()
        theirs, ours = set_aside(theirs, ours, known)
        tally(result, theirs, ours, lambda: {"name": case["name"], "method": case["method"],
                                             "path": case["path"].replace(PROTOBUF_PROJECT, "{project}"),
                                             "difference": difference(shown_official, answered),
                                             "official": shown_official, "production": answered}, case["name"], known)
    database.empty()


def replay_locks(production, recording, result):
    """Sends each case of how long a write waits for another transaction's lock
    (`datastore_locks.json`) in turn, to the database emptied before it, where the recorder sent
    them at once, each to a project of its own: its steps in order, `{reader}` and `{writer}` the
    transactions it began, the first and the second. While the write, the last step, waits, the
    reader stays in use, as the recorder kept it: a lookup in it every `keep_alive_seconds`. Each
    answer is compared as in `replay`, and how long the write waited, in whole seconds, with the
    official emulator's, within `WAIT_TOLERANCE`. The case's transactions are rolled back after
    it, so that none holds a lock into the next."""
    database = production.datastore
    every = recording.get("every_case", {})
    keep_alive = recording.get("keep_alive_seconds", 10)
    for case in recording["cases"]:
        database.empty()
        began = []
        steps = [{**every, **step} for step in case["steps"]]
        for i, step in enumerate(steps, 1):
            text = json.dumps(step["body"]).replace("{reader}", began[0] if began else "")
            text = text.replace("{writer}", began[1] if len(began) > 1 else "")
            verb = DATASTORE_PATH.fullmatch(step["path"]).group(1)
            done, keeper = threading.Event(), None
            if i == len(steps) and began:
                def keep(reader=began[0]):
                    while not done.wait(keep_alive):
                        database.send("lookup", {**KEPT_ALIVE_BY, "readOptions": {"transaction": reader}})
                keeper = threading.Thread(target=keep)
                keeper.start()
            start = time.monotonic()
            # The write is not sent again when no answer comes: that would be another write.
            status, answer_type, answer = (database.send(verb, json.loads(text), timeout=LOCK_TIMEOUT, tries=1) if keeper else
                                           database.send(verb, json.loads(text)))
            waited = round(time.monotonic() - start)
            done.set()
            if keeper:
                keeper.join()
            result["sent"] += 1
            value = parsed(answer)
            if verb == "beginTransaction" and status == 200 and isinstance(value, dict):
                began.append(value.get("transaction", ""))
            official = step["outcome"]
            answered = {"status": status, "contentType": answer_type, "body": shown(answer, production, database, {})}
            theirs = {"status": official["status"], "media": media(official.get("contentType"))}
            ours = {"status": status, "media": media(answer_type)}
            if step.get("compare", "exact") == "exact":
                theirs["body"], ours["body"] = parsed(official.get("body") or ""), parsed(answered["body"])
            known = set()
            theirs, ours = set_aside(theirs, ours, known)
            name = f"{case['name']}, step {i}"
            tally(result, theirs, ours, lambda: {"name": name, "method": step["method"], "path": step["path"],
                                                 "difference": difference(official, answered), "official": official,
                                                 "production": answered}, name, known)
        if "waited" in case:
            name = f"{case['name']}, the wait"
            ours = case["waited"] if abs(waited - case["waited"]) <= WAIT_TOLERANCE else waited
            tally(result, {"waited": case["waited"]}, {"waited": ours}, lambda: {
                "name": name, "method": "POST", "path": steps[-1]["path"],
                "difference": f"the write waited {case['waited']} s, production {waited} s",
                "official": {"waited": case["waited"]}, "production": {"waited": waited}}, name)
        for transaction in began:
            database.send("rollback", {"transaction": transaction})
    database.empty()


def micros(text):
    """A timestamp's or a version's microseconds."""
    if text.isdigit():
        return int(text)
    whole, _, fraction = text.rstrip("Z").partition(".")
    moment = datetime.strptime(whole, "%Y-%m-%dT%H:%M:%S").replace(tzinfo=timezone.utc)
    return int(moment.timestamp()) * 1_000_000 + int((fraction + "000000")[:6])


def stamp(microseconds):
    moment = datetime.fromtimestamp(microseconds // 1_000_000, timezone.utc)
    return moment.strftime("%Y-%m-%dT%H:%M:%S") + f".{microseconds % 1_000_000:06d}Z"


def masked(text):
    """An answer's text with its transactions and cursors masked, its times kept for `timed`."""
    text = re.sub(r'"transaction": "[^"]*"', '"transaction": "<id>"', text)
    return CURSOR.sub(lambda m: f'"{m.group(1)}": "<cursor>"', text)


def timed(official, production, times, known, loose=(), path="$"):
    """Production's answer with each of its times put as the label the official emulator's answer
    has in its place, as long as a label stands for one time and a time for one label all along
    the case (`times`, a label's time in production, grows as labels are first met); a time that
    breaks it is put as the label it already has, or as itself. A version the emulator gives in
    seconds (`<seconds>`) is one in production too. At a path of `loose` (a pattern, the entry
    that records why, and whether a time first met there labels it), a time that breaks it is
    taken for the label, and the entry is known."""
    if isinstance(official, dict) and isinstance(production, dict):
        return {k: timed(official[k], v, times, known, loose, f"{path}.{k}") if k in official else v
                for k, v in production.items()}
    if isinstance(official, list) and isinstance(production, list):
        return [timed(o, p, times, known, loose, f"{path}[{n}]") for n, (o, p) in enumerate(zip(official, production))] + \
            production[len(official):]
    if not (isinstance(official, str) and isinstance(production, str) and TIME.fullmatch(production)):
        return production
    if official == "<seconds>":
        return official if production.isdigit() and int(production) < 10 ** 12 else production
    if not LABEL.fullmatch(official):
        return production
    moment = micros(production)
    entry, labels = next(((entry, labels) for where, entry, labels in loose if re.fullmatch(where, path)), (None, True))
    if official not in times and moment not in times.values() and labels:
        times[official] = moment
    if times.get(official) == moment:
        return official
    if entry:
        known.add(entry)
        return official
    return next((label for label, at in times.items() if at == moment), production)


def relabelled(value, times):
    """Production's whole answer, for the report, with the times `timed` labelled put so."""
    labels = {at: label for label, at in times.items()}
    if isinstance(value, dict):
        return {k: relabelled(v, times) for k, v in value.items()}
    if isinstance(value, list):
        return [relabelled(v, times) for v in value]
    if isinstance(value, str) and TIME.fullmatch(value) and micros(value) in labels:
        return labels[micros(value)]
    return value


def step_time(answer, path):
    """The time at a path of an earlier answer of production's, or None. Production answers a
    commit outside a transaction without `commitTime` (D120): its last mutation's time stands in,
    as it is the commit's to the official emulator."""
    found = at(answer, path)
    if found is None and path == "commitTime":
        times = [at(result, "updateTime") or at(result, "version") for result in answer.get("mutationResults", [])]
        found = max((t for t in times if isinstance(t, str)), key=micros, default=None)
    return found if isinstance(found, str) else None


def filled(text, answers, transactions):
    """A step's request with the times and transactions production answered before it."""
    def time_of(m):
        found = step_time(answers[int(m.group(1))], m.group(2))
        return "" if found is None else stamp(micros(found) + int(m.group(3) or 0))
    text = STEP_TRANSACTION.sub(lambda m: transactions.get(m.group(1) or "last", ""), text)
    return STEP_TIME.sub(time_of, text)


def compared(step, official, production, times, known, ids=None):
    """A step's answers to compare (`tally`'s), as `set_aside` and `timed` put them, with what
    production does its own way in a sequence set aside:

    - a lookup reads at the time of the last write before it that changed the data, where the
      official emulator reads at the time it is asked, after it (D120);
    - a lookup gives each missing entity the time it read at as its version, where the emulator
      gives one deleted the version it was deleted at (D120);
    - the deletion of a key that is not there answers, in a transaction, the commit's version,
      and outside one a time of its own, where the emulator answers the version the key was
      deleted at, or a time before the commit's; a transaction that only deletes may commit at
      the time it read at (D120);
    - a query's snapshot version is its read time, where the emulator's is its first result's
      version, or its read time in seconds (D121);
    - `__kind__` and `__namespace__` list the kinds and namespaces of what is stored, where the
      emulator lists those of every key a lookup or a commit named, there or not (D122).

    Results in key order whose keys have IDs production allocated (`ids`, the official
    emulator's to production's) come in the order of production's IDs: the same results in
    another order are taken for the official emulator's."""
    body = step.get("body") if isinstance(step.get("body"), dict) else {}
    theirs, ours = official.get("body"), production.get("body")
    kinds = [k.get("name") for k in (body.get("query") or {}).get("kind", [])]
    if kinds in (["__kind__"], ["__namespace__"]) and isinstance(theirs, dict) and isinstance(ours, dict):
        def key(result):
            return json.dumps(result.get("entity", {}).get("key"), sort_keys=True)
        their_results = (theirs.get("batch") or {}).get("entityResults", [])
        listed = {key(r) for r in (ours.get("batch") or {}).get("entityResults", [])}
        if len(their_results) > len(listed) and listed <= {key(r) for r in their_results}:
            known.add("D122")
            kept = [r for r in their_results if key(r) in listed]
            batch = {k: v for k, v in theirs["batch"].items() if k != "entityResults"}
            official = {**official, "body": {**theirs, "batch": {**batch, **({"entityResults": kept} if kept else {})}}}
    official, production = set_aside(official, production, known)
    theirs, ours = official.get("body"), production.get("body")
    if official["status"] != 200 or not isinstance(theirs, dict) or not isinstance(ours, dict):
        return official, production
    loose = [(r"\$\.readTime", "D120", True), (r"\$\.missing\[\d+\]\.version", "D120", False)]
    loose += [(rf"\$\.mutationResults\[{n}\]\.version", "D120", False)
              for n, mutation in enumerate(body.get("mutations", [])) if "delete" in mutation]
    mutations = body.get("mutations") or []
    if body.get("mode") == "TRANSACTIONAL" and mutations and all("delete" in mutation for mutation in mutations):
        loose.append((r"\$\.commit(?:Version|Time)", "D120", True))
    batch, their_batch = ours.get("batch"), theirs.get("batch")
    if isinstance(batch, dict) and isinstance(their_batch, dict) and all(
            isinstance(batch.get(k), str) and TIME.fullmatch(batch[k]) for k in ("snapshotVersion", "readTime")) \
            and micros(batch["snapshotVersion"]) == micros(batch["readTime"]):
        loose.append((r"\$\.batch\.snapshotVersion", "D121", False))
        if their_batch.get("snapshotVersion") == "<seconds>":
            ours = {**ours, "batch": {**batch, "snapshotVersion": "<seconds>"}}
            known.add("D121")
    ours = timed(theirs, ours, times, known, loose)
    results, their_results = at(ours, "batch.entityResults"), at(theirs, "batch.entityResults")
    if ids and isinstance(results, list) and isinstance(their_results, list) and results != their_results:
        def shown_key(result):
            return json.dumps(result, sort_keys=True)
        moved = [r for r, o in zip(results, their_results) if r != o]
        if sorted(map(shown_key, results)) == sorted(map(shown_key, their_results)) and all(
                any(element.get("id") in ids for element in at(r, "entity.key.path") or []) for r in moved):
            ours = {**ours, "batch": {**ours["batch"], "entityResults": their_results}}
    return official, {**production, "body": ours}


def replay_sequences(production, recording, result):
    """Sends each case of `datastore_sequences.json`, its steps in turn, to the database emptied
    before it, as the recorder sent each to a project of its own; each of production's times is
    put as the label the official emulator's answer has in its place (`timed`). A case with a request to
    another database, or whose client gives up waiting for an answer, is not sent."""
    database = production.datastore
    every = recording.get("every_case", {})
    for n, case in enumerate(recording["cases"], 1):
        if n % 20 == 0:
            log(f"{result['fixture']}: case {n}, {production.sent} requests so far")
        steps = [{**every, **step} for step in case["steps"]]
        why = next((why for step in steps if (why := sendable(step))), None)
        if why is None and any("gives_up_after" in step for step in steps):
            why = "a client that gives up waiting"
        if why:
            result["skipped"][why] = result["skipped"].get(why, 0) + 1
            continue
        database.empty()
        answers, transactions, times, ids = [], {}, {}, {}
        for i, step in enumerate(steps):
            if "after" in step:
                time.sleep(step["after"])
            text = filled(json.dumps(step["body"]).replace("{project}", production.project), answers, transactions)
            for theirs, ours in ids.items():
                text = text.replace(f'"id": "{theirs}"', f'"id": "{ours}"')
            verb = DATASTORE_PATH.fullmatch(step["path"]).group(1)
            status, answer_type, answer = database.send(verb, json.loads(text))
            result["sent"] += 1
            value = parsed(answer)
            value = without_database(value, production.project, database.name) if isinstance(value, dict) else {}
            answers.append(value)
            if verb == "beginTransaction" and isinstance(value.get("transaction"), str):
                transactions[step.get("as", "tx")] = transactions["last"] = value["transaction"]
            official = step["outcome"]
            if status == 200 and verb in ("allocateIds", "commit") and official["status"] == 200:
                allocated(parsed(official["body"]), value, ids)
            text = shown(answer, production, database, ids, sequence=True)
            theirs = {"status": official["status"], "media": media(official.get("contentType")),
                      "body": parsed(official.get("body") or "")}
            ours = {"status": status, "media": media(answer_type), "body": parsed(text)}
            known = set()
            theirs, ours = compared(step, theirs, ours, times, known, ids)
            value = parsed(text)
            answered = {"status": status, "contentType": answer_type, "body": text if isinstance(value, str) else
                        json.dumps(relabelled(value, times), indent=2, ensure_ascii=False)}
            name = f"{case['name']}, step {i}"
            tally(result, theirs, ours, lambda: {"name": name, "method": "POST", "path": step["path"],
                                                 "difference": difference(official, answered), "official": official,
                                                 "production": answered}, name, known)
    database.empty()
