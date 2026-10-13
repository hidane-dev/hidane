#!/usr/bin/env python3
"""Opens or updates one GitHub issue per unexplained difference a fuzz run found.

Usage (in the Differential fuzzing workflow; GH_TOKEN with `issues: write`):

    python3 -I tools/fuzz/issues.py <out> [--dry-run] [--max-new 10]

`<out>` is the `--out` of `fuzz.py run` (its `summary.json`). Each issue carries the label
`fuzz` (the Upstream workflow's issues carry `upstream`) and starts with a marker that names the
difference's signature: `<!-- hidane-fuzz signature="…" first-run="…" -->`. A later run that
finds the same signature updates the open issue instead of filing another, and leaves a closed
one closed (fixed, or accepted into docs/parity-exceptions.md). At most `--max-new` issues are
opened per run, the most severe first; the rest wait for the next run.

The issue holds only values checked against strict patterns (signature, kind, area, request,
statuses, codes, counts, seed and case) and the error messages, escaped inside a code block:
everything the emulators answered is in the run's artifact. With `--dry-run` it only says what
it would do. It talks to `https://api.github.com` only, for the repository in
GITHUB_REPOSITORY. Stdlib only.
"""

import argparse
import json
import os
import re
import sys
import urllib.error
import urllib.request
from datetime import date
from pathlib import Path

LABEL = "fuzz"
LABEL_COLOR = "5319e7"
LABEL_DESCRIPTION = "A difference from the official emulator found by differential fuzzing (tools/fuzz/)"
EXTRA_LABELS = ["area:parity"]
SEVERITY = {"crash": 0, "data": 1, "status": 2, "order": 3, "message": 4}
AREAS = {"documents", "queries", "transactions", "batches", "transforms", "rules", "malformed"}
SIGNATURE = re.compile(r"^(crash|status|message|order|data)-[a-z0-9-]{1,40}-[0-9a-f]{12}$")
VERB = re.compile(r"^[A-Za-z :]{1,40}$")
CODE = re.compile(r"^[A-Z_]{1,40}$")
REPOSITORY = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")
MARKER = re.compile(r'^<!-- hidane-fuzz signature="([a-z0-9-]+)" first-run="([0-9]*)" -->')


class Stop(Exception):
    pass


def valid(entry):
    """The entry's fields this script uses, checked, or None when one is not as expected."""
    try:
        if entry["outcome"] != "unexplained" or not SIGNATURE.match(entry["signature"]):
            return None
        if entry["kind"] not in SEVERITY or entry["area"] not in AREAS or not VERB.match(entry["verb"]):
            return None
        sides = {}
        for side in ("official", "hidane"):
            s = entry[side]
            status = s.get("status")
            if status is not None and not (isinstance(status, int) and 100 <= status <= 599):
                return None
            code = s.get("code")
            if code is not None and not CODE.match(code):
                return None
            error = s.get("error")
            if error is not None and not CODE.match(error.upper()):
                return None
            message = s.get("message")
            sides[side] = {"status": status, "code": code, "error": error,
                           "message": message if isinstance(message, str) else None}
        found = entry["found"]
        numbers = [found["seed"], found["case"], found["step"], entry["occurrences"]]
        if not all(isinstance(n, int) and n >= 0 for n in numbers):
            return None
        return {"signature": entry["signature"], "kind": entry["kind"], "area": entry["area"],
                "verb": entry["verb"], "occurrences": entry["occurrences"], "seed": found["seed"],
                "case": found["case"], "step": found["step"], **sides}
    except (KeyError, TypeError, AttributeError):
        return None


def quote(text, limit=500):
    """Text from an emulator, safe inside a code block: no backticks, angle brackets or line
    breaks that could end it or look like markup, and short."""
    text = text.replace("`", "'").replace("<", "‹").replace(">", "›")
    text = json.dumps(text, ensure_ascii=False)[1:-1]
    return text[:limit] + ("…" if len(text) > limit else "")


def side_text(s):
    if s["status"] is None:
        return f"no answer ({s['error'] or 'unknown'})"
    return f"{s['status']} {s['code'] or ''}".strip()


def title(f):
    return f"Differential fuzzing: {f['kind']} difference in {f['verb'].strip()} ({f['signature']})"


def body(f, run, first_run):
    lines = [
        f'<!-- hidane-fuzz signature="{f["signature"]}" first-run="{first_run}" -->',
        "The differential fuzzer (`tools/fuzz/`) sent the same requests to the official emulator and to "
        "hidane and got different answers, and no rule in `tools/fuzz/exceptions.json` accepts the "
        "difference.",
        "",
        "| | |",
        "|---|---|",
        f"| Signature | `{f['signature']}` |",
        f"| Kind | {f['kind']} |",
        f"| Request | `{f['verb'].strip()}` (step {f['step']} of the case) |",
        f"| Official emulator | {side_text(f['official'])} |",
        f"| hidane | {side_text(f['hidane'])} |",
        f"| Found in | area `{f['area']}`, seed {f['seed']}, case {f['case']} |",
        f"| Last seen | {date.today().isoformat()}, {f['occurrences']} times in [run {run['id']}]({run['url']}) |",
    ]
    messages = [(side, f[side]["message"]) for side in ("official", "hidane") if f[side]["message"]]
    if messages:
        lines += ["", "Error messages (escaped):", "", "```text"]
        lines += [f"{side}: {quote(message)}" for side, message in messages]
        lines += ["```"]
    lines += [
        "",
        f"The minimised case is `differences/{f['signature']}.json` in the run's `fuzz` artifact. To see it "
        "and run it again (tools/fuzz/README.md):",
        "",
        "```sh",
        f"python3 -I tools/fuzz/fuzz.py show --seed {f['seed']} --case {f['case']}",
        "python3 -I tools/fuzz/fuzz.py replay --official <host:port> --hidane <host:port> "
        f"differences/{f['signature']}.json",
        "```",
        "",
        "If hidane is wrong, add the case to a recorder in `tools/oracle/` and record its fixture again. "
        "If hidane differs on purpose, add the difference to `docs/parity-exceptions.md` and a rule to "
        "`tools/fuzz/exceptions.json`, then close this issue. A later run updates this issue while it is "
        "open and leaves it closed once closed.",
    ]
    return "\n".join(lines) + "\n"


class GitHub:
    def __init__(self, repository, token, api="https://api.github.com"):
        if api != "https://api.github.com":
            raise Stop("GITHUB_API_URL is not https://api.github.com")
        if not REPOSITORY.match(repository or ""):
            raise Stop("GITHUB_REPOSITORY is not owner/name")
        self.base = f"{api}/repos/{repository}"
        self.token = token

    def call(self, method, path, payload=None):
        data = None if payload is None else json.dumps(payload).encode()
        request = urllib.request.Request(self.base + path, data=data, method=method, headers={
            "Accept": "application/vnd.github+json", "Authorization": f"Bearer {self.token}",
            "X-GitHub-Api-Version": "2022-11-28", "Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                raw = response.read()
                return response.status, json.loads(raw) if raw else None
        except urllib.error.HTTPError as err:
            return err.code, None


def existing(api):
    """Issues labelled `fuzz` (open and closed, not pull requests) by the signature their marker names."""
    found = {}
    for page in range(1, 51):
        status, data = api.call("GET", f"/issues?labels={LABEL}&state=all&per_page=100&page={page}")
        if status != 200 or not isinstance(data, list):
            raise Stop(f"listing the {LABEL} issues: HTTP {status}")
        for issue in data:
            if "pull_request" in issue:
                continue
            match = MARKER.match(issue.get("body") or "")
            if match and match.group(1) not in found:
                found[match.group(1)] = {"number": issue["number"], "state": issue["state"],
                                         "first_run": match.group(2)}
        if len(data) < 100:
            break
    return found


def labels(api):
    status, _ = api.call("GET", f"/labels/{LABEL}")
    if status == 404:
        status, _ = api.call("POST", "/labels", {"name": LABEL, "color": LABEL_COLOR,
                                                 "description": LABEL_DESCRIPTION})
        if status not in (201, 422):
            raise Stop(f"creating the {LABEL} label: HTTP {status}")
    out = [LABEL]
    for label in EXTRA_LABELS:
        if api.call("GET", "/labels/" + label.replace(":", "%3A"))[0] == 200:
            out.append(label)
    return out


def plan(findings, known, max_new):
    """What to do for each finding: ("update", issue), ("closed", issue), ("open", None) or ("later", None)."""
    actions = []
    new = 0
    for f in sorted(findings, key=lambda f: (SEVERITY[f["kind"]], -f["occurrences"], f["signature"])):
        issue = known.get(f["signature"])
        if issue and issue["state"] == "open":
            actions.append((f, "update", issue))
        elif issue:
            actions.append((f, "closed", issue))
        elif new < max_new:
            new += 1
            actions.append((f, "open", None))
        else:
            actions.append((f, "later", None))
    return actions


def run_info():
    server = os.environ.get("GITHUB_SERVER_URL", "https://github.com")
    repository = os.environ.get("GITHUB_REPOSITORY", "")
    run_id = os.environ.get("GITHUB_RUN_ID", "")
    if server != "https://github.com" or not REPOSITORY.match(repository) or not run_id.isdigit():
        return {"id": run_id if run_id.isdigit() else "?", "url": "https://github.com"}
    return {"id": run_id, "url": f"{server}/{repository}/actions/runs/{run_id}"}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("out", help="the --out directory of fuzz.py run")
    parser.add_argument("--dry-run", action="store_true", help="say what would be done, change nothing")
    parser.add_argument("--max-new", type=int, default=10, help="issues to open at most in this run")
    parser.add_argument("--summary", help="a Markdown file to append what was done to (GITHUB_STEP_SUMMARY)")
    args = parser.parse_args(argv)
    try:
        summary = json.loads(Path(args.out, "summary.json").read_text())
        entries = [e for e in summary.get("differences", []) if e.get("outcome") == "unexplained"]
        findings = [f for f in (valid(e) for e in entries) if f]
        skipped = len(entries) - len(findings)
        api = GitHub(os.environ.get("GITHUB_REPOSITORY"), os.environ.get("GH_TOKEN", ""),
                     os.environ.get("GITHUB_API_URL", "https://api.github.com"))
        known = existing(api)
        actions = plan(findings, known, args.max_new)
        run = run_info()
        names = None
        lines = [f"## Issues{' (dry run)' if args.dry_run else ''}", ""]
        for f, what, issue in actions:
            if what == "update":
                if not args.dry_run:
                    status, _ = api.call("PATCH", f"/issues/{issue['number']}",
                                         {"title": title(f), "body": body(f, run, issue["first_run"])})
                    if status != 200:
                        raise Stop(f"updating #{issue['number']}: HTTP {status}")
                lines.append(f"- `{f['signature']}`: updated #{issue['number']}")
            elif what == "closed":
                lines.append(f"- `{f['signature']}`: #{issue['number']} is closed; left as it is")
            elif what == "open":
                if args.dry_run:
                    lines.append(f"- `{f['signature']}`: would open an issue")
                    continue
                names = names or labels(api)
                status, data = api.call("POST", "/issues", {"title": title(f), "body": body(f, run, run["id"]),
                                                            "labels": names})
                if status != 201:
                    raise Stop(f"opening an issue for {f['signature']}: HTTP {status}")
                lines.append(f"- `{f['signature']}`: opened #{data['number']}")
            else:
                lines.append(f"- `{f['signature']}`: waits for a later run (at most {args.max_new} new issues a run)")
        if not actions:
            lines.append("No unexplained difference: nothing to file.")
        if skipped:
            lines.append(f"- {skipped} differences left out: a field did not have the expected form")
        text = "\n".join(lines) + "\n"
        print(text)
        if args.summary:
            with open(args.summary, "a", encoding="utf-8") as f:
                f.write(text)
    except Stop as err:
        print(f"issues.py: {err}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
