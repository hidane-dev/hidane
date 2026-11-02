"""Replays recordings of the official emulator in Datastore mode against a Datastore-mode database
of production (#81, #82).

`replay.py --datastore-database <id>` hands these recordings here: those of
`tools/oracle/datastore_*.py` that are REST requests sent in order (`datastore_entities.json`,
`datastore_queries.json`, `datastore_gql.json`, `datastore_firestore_api.json`). They go to that
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

import json
import re
import time

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
INDEX_LINE = re.compile(r"\s*(-?)\s*(kind|ancestor|name|direction):\s*(.*?)\s*")

# Differences of Datastore mode that docs/parity-exceptions.md records (D120 to D126), set aside
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
KNOWN_ERRORS = [
    ("D124", 500, "UNKNOWN", None, 400, "INVALID_ARGUMENT", r"^Invalid transaction\.$"),
    ("D124", 400, "INVALID_ARGUMENT", r"^non-transaction commit cannot specify a transaction$",
     400, "INVALID_ARGUMENT", r"^Invalid transaction\.$"),
    ("D124", 409, "ABORTED", TRANSACTION_ENDED, 400, "INVALID_ARGUMENT", TRANSACTION_ENDED),
    ("D124", 200, None, None, 400, "INVALID_ARGUMENT", TRANSACTION_ENDED),
    ("D124", 409, "ABORTED", r"^Transaction lock timeout\.$",
     409, "ABORTED", r"^too much contention on these datastore entities\. please try again\.$"),
    ("D125", 400, "FAILED_PRECONDITION", r"^List of Ids to reserve cannot be empty\.$", 200, None, None),
    ("D125", 400, "INVALID_ARGUMENT", r"^Firestore emulator does not support eventual consistency options\.$",
     200, None, None),
    ("D125", 400, "INVALID_ARGUMENT", r"^order by clause cannot contain duplicate fields ", 200, None, None),
    ("D125", None, None, None, 400, "INVALID_ARGUMENT", r"^The project \S+ does not exist\.$"),
    ("D125", 500, "UNKNOWN", None, 400, "INVALID_ARGUMENT",
     r"^(?:limit is negative|offset is negative|Unable to plan or invalidate query\.)$"),
    ("D125", 400, "INVALID_ARGUMENT", r"^kind is required for all orders except __key__ ascending$",
     400, "INVALID_ARGUMENT", r"^Unable to plan or invalidate query\.$"),
    ("D125", 500, "UNKNOWN", None, 500, "INTERNAL", r"^An internal error occurred\.$"),
    ("D125", 500, "UNKNOWN", None, 200, None, None),
    ("D125", 409, "ALREADY_EXISTS", r"^entity already exists: EntityRef\[",
     409, "ALREADY_EXISTS", r"^entity already exists: app: "),
    ("D126", None, None, None, 400, "FAILED_PRECONDITION", FIRESTORE_API),
]


class Database:
    """A Datastore-mode database of the replay's project; its requests count in the run's budget."""

    def __init__(self, production, name, base=DATASTORE_API):
        self.production = production
        self.name = name
        self.base = base.rstrip("/")

    def send(self, method, body=None, raw=None, content_type="application/json", verb="POST"):
        """A request of the Datastore API (`method`, as `lookup`) to the database: a JSON `body`
        gets the database's `databaseId`, and so does each `partitionId` of the project that names
        none (`with_database`); a `raw` one is sent as it is. Answers (status, content type, body
        as text)."""
        project = self.production.project
        if body is not None:
            raw = json.dumps({**with_database(body, project, self.name), "databaseId": self.name}).encode()
        return self.production.send(verb, f"/v1/projects/{project}:{method}", raw, content_type, base=self.base,
                                    headers={"x-goog-request-params": f"project_id={project}&database_id={self.name}"},
                                    budgeted=True)

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


def shown(text, production, database, ids):
    """Production's answer as the recording put the official emulator's: masked, the database
    left out of its keys (or, of the Firestore API, named `(default)`), its IDs read back."""
    value = parsed(text)
    if not isinstance(value, str):
        text = json.dumps(without_database(value, production.project, database.name), indent=2, ensure_ascii=False)
    text = text.replace(f"databases/{database.name}", "databases/(default)")
    for theirs, ours in ids.items():
        text = text.replace(f'"{ours}"', f'"{theirs}"')
    return mask(text, production.project)


def replay(production, recording, result):
    """Sends a recording's cases in order to the Datastore-mode database, after writing its
    dataset; compares each answer with the official emulator's."""
    database = production.datastore
    cases = recording["cases"]
    if database is None:
        result["skipped"]["no Datastore-mode database (--datastore-database)"] = len(cases)
        return
    project = production.project
    database.empty()
    database.seed(recording.get("dataset", []))
    # What production handed out, by the names the recording gives them, and the IDs it allocated
    # in place of the official emulator's.
    labels, ids = {}, {}
    for n, case in enumerate(cases, 1):
        if n % 50 == 0:
            log(f"{result['fixture']}: case {n}, {production.sent} requests so far")
        why = sendable(case)
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
            text = text.replace("{project}", project)
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
        if status != 200 and official["status"] == 200 and NEEDS_INDEX.search(
                str(at(value, "error.message") if not isinstance(value, str) else "")):
            result["needsIndex"].append(case["name"])
            for index in recommended(at(value, "error.message")):
                if index not in result.setdefault("datastoreIndexes", []):
                    result["datastoreIndexes"].append(index)
            continue
        theirs = {"status": official["status"], "media": media(official.get("contentType"))}
        ours = {"status": status, "media": media(answer_type)}
        if case.get("compare", "exact") == "exact":
            theirs["body"], ours["body"] = parsed(official.get("body") or ""), parsed(answered["body"])
        known = set()
        theirs, ours = set_aside(theirs, ours, known)
        tally(result, theirs, ours, lambda: {"name": case["name"], "method": case["method"], "path": case["path"],
                                             "difference": difference(official, answered), "official": official,
                                             "production": answered}, case["name"], known)
    database.empty()
