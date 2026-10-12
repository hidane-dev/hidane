"""Generates the parity table from the recorded fixtures (#43).

Usage (from the repository root):
    python3 tools/parity/table.py           # writes the outputs below
    python3 tools/parity/table.py --check   # fails if any of them is stale

Reads every fixture the oracle scripts recorded (`crates/*/tests/fixtures/*.json`, and the
recordings in `results/` that a test replays) and counts, per recording, the official answers
its test replays against hidane: their area, the transport the replay uses, how the recording
marks their comparison, and the oracle version its header states. Reads the SDK transcript pairs
in `results/` (`<name>-official-v<version>.json` and `<name>-hidane.json`) and says whether each
pair is equal as JSON, and counts the intentional differences in `docs/parity-exceptions.md`.
Writes, between `<!-- parity:start -->` and `<!-- parity:end -->`:

- `docs/parity.md`: the tables, per area and transport, per recording and per transcript;
- the Parity section of `website/public/index.html` and `website/public/ja/index.html`;

and `website/public/badge/parity.json`, a shields.io endpoint.

Every recording and every transcript needs an entry in RECORDINGS or TRANSCRIPTS below: a new
fixture or `results/` file fails the script until it has one. A recording's `count` returns one
`(transport, compared)` pair per answer its test replays. `compared` is what the recording says:
a `compare` mark (`exact`, `status`, `codes`, `none`), a `differs` mark, or nothing (UNSTATED);
in a format with `compare` marks, an answer without one is compared in full. Where a recording
does not say, the table says "not stated" rather than reading the test. Stdlib only.
"""

import json
import re
import sys
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
REPO = "https://github.com/hidane-dev/hidane"
START, END = "<!-- parity:start -->", "<!-- parity:end -->"
BADGE_COLOR = "E2553D"  # the site's ember accent (website/public/style.css)

# Areas, in table order: key, English, Japanese.
AREAS = [
    ("documents", "Documents & writes", "ドキュメントと書き込み"),
    ("queries", "Queries & aggregations", "クエリと集計"),
    ("transactions", "Transactions", "トランザクション"),
    ("streams", "Listen & Write streams", "Listen / Write ストリーム"),
    ("rest", "REST", "REST"),
    ("webchannel", "WebChannel", "WebChannel"),
    ("rules", "Security Rules", "Security Rules"),
    ("export", "Export & import", "エクスポートとインポート"),
    ("admin", "Auth & admin endpoints", "認証と管理エンドポイント"),
    ("values", "Values & limits", "値と上限"),
]
AREA_ORDER = [key for key, _, _ in AREAS]
AREA_NAME = {"en": {k: en for k, en, _ in AREAS}, "ja": {k: ja for k, _, ja in AREAS}}

# How a replay reaches hidane. In-process: the test calls hidane's crates directly.
GRPC, REST, WEBCHANNEL, WEBSOCKET, CLI, IN_PROCESS = (
    "gRPC", "REST", "WebChannel", "WebSocket", "CLI", "in-process")
TRANSPORTS = [GRPC, REST, WEBCHANNEL, WEBSOCKET, CLI, IN_PROCESS]
TRANSPORT_JA = {IN_PROCESS: "プロセス内"}

# How a recording marks an answer's comparison.
EXACT, STATUS, CODES, NONE, DIFFERS, UNSTATED = (
    "exact", "status", "codes", "none", "differs", "unstated")
COMPARED = [
    (EXACT, "exact"),
    (STATUS, "status only"),
    (CODES, "without messages"),
    (NONE, "not compared"),
    (DIFFERS, "intentionally different"),
    (UNSTATED, "not stated"),
]
COMPARE_MARKS = {"exact": EXACT, "status": STATUS, "codes": CODES, "none": NONE}


class Stop(Exception):
    """A problem the person running the script has to fix."""


def compare_mark(item):
    """A step's `compare` mark; in a format that has them, no mark means compared in full."""
    mark = item.get("compare", "exact")
    if mark not in COMPARE_MARKS:
        raise Stop(f"unknown compare mark {mark!r}: add it to COMPARE_MARKS")
    return COMPARE_MARKS[mark]


def each(items, transport, compared=UNSTATED):
    return [(transport, compared)] * len(items)


def steps_of(cases, transport):
    """Cases of REST calls (`tools/oracle/steps.py` and the like): one answer per step; a case
    marked `differs` is an intentional difference."""
    answers = []
    for case in cases:
        compared = DIFFERS if "differs" in case else UNSTATED
        answers += each(case["steps"], transport, compared)
    return answers


def value_order(f):
    values = [value for group in f["values"]["groups"] for value in group]
    return each(values, IN_PROCESS) + each(f["paths"]["order"], IN_PROCESS)


def rules_paths(f):
    # paths.rs compares the reads recorded under `allow read: if true`, and checks that the
    # paths the official compiler refused are refused; the other conditions go to warnings.rs.
    answers = []
    for result in f["results"]:
        if result["condition"] != "true":
            continue
        refused = "compile" in result
        answers += [(IN_PROCESS, UNSTATED)] if refused else each(result["reads"], IN_PROCESS)
    return answers


# The recorded messages crates/hidane/tests/documents.rs checks hidane's against (calls of
# `official_message`). It replays no case as a whole; keep this list in step with it.
DOCUMENT_WRITES_CHECKED = [
    ("get missing document", 0), ("update_time precondition", 1),
    ("reserved document id", 0), ("dot document id", 0), ("reserved field name", 0),
    ("nested array", 0), ("invalid mask path", 0),
    ("numeric id document", 2), ("numeric id document", 3), ("numeric id document", 4),
    ("numeric id document", 5), ("numeric id document", 6),
    ("list documents", 3), ("list collection ids", 1),
    ("batch write statuses", 1), ("batch write", 1),
]


def document_writes(f):
    cases = {case["name"]: case for case in f["cases"]}
    for name, step in DOCUMENT_WRITES_CHECKED:
        steps = cases.get(name, {}).get("steps", [])
        if step >= len(steps) or not (steps[step].get("response") or {}).get("error"):
            raise Stop(f"document_writes.json has no recorded error at {name!r}, step {step}: "
                       "update DOCUMENT_WRITES_CHECKED from crates/hidane/tests/documents.rs")
    return each(DOCUMENT_WRITES_CHECKED, GRPC)


def export_import(f):
    # Each export (its files), the large record, each import target, the import over existing
    # documents, the production-shaped import, the endpoints' answers and the seeding steps go
    # over REST; the start-up cases start the process with flags.
    once = [key for key in ("big", "import_over", "production") if key in f]
    over_rest = list(f["exports"]) + once + list(f["imported"]) + f["requests"] + f["seeding"]
    return each(over_rest, REST) + each(f["startup"], CLI)


def grpc_settings(f):
    # grpc_settings.rs sends gRPC commits up to 20 MB (larger ones are slow to send twice) and
    # does not replay the reflection listing (docs/parity-exceptions.md).
    sizes = [case for case in f["grpc_sizes"] if case["megabytes"] <= 20]
    return each(f["v1beta1"], GRPC) + each(sizes, GRPC) + each(f["rest_sizes"], REST)


def negative_zero(f):
    answers = []
    for step in f["steps"]:
        if "grpc" in step:
            answers.append((GRPC, UNSTATED))
        elif "webchannel" in step:
            answers.append((WEBCHANNEL, UNSTATED))
        elif any(key in step for key in ("rest", "rules", "export")):
            answers.append((REST, UNSTATED))
        else:
            raise Stop(f"negative_zero.json: no transport for step {step.get('name')!r}")
    return answers


def rest(cases):
    """Cases in the format of `tools/oracle/rest.py`'s fixture."""
    return [(REST, compare_mark(case)) for case in cases]


def rules_streams(f):
    # The responses of each stream, and the error that ended it where there was one.
    answers = []
    for case in f:
        answers += each(case["responses"], GRPC)
        if case.get("status") is not None:
            answers.append((GRPC, UNSTATED))
    return answers


def transactions(f):
    # transactions.rs leaves out the scenarios marked `slow` (unit tests cover them).
    answers = []
    for scenario in f["scenarios"]:
        if scenario.get("slow"):
            continue
        for step in scenario["steps"]:
            if "outcome" in step:
                answers.append((REST if step["op"] == "reset" else GRPC, compare_mark(step)))
    return answers


def webchannel(steps):
    """Steps in the format of `tools/oracle/webchannel.py`'s fixture; `commit` goes over REST."""
    return [(REST if step["op"] == "commit" else WEBCHANNEL, UNSTATED)
            for step in steps if "outcome" in step]


def websocket_requests(f):
    return [(WEBSOCKET, DIFFERS if "differs" in step else UNSTATED)
            for case in f for step in case["steps"]]


def write_stream(f):
    return [(GRPC, compare_mark(step))
            for scenario in f["scenarios"] for step in scenario["steps"] if "outcome" in step]


H = "crates/hidane/tests"
HR = "crates/hidane-rules/tests"
OVER_REST = "recorded over REST"

# Every recording a test replays: path -> area, the test, what one answer is, how to count.
RECORDINGS = {
    "crates/hidane-core/tests/fixtures/value_order.json": dict(
        area="values", test="crates/hidane-core/tests/official_order.rs",
        counted="each value placed in the official order, and each document name; " + OVER_REST,
        count=value_order),
    f"{HR}/fixtures/rules_eval.json": dict(
        area="rules", test=f"{HR}/library.rs",
        counted="each condition or ruleset decided; " + OVER_REST,
        count=lambda f: each(f["results"], IN_PROCESS)),
    f"{HR}/fixtures/rules_paths.json": dict(
        area="rules", test=f"{HR}/paths.rs",
        counted="each read under `allow read: if true`, and each `match` path refused; "
                + OVER_REST,
        count=rules_paths),
    f"{HR}/fixtures/rules_warnings.json": dict(
        area="rules", test=f"{HR}/warnings.rs",
        counted="each ruleset compiled, its `issues[]` or error; " + OVER_REST,
        count=lambda f: each(f["results"], IN_PROCESS)),
    "results/rules-conditions-official-v1.22.0.json": dict(
        area="rules", test=f"{HR}/conditions.rs",
        counted="each condition decided; " + OVER_REST,
        count=lambda f: each(f["results"], IN_PROCESS)),
    f"{H}/fixtures/aggregations.json": dict(
        area="queries", test=f"{H}/aggregations.rs",
        counted="each query; " + OVER_REST,
        count=lambda f: each(f["cases"], GRPC)),
    f"{H}/fixtures/auth.json": dict(
        area="admin", test=f"{H}/auth.rs", counted="each case",
        count=lambda f: (each(f["classify"], REST) + each(f["grpc"], GRPC)
                         + each(f["rest"], REST))),
    f"{H}/fixtures/cors.json": dict(
        area="rest", test=f"{H}/cors.rs", counted="each request",
        count=lambda f: each(f["cases"], REST)),
    f"{H}/fixtures/document_writes.json": dict(
        area="documents", test=f"{H}/documents.rs",
        counted="each recorded message the test checks hidane's against (it replays no case "
                "as a whole); " + OVER_REST,
        count=document_writes),
    f"{H}/fixtures/export_import.json": dict(
        area="export", test=f"{H}/export_import.rs",
        counted="each export, import, endpoint answer, seeding step and start-up",
        count=export_import),
    f"{H}/fixtures/field_order.json": dict(
        area="documents", test=f"{H}/field_order.rs", counted="each call",
        count=lambda f: steps_of(f, REST)),
    f"{H}/fixtures/find_nearest.json": dict(
        area="queries", test=f"{H}/find_nearest.rs", counted="each query, aggregation and write",
        count=lambda f: each(f["queries"] + f["aggregations"] + f["writes"], REST)),
    f"{H}/fixtures/function_triggers.json": dict(
        area="admin", test=f"{H}/function_triggers.rs", counted="each registration, in both runs",
        count=lambda f: [answer for run in f["runs"] for answer in each(run["cases"], REST)]),
    f"{H}/fixtures/grpc_settings.json": dict(
        area="values", test=f"{H}/grpc_settings.rs",
        counted="each v1beta1 call, gRPC size up to 20 MB and REST size (not the larger gRPC "
                "sizes nor the reflection listing)",
        count=grpc_settings),
    f"{H}/fixtures/limits.json": dict(
        area="values", test=f"{H}/limits.rs", counted="each commit",
        count=lambda f: each(f["cases"], REST)),
    f"{H}/fixtures/list_documents.json": dict(
        area="documents", test=f"{H}/list_documents.rs", counted="each case (a paged case is one)",
        count=lambda f: each(f["grpc"], GRPC) + each(f["rest"], REST)),
    f"{H}/fixtures/negative_zero.json": dict(
        area="values", test=f"{H}/negative_zero.rs", counted="each step",
        count=negative_zero),
    f"{H}/fixtures/queries.json": dict(
        area="queries", test=f"{H}/queries.rs", counted="each query; " + OVER_REST,
        count=lambda f: each(f["cases"], GRPC)),
    f"{H}/fixtures/json_escapes.json": dict(
        area="values", test=[f"{H}/rest.rs", f"{H}/webchannel.rs"],
        counted="each REST request and each WebChannel step",
        count=lambda f: rest(f["rest"]) + webchannel(f["webchannel"]["steps"])),
    f"{H}/fixtures/rest.json": dict(
        area="rest", test=f"{H}/rest.rs", counted="each request",
        count=lambda f: rest(f["cases"])),
    f"{H}/fixtures/rules_auth.json": dict(
        area="rules", test=f"{H}/rules_server.rs", counted="each call",
        count=lambda f: steps_of(f, REST)),
    f"{H}/fixtures/rules_compile.json": dict(
        area="rules", test=f"{H}/rules_compile.rs", counted="each ruleset loaded",
        count=lambda f: each(f, REST)),
    f"{H}/fixtures/rules_coverage.json": dict(
        area="rules", test=f"{H}/rules_coverage.rs", counted="each call",
        count=lambda f: steps_of(f, REST)),
    f"{H}/fixtures/rules_coverage_flag.json": dict(
        area="rules", test=f"{H}/rules_coverage.rs", counted="each call",
        count=lambda f: steps_of(f, REST)),
    f"{H}/fixtures/rules_denials.json": dict(
        area="rules", test=f"{H}/rules_server.rs", counted="each call",
        count=lambda f: steps_of(f, REST)),
    f"{H}/fixtures/rules_list.json": dict(
        area="rules", test=f"{H}/rules_server.rs", counted="each call",
        count=lambda f: steps_of(f, REST)),
    f"{H}/fixtures/rules_lookups.json": dict(
        area="rules", test=f"{H}/rules_server.rs", counted="each call",
        count=lambda f: steps_of(f, REST)),
    f"{H}/fixtures/rules_request.json": dict(
        area="rules", test=f"{H}/rules_server.rs", counted="each call",
        count=lambda f: steps_of(f, REST)),
    f"{H}/fixtures/rules_request_fields.json": dict(
        area="rules", test=f"{H}/rules_coverage.rs", counted="each call",
        count=lambda f: steps_of(f, REST)),
    f"{H}/fixtures/rules_streams.json": dict(
        area="rules", test=f"{H}/rules_streams.rs",
        counted="each stream response, and each error that ended a stream",
        count=rules_streams),
    f"{H}/fixtures/rules_verify.json": dict(
        area="rules", test=f"{H}/rules_coverage.rs", counted="each call",
        count=lambda f: steps_of(f, REST)),
    f"{H}/fixtures/rules_writes.json": dict(
        area="rules", test=f"{H}/rules_server.rs", counted="each call",
        count=lambda f: steps_of(f, REST)),
    f"{H}/fixtures/transactions.json": dict(
        area="transactions", test=f"{H}/transactions.rs",
        counted="each step with an outcome, except those of the scenario marked `slow`",
        count=transactions),
    f"{H}/fixtures/transforms.json": dict(
        area="documents", test=f"{H}/transforms.rs", counted="each call; " + OVER_REST,
        count=lambda f: steps_of(f["cases"], GRPC)),
    f"{H}/fixtures/unsupported.json": dict(
        area="queries", test=f"{H}/unsupported.rs", counted="each call",
        count=lambda f: each(f["grpc"], GRPC) + each(f["rest"], REST)),
    f"{H}/fixtures/webchannel.json": dict(
        area="webchannel", test=f"{H}/webchannel.rs", counted="each step",
        count=lambda f: webchannel(f["steps"])),
    f"{H}/fixtures/websocket_requests.json": dict(
        area="rules", test=f"{H}/websocket_requests.rs",
        counted="each step: the request monitor's records of a call, or a WebSocket exchange",
        count=websocket_requests),
    f"{H}/fixtures/write_stream.json": dict(
        area="streams", test=f"{H}/write_stream.rs", counted="each step with an outcome",
        count=write_stream),
}

# SDK transcripts in results/: name -> area, the client, its transport, and other transcripts
# compared with the official one besides `<name>-hidane.json`.
TRANSCRIPTS = {
    "browser-rules": dict(area="rules", client="firebase + rules-unit-testing in Chromium",
                          transport="WebChannel, REST"),
    "browser-webchannel": dict(area="webchannel", client="firebase in Chromium",
                               transport="WebChannel"),
    "export-import": dict(area="export", client="firebase-tools + Admin SDK",
                          transport="CLI, gRPC", variants={
                              "export-import-hidane-to-official.json":
                                  "exported by hidane, read by the official emulator",
                              "export-import-official-to-hidane.json":
                                  "exported by the official emulator, read by hidane",
                          }),
    "flutter-web": dict(area="webchannel", client="Flutter Web (cloud_firestore) in Chromium",
                        transport="WebChannel, REST"),
    "sdk-aggregations": dict(area="queries", client="Admin SDK", transport="gRPC"),
    "sdk-auth": dict(area="admin", client="rules-unit-testing, web SDK, Admin SDK",
                     transport="gRPC"),
    "sdk-clear": dict(area="admin", client="rules-unit-testing, web SDK, Admin SDK",
                      transport="gRPC, REST"),
    "sdk-documents": dict(area="documents", client="Admin SDK", transport="gRPC"),
    "sdk-find-nearest": dict(area="queries", client="Admin SDK, web SDK", transport="gRPC"),
    "sdk-listen": dict(area="streams", client="Admin SDK, web SDK", transport="gRPC"),
    "sdk-listen-resume": dict(area="streams", client="web SDK, Admin SDK", transport="gRPC"),
    "sdk-queries": dict(area="queries", client="Admin SDK", transport="gRPC"),
    "sdk-rest": dict(area="rest", client="web SDK Lite", transport="REST"),
    "sdk-rules-unit-testing": dict(area="rules", client="firebase + rules-unit-testing in Node",
                                   transport="gRPC"),
    "sdk-transactions": dict(area="transactions", client="Admin SDK", transport="gRPC"),
    "sdk-web-transactions": dict(area="transactions", client="web SDK in Node", transport="gRPC"),
    "sdk-web-writes": dict(area="streams", client="web SDK in Node", transport="gRPC"),
}

# JSON files in results/ that are neither a transcript nor a replayed recording.
IGNORED = {
    "results/issues-firebase-tools-metadata.json": "metadata of firebase-tools issues",
}


def load(rel):
    with open(ROOT / rel, encoding="utf-8") as f:
        return json.load(f)


def oracle_version(rel, fixture):
    """The official emulator version a recording states: its `oracle` header, or its name."""
    header = fixture.get("oracle") if isinstance(fixture, dict) else None
    match = re.search(r"\bv(\d+(?:\.\d+)+)\b", header or "") or re.search(
        r"-official-v(\d+(?:\.\d+)+)\.json$", rel)
    return f"v{match.group(1)}" if match else None


def recordings():
    found = {p.relative_to(ROOT).as_posix() for p in ROOT.glob("crates/*/tests/fixtures/*.json")}
    unknown = sorted(found - set(RECORDINGS))
    if unknown:
        raise Stop(f"{', '.join(unknown)}: not in RECORDINGS. Add each with its area, the test "
                   "that replays it and how to count its answers, then run the script.")
    missing = sorted(rel for rel in RECORDINGS if not (ROOT / rel).is_file())
    if missing:
        raise Stop(f"{', '.join(missing)}: in RECORDINGS but not found; remove the entries.")
    rows = []
    for rel, entry in RECORDINGS.items():
        fixture = load(rel)
        try:
            answers = Counter(entry["count"](fixture))
        except (KeyError, TypeError) as err:
            raise Stop(f"{rel}: its format changed ({err!r}); update its count in RECORDINGS.")
        if not answers:
            raise Stop(f"{rel}: no answers counted; check its count in RECORDINGS.")
        rows.append(dict(entry, path=rel, answers=answers, oracle=oracle_version(rel, fixture)))
    return sorted(rows, key=lambda r: (AREA_ORDER.index(r["area"]), r["path"]))


def normalized(rel):
    return json.dumps(load(rel), sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def transcripts():
    files = {p.relative_to(ROOT).as_posix() for p in (ROOT / "results").glob("*.json")}
    accounted = set(IGNORED) | {rel for rel in RECORDINGS if rel.startswith("results/")}
    rows = []
    for name, entry in TRANSCRIPTS.items():
        pattern = rf"results/{re.escape(name)}-official-v[\d.]+\.json"
        official = sorted(rel for rel in files if re.fullmatch(pattern, rel))
        hidane = f"results/{name}-hidane.json"
        if len(official) != 1 or hidane not in files:
            raise Stop(f"TRANSCRIPTS[{name!r}]: expected one results/{name}-official-v<version>"
                       f".json and {hidane}; found {official or 'no official one'}.")
        others = [(hidane, "hidane")] + [(f"results/{variant}", what)
                                         for variant, what in entry.get("variants", {}).items()]
        for rel, what in others:
            if rel not in files:
                raise Stop(f"TRANSCRIPTS[{name!r}]: {rel} not found.")
            accounted.add(rel)
            rows.append(dict(entry, name=name, official=official[0], other=rel, what=what,
                             identical=normalized(official[0]) == normalized(rel)))
        accounted.add(official[0])
    unknown = sorted(files - accounted)
    if unknown:
        raise Stop(f"{', '.join(unknown)}: not in the mapping. Add a transcript pair to "
                   "TRANSCRIPTS, a replayed recording to RECORDINGS, or the file to IGNORED.")
    return sorted(rows, key=lambda r: (AREA_ORDER.index(r["area"]), r["name"], r["other"]))


def intentional_differences():
    """The rows of the table under "## Intentional differences" in docs/parity-exceptions.md."""
    text = (ROOT / "docs/parity-exceptions.md").read_text(encoding="utf-8")
    match = re.search(r"^## Intentional differences.*?$(.*?)(?=^## |\Z)", text, re.M | re.S)
    if not match:
        raise Stop("docs/parity-exceptions.md: no \"## Intentional differences\" section.")
    rows = [line for line in match.group(1).splitlines() if line.startswith("|")]
    if len(rows) < 3:
        raise Stop("docs/parity-exceptions.md: the intentional differences table is empty.")
    return len(rows) - 2  # the header and its separator


def n(number):
    return f"{number:,}"


def summary(answers):
    """`53 exact, 4 status only`: the comparisons of some answers, in COMPARED order."""
    totals = Counter()
    for (_, compared), count in answers.items():
        totals[compared] += count
    return ", ".join(f"{n(totals[key])} {label}" for key, label in COMPARED if totals[key])


def transports_of(answers, lang="en"):
    used = {transport for transport, _ in answers}
    return ", ".join(TRANSPORT_JA.get(t, t) if lang == "ja" else t for t in TRANSPORTS if t in used)


def by_area(rows, pairs):
    areas = {}
    for key in AREA_ORDER:
        answers = Counter()
        for row in rows:
            if row["area"] == key:
                answers.update(row["answers"])
        same = [pair["identical"] for pair in pairs if pair["area"] == key]
        if answers or same:
            areas[key] = dict(answers=answers, pairs=len(same), identical=sum(same))
    return areas


def total(counter, compared=None):
    return sum(count for (_, c), count in counter.items() if compared is None or c == compared)


def link(rel, base="../"):
    return f"[`{Path(rel).name}`]({base}{rel})"


def markdown(rows, pairs, exceptions):
    everything = Counter()
    for row in rows:
        everything.update(row["answers"])
    versions = Counter(row["oracle"] for row in rows if row["oracle"])
    stated = sum(versions.values())
    identical = sum(pair["identical"] for pair in pairs)
    out = [START, "<!-- Generated by tools/parity/table.py; edit the script, not this. -->", ""]
    out.append(
        f"**{n(total(everything))} recorded answers** in {len(rows)} recordings, replayed against "
        f"hidane by `cargo test`: {summary(everything)}. Oracle: "
        + ", ".join(f"{v} ({count} recordings)" for v, count in sorted(versions.items()))
        + f"; {len(rows) - stated} recordings do not state a version. SDK transcripts: "
        f"{identical} of {len(pairs)} identical. Intentional differences: {exceptions} "
        "([parity-exceptions.md](parity-exceptions.md)).")
    out += ["", "### By area and transport", "",
            "| Area | Transport | Recorded answers | Compared |", "|---|---|--:|---|"]
    for key, area in by_area(rows, pairs).items():
        first = True
        for transport in TRANSPORTS:
            answers = Counter({k: v for k, v in area["answers"].items() if k[0] == transport})
            if not answers:
                continue
            name = AREA_NAME["en"][key] if first else ""
            out.append(f"| {name} | {transport} | {n(total(answers))} | {summary(answers)} |")
            first = False
    out.append(f"| **Total** | | **{n(total(everything))}** | {summary(everything)} |")
    out += ["", "### By recording", "",
            "| Recording | Area | Transports | Counted | Answers | Compared | Oracle "
            "| Replayed by |",
            "|---|---|---|---|--:|---|---|---|"]
    for row in rows:
        tested = [row["test"]] if isinstance(row["test"], str) else row["test"]
        tests = ", ".join(link(test) for test in tested)
        out.append(
            f"| {link(row['path'])} | {AREA_NAME['en'][row['area']]} | "
            f"{transports_of(row['answers'])} | {row['counted']} | {n(total(row['answers']))} | "
            f"{summary(row['answers'])} | {row['oracle'] or 'not stated'} | {tests} |")
    out += ["", "### SDK transcripts", "",
            "| Transcript | Area | Client | Transport | Compared with the official one | Result |",
            "|---|---|---|---|---|---|"]
    for pair in pairs:
        result = "identical" if pair["identical"] else "differs"
        out.append(
            f"| {link(pair['official'])} | {AREA_NAME['en'][pair['area']]} | {pair['client']} | "
            f"{pair['transport']} | {link(pair['other'])} ({pair['what']}) | {result} |")
    out += ["", END]
    return "\n".join(out)


def html(rows, pairs, exceptions, lang):
    ja = lang == "ja"
    everything = Counter()
    for row in rows:
        everything.update(row["answers"])
    identical = sum(pair["identical"] for pair in pairs)
    docs = f"{REPO}/blob/main/docs/parity.md"
    readme = f"<a href=\"{REPO}/blob/main/results/README.md\">results/README.md</a>"
    exceptions_link = (f"<a href=\"{REPO}/blob/main/docs/parity-exceptions.md\">"
                       "parity exceptions</a>")

    def exact(answers):
        """The cell of "compared exactly": a count, or "not stated" where nothing says."""
        count = total(answers, EXACT)
        if count or not total(answers, UNSTATED):
            return f"<td class=\"num\">{n(count)}</td>"
        return f"<td class=\"ns\">{'記載なし' if ja else 'not stated'}</td>"

    def sdk(same, of):
        if not of:
            text = "—"
        else:
            text = f"{same}/{of} 組が一致" if ja else f"{same} of {of} identical"
        return f"<td class=\"sdk\">{text}</td>"

    if ja:
        intro = (f"<p class=\"fine\">現時点で、公式エミュレータの応答 {n(total(everything))} 件を "
                 f"{len(rows)} の記録に収め、CI がそのすべてを hidane に対して再生しています。"
                 "SDK の記録も両方のエミュレータで突き合わせています"
                 f"（<a href=\"{REPO}/tree/main/results\">results</a>）。数え方は "
                 f"<a href=\"{docs}\">docs/parity.md</a>（英語）にあります。</p>")
        head = ["領域", "トランスポート", "記録した<wbr>応答", "全体を比較", "SDK の記録"]
        foot = "合計"
        notes = ("<p class=\"fine\">全体を比較: 記録が応答の全体を比べると示している応答の数です。"
                 "示していない記録は「記載なし」とし、その比べ方は再生するテストに書かれています。"
                 "SDK の記録: 同じ手順を SDK から両方のエミュレータに対して実行した記録の組で、"
                 f"一致した組の数を示します。一致しない組の違いは {readme} に説明があります。"
                 f"意図的に公式と違える点（{exceptions} 件）は {exceptions_link}（英語）に"
                 "一覧があります。</p>")
    else:
        intro = (f"<p class=\"fine\">So far: {n(total(everything))} answers of the official "
                 f"emulator in {len(rows)} recordings, each replayed against hidane in CI, and SDK "
                 "transcripts from both emulators compared side by side "
                 f"(<a href=\"{REPO}/tree/main/results\">results</a>). How they are counted: "
                 f"<a href=\"{docs}\">docs/parity.md</a>.</p>")
        head = ["Area", "Transports", "Recorded answers", "Compared exactly", "SDK transcripts"]
        foot = "Total"
        notes = ("<p class=\"fine\">Compared exactly: answers whose recording marks them as "
                 "compared in full; “not stated” where a recording does not say, and the test that "
                 "replays it does. SDK transcripts: the same steps run through an SDK against both "
                 f"emulators; {readme} describes each difference. Known, intentional differences "
                 f"({exceptions}) are listed in {exceptions_link}.</p>")
    # On a narrow screen the transports go under the area's name (style.css).
    num = ' class="num"'
    classes = {1: ' class="parity-tr"', 2: num, 3: num}
    out = [START, intro, "<div class=\"parity-scroll\">", "<table class=\"parity-table\">",
           "<thead>",
           "<tr>" + "".join(f"<th scope=\"col\"{classes.get(i, '')}>{h}</th>"
                            for i, h in enumerate(head)) + "</tr>",
           "</thead>", "<tbody>"]
    for key, area in by_area(rows, pairs).items():
        name = AREA_NAME[lang][key].replace("&", "&amp;")
        transports = transports_of(area["answers"], lang) or "—"
        out.append(f"<tr><th scope=\"row\">{name}<span class=\"parity-sub\">{transports}</span>"
                   f"</th><td class=\"parity-tr\">{transports}</td>"
                   f"<td{num}>{n(total(area['answers']))}</td>{exact(area['answers'])}"
                   f"{sdk(area['identical'], area['pairs'])}</tr>")
    out += ["</tbody>", "<tfoot>",
            f"<tr><th scope=\"row\">{foot}</th><td class=\"parity-tr\"></td>"
            f"<td{num}>{n(total(everything))}</td>{exact(everything)}"
            f"{sdk(identical, len(pairs))}</tr>",
            "</tfoot>", "</table>", "</div>", notes, END]
    return "\n".join(out)


def badge(rows):
    answers = sum(total(row["answers"]) for row in rows)
    return json.dumps({"schemaVersion": 1, "label": "parity",
                       "message": f"{n(answers)} recorded answers", "color": BADGE_COLOR},
                      indent=2) + "\n"


def between_markers(rel, block):
    text = (ROOT / rel).read_text(encoding="utf-8")
    if text.count(START) != 1 or text.count(END) != 1 or text.index(START) > text.index(END):
        raise Stop(f"{rel}: expected one {START} before one {END}.")
    return text[:text.index(START)] + block + text[text.index(END) + len(END):]


def outputs():
    rows, pairs, exceptions = recordings(), transcripts(), intentional_differences()
    return {
        "docs/parity.md": between_markers("docs/parity.md", markdown(rows, pairs, exceptions)),
        "website/public/index.html": between_markers(
            "website/public/index.html", html(rows, pairs, exceptions, "en")),
        "website/public/ja/index.html": between_markers(
            "website/public/ja/index.html", html(rows, pairs, exceptions, "ja")),
        "website/public/badge/parity.json": badge(rows),
    }


def main():
    check = sys.argv[1:] == ["--check"]
    if sys.argv[1:] not in ([], ["--check"]):
        raise Stop("usage: python3 tools/parity/table.py [--check]")
    stale = []
    for rel, text in outputs().items():
        path = ROOT / rel
        if path.is_file() and path.read_text(encoding="utf-8") == text:
            continue
        stale.append(rel)
        if not check:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
    if check and stale:
        raise Stop(f"stale: {', '.join(stale)}. Run python3 tools/parity/table.py and commit "
                   "the result.")
    for rel in stale:
        print(f"wrote {rel}")


if __name__ == "__main__":
    try:
        main()
    except Stop as stop:
        sys.exit(f"tools/parity/table.py: {stop}")
