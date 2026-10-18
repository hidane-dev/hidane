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

What differs goes to `<out>/report.json` and `<out>/report.md`, and to the job summary in GitHub
Actions. It is not a recording: it stays a workflow artifact until a person has looked at it
(docs/clean-room/OBSERVATION_POLICY.md). The token is never written anywhere. Stdlib only.
"""

import argparse
import http.client
import json
import os
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "oracle"))
sys.path.insert(0, str(Path(__file__).resolve().parent))  # `-I` leaves it out
from harness import mask  # noqa: E402
from indexes import index_asked  # noqa: E402

TOKEN_ENV = "PRODUCTION_ACCESS_TOKEN"
DATABASE = "/v1/projects/{project}/databases/(default)/"
# A project named anywhere in a case: only the replay's own may be.
PROJECT_NAME = re.compile(r"projects/([^/\s\"'?:]+)")
TRANSIENT = {429, 500, 502, 503, 504}
NETWORK_TRIES = 4


def log(message):
    print(message, file=sys.stderr, flush=True)


class Production:
    """Requests to one project's `(default)` database, with a bearer token."""

    def __init__(self, base, project, token, budget):
        self.base = base.rstrip("/")
        self.project = project
        self.token = token
        self.budget = budget
        self.sent = 0

    def send(self, method, path, data=None, content_type="application/json"):
        """One request; answers (status, content type, body as text). A connection that fails
        before an answer is tried again a few times; then the answer is status 0."""
        if self.sent >= self.budget:
            raise RuntimeError(f"stopped at the budget of {self.budget} requests")
        self.sent += 1
        req = urllib.request.Request(self.base + path, method=method, data=data,
                                     headers={"Authorization": f"Bearer {self.token}", "Content-Type": content_type})
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
        (a path below the documents) and everything below it; those under missing documents
        included."""
        documents = f"projects/{self.project}/databases/(default)/documents"
        names = []

        def url(name):
            return urllib.parse.quote(name, safe="/()")

        def collections(parent):
            ids, token = [], None
            while True:
                body = {"pageSize": 100, **({"pageToken": token} if token else {})}
                answer = self.call("POST", f"/v1/{url(parent)}:listCollectionIds", body)
                ids += answer.get("collectionIds", [])
                token, previous = answer.get("nextPageToken"), token
                if not token or token == previous:
                    return ids

        def collection(parent, collection_id):
            token = None
            while True:
                query = "?pageSize=300&showMissing=true&mask.fieldPaths=__name__"
                if token:
                    query += "&pageToken=" + urllib.parse.quote(token, safe="")
                answer = self.call("GET", f"/v1/{url(parent)}/{urllib.parse.quote(collection_id, safe='')}{query}")
                for document in answer.get("documents", []):
                    walk(document["name"])
                    names.append(document["name"])
                token, previous = answer.get("nextPageToken"), token
                if not token or token == previous:
                    return

        def walk(parent):
            for collection_id in collections(parent):
                collection(parent, collection_id)

        segments = [s for s in under.split("/") if s]
        if not segments:
            walk(documents)
        elif len(segments) % 2:
            collection("/".join([documents, *segments[:-1]]), segments[-1])
        else:
            walk("/".join([documents, *segments]))
            names.append("/".join([documents, *segments]))
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


def same_json(official, production):
    """Whether two JSON values are the same, a recording's placeholder matching anything.
    Numbers compare by value: production prints a double without `.0` (`1`), the official
    emulator as Java does (`1.0`)."""
    if isinstance(official, str):
        return isinstance(production, str) and same_string(official, production)
    if number(official) and number(production):
        return official == production
    if isinstance(official, dict):
        return (isinstance(production, dict) and official.keys() == production.keys()
                and all(same_json(official[k], production[k]) for k in official))
    if isinstance(official, list):
        return (isinstance(production, list) and len(official) == len(production)
                and all(same_json(o, p) for o, p in zip(official, production)))
    return official == production and type(official) is type(production)


def same_body(official, production):
    """Two answers' bodies: as JSON when both are (production prints it its own way), else as
    text."""
    try:
        return same_json(json.loads(official), json.loads(production))
    except ValueError:
        return official == production


def first_difference(official, production, at="$"):
    """Where two JSON values first differ, and how, or None."""
    if isinstance(official, dict) and isinstance(production, dict):
        if official.keys() != production.keys():
            only = sorted(official.keys() - production.keys()), sorted(production.keys() - official.keys())
            return f"{at}: keys only the official has {only[0]}, only production has {only[1]}"
        return next((d for k in official if (d := first_difference(official[k], production[k], f"{at}.{k}"))), None)
    if isinstance(official, list) and isinstance(production, list):
        if len(official) != len(production):
            return f"{at}: {len(official)} items, production {len(production)}"
        return next((d for i, (o, p) in enumerate(zip(official, production))
                     if (d := first_difference(o, p, f"{at}[{i}]"))), None)
    if same_json(official, production):
        return None
    return f"{at}: {json.dumps(official, ensure_ascii=False)[:80]}, production {json.dumps(production, ensure_ascii=False)[:80]}"


def difference(official, answered):
    """How an answer differs from the official emulator's, in a line."""
    if official["status"] != answered["status"]:
        return f"status {official['status']}, production {answered['status']}"
    if media(official.get("contentType")) != media(answered.get("contentType")):
        return f"media type {media(official.get('contentType'))}, production {media(answered.get('contentType'))}"
    try:
        return first_difference(json.loads(official.get("body") or ""), json.loads(answered["body"])) or "the body"
    except ValueError:
        return "the body, as text"


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
        if same_json(official, answered):
            result["same"] += 1
        else:
            where = "/" + case["parent"] if case["parent"] else ""
            result["differ"].append({"name": case["name"], "method": "POST",
                                     "path": f"/v1/projects/{{project}}/databases/(default)/documents{where}:{verb}",
                                     "difference": first_difference(official, answered) or "the outcome",
                                     "official": official, "production": answered})


def replay(production, fixture):
    """Sends a recording's cases; what each answered next to what the official emulator did."""
    recording = json.loads(Path(fixture).read_text())
    project = production.project
    result = {"fixture": Path(fixture).name, "cases": len(recording["cases"]), "sent": 0, "same": 0,
              "skipped": {}, "emulatorDeletes": 0, "needsIndex": [], "indexes": [], "differ": []}
    production.empty()
    log(f"{result['fixture']}: {len(recording['cases'])} cases")
    if "dataset" in recording:
        replay_queries(production, recording, result)
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
        same = status == official["status"] and media(content_type) == media(official.get("contentType"))
        if case.get("compare", "exact") == "exact":
            same = same and same_body(official.get("body") or "", answered["body"])
        if same:
            result["same"] += 1
        else:
            result["differ"].append({"name": case["name"], "method": case["method"], "path": case["path"],
                                     "difference": difference(official, answered), "official": official,
                                     "production": answered})
    production.empty()
    return result


def markdown(report):
    lines = ["## Production parity", "",
             f"Recordings of the official emulator replayed against production Cloud Firestore on {report['started']}.", "",
             "| Recording | Cases | Sent | Same | Differ | Needs an index | Not sent |",
             "|---|---:|---:|---:|---:|---:|---:|"]
    for r in report["recordings"]:
        lines.append(f"| `{r['fixture']}` | {r['cases']} | {r['sent']} | {r['same']} | {len(r['differ'])} | "
                     f"{len(r['needsIndex'])} | {sum(r['skipped'].values())} |")
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
                lines.append(f"| {name} | {d['official']['status']} | {d['production']['status']} | {what} |")
            if len(r["differ"]) > 300:
                lines.append(f"| … {len(r['differ']) - 300} more in `report.json` | | | |")
    return "\n".join(lines) + "\n"


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--project", required=True, help="the project of the maintainers' own")
    parser.add_argument("--out", required=True, help="where report.json and report.md go")
    parser.add_argument("--base", default="https://firestore.googleapis.com")
    parser.add_argument("--budget", type=int, default=5000, help="the most requests a run sends")
    parser.add_argument("fixtures", nargs="+")
    args = parser.parse_args(argv)
    token = os.environ.get(TOKEN_ENV)
    if not token:
        parser.error(f"{TOKEN_ENV} is not set")
    production = Production(args.base, args.project, token, args.budget)
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
