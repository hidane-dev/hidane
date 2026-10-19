#!/usr/bin/env python3
"""Replays recordings of the official emulator against production Cloud Firestore (#82).

Usage (from the repository root), with an OAuth access token for a project of the maintainers'
own in `PRODUCTION_ACCESS_TOKEN` (the Production parity workflow gets one through Workload
Identity Federation; no key exists):
    python3 -I -B tools/production/replay.py --project <id> --out production-parity \\
        crates/hidane/tests/fixtures/rest.json crates/hidane/tests/fixtures/conformance.json

Each recording's cases are sent in order, as its replaying test sends them to hidane, to
`https://firestore.googleapis.com` (`--base`), with the token in place of the emulator's
`Bearer owner`. A case is sent only if it is for the project's `(default)` database and names no
other project. Each answer is masked as the recordings are (`tools/oracle/harness.py`) and
compared with the official emulator's recorded answer: the status, the media type, and the body
where the recording compares it, as JSON (production prints JSON its own way), a recording's
placeholders (`<time>`, `<now>` and the like) matching any value, alone or within a string. A
query production refuses for want of a composite index, which the emulators never ask for, is
counted apart, not compared; the report lists the indexes production asked for (`indexes.py`). The official emulator's own
deletes are done in production by deleting the same documents.

Recordings of queries (`queries.json`, `aggregations.json`) hold a dataset and structured
queries: the dataset is written, each query run over REST as its recorder ran it, and the outcome
compared as the recorder put it (the documents in order, what an offset skipped, a projection's
fields, an aggregation's results, or the error), without `done` (D099). The database is emptied before each recording and at the end.

Recordings of Security Rules are tested with the Rules API's `projects.test`: the conditions of
`rules_eval.json` here (`replay_rules`), and how rulesets compile and which documents their
`match` paths cover in `rules.py`.

What differs goes to `<out>/report.json` and `<out>/report.md`, and to the job summary in GitHub
Actions. It is not a recording: it stays a workflow artifact until a person has looked at it
(docs/clean-room/OBSERVATION_POLICY.md). The token is never written anywhere. Stdlib only.
"""

import argparse
import base64
import http.client
import json
import os
import re
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "oracle"))
sys.path.insert(0, str(Path(__file__).resolve().parent))  # `-I` leaves it out
from harness import mask  # noqa: E402
from indexes import index_asked  # noqa: E402

TOKEN_ENV = "PRODUCTION_ACCESS_TOKEN"
RULES_API = "https://firebaserules.googleapis.com"
DATABASE = "/v1/projects/{project}/databases/(default)/"
# A project named anywhere in a case: only the replay's own may be.
PROJECT_NAME = re.compile(r"projects/([^/\s\"'?:]+)")
TRANSIENT = {429, 500, 502, 503, 504}
NETWORK_TRIES = 4
# The documents a query of the replay's own emptying reads at a time.
EMPTYING_PAGE = 1000
# Tests of rules sent at once: the Rules API takes seconds to compile each ruleset. It allows a
# project 300 tests a minute, so they are started at most 240 times a minute.
RULES_AT_ONCE = 8
RULES_PER_MINUTE = 240


class Pace:
    """Starts at most `per_minute` requests a minute, whichever thread sends them."""

    def __init__(self, per_minute):
        self.gap = 60 / per_minute
        self.next = 0.0
        self.lock = threading.Lock()

    def wait(self):
        with self.lock:
            now = time.monotonic()
            start = max(now, self.next)
            self.next = start + self.gap
        time.sleep(start - now)


def log(message):
    print(message, file=sys.stderr, flush=True)


class Production:
    """Requests to one project's `(default)` database, with a bearer token."""

    def __init__(self, base, project, token, budget, rules=RULES_API, quota_project=None):
        self.base = base.rstrip("/")
        self.rules = rules.rstrip("/")
        self.project = project
        self.token = token
        self.budget = budget
        self.quota_project = quota_project
        self.sent = 0
        self.firestore = 0
        self.counting = threading.Lock()

    def send(self, method, path, data=None, content_type="application/json", base=None):
        """One request, to Firestore or another API (`base`); answers (status, content type, body
        as text). A connection that fails before an answer is tried again a few times; then the
        answer is status 0. The budget counts the requests to Firestore, to stay within its free
        daily quota; tests of rules, which `Pace` keeps within the Rules API's, are not in it."""
        with self.counting:
            if base is None:
                if self.firestore >= self.budget:
                    raise RuntimeError(f"stopped at the budget of {self.budget} requests")
                self.firestore += 1
            self.sent += 1
        headers = {"Authorization": f"Bearer {self.token}", "Content-Type": content_type}
        if self.quota_project:
            headers["x-goog-user-project"] = self.quota_project
        req = urllib.request.Request((base or self.base) + path, method=method, data=data, headers=headers)
        for attempt in range(NETWORK_TRIES):
            try:
                with urllib.request.urlopen(req, timeout=60) as res:
                    return res.status, res.headers.get("Content-Type"), res.read().decode(errors="replace")
            except urllib.error.HTTPError as err:
                return err.code, err.headers.get("Content-Type"), err.read().decode(errors="replace")
            except (urllib.error.URLError, http.client.HTTPException, ConnectionError, TimeoutError) as err:
                log(f"{method} {mask(path, self.project)}: {type(err).__name__}, try {attempt + 1} of {NETWORK_TRIES}")
                time.sleep(2 ** attempt)
        return 0, None, ""

    def call(self, method, path, body=None):
        """A request of the replay's own (emptying the database), retried while the service is
        busy; its answer as JSON."""
        data = None if body is None else json.dumps(body).encode()
        for attempt in range(5):
            status, _, text = self.send(method, path, data)
            if status not in TRANSIENT:
                break
            time.sleep(2 ** attempt)
        if status >= 400:
            raise RuntimeError(f"{method} {mask(path, self.project)} answered {status}: {mask(text, self.project)[:300]}")
        return json.loads(text) if text else {}

    def empty(self, under=""):
        """Deletes every document of the database, or the document or collection `under` names
        (a path below the documents) and everything below it. A query of every collection below
        the parent (`allDescendants` without a collection ID) finds them, so no document's name
        goes into a URL: production serves no path for some IDs, control characters among them."""
        documents = f"projects/{self.project}/databases/(default)/documents"
        segments = [s for s in under.split("/") if s]
        # A collection's documents are found from its parent, then told apart by their names.
        parent = "/".join([documents, *(segments[:-1] if len(segments) % 2 else segments)])
        inside = "/".join([documents, *segments]) + "/" if segments else ""
        names, after = [], None
        while True:
            query = {"from": [{"allDescendants": True}], "select": {"fields": [{"fieldPath": "__name__"}]},
                     "orderBy": [{"field": {"fieldPath": "__name__"}}], "limit": EMPTYING_PAGE}
            if after:
                query["startAt"] = {"values": [{"referenceValue": after}], "before": False}
            answer = self.call("POST", f"/v1/{urllib.parse.quote(parent, safe='/()')}:runQuery", {"structuredQuery": query})
            found = [r["document"]["name"] for r in answer if "document" in r]
            names += [name for name in found if name.startswith(inside)]
            if len(found) < EMPTYING_PAGE:
                break
            after = found[-1]
        if segments and not len(segments) % 2:
            names.append(parent)
        log(f"emptying {'the database' if not segments else '/'.join(segments)}: {len(names)} documents")
        for start in range(0, len(names), 500):
            writes = [{"delete": name} for name in names[start:start + 500]]
            self.call("POST", f"/v1/{documents}:commit", {"writes": writes})
        return len(names)


# The official emulator's own delete: of the database's documents, or of a document or a
# collection and everything below it. It trims whitespace around each segment of the path.
EMULATOR_DELETE = re.compile(r"/emulator/v1/projects/([^/]*)/databases/([^/]*)/documents((?:/[^/]*)*)")


def emulator_delete(case):
    """What an emulator's own delete clears, as a path below the documents (`""` for all of them),
    or None if the case is not one for the project's `(default)` database."""
    found = EMULATOR_DELETE.fullmatch(case.get("path", "").split("?")[0])
    if case.get("method") != "DELETE" or not found:
        return None
    project, database = (urllib.parse.unquote(part).strip() for part in found.group(1, 2))
    if project != "{project}" or database != "(default)":
        return None
    return "/".join(urllib.parse.unquote(s).strip() for s in found.group(3).split("/") if s)


def sendable(case):
    """Why a case is not sent, or None: it must be for the project's `(default)` database, as the
    emulator's owner, and name no other project."""
    if not case.get("path", "").startswith(DATABASE):
        return "not the (default) database's"
    headers = {k.lower(): v for k, v in (case.get("headers") or {}).items()}
    if headers.get("authorization") != "Bearer owner":
        return "not sent as the owner"
    text = case["path"] + json.dumps(case.get("body")) + (case.get("raw") or "")
    if any(name != "{project}" for name in PROJECT_NAME.findall(text)):
        return "names another project"
    return None


# What a recording put in place of a value that changes from run to run: any value matches it,
# alone or within a string ("the stored version (<version>) does not match ...").
PLACEHOLDERS = {"<time>", "<now>", "<id>", "<token>", "<auto>", "<version>", "<cursor>"}
PLACEHOLDER = re.compile("(" + "|".join(re.escape(p) for p in sorted(PLACEHOLDERS)) + ")")
# Production's refusal of a query that needs an index: a composite one, one of a single field for
# a collection group (`The query requires a COLLECTION_GROUP_ASC index …`), or a vector index.
# The emulators never ask for one (D070). The link to create the index names the project in
# base64, so it is masked.
NEEDS_INDEX = re.compile(r"The query requires (?:an|a \w+) index|Missing vector index configuration")
INDEX_LINK = re.compile(r'https://console\.firebase\.google\.com/[^"\s]*')


# An error about a resource name says where in the name it went wrong, counting the characters of
# the project ID, which is not the recording's: `Document name "…" lacks "/" at index 53.`
NAME_INDEX = re.compile(r"(?<=\bat index )\d+")


def same_string(official, production):
    """Whether two strings are the same, a placeholder in the official one matching any text."""
    official, production = (NAME_INDEX.sub("<index>", s) for s in (official, production))
    parts = PLACEHOLDER.split(official)
    if len(parts) == 1:
        return official == production
    pattern = "".join(".+?" if n % 2 else re.escape(part) for n, part in enumerate(parts))
    return re.fullmatch(pattern, production, re.S) is not None


def number(value):
    return isinstance(value, (int, float)) and not isinstance(value, bool)


# Differences docs/parity-exceptions.md records, told apart from new ones: a message of the
# official emulator and one of production that match a pair, and the shapes of an answer that
# D097 (`details` in an error), D098 (an error inside an array) and D099 (no `done`) describe.
KNOWN_MESSAGES = [
    ("D097", r"^Document \(.*\) not found\.$", r'^Document ".*" not found\.$'),
    ("D097", r"^Payload isn't valid for request\.$", r"^Invalid (?:JSON payload received\.|value at )"),
    ("D097", r"^Transaction is invalid or expired\.$", r"^Invalid transaction\.$"),
    ("D097", r"^Input must be int64 or double\.$", r"^Input must be a number\.$"),
    ("D064", r"^entity already exists: ", r"^Document already exists: "),
    ("D064", r"^no entity to update: ", r"^No document to update: "),
    ("D106", r"^Only a single array-contains clause is allowed in a query$",
     r"^A maximum of 1 'ARRAY_CONTAINS' filter is allowed per disjunction\.$"),
    ("D106", r"requires an index that has fields \[.*\] after __name__", r"requires an index with fields \[.*\] after __name__"),
    ("D075", r"^maximum entity size is 1048576 bytes$",
     r"^Document '.*' cannot be written because its size \([\d,]+ bytes\) exceeds the maximum allowed size of 1,048,576 bytes\.$"),
]


# Errors whose status differs too: (entry, the official emulator's status and message, production's);
# None matches any answer of that side.
KNOWN_ERRORS = [
    ("D074", "FAILED_PRECONDITION", r"^Firestore does not support descending key scans$", None, None),
    ("D066", "INVALID_ARGUMENT", r"^Only timestamps past epoch are supported\.$", None, None),
    ("D110", "INVALID_ARGUMENT", r"^Invalid base version, it is greater than the stored version: app: ",
     "INVALID_ARGUMENT", r"^Invalid base version, it is greater than the stored version: projects/"),
    ("D111", None, None, "NOT_FOUND", r"^The database \S+ does not exist for project "),
    ("D107", "INVALID_ARGUMENT", r"^The query may not have more than 100 filters", None, None),
    ("D107", None, None, "INVALID_ARGUMENT", r"^Too many disjunctions after normalization"),
    # A query with several faults: the emulator reports its orders or filters, production what it holds.
    ("D105", None, r"^(?:order by clause cannot contain (?:duplicate fields|more fields after the key)|Equality on key is "
                   r"not allowed|This query requires an index that has fields|the name __key__ is reserved|kind is "
                   r"required for filter|Too many disjunctions after normalization)",
     None, r"^(?:limit is negative|__key__ filter value must be a Key|the name __key__ is reserved|Invalid property "
           r"path|Key path is empty\.|kind is required|order by clause cannot contain duplicate fields|This query "
           r"requires an index with fields)"),
    ("D106", "FAILED_PRECONDITION", r"^Only a single array-contains clause is allowed in a query$",
     "INVALID_ARGUMENT", r"^A maximum of 1 'ARRAY_CONTAINS' filter is allowed per disjunction\.$"),
    ("D097", "FAILED_PRECONDITION", r"^The requested 'read_time' is too old\.$",
     "INVALID_ARGUMENT", r"^The requested 'read_time' cannot be before database creation time\.$"),
    ("D108", "INVALID_ARGUMENT", r"^kind is required for all orders except __key__ ascending$",
     "INVALID_ARGUMENT", r"^kind is required for filter: "),
    ("D108", "INVALID_ARGUMENT", r"^kind is required for all orders except __key__ ascending$",
     "INTERNAL", r"^An internal error occurred\.$"),
    # Production refuses a read time over an hour old (D065), or before the database was created.
    ("D065", None, None, "FAILED_PRECONDITION", r"^The requested 'read_time' is too old\.$"),
    ("D097", None, None, "INVALID_ARGUMENT", r"^The requested 'read_time' cannot be before database creation time\.$"),
]
# Cases about a difference an entry records, by recording and name: what they answer differently
# is that difference.
KNOWN_CASES = {
    "rest.json": [
        ("D100", r"^JsonFormat: |lenient JSON|a null body|a body that does not read|unknown query parameter|"
                 r"numbers and booleans as strings|a number as "),
        ("D101", r"whitespace|path ending with a slash|^list it again$"),
        ("D102", r"__key__|__type__"),
        ("D103", r"is not REST|unknown verb|PUT is not a method|the database root|GET with a verb"),
        ("D104", r"Emulator UI|a page of all five"),
        ("D099", r"skippedResults"),
        ("D076", r"^partitionQuery$"),
        ("D074", r"__name__%20desc"),
        ("D112", r"^batchGet with documents named twice$"),
    ],
    "queries.json": [("D103", r"^parent is a collection$")],
    "document_writes.json": [("D112", r"^batch get order$"), ("D111", r"^named database$")],
    "transforms.json": [("D102", r"__name__|__type__")],
    "list_documents.json": [("D101", r"slash"), ("D103", r"^root without a slash$"),
                            ("D089", r"^collection, order by (?:a field|the name), pages of 2$")],
    "read_time_and_databases.json": [("D112", r"^REST :batchGet, (?:60|3540) s old$")],
    "unsupported.json": [("D079", r"explain"), ("D078", r"(?i)executePipeline"), ("D076", r"(?i)partitionQuery")],
    "json_escapes.json": [("D116", r"^get the document with the characters in its ID$|^an error that repeats a document name$")],
    "find_nearest.json": [("D080", r"^cosine, (?:a zero vector last|threshold)$|^a query vector with NaN$"),
                          ("D074", r"^name descending$")],
    "base_versions.json": [("D066", r"^REST patch"), ("D109", r"version 0, deleted document|the deleted version"),
                           ("D110", r"past the stored version|then a document that is missing"),
                           ("D111", r"named database")],
}


def known_error(official, production, known):
    """Whether two answers are a pair `KNOWN_ERRORS` records: a status (an error's name, where an
    answer has one) and a message for each side, or for one."""
    def matches(answer, status, message):
        name = answer.get("error") if isinstance(answer.get("error"), str) else answer.get("status")
        return (status is None or name == status) and (
            message is None or re.search(message, str(answer.get("message", ""))) is not None)
    for entry, their_status, theirs, our_status, ours in KNOWN_ERRORS:
        if matches(official, their_status, theirs) and matches(production, our_status, ours):
            known.add(entry)
            return True
    return False


def known_message(official, production, known):
    """Whether two messages are a pair `KNOWN_MESSAGES` records; adds its entry to `known`."""
    for entry, theirs, ours in KNOWN_MESSAGES:
        if re.search(theirs, official) and re.search(ours, production):
            known.add(entry)
            return True
    return False


def same_json(official, production, known=None):
    """Whether two JSON values are the same, a recording's placeholder matching anything.
    Numbers compare by value: production prints a double without `.0` (`1`), the official
    emulator as Java does (`1.0`). With a set for `known`, the differences the parity exceptions
    record count as the same, and their entries are added to it."""
    if isinstance(official, str):
        return isinstance(production, str) and (
            same_string(official, production) or (known is not None and known_message(official, production, known)))
    if number(official) and number(production):
        return official == production
    if isinstance(official, dict):
        if (known is not None and "error" in official and isinstance(production, list) and len(production) == 1
                and isinstance(production[0], dict) and "error" in production[0]):
            known.add("D098")
            return same_json(official, production[0], known)
        if not isinstance(production, dict):
            return False
        if known is not None and ("message" in official or "message" in production) and \
                not same_json(official, production) and known_error(official, production, known):
            return True
        theirs, ours = official.keys() - production.keys(), production.keys() - official.keys()
        if theirs or ours:
            if known is None or not (theirs <= {"done"} and ours <= {"details", "skipped"}):
                return False
            known.update(["D099"] if theirs or "skipped" in ours else [])
            known.update(["D097"] if "details" in ours else [])
        return all(same_json(official[k], production[k], known) for k in official.keys() & production.keys())
    if isinstance(official, list):
        return (isinstance(production, list) and len(official) == len(production)
                and all(same_json(o, p, known) for o, p in zip(official, production)))
    return official == production and type(official) is type(production)


def parsed(text):
    """A body as JSON if it is, else as text."""
    try:
        return json.loads(text)
    except ValueError:
        return text


def same_body(official, production):
    """Two answers' bodies: as JSON when both are (production prints it its own way), else as
    text."""
    return same_json(parsed(official), parsed(production))


def first_difference(official, production, at="$"):
    """Where two JSON values first differ in a way the parity exceptions do not record, and how,
    or None."""
    if same_json(official, production, set()):
        return None
    if isinstance(official, dict) and "error" in official and isinstance(production, list) and len(production) == 1:
        return first_difference(official, production[0], at + "[0]")
    if isinstance(official, dict) and isinstance(production, dict):
        theirs, ours = official.keys() - production.keys() - {"done"}, production.keys() - official.keys() - {"details", "skipped"}
        if theirs or ours:
            return f"{at}: keys only the official has {sorted(theirs)}, only production has {sorted(ours)}"
        return next((d for k in official if k in production
                     and (d := first_difference(official[k], production[k], f"{at}.{k}"))), None)
    if isinstance(official, list) and isinstance(production, list):
        if len(official) != len(production):
            return f"{at}: {len(official)} items, production {len(production)}"
        return next((d for i, (o, p) in enumerate(zip(official, production))
                     if (d := first_difference(o, p, f"{at}[{i}]"))), None)
    return f"{at}: {json.dumps(official, ensure_ascii=False)[:80]}, production {json.dumps(production, ensure_ascii=False)[:80]}"


def tally(result, official, answered, entry, name=None):
    """Counts a case: the same; the same but for differences the parity exceptions record (by
    entry, or by the case's name, `KNOWN_CASES`); or different, with `entry()` saying how."""
    known = set()
    if not same_json(official, answered, known):
        about = [k for k, pattern in KNOWN_CASES.get(result.get("fixture"), []) if name and re.search(pattern, name)]
        if about:
            result["knownCases"] += 1
            result["known"][about[0]] = result["known"].get(about[0], 0) + 1
            return
        result["differ"].append(entry())
    elif known:
        result["knownCases"] += 1
        for k in known:
            result["known"][k] = result["known"].get(k, 0) + 1
    else:
        result["same"] += 1


def difference(official, answered):
    """How an answer differs from the official emulator's, in a line."""
    if official["status"] != answered["status"]:
        return f"status {official['status']}, production {answered['status']}"
    if media(official.get("contentType")) != media(answered.get("contentType")):
        return f"media type {media(official.get('contentType'))}, production {media(answered.get('contentType'))}"
    return first_difference(parsed(official.get("body") or ""), parsed(answered["body"])) or "the body"


def media(content_type):
    return (content_type or "").split(";")[0].strip().lower() or None


def asked(result, text):
    """Keeps the index an answer asks for, if any, in the recording's result (`indexes.py`)."""
    index = index_asked(text) if NEEDS_INDEX.search(text) else None
    if index and index not in result["indexes"]:
        result["indexes"].append(index)


def official_outcome(outcome, aggregation):
    """A recorded outcome of a query (`tools/oracle/queries.py`, `aggregations.py`), as it is
    compared: the error; or the documents in order, the number an offset skipped and, with a
    projection, their fields; or an aggregation's results. `done` is left out: production does
    not send it (D099)."""
    if outcome["status"] != "OK":
        return {"status": outcome["status"], "message": outcome.get("message", "")}
    responses = outcome["responses"]
    if aggregation:
        return {"status": "OK", "results": [r["result"] for r in responses if "result" in r]}
    compared = {"status": "OK", "documents": [r["document"] for r in responses if "document" in r]}
    skipped = max((r.get("skipped", 0) for r in responses), default=0)
    if skipped:
        compared["skipped"] = skipped
    if "fields" in outcome:
        compared["fields"] = outcome["fields"]
    return compared


def production_outcome(status, text, documents, aggregation, projection):
    """Production's answer to a query, as `official_outcome` puts the official emulator's.
    Production sends an error inside an array (D098), and the number an offset skipped once, in
    a result of its own (D099)."""
    try:
        answer = json.loads(text)
    except ValueError:
        return {"status": str(status), "message": text[:300]}
    results = answer if isinstance(answer, list) else [answer]
    errors = [r["error"] for r in results if isinstance(r, dict) and "error" in r]
    if status != 200 or errors:
        error = errors[0] if errors else {}
        return {"status": error.get("status", str(status)), "message": error.get("message", "")}
    if aggregation:
        return {"status": "OK", "results": [r["result"] for r in results if "result" in r]}
    compared, fields, skipped = {"status": "OK", "documents": []}, {}, 0
    for r in results:
        if "document" in r:
            name = r["document"]["name"][len(documents) + 1:]
            compared["documents"].append(name)
            fields[name] = json.loads(json.dumps(r["document"].get("fields", {})).replace(documents, "{documents}"))
        skipped = max(skipped, r.get("skippedResults", 0))
    if skipped:
        compared["skipped"] = skipped
    if projection:
        compared["fields"] = fields
    return compared


def replay_queries(production, recording, result):
    """A recording of queries or aggregations: its dataset is written, then each case is run over
    REST, as its recorder ran it, and compared as `official_outcome` puts it."""
    project = production.project
    database = f"projects/{project}/databases/(default)"
    documents = f"{database}/documents"

    def fill(value):
        return json.loads(json.dumps(value).replace("{documents}", documents).replace("{database}", database))

    writes = [{"update": {"name": f"{documents}/{doc['name']}", "fields": fill(doc["fields"])}}
              for doc in recording["dataset"]]
    for start in range(0, len(writes), 500):
        production.call("POST", f"/v1/{documents}:commit", {"writes": writes[start:start + 500]})
    for n, case in enumerate(recording["cases"], 1):
        if n % 100 == 0:
            log(f"{result['fixture']}: case {n}, {production.sent} requests so far")
        sent = {k: case[k] for k in ("parent", "query", "aggregations") if k in case}
        if any(name != "{project}" for name in PROJECT_NAME.findall(json.dumps(sent))):
            result["skipped"]["names another project"] = result["skipped"].get("names another project", 0) + 1
            continue
        aggregation = "aggregations" in case
        parent = documents + ("/" + case["parent"] if case["parent"] else "")
        if aggregation:
            verb, body = "runAggregationQuery", {"structuredAggregationQuery": {
                "structuredQuery": fill(case["query"]), "aggregations": fill(case["aggregations"])}}
        else:
            verb, body = "runQuery", {"structuredQuery": fill(case["query"])}
        status, _, text = production.send("POST", f"/v1/{urllib.parse.quote(parent, safe='/()')}:{verb}",
                                          json.dumps(body).encode())
        result["sent"] += 1
        official = official_outcome(case["outcome"], aggregation)
        asked(result, text)
        if NEEDS_INDEX.search(text) and official["status"] == "OK":
            result["needsIndex"].append(case["name"])
            continue
        answered = production_outcome(status, INDEX_LINK.sub("<index link>", mask(text, project)),
                                      f"projects/{{project}}/databases/(default)/documents", aggregation,
                                      "select" in case["query"])
        where = "/" + case["parent"] if case["parent"] else ""
        tally(result, official, answered, lambda: {
            "name": case["name"], "method": "POST",
            "path": f"/v1/projects/{{project}}/databases/(default)/documents{where}:{verb}",
            "difference": first_difference(official, answered) or "the outcome", "official": official,
            "production": answered}, case["name"])


def limit_text(spec):
    """A document ID or a collection ID of `limits.json`: text, or `["repeat", char, n]`."""
    return spec if isinstance(spec, str) else spec[1] * spec[2]


def limit_value(spec):
    """A value of `limits.json`, from its spec, as `tools/oracle/limits.py` writes it:
    `["string", n, char]`, `["bytes", n]`, `["int"]`, `["map", {name: spec}]`, `["array", [spec]]`,
    or `["nest", levels, mixed]` (maps, or maps and arrays in turn, `levels` deep)."""
    kind = spec[0]
    if kind == "string":
        return {"stringValue": spec[2] * spec[1]}
    if kind == "bytes":
        return {"bytesValue": base64.b64encode(b"x" * spec[1]).decode()}
    if kind == "int":
        return {"integerValue": "1"}
    if kind == "map":
        return {"mapValue": {"fields": {k: limit_value(v) for k, v in spec[1].items()}}}
    if kind == "array":
        return {"arrayValue": {"values": [limit_value(v) for v in spec[1]]}}
    if kind == "nest":
        value = {"integerValue": "1"}
        for level in range(spec[1]):
            value = ({"arrayValue": {"values": [value]}} if spec[2] and level % 2 == 0
                     else {"mapValue": {"fields": {"m": value}}})
        return value
    raise ValueError(f"a value spec of kind {kind}")


def replay_limits(production, recording, result):
    """A recording of the limits of a document (`limits.json`): each case's document is committed
    as its recorder committed it, and the status and the error's message compared."""
    documents = f"projects/{production.project}/databases/(default)/documents"
    for case in recording["cases"]:
        name = f"{documents}/" + "/".join(limit_text(s) for s in case["path"])
        fields = {k: limit_value(v) for k, v in case["fields"].items()}
        body = {"writes": [{"update": {"name": name, "fields": fields}}]}
        status, _, text = production.send("POST", f"/v1/{documents}:commit", json.dumps(body).encode())
        result["sent"] += 1
        try:
            message = json.loads(text)["error"]["message"] if status != 200 else ""
        except (ValueError, KeyError, TypeError):
            message = text[:300]
        answered = {"status": status, "message": mask(message, production.project)}
        tally(result, case["outcome"], answered, lambda: {
            "name": case["name"], "method": "POST", "path": "/v1/projects/{project}/databases/(default)/documents:commit",
            "difference": first_difference(case["outcome"], answered) or "the outcome", "official": case["outcome"],
            "production": answered}, case["name"])


def replay_value_order(production, recording, result):
    """The recording of how values and document names are ordered (`value_order.json`), redone as
    its recorder (`tools/oracle/value_order.py`) did it: each value is written as the field `v` of
    a document in two collections, named in the values' order in one and backwards in the other,
    and both are read back ordered by `v`; neighbours that swap places between the two are equal.
    Each document name is written, then read back with a collection group query. The groups of
    equal values, the names' order, and what was refused are compared."""
    documents = f"projects/{production.project}/databases/(default)/documents"

    def write(name, fields):
        """Writes a document; answers the error's message if it is refused, else None."""
        body = {"writes": [{"update": {"name": f"{documents}/{name}", "fields": fields}}]}
        status, _, text = production.send("POST", f"/v1/{documents}:commit", json.dumps(body).encode())
        if status == 200:
            return None
        try:
            return mask(json.loads(text)["error"]["message"], production.project)
        except (ValueError, KeyError, TypeError):
            return f"{status} {mask(text, production.project)[:300]}"

    def names(query):
        """The names of the documents a query finds, below the documents; or the error's message."""
        status, _, text = production.send("POST", f"/v1/{documents}:runQuery",
                                          json.dumps({"structuredQuery": query}).encode())
        answer = parsed(text)
        if status != 200 or not isinstance(answer, list):
            errors = [r["error"] for r in (answer if isinstance(answer, list) else [answer])
                      if isinstance(r, dict) and isinstance(r.get("error"), dict)]
            return mask(errors[0].get("message", "") if errors else f"{status} {text[:300]}", production.project)
        return [r["document"]["name"][len(documents) + 1:] for r in answer if "document" in r]

    def compare(name, official, answered):
        result["sent"] += 1
        tally(result, official, answered, lambda: {
            "name": name, "method": "POST", "path": "/v1/projects/{project}/databases/(default)/documents:runQuery",
            "difference": first_difference(official, answered) or "the outcome", "official": official,
            "production": answered}, name)

    recorded = recording["values"]
    values = [item for group in recorded["groups"] for item in group] + recorded["rejected"]
    count, refused = len(values), []
    for i, item in enumerate(values):
        error = write(f"orderA/{i:03d}", {"v": item["value"]})
        if error is None:
            write(f"orderB/{count - 1 - i:03d}", {"v": item["value"]})
        else:
            refused.append({"label": item["label"], "error": error})
    by_v = {"orderBy": [{"field": {"fieldPath": "v"}, "direction": "ASCENDING"}]}
    found_a = names({"from": [{"collectionId": "orderA"}], **by_v})
    found_b = names({"from": [{"collectionId": "orderB"}], **by_v})
    if isinstance(found_a, str) or isinstance(found_b, str):
        groups = found_a if isinstance(found_a, str) else found_b
    else:
        a = [int(name.split("/")[1]) for name in found_a]
        place = {count - 1 - int(name.split("/")[1]): at for at, name in enumerate(found_b)}
        groups = [[a[0]]] if a else []
        for before, case in zip(a, a[1:]):
            if place.get(case, -1) < place.get(before, -1):
                groups[-1].append(case)
            else:
                groups.append([case])
        groups = [[values[case]["label"] for case in group] for group in groups]
    compare("values: groups of equal values, in order", [[item["label"] for item in group] for group in recorded["groups"]],
            groups)
    compare("values: refused", [{"label": r["label"], "error": r["error"]} for r in recorded["rejected"]], refused)

    paths, refused = recording["paths"], []
    for path in paths["order"] + [r["path"] for r in paths["rejected"]]:
        error = write(path, {"p": {"stringValue": path}})
        if error is not None:
            refused.append({"path": path, "error": error})
    compare("names: in order", paths["order"], names({"from": [{"collectionId": "items", "allDescendants": True}]}))
    compare("names: refused", paths["rejected"], refused)


# The project `tools/oracle/list_documents.py` recorded in; its answers name it.
LIST_PROJECT = "list"
# ListDocuments' fields, as the gRPC requests of `list_documents.json` name them, and as REST takes
# them in its query.
LIST_PARAMETERS = {"page_size": "pageSize", "order_by": "orderBy", "show_missing": "showMissing",
                   "read_time": "readTime", "page_token": "pageToken"}


def replay_list_documents(production, recording, result):
    """The recording of ListDocuments (`list_documents.json`): its seed is written in batches, as
    its recorder (`tools/oracle/list_documents.py`) wrote it. Each gRPC request is sent as its
    REST form, `GET …/documents/{parent}/{collection ID}` with the other fields in the query; a
    request read at the first commit reads at production's. One without a collection ID, which
    lists every collection, is not: REST has no form of it (`…/documents/` is no path production
    serves). Each REST path is sent as it is. The documents listed, page by page where the
    recorder followed the pages, or the error, are compared as the recorder put them."""
    documents = f"projects/{production.project}/databases/(default)/documents"
    commits = []
    for batch in recording["seed"]:
        writes = [{"update": {"name": f"{documents}/{path}", "fields": {
            "n": {"integerValue": str(i)}, "m": {"booleanValue": True}}}} for i, path in enumerate(batch)]
        commits.append(production.call("POST", f"/v1/{documents}:commit", {"writes": writes})["commitTime"])

    def get(path):
        status, _, text = production.send("GET", f"/v1/{documents}{path}")
        return status, parsed(text)

    def masked(value):
        return json.loads(mask(json.dumps(value), production.project))

    def listed(request, parent):
        """One gRPC request's answer, as the recorder put it: the documents and the next page's
        token, or the error's code and message."""
        query = [(LIST_PARAMETERS[k], json.dumps(v) if isinstance(v, bool) else str(v))
                 for k, v in request.items() if k in LIST_PARAMETERS]
        query += [("mask.fieldPaths", path) for path in request.get("mask", {}).get("field_paths", [])]
        path = (f"/{parent}" if parent else "") + "/" + urllib.parse.quote(request.get("collection_id", ""), safe="")
        status, answer = get(path + ("?" + urllib.parse.urlencode(query) if query else ""))
        if isinstance(answer, dict) and "error" in answer:
            asked(result, json.dumps(answer))
        if status != 200 or not isinstance(answer, dict):
            error = answer.get("error", {}) if isinstance(answer, dict) else {}
            return masked({"status": error.get("status", str(status)), "message": error.get("message", str(answer)[:300])})
        return {"documents": [{"name": d["name"][len(documents) + 1:], "fields": sorted(d.get("fields", {}))}
                              for d in answer.get("documents", [])],
                "next_page_token": answer.get("nextPageToken", "")}

    def compare(name, path, official, answered):
        result["sent"] += 1
        official = json.loads(json.dumps(official).replace(f"projects/{LIST_PROJECT}/", "projects/{project}/"))
        tally(result, official, answered, lambda: {
            "name": name, "method": "GET", "path": path, "difference": first_difference(official, answered) or "the outcome",
            "official": official, "production": answered}, name)

    for case in recording["grpc"]:
        why = ("not sent as the owner" if not case["owner"] else
               "lists every collection, which REST cannot ask" if not case["request"].get("collection_id") else None)
        if why:
            result["skipped"][why] = result["skipped"].get(why, 0) + 1
            continue
        request = dict(case["request"])
        pages = request.pop("pages", False)
        if request.pop("at_first_commit", False):
            request["read_time"] = commits[0]
        if not pages:
            answered = listed(request, case["parent"])
            answered.pop("next_page_token", None)
        else:
            answered, found = {}, []
            while True:
                answer = listed(request, case["parent"])
                if "status" in answer:
                    answered = {"pages": found, "error": answer} if found else answer
                    break
                found.append([d["name"] for d in answer["documents"]])
                if not answer["next_page_token"]:
                    answered = {"pages": found}
                    break
                request["page_token"] = answer["next_page_token"]
        # The recorder named a gRPC error's status `code`.
        official = json.loads(json.dumps(case["outcome"]).replace('{"code": ', '{"status": '))
        if NEEDS_INDEX.search(answered.get("message", "")) and "status" not in official:
            result["sent"] += 1
            result["needsIndex"].append(case["name"])
            continue
        answered = json.loads(INDEX_LINK.sub("<index link>", json.dumps(answered)))
        compare(case["name"], f"/v1/projects/{{project}}/databases/(default)/documents/{case['parent']} ListDocuments",
                official, answered)

    for case in recording["rest"]:
        status, answer = get(case["path"])
        if not isinstance(answer, dict):
            answered = {"status": status, "body": masked(answer)}
        elif "documents" in answer or status == 200:
            answered = {"status": status, "next_page": bool(answer.get("nextPageToken")),
                        "documents": [d["name"][len(documents) + 1:] for d in answer.get("documents", [])]}
        else:
            answered = {"status": status, "error": masked(answer.get("error", answer))}
        compare(case["name"], f"/v1/projects/{{project}}/databases/(default)/documents{case['path']}", case["outcome"], answered)


# The project `tools/oracle/read_time_and_databases.py` recorded its reads in, and the collection
# they read.
READ_TIME_PROJECT = "read-time"
READ_TIME_COLLECTION = "c"


def read_answer(status, answer):
    """A REST read's answer as `replay_read_times` compares it: its status, and the error's
    status and message, the first error of a stream's included, or the body."""
    errors = [r["error"] for r in (answer if isinstance(answer, list) else [answer])
              if isinstance(r, dict) and isinstance(r.get("error"), dict)]
    if errors:
        return {"status": status, "error": errors[0].get("status"), "message": errors[0].get("message", "")}
    return {"status": status, "body": answer}


def replay_read_times(production, recording, result):
    """The recording of how old a read time may be (`read_time_and_databases.json`'s `read_time`):
    a document is written, then each read is made at each age before now, as its recorder made
    them. A gRPC read is sent as its REST form: GetDocument and ListDocuments as a `GET` with
    `readTime` in the query, RunQuery as `:runQuery`. Its status and message, or the documents it
    found, are compared; a REST read's status and answer (`read_answer`). The recording's named
    databases are not: they are not the project's `(default)`.

    Production keeps the database's history for an hour, the replays' before this one included,
    so the reads go to a collection of the run's own, read back as the recording's, and the
    collection IDs listed are those of it alone."""
    database = f"projects/{production.project}/databases/(default)"
    documents = f"{database}/documents"
    collection = f"read-time-{int(time.time())}"
    production.call("POST", f"/v1/{documents}:commit", {"writes": [
        {"update": {"name": f"{documents}/{collection}/d", "fields": {"a": {"integerValue": "1"}}}}]})
    reads = recording["read_time"]["reads"]

    def ours(value):
        return json.loads(json.dumps(value).replace(f"projects/{READ_TIME_PROJECT}/", f"projects/{production.project}/")
                          .replace(f"/documents/{READ_TIME_COLLECTION}/", f"/documents/{collection}/")
                          .replace(f'"collectionId": "{READ_TIME_COLLECTION}"', f'"collectionId": "{collection}"'))

    def theirs(value):
        return json.loads(json.dumps(value).replace(f"projects/{READ_TIME_PROJECT}/", "projects/{project}/"))

    def answer_of(text):
        answer = parsed(mask(text.replace(f"/documents/{collection}/", f"/documents/{READ_TIME_COLLECTION}/"),
                             production.project))
        if isinstance(answer, dict) and "collectionIds" in answer:
            ids = [READ_TIME_COLLECTION for c in answer["collectionIds"] if c == collection]
            answer = {**answer, "collectionIds": ids} if ids else {k: v for k, v in answer.items() if k != "collectionIds"}
        return answer

    def read(spec, read_time):
        body = json.loads(json.dumps(ours(spec["body"])).replace("{readTime}", read_time))
        if spec["transport"] == "rest":
            path = ours(spec["path"]).replace("{readTime}", urllib.parse.quote(read_time, safe=""))
            status, _, text = production.send(spec["method"], path, None if body is None else json.dumps(body).encode())
            return read_answer(status, answer_of(text) if text else None)
        when = urllib.parse.quote(body["readTime"], safe="")
        if spec["method"] == "GetDocument":
            status, _, text = production.send("GET", f"/v1/{urllib.parse.quote(body['name'], safe='/()')}?readTime={when}")
        elif spec["method"] == "ListDocuments":
            status, _, text = production.send(
                "GET", f"/v1/{urllib.parse.quote(body['parent'], safe='/()')}/{body['collectionId']}?readTime={when}")
        else:
            status, _, text = production.send("POST", f"/v1/{urllib.parse.quote(body['parent'], safe='/()')}:runQuery",
                                              json.dumps({k: v for k, v in body.items() if k != "parent"}).encode())
        answer = answer_of(text)
        errors = [r["error"] for r in (answer if isinstance(answer, list) else [answer])
                  if isinstance(r, dict) and isinstance(r.get("error"), dict)]
        if status != 200 or errors:
            error = errors[0] if errors else {}
            return {"status": error.get("status", str(status)), "message": error.get("message", text[:300])}
        found = [answer] if spec["method"] == "GetDocument" else (
            answer.get("documents", []) if spec["method"] == "ListDocuments" else [r["document"] for r in answer if "document" in r])
        prefix = "projects/{project}/databases/(default)/documents/"
        return {"status": "OK", "message": "", "documents": [d["name"][len(prefix):] for d in found]}

    for case in recording["read_time"]["cases"]:
        for spec, official in zip(reads, case["answers"]):
            name = f"{spec['name']}, {case['age']} s old"
            text = json.dumps(spec)
            if any(p != READ_TIME_PROJECT for p in PROJECT_NAME.findall(text)) or "/databases/(default)/" not in text:
                result["skipped"]["names another project"] = result["skipped"].get("names another project", 0) + 1
                continue
            read_time = rfc3339(int(time.time() * 1_000_000) - case["age"] * 1_000_000)
            answered = read(spec, read_time)
            result["sent"] += 1
            official = theirs(official)
            if spec["transport"] == "rest":
                official = read_answer(official["status"], official["response"])
            tally(result, official, answered, lambda: {
                "name": name, "method": spec.get("method"), "path": theirs(spec.get("path") or spec["method"]),
                "difference": first_difference(official, answered) or "the outcome", "official": official,
                "production": answered}, name)


# The project `tools/oracle/find_nearest.py` recorded in; its queries and answers name it.
FIND_NEAREST_PROJECT = "find-nearest"


def replay_find_nearest(production, recording, result):
    """The recording of vector search (`find_nearest.json`): its seed is written, then each query
    is run with `:runQuery` and each aggregation with `:runAggregationQuery`, and each malformed
    vector value is committed, as its recorder (`tools/oracle/find_nearest.py`) did, and compared
    as it put them: the documents found, in order, with their fields but the vector, or the
    aggregations' results, or the error's status and message. A query production refuses for want
    of a vector index is counted apart, and the index kept in the report (`indexes.py`)."""
    documents = f"projects/{production.project}/databases/(default)/documents"

    def ours(value):
        return json.loads(json.dumps(value).replace(f"projects/{FIND_NEAREST_PROJECT}/", f"projects/{production.project}/"))

    def theirs(value):
        return json.loads(json.dumps(value).replace(f"projects/{FIND_NEAREST_PROJECT}/", "projects/{project}/"))

    def error(status, answer):
        errors = [r["error"] for r in (answer if isinstance(answer, list) else [answer])
                  if isinstance(r, dict) and isinstance(r.get("error"), dict)]
        return {"status": status, "message": mask(errors[0].get("message", "") if errors else str(answer)[:300],
                                                  production.project)}

    def compare(name, verb, official, answered):
        result["sent"] += 1
        official = theirs(official)
        tally(result, official, answered, lambda: {
            "name": name, "method": "POST", "path": f"/v1/projects/{{project}}/databases/(default)/documents:{verb}",
            "difference": first_difference(official, answered) or "the outcome", "official": official,
            "production": answered}, name)

    production.call("POST", f"/v1/{documents}:commit", {"writes": [
        {"update": {"name": f"{documents}/{path}", "fields": ours(fields)}} for path, fields in recording["seed"].items()]})
    for case in recording["queries"]:
        status, _, text = production.send("POST", f"/v1/{documents}:runQuery",
                                          json.dumps({"structuredQuery": ours(case["query"])}).encode())
        asked(result, text)
        answer = parsed(text)
        if NEEDS_INDEX.search(text) and isinstance(case["outcome"], list):
            result["sent"] += 1
            result["needsIndex"].append(case["name"])
            continue
        if status != 200 or not isinstance(answer, list) or any(isinstance(r, dict) and "error" in r for r in answer):
            answered = json.loads(INDEX_LINK.sub("<index link>", json.dumps(error(status, answer))))
        else:
            answered = [{"name": r["document"]["name"][len(documents) + 1:],
                         "fields": {k: v for k, v in r["document"].get("fields", {}).items() if k != "v"}}
                        for r in answer if "document" in r]
        compare(case["name"], "runQuery", case["outcome"], answered)
    for case in recording["aggregations"]:
        status, _, text = production.send("POST", f"/v1/{documents}:runAggregationQuery",
                                          json.dumps(ours(case["request"])).encode())
        asked(result, text)
        answer = parsed(text)
        if NEEDS_INDEX.search(text) and isinstance(case["outcome"], list):
            result["sent"] += 1
            result["needsIndex"].append(case["name"])
            continue
        if status != 200 or not isinstance(answer, list) or any(isinstance(r, dict) and "error" in r for r in answer):
            answered = error(status, answer)
        else:
            answered = [r["result"]["aggregateFields"] for r in answer if "result" in r]
        compare(case["name"], "runAggregationQuery", case["outcome"], answered)
    for case in recording["writes"]:
        body = {"writes": [{"update": {"name": f"{documents}/writes/w", "fields": ours(case["fields"])}}]}
        status, _, text = production.send("POST", f"/v1/{documents}:commit", json.dumps(body).encode())
        answered = {"status": status, "message": "" if status == 200 else error(status, parsed(text))["message"]}
        compare(case["name"], "commit", case["outcome"], answered)


# The project `tools/oracle/unsupported.py` recorded in; its requests and answers name it.
UNSUPPORTED_PROJECT = "unsupported"


def replay_unsupported(production, recording, result):
    """The recording of what the official emulator ignores or refuses (`unsupported.json`): its
    two documents are written, as its recorder (`tools/oracle/unsupported.py`) wrote them, then
    each REST request is sent as it is, and each gRPC one as its REST form (`:runQuery`,
    `:runAggregationQuery`, `:executePipeline`, `:partitionQuery`). A REST answer's status and
    body are compared; a gRPC one as the recorder summed it up: the documents, the aggregations,
    whether explain metrics came back, or the error. `done`, which production does not send
    (D099), is not. Requests as someone other than the owner, or naming another database, are
    not sent."""
    database = f"projects/{production.project}/databases/(default)"
    documents = f"{database}/documents"
    production.call("POST", f"/v1/{documents}:commit", {"writes": [
        {"update": {"name": f"{documents}/c/{name}", "fields": {"n": {"integerValue": str(n)}}}}
        for n, name in enumerate("ab", 1)]})

    def theirs(value):
        return json.loads(json.dumps(value).replace(f"projects/{UNSUPPORTED_PROJECT}/", "projects/{project}/"))

    def compare(name, path, official, answered):
        result["sent"] += 1
        official = theirs(official)
        tally(result, official, answered, lambda: {
            "name": name, "method": "POST", "path": path, "difference": first_difference(official, answered)
            or "the outcome", "official": official, "production": answered}, name)

    for case in recording["grpc"]:
        request = case["request"]
        named = request.get("parent") or request.get("database") or ""
        if case["authorization"] != "Bearer owner" or not named.startswith(f"projects/{UNSUPPORTED_PROJECT}/databases/"):
            why = "not sent as the owner" if case["authorization"] != "Bearer owner" else "names no database"
            result["skipped"][why] = result["skipped"].get(why, 0) + 1
            continue
        verb = case["rpc"][0].lower() + case["rpc"][1:]
        body = {k: v for k, v in request.items() if k not in ("parent", "database")}
        status, _, text = production.send("POST", f"/v1/{documents if 'parent' in request else database + '/documents'}:{verb}",
                                          json.dumps(body).encode())
        answer = parsed(mask(text, production.project))
        errors = [r["error"] for r in (answer if isinstance(answer, list) else [answer])
                  if isinstance(r, dict) and isinstance(r.get("error"), dict)]
        if status != 200 or errors:
            error = errors[0] if errors else {}
            answered = {"status": error.get("status", str(status)), "message": error.get("message", "")}
        else:
            answers = answer if isinstance(answer, list) else [answer]
            answered = {"documents": [r["document"]["name"].split("/documents/", 1)[1] for r in answers if "document" in r],
                        "aggregations": [r["result"]["aggregateFields"] for r in answers if "result" in r],
                        "explain_metrics": any("explainMetrics" in r for r in answers)}
        official = {("status" if k == "code" else k): v for k, v in case["outcome"].items() if k != "done"}
        compare(case["name"], f"/v1/projects/{{project}}/databases/(default)/documents:{verb}", official, answered)

    for case in recording["rest"]:
        status, content_type, text = production.send("POST", f"/v1/{documents}:{case['verb']}",
                                                      json.dumps(case["body"]).encode())
        compare(case["name"], f"/v1/projects/{{project}}/databases/(default)/documents:{case['verb']}",
                {"status": case["outcome"]["status"], "body": parsed(case["outcome"]["body"])},
                {"status": status, "body": parsed(mask(text, production.project))})


# A condition that reads the document, the request or other documents. The Rules API takes the
# document as JSON, whose numbers are all floats (`1 is int` is false), and runs lookups only on
# mocks; such a condition is not tested.
READS = re.compile(r"\b(?:resource|request)\b|(?<![.\w])(?:get|exists|getAfter|existsAfter|value)\(")


def rules_tests(production, bodies):
    """Each request tested with the Rules API (`projects.test`), `RULES_AT_ONCE` at a time and at
    most `RULES_PER_MINUTE` a minute, and tried again while the API is busy; the answers (status,
    body as text), in order."""
    pace = Pace(RULES_PER_MINUTE)

    def test(body):
        for attempt in range(6):
            pace.wait()
            status, _, text = production.send("POST", f"/v1/projects/{production.project}:test",
                                              json.dumps(body).encode(), base=production.rules)
            if status not in TRANSIENT:
                break
            time.sleep(2 ** attempt)
        return status, text

    with ThreadPoolExecutor(RULES_AT_ONCE) as pool:
        return list(pool.map(test, bodies))


# Differences of the Rules API docs/parity-exceptions.md records: (entry, what the ruleset calls,
# what production's answer says). `projects.test` has no `debug()`, and a condition that calls it
# fails (D113); it refuses a file without content or a name that is text (D114).
KNOWN_RULES = [("D113", r"(?<![.\w])debug\(", r"Function not found error: Name: \[debug\]\."),
               ("D114", r"", r"(?:Content|Name) must not be blank\.|Invalid value at 'source\.files\[\d+\]\.name'")]


def known_rules(result, source, text):
    """Whether production's answer to a ruleset shows a difference `KNOWN_RULES` records; if it
    does, the case counts as known."""
    for entry, calls, says in KNOWN_RULES:
        if re.search(calls, source) and re.search(says, text):
            result["knownCases"] += 1
            result["known"][entry] = result["known"].get(entry, 0) + 1
            return True
    return False


def rules_outcome(status, text):
    """The Rules API's answer to a test, as a recording of rules puts the official emulator's:
    `allow`, `deny` or `compile`, and the warnings (`issues`)."""
    try:
        answer = json.loads(text)
    except ValueError:
        return {"outcome": str(status), "issues": []}
    if status != 200:
        return {"outcome": "compile" if status == 400 else str(status), "issues": []}
    issues = answer.get("issues", [])
    if any(i.get("severity") == "ERROR" for i in issues):
        return {"outcome": "compile", "issues": []}
    state = (answer.get("testResults") or [{}])[0].get("state")
    return {"outcome": {"SUCCESS": "allow", "FAILURE": "deny"}.get(state, str(state)),
            "issues": sorted(i["description"] for i in issues)}


def replay_rules(production, recording, result):
    """A recording of Security Rules (`rules_eval.json`): each condition, in the recording's
    ruleset, or each whole ruleset, is tested with the Rules API (`projects.test`), for a `get` of
    `rooms/r1` with no `auth`, and its outcome and warnings are compared. A denial's message is
    the emulator's own and is not; nor are batches of writes, and conditions that read the
    document, the request or other documents (`READS`). A case that shows a difference
    `KNOWN_RULES` records counts as known."""
    tested = []
    for case in recording["results"]:
        if "writes" in case or "answers" in case:
            why = "a batch of writes"
        elif READS.search(case.get("condition") or case.get("rules", "")):
            why = "reads the document, the request or other documents"
        else:
            why = None
        if why:
            result["skipped"][why] = result["skipped"].get(why, 0) + 1
        else:
            tested.append(case)

    sources = [case.get("rules") or recording["rules"].replace("%s", case["condition"], 1) for case in tested]
    answers = rules_tests(production, [
        {"source": {"files": [{"name": "firestore.rules", "content": source}]},
         "testSuite": {"testCases": [{"expectation": "ALLOW", "request": {
             "auth": None, "method": "get", "path": "/databases/(default)/documents/rooms/r1"}}]}}
        for source in sources])
    for case, source, (status, text) in zip(tested, sources, answers):
        result["sent"] += 1
        outcome = case["outcome"] if case["outcome"] in ("allow", "deny", "compile") else str(case["outcome"])
        official = {"outcome": outcome, "issues": [] if outcome == "compile" else sorted(case.get("issues", []))}
        answered = rules_outcome(status, mask(text, production.project))
        if same_json(official, answered):
            result["same"] += 1
        elif not known_rules(result, source, text):
            result["differ"].append({"name": case["name"], "method": "POST", "path": "/v1/projects/{project}:test",
                                     "difference": first_difference(official, answered) or "the outcome",
                                     "official": {"status": outcome, **official},
                                     "production": {"status": answered["outcome"], **answered},
                                     "rules": case.get("condition") or source})


TIMESTAMP = re.compile(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?Z")
# A time in a step's path or body, `:` maybe written `%3A`.
SENT_TIME = re.compile(r"\d{4}-\d\d-\d\dT\d\d(?::|%3A)\d\d(?::|%3A)\d\d(?:\.\d+)?Z")
SERVER_TIMES = {"createTime", "updateTime", "readTime", "commitTime"}
# A document ID the server chose: 20 letters and digits, at the end of a document's name.
AUTO_ID = re.compile(r"(?<=/documents/)((?:[^/]+/[^/]+/)*[^/]+/)[A-Za-z0-9]{20}")
# A stored version in an error is the document's update time in microseconds.
STORED_VERSION = re.compile(r"(?<=version \()(?:1[6-9]|[2-9]\d)\d{14}(?=\))")


def micros(timestamp):
    """An RFC 3339 UTC time, in microseconds since the epoch."""
    whole = datetime.strptime(timestamp[:19], "%Y-%m-%dT%H:%M:%S").replace(tzinfo=timezone.utc)
    return int(whole.timestamp()) * 1_000_000 + int((timestamp[20:-1] + "000000")[:6] or 0)


def rfc3339(us):
    return datetime.fromtimestamp(us // 1_000_000, timezone.utc).strftime("%Y-%m-%dT%H:%M:%S") + f".{us % 1_000_000:06d}Z"


def shift(text, official, production):
    """A step's path or body with each time near one of the official emulator's commits of the
    case (within a day) moved by as much as production's commit is from it: a precondition on
    the update time a step got, or a read time an hour after it."""
    def moved(found):
        sent = found.group(0)
        at = micros(sent.replace("%3A", ":"))
        near = [(abs(at - micros(c)), i) for i, c in enumerate(official) if abs(at - micros(c)) < 86_400_000_000]
        if not near or min(near)[1] >= len(production):
            return sent
        i = min(near)[1]
        new = rfc3339(micros(production[i]) + at - micros(official[i]))
        return new.replace(":", "%3A") if "%3A" in sent else new
    return SENT_TIME.sub(moved, text)


def step_answer(status, answer, project, commits):
    """A step's answer as it is compared: its status, and the error's status and message or the
    body. The case's project reads `{project}`. The times in a body are read against the case's
    commits so far (`commits`, which a body with a `commitTime` adds to): a commit's own time is
    `<commit 2>`, one within a minute of a commit `<near commit 2>`, another server time (a
    `readTime`) `<time>`; a time the client wrote stays as it is. A read time is the snapshot the
    server chose, which production may put at the commit itself: it is `<near commit 2>` there
    too. Page tokens read `<token>`, and IDs the server chose `<auto>`."""
    if answer is None:
        return {"status": status}
    alone = re.compile(r"(?<![\w-])" + re.escape(project) + r"(?![\w-])")
    answer = json.loads(alone.sub("{project}", json.dumps(answer)))
    error = answer.get("error") if isinstance(answer, dict) else None
    if isinstance(error, dict):
        return {"status": status, "error": error.get("status"),
                "message": STORED_VERSION.sub("<version>", error.get("message", ""))}
    if isinstance(answer, dict) and isinstance(answer.get("commitTime"), str):
        commits.append(answer["commitTime"])

    def read(value, key=None):
        if isinstance(value, dict):
            return {k: read(v, k) for k, v in value.items()}
        if isinstance(value, list):
            return [read(v) for v in value]
        if isinstance(value, str) and TIMESTAMP.fullmatch(value):
            if value in commits and key != "readTime":
                return f"<commit {commits.index(value) + 1}>"
            near = [i for i, c in enumerate(commits) if abs(micros(c) - micros(value)) < 60_000_000]
            if near:
                return f"<near commit {near[-1] + 1}>"
            if key in SERVER_TIMES:
                return "<time>"
        if key == "nextPageToken":
            return "<token>"
        if isinstance(value, str) and "/documents/" in value:
            return AUTO_ID.sub(r"\1<auto>", value)
        return value

    return {"status": status, "body": read(answer)}


# A step's template for a time of the case's commits (`{C1}`, `{C2+1}`: the second commit's time
# and a microsecond), and for its version in an answer (`{V1}`), as `base_versions.json` writes them.
TEMPLATE = re.compile(r"\{([CV])(\d+)([+-]\d+)?\}")


def filled(text, commits):
    """A step's templates filled with the times or versions of production's commits so far."""
    def one(found):
        kind, n, by = found.group(1), int(found.group(2)), int(found.group(3) or 0)
        if n > len(commits):
            return found.group(0)
        us = micros(commits[n - 1]) + by
        return str(us) if kind == "V" else rfc3339(us)
    return TEMPLATE.sub(one, text)


# What a step carries from an earlier answer: a page token, a transaction.
CARRIED = {"nextPageToken", "transaction"}


def carried(official, production, found):
    """Pairs what two answers hand out for later steps (`CARRIED`), the official emulator's value
    with production's, in `found`."""
    if isinstance(official, dict) and isinstance(production, dict):
        for k, v in official.items():
            if k in CARRIED and isinstance(v, str) and isinstance(production.get(k), str):
                found[v] = production[k]
            elif k in production:
                carried(v, production[k], found)
    elif isinstance(official, list) and isinstance(production, list):
        for o, p in zip(official, production):
            carried(o, p, found)


def handed_on(text, found):
    """A step's path or body with what the official emulator handed out replaced by what
    production did, as it is or percent-encoded."""
    for official, production in found.items():
        text = text.replace(official, production)
        text = text.replace(urllib.parse.quote(official, safe=""), urllib.parse.quote(production, safe=""))
    return text


# Recordings of steps that record the order of an answer's keys, the fields' above all
# (`tools/oracle/field_order.py`): theirs is compared too, and the entry that records how
# production orders them.
ORDERED = {"field_order.json": "D115"}


def order_difference(official, production, at="$"):
    """Where two JSON values hold the keys they share in another order, and how, or None."""
    if isinstance(official, dict) and isinstance(production, dict):
        theirs, ours = [k for k in official if k in production], [k for k in production if k in official]
        if theirs != ours:
            return f"{at}: keys in the order {theirs}, production {ours}"
        return next((d for k in theirs if (d := order_difference(official[k], production[k], f"{at}.{k}"))), None)
    if isinstance(official, list) and isinstance(production, list):
        return next((d for i, (o, p) in enumerate(zip(official, production))
                     if (d := order_difference(o, p, f"{at}[{i}]"))), None)
    return None


def replay_steps(production, recording, result):
    """A recording of steps (`same_document_writes.json`, `document_writes.json`,
    `transforms.json`, `base_versions.json`). On the official emulator each case ran in a project
    of its own; here each runs on the database emptied, in the replay's project, and every step's
    answer is compared as `step_answer` reads it. What an earlier answer handed out (a page token,
    a transaction) is replaced by what production handed out, and times near a commit are moved
    with it (`shift`), and templates of a commit's time or version are filled with production's
    (`filled`). A case that sets rules, or sends a step as someone other than the owner, is
    not run. A step the emulator refused for want of admin credentials (`… require admin
    authentication.`: its recorder sent none) is not sent: the replay always has them."""
    project = production.project
    for case in recording["cases"]:
        if case.get("rules") or not all(step.get("owner", True) for step in case["steps"]):
            result["skipped"]["evaluates rules"] = result["skipped"].get("evaluates rules", 0) + 1
            continue
        own = case["steps"][0]["path"].split("projects/")[1].split("/")[0]
        production.empty()
        result["sent"] += 1
        official_commits, production_commits, first, found, known = [], [], None, {}, set()
        for n, step in enumerate(case["steps"], 1):
            recorded = step["response"] if "response" in step else ({"error": step["error"]} if "error" in step else None)
            if "require admin authentication" in json.dumps(recorded):
                continue

            def sent(text):
                return filled(handed_on(shift(text.replace(f"projects/{own}/", f"projects/{project}/"), official_commits,
                                              production_commits), found), production_commits)

            path = sent(step["path"] if step["path"].startswith("/") else "/v1/" + step["path"])
            data = None if step.get("body") is None else sent(json.dumps(step["body"])).encode()
            status, _, text = production.send(step["method"], path, data)
            try:
                answer = json.loads(text) if text.strip() else None
            except ValueError:
                answer = {"error": {"message": text[:300]}}
            carried(recorded, answer, found)
            if recorded is not None and TEMPLATE.search(json.dumps(recorded)):
                recorded = json.loads(filled(json.dumps(recorded), production_commits))
            official = step_answer(step["status"], recorded, own, official_commits)
            # A step recorded with its status alone (`base_versions.json`) is compared on it; its
            # commit still counts for the templates of later steps.
            if recorded is None and isinstance(answer, dict) and isinstance(answer.get("commitTime"), str):
                production_commits.append(answer["commitTime"])
            answered = step_answer(status, answer if recorded is not None else None, project, production_commits)
            if first is None and not same_json(official, answered, known):
                first = (n, official, answered)
            elif first is None and result["fixture"] in ORDERED and order_difference(official, answered):
                known.add(ORDERED[result["fixture"]])
        about = [k for k, pattern in KNOWN_CASES.get(result["fixture"], []) if re.search(pattern, case["name"])]
        if first is not None and about:
            first, known = None, {about[0]}
        if first is None and not known:
            result["same"] += 1
        elif first is None:
            result["knownCases"] += 1
            for k in known:
                result["known"][k] = result["known"].get(k, 0) + 1
        else:
            n, official, answered = first
            result["differ"].append({"name": f"{case['name']}, step {n}", "method": case["steps"][n - 1]["method"],
                                     "path": case["steps"][n - 1]["path"].replace(own, "{project}"),
                                     "difference": first_difference(official, answered) or "the answer",
                                     "official": official, "production": answered})


def replay(production, fixture):
    """Sends a recording's cases; what each answered next to what the official emulator did."""
    recording = json.loads(Path(fixture).read_text())
    if isinstance(recording, list) and recording and "steps" in recording[0]:
        # A list of cases of steps (`field_order.json`).
        recording = {"cases": recording}
    project = production.project
    if isinstance(recording, list) or "results" in recording:
        # Recordings of Security Rules: conditions evaluated on a document (`rules_eval.json`);
        # and, in rules.py, rulesets compiled (`rules_compile.json`, a list of cases, and
        # `rules_warnings.json`) and the documents `match` paths cover (`rules_paths.json`).
        # rules.py takes its comparisons from this module, so it is imported once this one is.
        import rules
        cases = recording if isinstance(recording, list) else recording["results"]
        result = {"fixture": Path(fixture).name, "cases": len(cases), "sent": 0, "same": 0,
                  "skipped": {}, "emulatorDeletes": 0, "needsIndex": [], "indexes": [], "knownCases": 0,
                  "known": {}, "differ": []}
        log(f"{result['fixture']}: {result['cases']} rules")
        if isinstance(recording, list):
            rules.replay_compile(production, recording, result)
        elif "documents" in recording:
            rules.replay_paths(production, recording, result)
        elif "document" in recording:
            replay_rules(production, recording, result)
        else:
            rules.replay_warnings(production, recording, result)
        return result
    if "groups" in recording.get("values", {}):
        result = {"fixture": Path(fixture).name, "cases": 4, "sent": 0, "same": 0, "skipped": {}, "emulatorDeletes": 0,
                  "needsIndex": [], "indexes": [], "knownCases": 0, "known": {}, "differ": []}
        production.empty()
        log(f"{result['fixture']}: the order of values and of document names")
        replay_value_order(production, recording, result)
        production.empty()
        return result
    if "seed" in recording and "grpc" in recording and "rest" in recording:
        result = {"fixture": Path(fixture).name, "cases": len(recording["grpc"]) + len(recording["rest"]), "sent": 0,
                  "same": 0, "skipped": {}, "emulatorDeletes": 0, "needsIndex": [], "indexes": [], "knownCases": 0,
                  "known": {}, "differ": []}
        production.empty()
        log(f"{result['fixture']}: {result['cases']} listings")
        replay_list_documents(production, recording, result)
        production.empty()
        return result
    if "queries" in recording and "writes" in recording and "seed" in recording:
        result = {"fixture": Path(fixture).name, "sent": 0, "same": 0, "skipped": {}, "emulatorDeletes": 0,
                  "cases": sum(len(recording[k]) for k in ("queries", "aggregations", "writes")),
                  "needsIndex": [], "indexes": [], "knownCases": 0, "known": {}, "differ": []}
        production.empty()
        log(f"{result['fixture']}: {result['cases']} vector searches and writes")
        replay_find_nearest(production, recording, result)
        production.empty()
        return result
    if "read_time" in recording:
        read_time = recording["read_time"]
        result = {"fixture": Path(fixture).name, "cases": len(read_time["reads"]) * len(read_time["cases"]), "sent": 0,
                  "same": 0, "skipped": {}, "emulatorDeletes": 0, "needsIndex": [], "indexes": [], "knownCases": 0,
                  "known": {}, "differ": []}
        production.empty()
        log(f"{result['fixture']}: {result['cases']} reads at a read time")
        replay_read_times(production, recording, result)
        production.empty()
        return result
    if "grpc" in recording and "rest" in recording and "verb" in (recording["rest"] or [{}])[0]:
        result = {"fixture": Path(fixture).name, "cases": len(recording["grpc"]) + len(recording["rest"]), "sent": 0,
                  "same": 0, "skipped": {}, "emulatorDeletes": 0, "needsIndex": [], "indexes": [], "knownCases": 0,
                  "known": {}, "differ": []}
        production.empty()
        log(f"{result['fixture']}: {result['cases']} requests the official emulator ignores or refuses")
        replay_unsupported(production, recording, result)
        production.empty()
        return result
    if "cases" not in recording and isinstance(recording.get("rest"), list) and "headers" in (recording["rest"] or [{}])[0]:
        # The REST cases of a recording that holds others too (`json_escapes.json`, whose WebChannel
        # cases are not sent).
        recording = {**recording, "cases": recording["rest"]}
    result = {"fixture": Path(fixture).name, "cases": len(recording["cases"]), "sent": 0, "same": 0,
              "skipped": {}, "emulatorDeletes": 0, "needsIndex": [], "indexes": [], "knownCases": 0,
                  "known": {}, "differ": []}
    production.empty()
    log(f"{result['fixture']}: {len(recording['cases'])} cases")
    if "dataset" in recording:
        replay_queries(production, recording, result)
        production.empty()
        return result
    if recording["cases"] and "steps" in recording["cases"][0]:
        replay_steps(production, recording, result)
        production.empty()
        return result
    if recording["cases"] and "fields" in recording["cases"][0]:
        replay_limits(production, recording, result)
        production.empty()
        return result
    for n, case in enumerate(recording["cases"], 1):
        if n % 50 == 0:
            log(f"{result['fixture']}: case {n}, {production.sent} requests so far")
        under = emulator_delete(case)
        if under is not None:
            production.empty(under)
            result["emulatorDeletes"] += 1
            continue
        why = sendable(case)
        if why:
            result["skipped"][why] = result["skipped"].get(why, 0) + 1
            continue
        headers = {k.lower(): v for k, v in (case.get("headers") or {}).items()}
        if "raw" in case:
            data = case["raw"].replace("{project}", project).encode()
        elif "body" in case:
            data = json.dumps(case["body"]).replace("{project}", project).encode()
        else:
            data = None
        status, content_type, text = production.send(case["method"], case["path"].replace("{project}", project), data,
                                                      headers.get("content-type", "application/json"))
        result["sent"] += 1
        official = case["outcome"]
        answered = {"status": status, "contentType": content_type,
                    "body": INDEX_LINK.sub("<index link>", mask(text, project))}
        asked(result, text)
        if status == 400 and NEEDS_INDEX.search(text) and official["status"] < 400:
            result["needsIndex"].append(case["name"])
            continue
        theirs = {"status": official["status"], "media": media(official.get("contentType"))}
        ours = {"status": status, "media": media(content_type)}
        if case.get("compare", "exact") == "exact":
            theirs["body"], ours["body"] = parsed(official.get("body") or ""), parsed(answered["body"])
        tally(result, theirs, ours, lambda: {"name": case["name"], "method": case["method"], "path": case["path"],
                                             "difference": difference(official, answered), "official": official,
                                             "production": answered}, case["name"])
    production.empty()
    return result


def markdown(report):
    lines = ["## Production parity", "",
             f"Recordings of the official emulator replayed against production Cloud Firestore on {report['started']}.", "",
             "Known: the same but for differences `docs/parity-exceptions.md` records.", "",
             "| Recording | Cases | Sent | Same | Known | Differ | Needs an index | Not sent |",
             "|---|---:|---:|---:|---:|---:|---:|---:|"]
    for r in report["recordings"]:
        lines.append(f"| `{r['fixture']}` | {r['cases']} | {r['sent']} | {r['same']} | {r['knownCases']} | "
                     f"{len(r['differ'])} | {len(r['needsIndex'])} | {sum(r['skipped'].values())} |")
    known = {}
    for r in report["recordings"]:
        for k, n in r["known"].items():
            known[k] = known.get(k, 0) + n
    if known:
        lines += ["", "Known differences, by the cases they appear in: "
                  + ", ".join(f"{k} {n}" for k, n in sorted(known.items())) + "."]
    indexes = {json.dumps(i, sort_keys=True) for r in report["recordings"] for i in r["indexes"]}
    if indexes:
        lines += ["", f"Production asked for {len(indexes)} indexes the project does not have: "
                  "`tools/production/indexes.py add` lists them in `indexes.json`, and `create` makes them."]
    for r in report["recordings"]:
        if r["differ"]:
            lines += ["", f"### `{r['fixture']}`", "", "| Case | Official | Production | First difference |",
                      "|---|---|---|---|"]
            for d in r["differ"][:300]:
                name, what = (s.replace("|", "\\|").replace("\n", " ") for s in (d["name"], d["difference"]))
                theirs, ours = (side.get("status", "") if isinstance(side, dict) else "" for side in (d["official"], d["production"]))
                lines.append(f"| {name} | {theirs} | {ours} | {what} |")
            if len(r["differ"]) > 300:
                lines.append(f"| … {len(r['differ']) - 300} more in `report.json` | | | |")
    return "\n".join(lines) + "\n"


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--project", required=True, help="the project of the maintainers' own")
    parser.add_argument("--out", required=True, help="where report.json and report.md go")
    parser.add_argument("--base", default="https://firestore.googleapis.com")
    parser.add_argument("--rules-base", default=RULES_API)
    parser.add_argument("--budget", type=int, default=10000, help="the most requests a run sends")
    parser.add_argument("--quota-project", help="the project a user's token is billed to (x-goog-user-project); "
                                                "the Rules API needs one with a user's credentials, not a service account's")
    parser.add_argument("fixtures", nargs="+")
    args = parser.parse_args(argv)
    token = os.environ.get(TOKEN_ENV)
    if not token:
        parser.error(f"{TOKEN_ENV} is not set")
    production = Production(args.base, args.project, token, args.budget, args.rules_base, args.quota_project)
    report = {"started": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"), "project": "{project}",
              "recordings": []}
    try:
        for fixture in args.fixtures:
            report["recordings"].append(replay(production, fixture))
    finally:
        report["requests"] = production.sent
        out = Path(args.out)
        out.mkdir(parents=True, exist_ok=True)
        (out / "report.json").write_text(json.dumps(report, ensure_ascii=False, indent=1) + "\n")
        summary = markdown(report)
        (out / "report.md").write_text(summary)
        if os.environ.get("GITHUB_STEP_SUMMARY"):
            with open(os.environ["GITHUB_STEP_SUMMARY"], "a") as f:
                f.write(summary)
    print(f"{production.sent} requests; " + ", ".join(
        f"{r['fixture']}: {r['same']} same, {len(r['differ'])} differ" for r in report["recordings"]))


if __name__ == "__main__":
    main()
