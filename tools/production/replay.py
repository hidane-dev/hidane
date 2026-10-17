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
counted apart, not compared. The database is emptied before each recording and at the end.

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
from harness import mask  # noqa: E402

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

    def empty(self):
        """Deletes every document of the database, those under missing documents included."""
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

        def walk(parent):
            for collection in collections(parent):
                token = None
                while True:
                    query = "?pageSize=300&showMissing=true&mask.fieldPaths=__name__"
                    if token:
                        query += "&pageToken=" + urllib.parse.quote(token, safe="")
                    answer = self.call("GET", f"/v1/{url(parent)}/{urllib.parse.quote(collection, safe='')}{query}")
                    for document in answer.get("documents", []):
                        walk(document["name"])
                        names.append(document["name"])
                    token, previous = answer.get("nextPageToken"), token
                    if not token or token == previous:
                        break

        walk(documents)
        log(f"emptying the database: {len(names)} documents")
        for start in range(0, len(names), 500):
            writes = [{"delete": name} for name in names[start:start + 500]]
            self.call("POST", f"/v1/{documents}:commit", {"writes": writes})
        return len(names)


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
# Production's refusal of a query that needs a composite index. The emulators never ask for one;
# such a case is counted apart until the replay creates the indexes. The link to create the
# index names the project in base64, so it is masked.
NEEDS_INDEX = "The query requires an index."
INDEX_LINK = re.compile(r'https://console\.firebase\.google\.com/[^"\s]*')


def same_string(official, production):
    """Whether two strings are the same, a placeholder in the official one matching any text."""
    parts = PLACEHOLDER.split(official)
    if len(parts) == 1:
        return official == production
    pattern = "".join(".+?" if n % 2 else re.escape(part) for n, part in enumerate(parts))
    return re.fullmatch(pattern, production, re.S) is not None


def same_json(official, production):
    """Whether two JSON values are the same, a recording's placeholder matching anything."""
    if isinstance(official, str):
        return isinstance(production, str) and same_string(official, production)
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


def media(content_type):
    return (content_type or "").split(";")[0].strip().lower() or None


def replay(production, fixture):
    """Sends a recording's cases; what each answered next to what the official emulator did."""
    recording = json.loads(Path(fixture).read_text())
    project = production.project
    result = {"fixture": Path(fixture).name, "cases": len(recording["cases"]), "sent": 0, "same": 0,
              "skipped": {}, "needsIndex": [], "differ": []}
    production.empty()
    log(f"{result['fixture']}: {len(recording['cases'])} cases")
    for n, case in enumerate(recording["cases"], 1):
        if n % 50 == 0:
            log(f"{result['fixture']}: case {n}, {production.sent} requests so far")
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
        if status == 400 and NEEDS_INDEX in text and official["status"] < 400:
            result["needsIndex"].append(case["name"])
            continue
        same = status == official["status"] and media(content_type) == media(official.get("contentType"))
        if case.get("compare", "exact") == "exact":
            same = same and same_body(official.get("body") or "", answered["body"])
        if same:
            result["same"] += 1
        else:
            result["differ"].append({"name": case["name"], "method": case["method"], "path": case["path"],
                                     "official": official, "production": answered})
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
    for r in report["recordings"]:
        if r["differ"]:
            lines += ["", f"### `{r['fixture']}`", "", "| Case | Official | Production |", "|---|---|---|"]
            for d in r["differ"][:200]:
                name = d["name"].replace("|", "\\|")
                lines.append(f"| {name} | {d['official']['status']} | {d['production']['status']} |")
            if len(r["differ"]) > 200:
                lines.append(f"| … {len(r['differ']) - 200} more in `report.json` | | |")
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
