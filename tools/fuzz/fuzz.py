#!/usr/bin/env python3
"""Differential fuzzing: the same generated requests to the official emulator and to hidane.

Usage (from the repository root; both emulators running, the official one under an English
locale, see tools/oracle/README.md):

    python3 -I tools/fuzz/fuzz.py run --official 127.0.0.1:8080 --hidane 127.0.0.1:8180 \\
        --seed 1 --cases 500 --out "$(mktemp -d)" [--hidane-build 0.4.0:<commit>]
    python3 -I tools/fuzz/fuzz.py replay --official … --hidane … <out>/differences/<signature>.json
    python3 -I tools/fuzz/fuzz.py show --seed 1 --case 17      # print a case, no emulator needed
    python3 -I tools/fuzz/fuzz.py check                         # exceptions.json against the docs
    python3 -I tools/fuzz/fuzz.py record <out>/run.json         # add a run to results/differential-fuzzing.json

`run` sends case after case (`--cases`, from `--first`, each in a fresh project, for at most
`--minutes`), compares the answers (`diff.py`), drops the differences docs/parity-exceptions.md
accepts (`exceptions.json`), runs each new difference again to confirm it, minimises it
(`shrink.py`) and writes it to `<out>/differences/<signature>.json`, in the recordings' format,
for a person to look at. It never writes into the repository. `<out>/summary.json`,
`<out>/summary.md` and `<out>/run.json` (an entry for results/differential-fuzzing.json) say
what the run found. It exits 0 unless it could not run (an emulator stopped answering, a bad
argument); differences are results, not failures. Stdlib only.
"""

import argparse
import datetime
import json
import re
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import diff  # noqa: E402
import gen  # noqa: E402
import shrink  # noqa: E402
from harness import send  # noqa: E402  (diff.py puts tools/oracle on the path)

ROOT = diff.ROOT
RESULTS = ROOT / "results" / "differential-fuzzing.json"
SIGNATURE = re.compile(r"(crash|status|message|order|data)-[a-z0-9-]+-[0-9a-f]{12}")


class Stop(Exception):
    pass


def pinned():
    """The official emulator the fixtures were recorded from (.github/actions/official-emulator/pinned.sh)."""
    text = (ROOT / ".github" / "actions" / "official-emulator" / "pinned.sh").read_text()
    version = re.search(r"^PINNED_VERSION=([0-9.]+)$", text, re.M).group(1)
    sha256 = re.search(r"^PINNED_SHA256=([0-9a-f]{64})$", text, re.M).group(1)
    return version, sha256


VERSION = re.compile(r"[0-9]+(\.[0-9]+)*([-+][0-9A-Za-z.+-]+)?")
COMMIT = re.compile(r"[0-9a-f]{40}")
UNKNOWN_BUILD = {"version": "unknown", "commit": "unknown"}


def hidane_build(value):
    """`--hidane-build VERSION:COMMIT`: the hidane a run fuzzes, as `hidane --version` names it and
    the commit it was built from, in full."""
    version, _, commit = value.partition(":")
    if not VERSION.fullmatch(version) or not COMMIT.fullmatch(commit):
        raise argparse.ArgumentTypeError(
            f"{value!r} is not VERSION:COMMIT (a version such as 0.4.0, and a commit of 40 lowercase hex digits)")
    return {"version": version, "commit": commit}


def build_text(build):
    """How summary.md names the hidane build."""
    if build["version"] == "unknown":
        return "hidane (build unknown)"
    return f"hidane {build['version']} (commit {build['commit'][:12]})"


def answers(host):
    """Whether the emulator at `host` answers `GET /`."""
    try:
        return send(host, "GET", "/", timeout=10)[0] == 200
    except OSError:
        return False


def write_json(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    text = json.dumps(data, ensure_ascii=False, indent=1) + "\n"
    try:
        text.encode()
    except UnicodeEncodeError:
        # A lone surrogate (a malformed case) has no UTF-8: every non-ASCII character as \u escapes.
        text = json.dumps(data, indent=1) + "\n"
    path.write_text(text, encoding="utf-8")


def fixture_steps(case, official):
    """The case's steps in the format of tools/oracle/rest.py's fixture, with the official answers."""
    out = []
    for n, step in enumerate(case["steps"]):
        entry = {"name": f"{case['name']} step {n}", "method": step["method"], "path": step["path"],
                 "headers": step.get("headers", {}), "compare": "exact"}
        if "raw" in step:
            entry["raw"] = step["raw"]
        elif "body" in step:
            entry["body"] = step["body"]
        answer = official[n] if n < len(official) else {"status": None}
        entry["outcome"] = {k: answer.get(k) for k in ("status", "contentType", "body", "error") if k in answer}
        out.append(entry)
    return out


class Session:
    def __init__(self, args, exceptions, out):
        self.args = args
        self.exceptions = exceptions
        self.out = out
        self.areas = tuple(args.areas.split(",")) if args.areas else gen.AREAS
        for area in self.areas:
            if area not in gen.AREAS:
                raise Stop(f"unknown area {area!r}; the areas are {', '.join(gen.AREAS)}")
        self.by_area = {area: {"cases": 0, "differing": 0, "accepted": 0, "unexplained": 0, "flaky": 0}
                        for area in self.areas}
        self.signatures = {}
        self.deadline = time.monotonic() + args.minutes * 60 if args.minutes else None
        self.minimize_deadline = None
        self.stopped = None

    def both(self, case):
        official = diff.run(self.args.official, case, self.args.timeout)
        hidane = diff.run(self.args.hidane, case, self.args.timeout)
        return official, hidane

    def reproduces(self, signature):
        def test(candidate):
            found = diff.compare(candidate, *self.both(candidate))
            return found is not None and found["signature"] == signature
        return test

    def check_alive(self, found):
        for side in ("official", "hidane"):
            if found[side].get("status") is None and not answers(getattr(self.args, side)):
                self.stopped = f"the {side} emulator stopped answering ({found['signature']})"
                return False
        return True

    def run(self):
        version, sha256 = self.args.official_version
        started = time.monotonic()
        last = self.args.first
        for index in range(self.args.first, self.args.first + self.args.cases):
            if self.deadline is not None and time.monotonic() > self.deadline:
                break
            case = gen.case(self.args.seed, index, self.areas)
            area = case["area"]
            official, hidane = self.both(case)
            self.by_area[area]["cases"] += 1
            last = index + 1
            found = diff.compare(case, official, hidane)
            if found is None:
                continue
            self.by_area[area]["differing"] += 1
            if not self.check_alive(found):
                break
            self.record(case, found)
        duration = round(time.monotonic() - started, 1)
        return self.summary(version, sha256, last, duration)

    def record(self, case, found):
        area, signature = case["area"], found["signature"]
        known = self.signatures.get(signature)
        if known:
            known["occurrences"] += 1
            self.by_area[area][known["outcome"]] += 1
            return
        entry = {"signature": signature, "kind": found["kind"], "area": area, "verb": found["verb"],
                 "detail": found["detail"], "official": found["official"], "hidane": found["hidane"],
                 "found": {"seed": case["seed"], "case": case["index"], "step": found["step"]},
                 "occurrences": 1}
        rule = self.exceptions.match(area, found)
        self.signatures[signature] = entry
        if rule:
            entry["outcome"] = "accepted"
            entry["rule"] = rule["id"]
            entry["exception"] = rule["exception"]
            self.by_area[area]["accepted"] += 1
            return
        if not self.reproduces(signature)(case):
            entry["outcome"] = "flaky"
            self.by_area[area]["flaky"] += 1
            return
        entry["outcome"] = "unexplained"
        self.by_area[area]["unexplained"] += 1
        deadline = time.monotonic() + self.args.minimize_seconds
        if self.deadline is not None:
            deadline = min(deadline, self.deadline + self.args.minimize_seconds)
        small, tests = shrink.minimize(case, found["step"], self.reproduces(signature),
                                       self.args.minimize_budget, deadline)
        official, hidane = self.both(small)
        final = diff.compare(small, official, hidane)
        if final is None or final["signature"] != signature:
            small, (official, hidane) = case, self.both(case)
            final = diff.compare(case, official, hidane) or found
        entry["minimized"] = {"steps": len(small["steps"]), "from_steps": len(case["steps"]), "tests": tests}
        entry["file"] = f"differences/{signature}.json"
        version, sha256 = self.args.official_version
        self.write(entry["file"], {
            "oracle": f"cloud-firestore-emulator v{version} (sha256 {sha256})",
            "generated_by": "tools/fuzz/fuzz.py",
            "note": "Minimised by the differential fuzzer; review before it becomes a fixture (tools/fuzz/README.md).",
            **{k: entry[k] for k in ("signature", "kind", "area", "verb", "detail", "found")},
            "step": final["step"], "official": final["official"], "hidane": final["hidane"],
            "case": small,
            "cases": fixture_steps(small, official),
            "hidane_outcomes": hidane,
            "original": case,
        })

    def write(self, rel, data):
        write_json(self.out / rel, data)

    def summary(self, version, sha256, last, duration):
        entries = sorted(self.signatures.values(), key=lambda e: (e["outcome"] != "unexplained", e["signature"]))
        totals = {key: sum(a[key] for a in self.by_area.values()) for key in ("cases", "differing", "accepted",
                                                                             "unexplained", "flaky")}
        distinct = {outcome: sum(1 for e in entries if e["outcome"] == outcome)
                    for outcome in ("unexplained", "accepted", "flaky")}
        summary = {
            "seed": self.args.seed, "first_case": self.args.first, "next_case": last,
            "official_emulator": {"version": version, "sha256": sha256},
            "hidane": dict(self.args.hidane_build or UNKNOWN_BUILD),
            "duration_seconds": duration, "stopped": self.stopped,
            "cases": totals["cases"], "differing_cases": totals["differing"],
            "signatures": distinct, "by_area": self.by_area,
            "by_kind": {kind: sum(1 for e in entries if e["kind"] == kind and e["outcome"] == "unexplained")
                        for kind in diff.KINDS},
            "differences": entries,
        }
        self.write("summary.json", summary)
        self.write("run.json", run_entry(summary, self.areas))
        (self.out / "summary.md").write_text(markdown(summary), encoding="utf-8")
        return summary


def run_entry(summary, areas):
    """The run as an entry of results/differential-fuzzing.json (docs/compatibility.schema.json's `fuzzing_run`)."""
    return {
        "name": f"REST, seed {summary['seed']}, cases {summary['first_case']} to {summary['next_case'] - 1}",
        "date": datetime.date.today().isoformat(),
        "target": "REST requests to both emulators: " + ", ".join(areas),
        "official_emulator": summary["official_emulator"]["version"],
        "hidane": summary["hidane"],
        "cases": summary["cases"],
        "differences": sum(summary["signatures"].values()),
        "unexplained": summary["signatures"]["unexplained"],
        "evidence": "tools/fuzz/",
        "seed": summary["seed"],
        "first_case": summary["first_case"],
        "accepted": summary["signatures"]["accepted"],
        "flaky": summary["signatures"]["flaky"],
        "by_area": summary["by_area"],
        "accepted_by": {exception: sum(1 for e in summary["differences"] if e.get("exception") == exception)
                        for exception in sorted({e["exception"] for e in summary["differences"] if "exception" in e})},
        "unexplained_by_kind": summary["by_kind"],
    }


def cell(text):
    """Text for a Markdown table cell: no pipes, backticks or line breaks, and short."""
    text = re.sub(r"[`|\r\n<>]", " ", str(text))
    return text[:120] + ("…" if len(text) > 120 else "")


def markdown(summary):
    lines = [f"## Differential fuzzing, seed {summary['seed']}", ""]
    lines.append(f"{summary['cases']} cases (from case {summary['first_case']}) against the official emulator "
                 f"v{summary['official_emulator']['version']} and {build_text(summary['hidane'])} in "
                 f"{summary['duration_seconds']} s; "
                 f"{summary['differing_cases']} differed. Distinct differences: "
                 f"{summary['signatures']['unexplained']} unexplained, {summary['signatures']['accepted']} accepted by "
                 f"docs/parity-exceptions.md, {summary['signatures']['flaky']} not reproduced.")
    if summary["stopped"]:
        lines += ["", f"**Stopped early:** {cell(summary['stopped'])}."]
    lines += ["", "| Area | Cases | Differing | Unexplained | Accepted | Not reproduced |", "|---|---:|---:|---:|---:|---:|"]
    for area, a in summary["by_area"].items():
        lines.append(f"| {area} | {a['cases']} | {a['differing']} | {a['unexplained']} | {a['accepted']} | {a['flaky']} |")
    shown = [e for e in summary["differences"] if e["outcome"] != "accepted"]
    if shown:
        lines += ["", "| Signature | Outcome | Verb | Official | hidane | Seen |", "|---|---|---|---|---|---:|"]
        for e in shown:
            side = lambda s: cell(f"{s.get('status')} {s.get('code') or s.get('error') or ''}".strip())
            lines.append(f"| `{e['signature']}` | {e['outcome']} | {cell(e['verb'])} | {side(e['official'])} | "
                         f"{side(e['hidane'])} | {e['occurrences']} |")
        lines += ["", "Each unexplained difference is minimised in the `differences/` folder of the run's artifact; "
                  "`not reproduced` ones differed once and not when run again."]
    accepted = {}
    for e in summary["differences"]:
        if e["outcome"] == "accepted":
            entry = accepted.setdefault(e["exception"], [0, 0])
            entry[0] += 1
            entry[1] += e["occurrences"]
    if accepted:
        lines += ["", "| Accepted by | Distinct | Seen |", "|---|---:|---:|"]
        for exception, (distinct, seen) in sorted(accepted.items()):
            lines.append(f"| {exception} (docs/parity-exceptions.json) | {distinct} | {seen} |")
    return "\n".join(lines) + "\n"


def cmd_run(args):
    if not answers(args.official) or not answers(args.hidane):
        raise Stop("both emulators must answer GET / before the run")
    out = Path(args.out).resolve()
    if out == ROOT or ROOT in out.parents:
        raise Stop("--out must be outside the repository (differences are reviewed, not committed)")
    out.mkdir(parents=True, exist_ok=True)
    summary = Session(args, diff.Exceptions.load(), out).run()
    sys.stdout.write(markdown(summary))
    if summary["stopped"]:
        raise Stop(summary["stopped"])


def cmd_replay(args):
    data = json.loads(Path(args.file).read_text())
    case = data.get("case", data)
    official, hidane = diff.run(args.official, case, args.timeout), diff.run(args.hidane, case, args.timeout)
    found = diff.compare(case, official, hidane)
    print(json.dumps({"difference": found, "official": official, "hidane": hidane}, ensure_ascii=False, indent=1))


def cmd_show(args):
    areas = tuple(args.areas.split(",")) if args.areas else gen.AREAS
    print(json.dumps(gen.case(args.seed, args.case, areas), ensure_ascii=False, indent=1))


def cmd_check(_args):
    problems = diff.Exceptions.load().check(diff.parity_entries())
    for p in problems:
        print(f"tools/fuzz/exceptions.json: {p}", file=sys.stderr)
    if problems:
        raise Stop(f"{len(problems)} problems")
    print("tools/fuzz/exceptions.json: every rule names an entry of docs/parity-exceptions.json")


def cmd_record(args):
    entry = json.loads(Path(args.file).read_text())
    missing = [k for k in ("name", "date", "target", "cases", "differences", "unexplained", "evidence") if k not in entry]
    if missing:
        raise Stop(f"{args.file} is not a run.json of fuzz.py run (no {', '.join(missing)})")
    data = json.loads(RESULTS.read_text()) if RESULTS.exists() else {
        "description": "Differential fuzzing runs (tools/fuzz/): generated REST requests sent to the official "
                       "emulator and to hidane, and how many distinct differences each found. Recorded by hand "
                       "with `tools/fuzz/fuzz.py record`; the scheduled workflow's runs are its artifacts.",
        "runs": []}
    data["runs"] = [r for r in data["runs"] if r.get("name") != entry["name"] or r.get("date") != entry["date"]]
    data["runs"].append(entry)
    RESULTS.write_text(json.dumps(data, ensure_ascii=False, indent=1) + "\n", encoding="utf-8")
    print(f"added the run {entry['name']!r} of {entry['date']}")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = parser.add_subparsers(dest="command", required=True)
    run = sub.add_parser("run", help="fuzz both emulators")
    replay = sub.add_parser("replay", help="run one case (or a difference file) on both emulators")
    for p in (run, replay):
        p.add_argument("--official", required=True, help="the official emulator's host:port")
        p.add_argument("--hidane", required=True, help="hidane's host:port")
        p.add_argument("--timeout", type=float, default=10.0, help="seconds to wait for each answer")
    run.add_argument("--seed", type=int, required=True)
    run.add_argument("--cases", type=int, default=500)
    run.add_argument("--first", type=int, default=0, help="the first case's index")
    run.add_argument("--minutes", type=float, default=0, help="stop sending new cases after this long (0: no limit)")
    run.add_argument("--areas", default="", help=f"comma-separated, from {','.join(gen.AREAS)} (default: all)")
    run.add_argument("--minimize-budget", type=int, default=150, help="candidates to try per difference")
    run.add_argument("--minimize-seconds", type=float, default=60, help="time to minimise each difference")
    run.add_argument("--official-version", type=lambda v: tuple(v.split(":", 1)), default=pinned(),
                     help="VERSION:SHA256 of the official jar, for the files written (default: pinned.sh's)")
    run.add_argument("--hidane-build", type=hidane_build, default=None,
                     help="VERSION:COMMIT of the hidane fuzzed (`hidane --version`, the full commit it was built "
                          "from), for the files written (default: unknown)")
    run.add_argument("--out", required=True, help="a directory outside the repository")
    replay.add_argument("file")
    show = sub.add_parser("show", help="print a generated case")
    show.add_argument("--seed", type=int, required=True)
    show.add_argument("--case", type=int, required=True)
    show.add_argument("--areas", default="")
    sub.add_parser("check", help="check exceptions.json against docs/parity-exceptions.json")
    record = sub.add_parser("record", help="add a run.json to results/differential-fuzzing.json")
    record.add_argument("file")
    args = parser.parse_args(argv)
    try:
        {"run": cmd_run, "replay": cmd_replay, "show": cmd_show, "check": cmd_check, "record": cmd_record}[args.command](args)
    except Stop as err:
        print(f"fuzz.py: {err}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
