"""Generates the compatibility report from the test results.

Usage (from the repository root):
    python3 tools/parity/compatibility.py           # writes the outputs below
    python3 tools/parity/compatibility.py --check   # fails if any is stale or invalid

Puts in one file what the repository's recordings and tests show about hidane's compatibility
with the official Cloud Firestore emulator and the tools around it:

- the Hidane Oracle Corpus (docs/oracle-corpus.json): its recordings, answers and release;
- the record/replay fixtures `cargo test` replays against hidane: their answers per area and per
  way of comparing them (tools/parity/table.py);
- the SDK transcripts compared side by side, and the SDK certification matrix
  (docs/sdk-certification.json);
- the browser tests: firebase-js-sdk's browser suite run against both emulators, and the
  transcripts made in Chromium;
- the Functions events: the fixtures of trigger registrations and events, and the transcripts of
  functions under firebase-tools' Functions emulator;
- the integration test files `cargo test` runs (`crates/*/tests/*.rs`), and the recordings each
  replays;
- differential fuzzing, once a run is recorded in results/differential-fuzzing.json;
- the intentional differences (docs/parity-exceptions.md);

and the compatibility matrix, hidane × firebase-tools × the official emulator. A row exists only
where results cover the combination: the official emulator releases the corpus was recorded
against, the firebase-tools versions transcripts ran under (the SDK certification matrix's
`firebase-tools` row), and hidane as this tree builds it (the workspace version, as its series,
`0.4.x`), which CI tests on every pull request and on `main`. Writes `docs/compatibility.json`,
validated against `docs/compatibility.schema.json`, and the section of `docs/compatibility.md`
between `<!-- compatibility:start -->` and `<!-- compatibility:end -->`. Run it after
tools/parity/table.py, tools/oracle/corpus.py and tools/parity/sdk_certification.py. Stdlib only.
"""

import json
import re
import sys
import tomllib
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools" / "parity"))
import sdk_certification as sdk  # noqa: E402  (tools/parity/sdk_certification.py)

corpus, table = sdk.corpus, sdk.table

OUTPUT = "docs/compatibility.json"
SCHEMA = "docs/compatibility.schema.json"
PAGE = "docs/compatibility.md"
FUZZING = "results/differential-fuzzing.json"
SUITE = "js-sdk-browser"
SUITE_WORKFLOW = ".github/workflows/js-sdk-browser.yml"
SCHEMA_VERSION = 1
START, END = "<!-- compatibility:start -->", "<!-- compatibility:end -->"
REPLAYED = "every pull request and push to main"
TEST = re.compile(r"^\s*#\[(?:tokio::)?test(?:\([^)]*\))?\]", re.M)


class Stop(Exception):
    """A problem the person running the script has to fix."""


def load(rel):
    with open(ROOT / rel, encoding="utf-8") as f:
        return json.load(f)


def hidane_version():
    with open(ROOT / "Cargo.toml", "rb") as f:
        version = tomllib.load(f)["workspace"]["package"]["version"]
    match = re.fullmatch(r"(\d+)\.(\d+)\.\d+(?:-[\w.]+)?", version)
    if not match:
        raise Stop(f"Cargo.toml: workspace version {version!r} is not <major>.<minor>.<patch>.")
    return dict(version=version, series=f"{match.group(1)}.{match.group(2)}.x")


def bare(release):
    return release.removeprefix("v")


def levels(answers):
    return {key: table.total(answers, key) for key, _ in table.LEVELS
            if table.total(answers, key)}


def replay(rows):
    everything = Counter()
    for row in rows:
        everything.update(row["answers"])
    areas = {}
    for key in table.AREA_ORDER:
        answers = Counter()
        fixtures = 0
        for row in rows:
            if row["area"] == key:
                answers.update(row["answers"])
                fixtures += 1
        if fixtures:
            areas[key] = dict(fixtures=fixtures, answers=table.total(answers),
                              full=table.total(answers, table.FULL),
                              transports=table.transports_of(answers))
    return dict(fixtures=len(rows), answers=table.total(everything), levels=levels(everything),
                by_area=areas, replayed_by="cargo test", runs=REPLAYED, where="docs/parity.md")


def transcripts(pairs):
    results = Counter(pair["result"] for pair in pairs)
    return dict(pairs=len({pair["name"] for pair in pairs}), comparisons=len(pairs),
                **{key: results[key] for key in sdk.RESULT_ORDER}, where="docs/parity.md")


def integration_tests(rows):
    replaying = {}
    for row in rows:
        for test in row["tests"]:
            replaying.setdefault(test.partition("::")[0], set()).add(row["path"])
    # Files, not test functions: a test added to a file does not make the report stale.
    files = []
    for path in sorted(ROOT.glob("crates/*/tests/*.rs")):
        rel = path.relative_to(ROOT).as_posix()
        if TEST.search(path.read_text(encoding="utf-8")):
            files.append(dict(path=rel, replays=sorted(replaying.get(rel, ()))))
    return dict(files=len(files), replaying_files=sum(bool(f["replays"]) for f in files),
                runs=REPLAYED, suites=files)


def suite(pairs):
    """firebase-js-sdk's browser suite: the outcomes on each emulator."""
    pair = next((p for p in pairs if p["name"] == SUITE), None)
    if pair is None:
        raise Stop(f"results/{SUITE}-*.json: not a transcript pair of tools/parity/table.py.")
    out = dict(workflow=SUITE_WORKFLOW, result=pair["result"])
    for side, rel in (("official", pair["official"]), ("hidane", pair["other"])):
        data = load(rel)
        out[side] = dict(path=rel, **{key: data["counts"][key]
                                      for key in ("passed", "failed", "skipped")})
        out["commit"] = data["firebase-js-sdk"]
    return out


def browser(pairs):
    chromium = [dict(name=p["name"], client=p["client"], transport=p["transport"],
                     result=p["result"], **({"explained_in": p["where"]} if p["where"] else {}))
                for p in pairs if "Chromium" in p["client"] and p["name"] != SUITE]
    return dict(suite=suite(pairs), transcripts=chromium)


def functions(rows, pairs):
    fixtures = [dict(path=row["path"], answers=table.total(row["answers"]),
                     levels=levels(row["answers"]), tests=row["tests"])
                for row in rows if "function_" in Path(row["path"]).name]
    found = []
    for pair in pairs:
        if pair["name"].startswith("functions-"):
            found.append(dict(name=pair["name"], client=pair["client"],
                              official_events=len(load(pair["official"])),
                              hidane_events=len(load(pair["other"])), result=pair["result"],
                              **({"explained_in": pair["where"]} if pair["where"] else {})))
    if not fixtures or not found:
        raise Stop("no Functions fixtures or transcripts found; update functions().")
    return dict(fixtures=fixtures, transcripts=found)


def fuzzing():
    """Differential fuzzing runs, from results/differential-fuzzing.json once one exists."""
    if not (ROOT / FUZZING).is_file():
        return dict(status="not-run", where=None, runs=[])
    runs = load(FUZZING).get("runs")
    if not isinstance(runs, list):
        raise Stop(f"{FUZZING}: expected an object with a `runs` list.")
    return dict(status="run" if runs else "not-run", where=FUZZING, runs=runs)


def sdk_matrix(data):
    by_status = {status: [c["id"] for c in data["clients"] if c["status"] == status]
                 for status in sdk.STATUSES}
    return dict(where=sdk.OUTPUT, totals=data["totals"], by_status=by_status)


def matrix(hidane, manifest, rows, pairs, certification):
    """hidane × firebase-tools × the official emulator, from the results only."""
    tools = next(c for c in certification["clients"] if c["id"] == "firebase-tools")
    # The release of each recording as the corpus knows it, also where the recording does not
    # state it.
    release_of = {entry["path"]: entry["oracle"]["release"] for entry in manifest["recordings"]}
    out = []
    for release in manifest["releases"]:
        name = release["release"]
        replayed = [row for row in rows if release_of.get(row["path"]) == name]
        answers = sum(table.total(row["answers"]) for row in replayed)
        compared = [p for p in pairs if re.search(rf"-official-{re.escape(name)}\.json$",
                                                  p["official"])]
        evidence = []
        if answers:
            evidence.append(dict(
                kind="replay", count=answers,
                description=f"{table.n(answers)} recorded answers of the official emulator "
                            f"{name} in {len(replayed)} fixtures, replayed against hidane by "
                            "cargo test",
                where="docs/parity.md", runs=REPLAYED, last_recorded=None))
        if compared:
            results = Counter(p["result"] for p in compared)
            evidence.append(dict(
                kind="transcripts", count=len(compared),
                description=f"{len(compared)} comparisons of SDK transcripts with the "
                            f"official emulator {name}'s: " + ", ".join(sdk_summary(results)),
                where="docs/parity.md", runs="recorded", last_recorded=None))
        by_tools = {}
        for e in tools["evidence"]:
            if e["official_emulator"] != name:
                continue
            version = next((p["version"] for p in e["packages"]
                            if p["name"] == "firebase-tools" and p["version"]), None)
            by_tools.setdefault(version, []).append(e)
        for version, found in sorted(by_tools.items(), key=lambda kv: str(kv[0])):
            last = max(c["recorded"]["last_changed"] for e in found for c in e["compared"])
            names = [e["transcript"] for e in found]
            ok = all(e["result"] in sdk.OK_RESULTS for e in found)
            row_evidence = evidence + [dict(
                kind="firebase-tools", count=len(found),
                description=f"{len(found)} transcripts run under firebase-tools {version}: "
                            + ", ".join(names),
                where=sdk.PAGE, runs="recorded", last_recorded=last)]
            verified = (answers and ok and version is not None and tools["status"] == sdk.VERIFIED
                        and all(p["result"] in sdk.OK_RESULTS for p in compared))
            out.append(dict(hidane=hidane["series"], firebase_tools=version,
                            official_emulator=bare(name),
                            status=sdk.VERIFIED if verified else sdk.PARTIAL,
                            evidence=row_evidence))
        if not by_tools and evidence:
            out.append(dict(hidane=hidane["series"], firebase_tools=None,
                            official_emulator=bare(name), status=sdk.PARTIAL,
                            evidence=evidence))
    return out


def sdk_summary(results):
    labels = {table.IDENTICAL: "identical", table.DOCUMENTED: "documented differences",
              table.UNEXPLAINED: "unexplained"}
    return [f"{results[key]} {labels[key]}" for key in sdk.RESULT_ORDER if results[key]]


def output():
    manifest = load(corpus.MANIFEST)
    certification = load(sdk.OUTPUT)
    rows, pairs = table.recordings(), table.transcripts()
    hidane = hidane_version()
    pinned = next(r for r in manifest["releases"] if r["pinned"])
    totals = manifest["totals"]
    return {
        "$schema": "./compatibility.schema.json",
        "name": "hidane compatibility report",
        "description": "What the recorded results and the tests show about hidane's "
                       "compatibility with the official Cloud Firestore emulator, its SDKs and "
                       "firebase-tools, for this tree. Generated from the results only. See "
                       "docs/compatibility.md.",
        "schema_version": SCHEMA_VERSION,
        "generated_by": "tools/parity/compatibility.py",
        "hidane": hidane,
        "official_emulator": dict(product=pinned["product"], release=bare(pinned["release"]),
                                  jar_sha256=pinned["jar_sha256"]),
        "matrix": matrix(hidane, manifest, rows, pairs, certification),
        "oracle_corpus": dict(where=corpus.MANIFEST, digest=manifest["digest"],
                              releases=[bare(r["release"]) for r in manifest["releases"]],
                              **{key: totals[key] for key in ("files", "fixtures", "transcripts",
                                                              "answers", "bytes")}),
        "replay": replay(rows),
        "transcripts": transcripts(pairs),
        "sdk_certification": sdk_matrix(certification),
        "browser": browser(pairs),
        "functions": functions(rows, pairs),
        "integration_tests": integration_tests(rows),
        "differential_fuzzing": fuzzing(),
        "intentional_differences": dict(count=table.intentional_differences(),
                                        where="docs/parity-exceptions.md"),
    }


def markdown(data):
    n = table.n
    out = [START, "<!-- Generated by tools/parity/compatibility.py; edit the script, not this. "
                  "-->", ""]
    out += ["| hidane | firebase-tools | Official emulator | Status | Evidence |",
            "|---|---|---|---|---|"]
    for row in data["matrix"]:
        evidence = "; ".join(e["description"] + (f" (last recorded {e['last_recorded']})"
                                                 if e["last_recorded"] else "")
                             for e in row["evidence"])
        out.append(f"| {row['hidane']} | {row['firebase_tools'] or '—'} | "
                   f"{row['official_emulator']} | {sdk.STATUSES[row['status']][0]} | "
                   f"{evidence} |")
    corpus_, replay_, pairs = data["oracle_corpus"], data["replay"], data["transcripts"]
    certification = data["sdk_certification"]["totals"]
    suite_ = data["browser"]["suite"]
    tests = data["integration_tests"]
    fuzz = data["differential_fuzzing"]
    out += ["", "| Evidence | What the results show | Where |", "|---|---|---|"]
    out.append(f"| Oracle corpus | {corpus_['files']} recordings of the official emulator "
               f"{', '.join(corpus_['releases'])} ({corpus_['fixtures']} fixtures, "
               f"{corpus_['transcripts']} official transcripts), `{corpus_['digest'][:19]}…` | "
               "[oracle-corpus.md](oracle-corpus.md) |")
    out.append(f"| Record and replay | {n(replay_['answers'])} recorded answers in "
               f"{replay_['fixtures']} fixtures, replayed by `cargo test` on "
               f"{replay_['runs']}: {table.summary(Counter(_level_pairs(replay_['levels'])))} | "
               "[parity.md](parity.md) |")
    out.append(f"| SDK transcripts | {pairs['comparisons']} comparisons of {pairs['pairs']} "
               "transcript pairs: " + ", ".join(sdk_summary(Counter(
                   {key: pairs[key] for key in sdk.RESULT_ORDER}))) + " | [parity.md](parity.md) |")
    out.append(f"| SDK certification | {certification['clients']} clients and tools: "
               f"{certification['verified']} verified, {certification['partially-verified']} "
               f"partially verified, {certification['not-verified']} not verified | "
               "[sdk-certification.md](sdk-certification.md) |")
    official, hidane = suite_["official"], suite_["hidane"]
    chromium = data["browser"]["transcripts"]
    out.append(
        f"| Browser | firebase-js-sdk's browser suite at `{suite_['commit'][:7]}`: "
        f"{n(hidane['passed'])} passed, {n(hidane['failed'])} failed, {n(hidane['skipped'])} "
        f"skipped on hidane; {n(official['passed'])} passed, {n(official['failed'])} failed on "
        f"the official emulator ({suite_['result']}). {len(chromium)} transcripts in Chromium: "
        + ", ".join(f"{t['name']} ({t['result']})" for t in chromium)
        + f" | [`{Path(SUITE_WORKFLOW).name}`](../{SUITE_WORKFLOW}) |")
    fx = data["functions"]
    out.append(
        "| Functions events | "
        + ", ".join(f"`{Path(f['path']).name}`: {n(f['answers'])} recorded answers"
                    for f in fx["fixtures"])
        + "; " + ", ".join(f"{t['name']}: {t['hidane_events']} events on hidane, "
                           f"{t['official_events']} on the official emulator ({t['result']})"
                           for t in fx["transcripts"])
        + " | [parity-exceptions.md](parity-exceptions.md) |")
    out.append(f"| Integration tests | {tests['files']} test files (`crates/*/tests/*.rs`), "
               f"{tests['replaying_files']} of them replaying recordings, run on {tests['runs']} "
               "| [`crates/`](../crates/) |")
    if fuzz["status"] == "run":
        out.append(f"| Differential fuzzing | {len(fuzz['runs'])} runs | "
                   f"[`{Path(FUZZING).name}`](../{FUZZING}) |")
    else:
        out.append("| Differential fuzzing | not run yet | — |")
    out.append(f"| Intentional differences | {data['intentional_differences']['count']} | "
               "[parity-exceptions.md](parity-exceptions.md) |")
    out += ["", END]
    return "\n".join(out)


def _level_pairs(counts):
    """`{level: count}` as the `(transport, level)` counter table.summary reads."""
    return {(None, level): count for level, count in counts.items()}


def outputs():
    data = output()
    schema = load(SCHEMA)
    found = corpus.problems(data, schema, schema)
    if found:
        raise Stop(f"{OUTPUT} does not fit {SCHEMA}:\n  " + "\n  ".join(found[:20]))
    return {
        OUTPUT: json.dumps(data, indent=1, ensure_ascii=False) + "\n",
        PAGE: sdk.between(PAGE, markdown(data), START, END),
    }


def main():
    args = sys.argv[1:]
    if args not in ([], ["--check"]):
        raise Stop("usage: python3 tools/parity/compatibility.py [--check]")
    check = args == ["--check"]
    stale = []
    for rel, text in outputs().items():
        path = ROOT / rel
        if path.is_file() and path.read_text(encoding="utf-8") == text:
            continue
        stale.append(rel)
        if not check:
            path.write_text(text, encoding="utf-8")
    if check and stale:
        raise Stop(f"stale: {', '.join(stale)}. Run python3 tools/parity/compatibility.py and "
                   "commit the result.")
    for rel in stale:
        print(f"wrote {rel}")


if __name__ == "__main__":
    try:
        main()
    except (Stop, sdk.Stop, corpus.Stop, table.Stop) as stop:
        sys.exit(f"tools/parity/compatibility.py: {stop}")
