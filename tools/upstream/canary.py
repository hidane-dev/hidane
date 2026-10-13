"""The compatibility canary: hidane against new upstream releases (tools/upstream/README.md).

Usage (from the repository root; the Upstream workflow runs each step):
    python3 tools/upstream/canary.py replay --recordings DIR --out FILE [--log FILE]
        Puts each recording a new official emulator made that differs from the committed
        fixture (its result.json says `different`) in the fixture's place, runs hidane's tests,
        and writes what failed. A test that fails is run again with the committed fixtures: it
        is a regression only if it passes there.
    python3 tools/upstream/canary.py report --upstream FILE --artifacts DIR --out FILE
        [--summary FILE] [--outputs FILE]
        Builds the compatibility report from the watcher's report (watch.py detect) and the
        canary's results in DIR, with one finding per release whose difference is confirmed.
    python3 tools/upstream/canary.py issues --report FILE [--dry-run] [--summary FILE]
        Opens one issue per finding, or updates the open one that carries the same marker
        (GITHUB_REPOSITORY, GH_TOKEN, GITHUB_API_URL). With --dry-run it only reads the issues
        and says what it would do.

Every value written to the report or an issue was checked against a strict pattern by watch.py
or here; nothing else from a registry or a recording is copied into them. Stdlib only.
"""

import argparse
import json
import os
import re
import subprocess
import sys
import urllib.error
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import watch  # noqa: E402  (tools/upstream/watch.py)

ROOT = watch.ROOT
SCHEMA_VERSION = 1
README = "tools/upstream/README.md"
RECORDINGS_WORKFLOW = ".github/workflows/official-recordings.yml"
LABEL = "upstream"
LABEL_COLOR = "c5def5"
LABEL_DESCRIPTION = "A new upstream release differs (tools/upstream)"
EXTRA_LABELS = ("area:parity",)

RECORDER = re.compile(r"[a-z][a-z0-9_]{0,63}")
FIXTURE = re.compile(r"crates/[a-z0-9-]{1,64}/tests/fixtures/[a-z0-9_]{1,64}\.json")
SLUG = re.compile(r"v[0-9.]{1,40}-[0-9a-f]{12}")
KEY = re.compile(r"[a-z]{2,10}:[A-Za-z0-9@/._:+-]{1,200}")
REPOSITORY = re.compile(r"[A-Za-z0-9_.-]{1,100}/[A-Za-z0-9_.-]{1,100}")
RUN_ID = re.compile(r"[0-9]{1,20}")

# A recorder's result (official-recordings.yml).
IDENTICAL, DIFFERENT, UNSTABLE, ERROR, MISSING = (
    "identical", "different", "unstable", "error", "missing")
RESULTS = (IDENTICAL, DIFFERENT, UNSTABLE, ERROR)
# A release's verdict.
SAME, DIFFERS, INCONCLUSIVE, NOT_RUN = "same", "different", "inconclusive", "not-run"


class Stop(Exception):
    pass


def load_json(path):
    with open(path, encoding="utf-8") as f:
        return json.load(f)


# replay -----------------------------------------------------------------------------------------

def recorder_results(directory, root=ROOT):
    """The result.json files under a directory, checked: [(result, recording path)]."""
    found = []
    for path in sorted(Path(directory).rglob("result.json")):
        data = load_json(path)
        recorder, fixture, result = data.get("recorder"), data.get("fixture"), data.get("result")
        if not (watch.valid(RECORDER, recorder) and watch.valid(FIXTURE, fixture)
                and result in RESULTS):
            raise Stop(f"{path}: not a result of official-recordings.yml")
        if not (root / fixture).is_file():
            raise Stop(f"{path}: {fixture} is not a fixture of the repository")
        found.append((dict(recorder=recorder, fixture=fixture, result=result,
                           version=data.get("version") if watch.valid(
                               watch.EMULATOR_VERSION, data.get("version")) else None),
                      path.parent / f"{recorder}.json"))
    return found


_RUNNING = re.compile(r"^\s*(?:Running|Doc-tests)\s+(\S+)")
_FAILED = re.compile(r"^test (.+?) \.\.\. FAILED\s*$")


def failures(output):
    """The tests `cargo test` reported as failed, as `<target>::<test>`."""
    target, out = "?", []
    for line in output.splitlines():
        running = _RUNNING.match(line)
        if running:
            target = running.group(1)
            continue
        failed = _FAILED.match(line)
        if failed:
            out.append(f"{target}::{failed.group(1)}")
    return sorted(set(out))


def cargo_test(run, log):
    result = run(["cargo", "test", "--workspace", "--locked", "--no-fail-fast"])
    log.append(result.stdout)
    return result.returncode, failures(result.stdout)


def default_run(command):
    return subprocess.run(command, cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                          text=True, errors="replace")


def replay(directory, run=default_run, root=ROOT):
    """Runs hidane's tests with the confirmed new recordings in place of the fixtures."""
    changed = {}
    for result, recording in recorder_results(directory, root):
        if result["result"] == DIFFERENT:
            if not recording.is_file():
                raise Stop(f"{recording}: missing")
            changed[result["fixture"]] = recording.read_bytes()
    originals = {fixture: (root / fixture).read_bytes() for fixture in changed}
    log = []
    try:
        for fixture, data in changed.items():
            (root / fixture).write_bytes(data)
        code, failed = cargo_test(run, log)
        # Without a changed fixture every failure is hidane's own; with one, a test is a
        # regression only if it passes with the committed fixtures.
        preexisting = failed
        if failed and changed:
            for fixture, data in originals.items():
                (root / fixture).write_bytes(data)
            _, preexisting = cargo_test(run, log)
    finally:
        for fixture, data in originals.items():
            (root / fixture).write_bytes(data)
    if code == 0:
        tests = "passed"
    elif failed:
        tests = "failed"
    else:
        tests = ERROR  # it did not build, or failed without naming a test
    return {"changed": sorted(changed), "tests": tests, "failures": failed,
            "regressions": [t for t in failed if t not in preexisting],
            "preexisting": [t for t in failed if t in preexisting]}, "\n".join(log)


# report -----------------------------------------------------------------------------------------

def repository_url():
    repository = os.environ.get("GITHUB_REPOSITORY", "hidane-dev/hidane")
    if os.environ.get("GITHUB_SERVER_URL", "https://github.com") != "https://github.com" or \
            not watch.valid(REPOSITORY, repository):
        return "https://github.com/hidane-dev/hidane"
    return f"https://github.com/{repository}"


def run_url():
    run_id = os.environ.get("GITHUB_RUN_ID", "")
    return f"{repository_url()}/actions/runs/{run_id}" if watch.valid(RUN_ID, run_id) else None


def expected_recorders(root=ROOT):
    """The recorders official-recordings.yml runs, each of which must have left a result."""
    path = root / RECORDINGS_WORKFLOW
    if not path.is_file():
        return []
    return re.findall(r"^\s*- recorder: ([a-z][a-z0-9_]*)\s*$", path.read_text(encoding="utf-8"),
                      re.M)


def emulator_entry(candidate, artifacts, root=ROOT):
    """One official emulator release: its recordings, hidane's tests, the verdict."""
    slug = candidate["slug"]
    prefix = f"canary-{slug}-"
    recordings, replayed = [], None
    for path in sorted(Path(artifacts).glob(f"{prefix}*")):
        if path.name == f"{prefix}hidane":
            replay_file = path / "replay.json"
            replayed = load_json(replay_file) if replay_file.is_file() else None
            continue
        results = recorder_results(path, root)
        if not results:
            recordings.append({"recorder": path.name[len(prefix):], "fixture": None,
                               "result": MISSING})
        for result, _ in results:
            recording = {k: result[k] for k in ("recorder", "fixture", "result")}
            if result["version"] != candidate["version"]:
                recording["result"] = ERROR  # not a recording of this release
            recordings.append(recording)
    seen = {r["recorder"] for r in recordings}
    for recorder in expected_recorders(root):
        if recorder not in seen:  # its job left nothing
            recordings.append({"recorder": recorder, "fixture": None, "result": MISSING})
    hidane = {"tests": NOT_RUN, "regressions": [], "preexisting": [], "changed": []}
    if isinstance(replayed, dict) and replayed.get("tests") in ("passed", "failed", ERROR):
        hidane = {"tests": replayed["tests"],
                  "changed": [f for f in replayed.get("changed", []) if watch.valid(FIXTURE, f)],
                  "regressions": [shown_test(t) for t in replayed.get("regressions", [])],
                  "preexisting": [shown_test(t) for t in replayed.get("preexisting", [])]}
    results = [r["result"] for r in recordings]
    if not recordings or any(r in (ERROR, MISSING) for r in results) or hidane["tests"] in (
            NOT_RUN, ERROR):
        verdict = INCONCLUSIVE  # something could not be checked
    elif DIFFERENT in results or hidane["regressions"]:
        verdict = DIFFERS  # confirmed
    elif UNSTABLE in results or hidane["preexisting"]:
        verdict = INCONCLUSIVE  # not confirmed, or hidane's tests fail on their own
    else:
        verdict = SAME
    fixtures = list(root.glob("crates/*/tests/fixtures/*.json"))
    entry = {k: candidate[k] for k in ("key", "slug", "version", "sha256", "channel")}
    entry.update(pinned_by=candidate.get("pinned_by", []), recordings=recordings, hidane=hidane,
                 fixtures_recorded=len(recordings), fixtures_total=len(fixtures),
                 verdict=verdict)
    return entry


def shown_test(name):
    return re.sub(r"[^0-9A-Za-z_./:() -]", "?", str(name))[:160]


# Why the canary does not run a harness for a package yet; tools/upstream/README.md has the list.
NOT_RUN_REASONS = {
    "npm:firebase-tools": "the canary records against the jar it pins (above) when that jar is "
                          "new; its CLI (`hidane exec`, exports and imports) is not run yet",
    "npm:firebase-functions": "tools/oracle/functions/ needs firebase-tools and both emulators; "
                              "not run by the canary yet",
    "pub:cloud_firestore": "tools/oracle/flutter_web/ needs Flutter and a browser; not run by the "
                           "canary yet",
}


def sdk_entry(sdk):
    entry = {k: sdk[k] for k in ("key", "ecosystem", "name", "version", "channel", "verified")}
    ident = f"{sdk['ecosystem']}:{sdk['name']}"
    if not sdk["verified"]:
        reason = "watched without being verified: check the Firestore client it depends on"
    else:
        reason = NOT_RUN_REASONS.get(ident, "the canary does not run its harness yet")
    entry.update(result=NOT_RUN, reason=reason)
    return entry


def report(upstream, artifacts, root=ROOT):
    emulators, sdks = watch.canary_inputs(upstream)
    entries = [emulator_entry(c, artifacts, root) for c in emulators]
    sdk_entries = [sdk_entry(s) for s in sdks]
    url = run_url()
    findings = [finding_for_emulator(e, upstream, url) for e in entries if e["verdict"] == DIFFERS]
    return {
        "schema_version": SCHEMA_VERSION,
        "generated": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "run": url,
        "watched": {"emulator": upstream["emulator"]["watched"],
                    "packages": {p["id"]: p["watched"] for p in upstream["packages"]}},
        "emulators": entries,
        "sdks": sdk_entries,
        "findings": findings,
        "conclusive": all(e["verdict"] != INCONCLUSIVE for e in entries),
    }


def marker(key):
    if not watch.valid(KEY, key):
        raise Stop(f"not a finding key: {watch.shown(key)}")
    return f'<!-- hidane-upstream-canary key="{key}" -->'


def recordings_table(entry):
    lines = ["| Recording | Recorder | Result |", "|---|---|---|"]
    for r in entry["recordings"]:
        result = r["result"] if r["result"] == IDENTICAL else f"**{r['result']}**"
        fixture = f"`{r['fixture']}`" if r["fixture"] else "–"
        lines.append(f"| {fixture} | `{r['recorder']}.py` | {result} |")
    return lines


def hidane_line(entry):
    hidane = entry["hidane"]
    changed = len(hidane["changed"])
    if hidane["tests"] not in ("passed", "failed"):
        return f"hidane's tests (`cargo test --workspace`) did not run to the end ({hidane['tests']})."
    what = "pass" if hidane["tests"] == "passed" else "**fail**"
    if changed:
        line = (f"hidane's tests (`cargo test --workspace`) with the {changed} recording(s) that "
                f"differ in place of the committed fixtures: {what}.")
    else:
        line = (f"hidane's tests (`cargo test --workspace`), which replay the committed fixtures "
                f"these recordings match: {what}.")
    if hidane["regressions"]:
        line += " Failing with the new recordings, passing with the committed ones: " + ", ".join(
            f"`{t}`" for t in hidane["regressions"]) + "."
    if hidane["preexisting"]:
        line += " Failing with the committed recordings too: " + ", ".join(
            f"`{t}`" for t in hidane["preexisting"]) + "."
    return line


def finding_for_emulator(entry, upstream, url):
    watched = upstream["emulator"]["watched"]
    title = (f"Upstream canary: official emulator v{entry['version']} ({entry['channel']}) "
             "answers differently")
    body = [
        marker(entry["key"]),
        f"The Upstream workflow's compatibility canary recorded against the official Cloud "
        f"Firestore emulator **v{entry['version']}** (SHA-256 `{entry['sha256']}`; "
        f"{watch.origin(entry['pinned_by'])}) and compared each recording with the committed "
        f"fixture, recorded from "
        f"v{watched['version']}. A difference counts only when two freshly started emulators "
        "recorded the same thing.",
        "",
        *recordings_table(entry),
        "",
        hidane_line(entry),
        "",
        f"Re-recorded by the canary: {entry['fixtures_recorded']} of the "
        f"{entry['fixtures_total']} fixtures. The others were not checked against this release.",
        "",
        f"Run: {url}" if url else "Run: (not recorded)",
        f"The recordings are in the run's `canary-{entry['slug']}-*` artifacts, the report in "
        "`compatibility`.",
        "",
        f"What to do: [{README}]({repository_url()}/blob/main/{README}#acting-on-a-canary-issue).",
        "",
        "<sub>Filed by the Upstream workflow, which updates this issue while the release stays "
        "new and files no other for it.</sub>",
    ]
    return {"key": entry["key"], "title": title, "body": "\n".join(body) + "\n"}


def summary(data):
    lines = ["## Compatibility canary", ""]
    if data["run"]:
        lines += [f"Run: {data['run']}", ""]
    watched = data["watched"]["emulator"]
    if not data["emulators"]:
        lines += [f"No new official emulator: the canary did not record (v{watched['version']} "
                  "is pinned).", ""]
    for entry in data["emulators"]:
        lines += [f"### Official emulator v{entry['version']} ({entry['channel']}): "
                  f"{entry['verdict']}", "",
                  f"SHA-256 `{entry['sha256']}`; {watch.origin(entry['pinned_by'])}. Compared "
                  f"with the fixtures recorded from v{watched['version']}; re-recorded "
                  f"{entry['fixtures_recorded']} of the {entry['fixtures_total']}.", "",
                  *recordings_table(entry), "", hidane_line(entry), ""]
    if data["sdks"]:
        lines += ["### New SDK and tool releases", "",
                  "| Package | Release | Channel | Canary |", "|---|---|---|---|"]
        for sdk in data["sdks"]:
            lines.append(f"| {sdk['ecosystem']}: `{sdk['name']}` | `{sdk['version']}` | "
                         f"{sdk['channel']} | {sdk['result']}: {sdk['reason']} |")
        lines.append("")
    if data["findings"]:
        lines += ["### Findings", "", "Each gets an issue (one per release, updated if open):", ""]
        lines += [f"- {f['title']}" for f in data["findings"]]
        lines.append("")
    elif data["emulators"]:
        lines += ["No confirmed difference: no issue.", ""]
    lines.append(f"Nothing here makes a release verified; `{README}` says how to move to one.")
    return "\n".join(lines) + "\n"


# issues -----------------------------------------------------------------------------------------

class GitHub:
    """The few REST calls the issues step makes, with GITHUB_TOKEN."""

    def __init__(self, repository, token, api="https://api.github.com"):
        if not watch.valid(REPOSITORY, repository):
            raise Stop("GITHUB_REPOSITORY is not owner/name")
        if api != "https://api.github.com":
            raise Stop("GITHUB_API_URL is not https://api.github.com")
        self.base = f"{api}/repos/{repository}"
        self.token = token

    def call(self, method, path, body=None):
        data = json.dumps(body).encode() if body is not None else None
        request = urllib.request.Request(self.base + path, data=data, method=method, headers={
            "Authorization": f"Bearer {self.token}", "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28", "User-Agent": watch.USER_AGENT,
            "Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                text = response.read()
                return response.status, json.loads(text) if text else None
        except urllib.error.HTTPError as err:
            return err.code, None


def labelled_issues(api):
    """Every issue (open or closed, not pull requests) with the label."""
    issues, page = [], 1
    while True:
        status, data = api.call("GET", f"/issues?labels={LABEL}&state=all&per_page=100&page={page}")
        if status == 404:
            return issues
        if status != 200 or not isinstance(data, list):
            raise Stop(f"listing issues: HTTP {status}")
        issues += [i for i in data if "pull_request" not in i]
        if len(data) < 100:
            return issues
        page += 1


def issue_labels(api):
    """The labels a new issue gets: LABEL, created if missing, and those of EXTRA_LABELS that
    exist."""
    status, _ = api.call("GET", f"/labels/{LABEL}")
    if status == 404:
        status, _ = api.call("POST", "/labels", {"name": LABEL, "color": LABEL_COLOR,
                                                 "description": LABEL_DESCRIPTION})
        if status != 201:
            raise Stop(f"creating the {LABEL} label: HTTP {status}")
    labels = [LABEL]
    for label in EXTRA_LABELS:
        status, _ = api.call("GET", f"/labels/{label.replace(':', '%3A')}")
        if status == 200:
            labels.append(label)
    return labels


def file_issues(findings, api, dry_run):
    """Opens or updates one issue per finding; what was done, line by line."""
    done = []
    if not findings:
        return ["No finding: no issue."]
    existing = labelled_issues(api)
    labels = None
    for finding in findings:
        mark = marker(finding["key"])
        same = [i for i in existing if mark in (i.get("body") or "")]
        open_issue = next((i for i in same if i.get("state") == "open"), None)
        if open_issue is not None:
            number = open_issue["number"]
            if (open_issue.get("body") or "") == finding["body"]:
                done.append(f"#{number} already says this: {finding['title']}")
            elif dry_run:
                done.append(f"would update #{number}: {finding['title']}")
            else:
                status, _ = api.call("PATCH", f"/issues/{number}", {"body": finding["body"]})
                if status != 200:
                    raise Stop(f"updating #{number}: HTTP {status}")
                done.append(f"updated #{number}: {finding['title']}")
        elif same:
            numbers = ", ".join(f"#{i['number']}" for i in same)
            done.append(f"{numbers} closed for this release; not reopened: {finding['title']}")
        elif dry_run:
            done.append(f"would open: {finding['title']}")
        else:
            if labels is None:
                labels = issue_labels(api)
            status, data = api.call("POST", "/issues", {
                "title": finding["title"], "body": finding["body"], "labels": labels})
            if status != 201:
                raise Stop(f"opening an issue: HTTP {status}")
            done.append(f"opened #{data['number']}: {finding['title']}")
    return done


# main -------------------------------------------------------------------------------------------

def write_outputs(path, data):
    with open(path, "a", encoding="utf-8") as f:
        f.write(f"findings={len(data['findings'])}\n")
        f.write(f"conclusive={'true' if data['conclusive'] else 'false'}\n")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    sub = parser.add_subparsers(dest="command", required=True)
    p = sub.add_parser("replay")
    p.add_argument("--recordings", required=True)
    p.add_argument("--out", required=True)
    p.add_argument("--log")
    p = sub.add_parser("report")
    p.add_argument("--upstream", required=True)
    p.add_argument("--artifacts", required=True)
    p.add_argument("--out", required=True)
    p.add_argument("--summary")
    p.add_argument("--outputs")
    p = sub.add_parser("issues")
    p.add_argument("--report", required=True)
    p.add_argument("--dry-run", action="store_true")
    p.add_argument("--summary")
    args = parser.parse_args(argv)
    try:
        if args.command == "replay":
            result, log = replay(args.recordings)
            write_json(args.out, result)
            if args.log:
                Path(args.log).write_text(log, encoding="utf-8")
            print(json.dumps(result, indent=1))
            return 0
        if args.command == "report":
            upstream = load_json(args.upstream)
            data = report(upstream, args.artifacts)
            write_json(args.out, data)
            text = summary(data)
            append(args.summary, text)
            if args.outputs:
                write_outputs(args.outputs, data)
            return 0
        data = load_json(args.report)
        api = GitHub(os.environ.get("GITHUB_REPOSITORY", ""), os.environ.get("GH_TOKEN", ""),
                     os.environ.get("GITHUB_API_URL", "https://api.github.com"))
        done = file_issues(data["findings"], api, args.dry_run)
        head = "## Canary issues (dry run)" if args.dry_run else "## Canary issues"
        append(args.summary, "\n".join([head, ""] + [f"- {line}" for line in done]) + "\n")
        return 0
    except Stop as err:
        print(f"error: {err}", file=sys.stderr)
        return 1


def write_json(path, data):
    with open(path, "w", encoding="utf-8") as f:
        json.dump(data, f, indent=1, sort_keys=True)
        f.write("\n")


def append(path, text):
    if path:
        with open(path, "a", encoding="utf-8") as f:
            f.write(text)
    else:
        print(text)


if __name__ == "__main__":
    sys.exit(main())
