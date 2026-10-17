"""Generates the parity table from the recorded fixtures (#43).

Usage (from the repository root):
    python3 tools/parity/table.py           # writes the outputs below
    python3 tools/parity/table.py --check   # fails if any of them is stale

Reads every fixture the oracle scripts recorded (`crates/*/tests/fixtures/*.json`, and the
recordings in `results/` that a test replays) and counts, per recording, the official answers
its test replays against hidane: their area, the transport the replay uses, how the test compares
each one, and the oracle version the recording states. Reads the SDK transcript pairs in
`results/` (`<name>-official-v<version>.json` and `<name>-hidane.json`), says whether each pair
is equal as JSON and, where it is not, whether the documentation explains the difference, and
counts the intentional differences in `docs/parity-exceptions.md`. Writes, between
`<!-- parity:start -->` and `<!-- parity:end -->`:

- `docs/parity.md`: the tables, per area and transport, per recording and per transcript;
- the Parity section of `website/public/index.html` and `website/public/ja/index.html`;

and `website/public/badge/parity.json`, a shields.io endpoint.

Every recording and every transcript needs an entry in RECORDINGS or TRANSCRIPTS below: a new
fixture or `results/` file fails the script until it has one. A recording's `count` returns one
`(transport, level)` pair per answer its test replays, the level being how that test compares the
answer (LEVELS). The levels come from reading the test, which the entry names (`tests`, checked
to exist); where the recording itself marks an answer (`compare`, `differs`), the mark decides.
The allow-lists the tests keep (cases compared on their first line only, known differences) and
the cases they skip are read from the tests' sources. Stdlib only.
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
    ("datastore", "Datastore mode", "Datastore モード"),
]
AREA_ORDER = [key for key, _, _ in AREAS]
AREA_NAME = {"en": {k: en for k, en, _ in AREAS}, "ja": {k: ja for k, _, ja in AREAS}}

# How a replay reaches hidane. In-process: the test calls hidane's crates directly.
GRPC, REST, WEBCHANNEL, WEBSOCKET, CLI, IN_PROCESS = (
    "gRPC", "REST", "WebChannel", "WebSocket", "CLI", "in-process")
TRANSPORTS = [GRPC, REST, WEBCHANNEL, WEBSOCKET, CLI, IN_PROCESS]
TRANSPORT_JA = {IN_PROCESS: "プロセス内"}

# How a test compares an answer, from most to least (docs/parity.md says what each means).
FULL, REASONS, PART, STATUS, CODES, NONE, DIFFERS, UNSTATED = (
    "full", "reasons", "part", "status", "codes", "none", "differs", "unstated")
LEVELS = [
    (FULL, "in full"),
    (REASONS, "decision and reasons"),
    (PART, "in part"),
    (STATUS, "status only"),
    (CODES, "without messages"),
    (NONE, "not compared"),
    (DIFFERS, "intentionally different"),
    (UNSTATED, "not stated"),
]
# A recording's own `compare` marks; a step without one is compared in full.
COMPARE_MARKS = {"exact": FULL, "status": STATUS, "codes": CODES, "none": NONE}


class Stop(Exception):
    """A problem the person running the script has to fix."""


def source(rel):
    return (ROOT / rel).read_text(encoding="utf-8")


def rust_strings(rel, name):
    """The string literals of `const <name>` in a test, or of the `&[…]` after the call that
    names `<name>` (a fixture); for a list of pairs, the first of each pair."""
    text = source(rel)
    match = (re.search(rf"const {re.escape(name)}: &\[(\(&str, &str\)|&str)\] = &\[(.*?)\];",
                       text, re.S)
             or re.search(rf"{re.escape(name)}\"\), &\[(.*?)\]\)", text, re.S))
    if not match:
        raise Stop(f"{rel}: no {name} list found; update tools/parity/table.py.")
    pairs = match.lastindex == 2 and match.group(1).startswith("(")
    # A literal may hold escapes, a backslash before a line break among them.
    strings = re.findall(r'"((?:[^"\\]|\\[\s\S])*)"', match.group(match.lastindex))
    return set(strings[::2] if pairs else strings)


def compare_mark(item):
    mark = item.get("compare", "exact")
    if mark not in COMPARE_MARKS:
        raise Stop(f"unknown compare mark {mark!r}: add it to COMPARE_MARKS")
    return COMPARE_MARKS[mark]


def each(items, transport, level):
    return [(transport, level)] * len(items)


def denial(step):
    """A Security Rules denial: a 403 with the official message."""
    return step.get("status") == 403 and bool(((step.get("response") or {}).get("error") or {})
                                               .get("message"))


H = "crates/hidane/tests"
HR = "crates/hidane-rules/tests"


def value_order(f):
    values = [value for group in f["values"]["groups"] for value in group]
    return each(values, IN_PROCESS, FULL) + each(f["paths"]["order"], IN_PROCESS, FULL)


def rules_eval(f):
    known = rust_strings(f"{HR}/library.rs", "KNOWN")
    answers = []
    for result in f["results"]:
        outcome = result.get("outcome")
        if result["name"] in known or "answers" in result:
            level = DIFFERS  # `answers`: a batch the official emulator answers both ways (D020)
        elif result["name"].startswith("batches/"):
            level = FULL if outcome == "allow" else PART  # a denied batch: the decision only
        else:
            level = {"allow": FULL, "deny": REASONS, "compile": FULL}.get(outcome, DIFFERS)
        answers.append((IN_PROCESS, level))
    return answers


def rules_paths(f):
    answers = []
    for result in f["results"]:
        if result["condition"] != "true":
            continue  # the other conditions are compiled by warnings.rs
        if "compile" in result:
            answers.append((IN_PROCESS, PART))  # refused, its message is not compared here
        else:
            answers += each(result["reads"], IN_PROCESS, FULL)
    return answers


def rules_warnings(f):
    rel = f"{HR}/warnings.rs"
    differs = rust_strings(rel, "FIRST_LINE_ONLY") | rust_strings(rel, "KNOWN")
    return [(IN_PROCESS, DIFFERS if case["name"] in differs else FULL) for case in f["results"]]


def rules_conditions(f):
    known = rust_strings(f"{HR}/conditions.rs", "KNOWN")
    levels = {"allow": FULL, "false": REASONS, "error": REASONS, "compile": PART}
    return [(IN_PROCESS, DIFFERS if r["name"] in known else levels[r["outcome"]])
            for r in f["results"]]


def aggregations(f):
    return [(GRPC, FULL if case["outcome"]["status"] != "OK" else PART) for case in f["cases"]]


def enterprise_aggregations(f):
    """aggregations.rs's replay on an enterprise database: as on a standard one, but the sums
    that overflow in some orders of addition only (`ORDER_DEPENDENT`) are not compared."""
    skipped = rust_strings(f"{H}/aggregations.rs", "ORDER_DEPENDENT")
    return [(GRPC, NONE if case["name"] in skipped else level)
            for case, (_, level) in zip(f["cases"], aggregations(f))]


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
    return each(DOCUMENT_WRITES_CHECKED, GRPC, FULL)


def function_events(f):
    """Each step. A step whose v2 events the test compares with the official `ce-source`
    corrected, whose events the official emulator sends for writes that change nothing, which
    follows `POST /reset`, or which the official emulator fails for a v2 trigger without a
    database (keep in step with crates/hidane/tests/function_events.rs) is intentionally
    different; the events of a deleted path or a cleared database are compared in any order."""
    answers = []
    for run in f["runs"]:
        for case in run["cases"]:
            after_reset = False
            for step in case["steps"]:
                transport = GRPC if step["kind"] == "grpc" else REST
                v2 = [d for d in step["deliveries"]
                      if any(name.lower() == "ce-type" for name, _ in d["headers"])]
                failed = case["name"] == "a v2 trigger without a database" and step["status"] == 500
                if v2 or after_reset or failed:
                    level = DIFFERS
                elif step.get("method") == "DELETE" and step["path"].startswith("/emulator/v1/"):
                    level = PART
                else:
                    level = FULL
                after_reset |= step.get("path") == "/reset"
                answers.append((transport, level))
    return answers


def export_import(f):
    once = [key for key in ("big", "import_over", "production") if key in f]
    whole = once + list(f["imported"]) + f["requests"] + f["seeding"]
    return (each(f["exports"], REST, PART) + each(whole, REST, FULL)
            + each(f["startup"], CLI, FULL))


def field_order(f):
    answers = []
    for case in f:
        for step in case["steps"]:
            if step["status"] != 200:
                answers.append((REST, FULL))  # the status and the message
            elif "commitTime" in (step["response"] or {}):
                answers.append((REST, STATUS))  # its commit time names the commit later
            else:
                answers.append((REST, PART))  # the fields' order and the commit of updateTime
    return answers


def grpc_settings(f):
    sizes = [case for case in f["grpc_sizes"] if case["megabytes"] <= 20]
    return (each(f["v1beta1"], GRPC, FULL) + each(sizes, GRPC, FULL)
            + each(f["rest_sizes"], REST, FULL))


def read_time_and_databases(f):
    # The versions hidane keeps (the test's HIDANE_WINDOW): an answer the official emulator
    # serves from further back is intentionally "too old" (D065), and the REST `?readTime=`
    # parameter, which it always refuses, intentionally reads (D066).
    window = re.search(r"const HIDANE_WINDOW: u64 = ([\d_ *]+);",
                       source(f"{H}/read_time_and_databases.rs"))
    if not window:
        raise Stop(f"{H}/read_time_and_databases.rs: no HIDANE_WINDOW; update tools/parity/table.py.")
    seconds = 1
    for factor in window.group(1).split("*"):
        seconds *= int(factor.strip().replace("_", ""))
    answers = []
    for case in f["read_time"]["cases"]:
        for read, answer in zip(f["read_time"]["reads"], case["answers"]):
            transport = GRPC if read["transport"] == "grpc" else REST
            served = "too old" not in json.dumps(answer)
            differs = (read["name"] == "REST GET ?readTime=" or (case["age"] > seconds and served))
            answers.append((transport, DIFFERS if differs else FULL))
    # The writes to every database as one answer (their statuses), then each read back.
    return answers + [(REST, FULL)] + each(f["databases"]["reads"], REST, FULL)


def negative_zero(f):
    answers = []
    for step in f["steps"]:
        level = DIFFERS if "-1e-400" in step["name"] else FULL
        if "grpc" in step:
            answers.append((GRPC, level))
        elif "webchannel" in step:
            answers.append((WEBCHANNEL, level))
        elif any(key in step for key in ("rest", "rules", "export")):
            answers.append((REST, level))
        else:
            raise Stop(f"negative_zero.json: no transport for step {step.get('name')!r}")
    return answers


def list_documents(f):
    differs = rust_strings(f"{H}/list_documents.rs", "INTENTIONAL")
    return ([(GRPC, DIFFERS if case["name"] in differs else FULL) for case in f["grpc"]]
            + each(f["rest"], REST, FULL))


def queries(f):
    # queries.rs leaves out the `findNearest` cases (find_nearest.json covers them).
    return [(GRPC, FULL) for case in f["cases"] if "findNearest" not in case["query"]]


def listen_queries(skip=()):
    """listen_queries.rs's replay: each target's documents, or the cause of its removal, in
    full (the documents as a set); a target that ends the stream on its status only; the
    targets `skip` names (a `const` of the test) as intentionally different."""
    def count(f):
        ends = rust_strings(f"{H}/listen_queries.rs", "ENDS_THE_STREAM")
        skipped = rust_strings(f"{H}/listen_queries.rs", skip) if skip else set()
        return [(GRPC, DIFFERS if case["name"] in skipped else STATUS if case["name"] in ends
                 else FULL) for case in f["cases"]]
    return count


def enterprise_queries(f):
    """queries.rs's replay on an enterprise database: an error in full, but an `array_contains`
    error without the type it names; documents without their order where the query's orders tie,
    and without which ones a limit or an offset picks there."""
    def level(outcome):
        if outcome["status"] != "OK":
            return PART if "requires `Array`" in outcome.get("message", "") else FULL
        return PART
    return [(GRPC, level(case["outcome"])) for case in f["cases"]
            if "findNearest" not in case["query"]]


def rest(cases):
    """Cases in the format of `tools/oracle/rest.py`'s fixture."""
    return [(REST, compare_mark(case)) for case in cases]


def rules_server(name, skip=None):
    """rules_server.rs's replay: every status, and a denial's reasons; the denials of the
    cases it leaves out (`skip`, a `const` of the test) are intentionally different."""
    def count(f):
        skipped = rust_strings(f"{H}/rules_server.rs", skip or f"fixtures/{name}")
        return [(REST, DIFFERS if case["name"] in skipped else REASONS if denial(step) else STATUS)
                for case in f for step in case["steps"]
                if case["name"] not in skipped or denial(step)]
    return count


def rules_coverage(f):
    """rules_coverage.rs's replay: every status, what `debug()` printed, a denial's reasons,
    and the body of the rules and coverage reports; a case marked `differs`, its statuses."""
    answers = []
    for case in f:
        for step in case["steps"]:
            if "differs" in case:
                level = DIFFERS
            elif "raw" in step and step["status"] == 200:
                # The coverage page is hidane's own around the same data (parity exceptions).
                level = DIFFERS if ":ruleCoverage.html" in step["path"] else FULL
            elif denial(step):
                level = REASONS
            else:
                level = PART if step.get("log") else STATUS
            answers.append((REST, level))
    return answers


def rules_compile(f):
    rel = f"{H}/rules_compile.rs"
    differs = rust_strings(rel, "CRASHES") | rust_strings(rel, "FIRST_LINE_ONLY")
    return [(REST, DIFFERS if case["name"] in differs else FULL) for case in f]


def rules_streams(f):
    answers = []
    for case in f:
        for response in case["responses"]:
            if "targetChange" in response:
                cause = response["targetChange"].get("cause", {}).get("message")
                answers.append((GRPC, REASONS if cause else FULL))
            else:
                answers.append((GRPC, PART))  # a document's name and targets, a write's count
        if case.get("status") is not None:
            answers.append((GRPC, REASONS))  # the code, and the denial's reasons
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


def transforms(f):
    answers = []
    for case in f["cases"]:
        for step in case["steps"]:
            # A read back: its fields, or the code of a missing document. A commit: its
            # transform results, or its error.
            if step["method"] == "GET":
                answers.append((GRPC, PART if step["status"] == 200 else STATUS))
            else:
                answers.append((GRPC, FULL))
    return answers


def same_document_writes(f):
    answers = []
    for case in f["cases"]:
        for step in case["steps"]:
            error = (step["response"] or {}).get("error")
            if step["method"] == "GET":
                # Found: its fields and their order. Missing: the code.
                level = PART if step["status"] == 200 else STATUS
            elif error:
                # A failed precondition prints the official emulator's internal key (parity
                # exceptions): its code only. Other errors: the code and the message.
                level = STATUS if error["status"] in ("NOT_FOUND", "ALREADY_EXISTS") else FULL
            else:
                # The number of write results and the commit each one's update time comes from
                # (the times themselves differ from run to run); their transform results are not
                # compared.
                results = step["response"].get("writeResults", [])
                level = PART if any("transformResults" in r for r in results) else FULL
            answers.append((GRPC, level))
    return answers


def base_versions(f):
    differs = rust_strings(f"{H}/base_versions.rs", "INTENTIONAL")
    answers = []
    for case in f["cases"]:
        for step in case["steps"]:
            error = step.get("error") or {}
            if case["name"] in differs and step["method"] == "PATCH":
                level = DIFFERS
            elif error and error["status"] != "NOT_FOUND":
                level = FULL
            else:
                # A success on its status; a missing document on its code (its message prints
                # the official emulator's internal key, parity exceptions).
                level = STATUS
            answers.append((REST, level))
    return answers


def commit_checks(f, known_list="KNOWN"):
    rel = f"{H}/commit_checks.rs"
    known, differs = rust_strings(rel, known_list), rust_strings(rel, "DIFFERS")
    precondition = lambda outcome: outcome.get("code") in ("NOT_FOUND", "ALREADY_EXISTS")
    answers = []
    for case in f["order"]:
        for transport, outcome in case["outcomes"].items():
            if case["name"] in known:
                level = DIFFERS
            elif precondition(outcome) or any(map(precondition, outcome.get("statuses", []))):
                level = CODES  # its message prints the official emulator's internal key
            else:
                level = FULL
            answers.append((REST if transport in ("commit", "batch_write") else GRPC, level))
    for step in f["other_database"]:
        transport = GRPC if step["op"] in ("grpc_commit", "write_stream") else REST
        answers.append((transport, DIFFERS if step["name"] in differs else FULL))
    return answers


def webchannel(steps):
    """Steps in the format of `tools/oracle/webchannel.py`'s fixture; `commit` goes over REST,
    and a long-polling read is compared on its first data and whether it ended."""
    long_poll, answers = set(), []
    for step in steps:
        if step["op"] == "open":
            (long_poll.add if step.get("ci") == 1 else long_poll.discard)(step.get("session"))
        if "outcome" not in step:
            continue
        if step["op"] == "commit":
            answers.append((REST, FULL))
        elif step["op"] == "read" and step.get("session") in long_poll:
            answers.append((WEBCHANNEL, PART))
        else:
            answers.append((WEBCHANNEL, FULL))
    return answers


def first_pass(record):
    """A request monitor record (or a backlog of them) of the official emulator's first pass,
    which shows `resource` undefined where hidane's shows the stored document."""
    if isinstance(record, list):
        return any(first_pass(r) for r in record)
    if not isinstance(record, dict):
        return False
    context = record.get("rulesContext") or {}
    undefined = {"undefined": {}}
    request = (context.get("request") or {}).get("mapValue", {}).get("fields", {})
    return context.get("resource") == undefined or request.get("resource") == undefined


def websocket_requests(f):
    answers = []
    for case in f:
        for step in case["steps"]:
            if "differs" in step:
                level = DIFFERS
            else:
                records = step.get("records", []) + step.get("frames", [])
                level = PART if any(first_pass(r) for r in records) else FULL
            answers.append((WEBSOCKET, level))
    return answers


def websocket_escapes(f):
    undefined = ',"resource":{"undefined":{}}}}'
    return [(WEBSOCKET, PART if any(frame.endswith(undefined) for frame in step["frames"]) else FULL)
            for case in f for step in case["steps"]]


def write_stream(f):
    return [(GRPC, compare_mark(step))
            for scenario in f["scenarios"] for step in scenario["steps"] if "outcome" in step]


OVER_REST = "recorded over REST"

# Every recording a test replays: path -> area, the tests that replay it, what one answer is,
# what the tests compare, and how to count.
RECORDINGS = {
    "crates/hidane-core/tests/fixtures/value_order.json": dict(
        area="values",
        tests=["crates/hidane-core/tests/official_order.rs::values_within_a_group_are_equal",
               "crates/hidane-core/tests/official_order.rs::groups_are_strictly_ascending",
               "crates/hidane-core/tests/official_order.rs::"
               "document_names_sort_like_a_collection_group_query"],
        counted="each value placed in the official order, and each document name; " + OVER_REST,
        compares="in full: each value's place and its ties, the names' order",
        count=value_order),
    f"{HR}/fixtures/rules_eval.json": dict(
        area="rules", tests=[f"{HR}/library.rs::evaluation_matches_the_official_emulator"],
        counted="each condition or ruleset decided; " + OVER_REST,
        compares="an allow and a compile error in full; a denial as decision and reasons (the "
                 "entries of one evaluation where the official emulator lists two); a denied "
                 "batch on its decision only (in part); the official 500s, which hidane answers "
                 "with an evaluation error, and the batches the official emulator allows in "
                 "some projects and runs and denies in others, which hidane allows, as "
                 "intentionally different",
        count=rules_eval),
    f"{HR}/fixtures/rules_paths.json": dict(
        area="rules",
        tests=[f"{HR}/paths.rs::matches_cover_the_documents_the_official_emulator_allows"],
        counted="each read under `allow read: if true`, and each `match` path refused; "
                + OVER_REST,
        compares="each read's decision in full; a refused path on the refusal only (in part; "
                 "`rules_warnings.json` holds the message)",
        count=rules_paths),
    f"{HR}/fixtures/rules_warnings.json": dict(
        area="rules", tests=[f"{HR}/warnings.rs::warnings_are_the_official_emulators"],
        counted="each ruleset compiled, its `issues[]` or error; " + OVER_REST,
        compares="in full: every issue with its position and severity, or the error message; "
                 "the cases the test lists in `FIRST_LINE_ONLY` (the first line) and `KNOWN` "
                 "as intentionally different",
        count=rules_warnings),
    "results/rules-conditions-official-v1.22.0.json": dict(
        area="rules",
        tests=[f"{HR}/conditions.rs::conditions_decide_as_on_the_official_emulator"],
        counted="each condition decided; " + OVER_REST,
        compares="an allow in full; a denial (`false` or an error) as decision and reasons; a "
                 "ruleset that does not compile on the refusal only (in part; "
                 "`rules_warnings.json` holds the message)",
        count=rules_conditions),
    f"{H}/fixtures/aggregations.json": dict(
        area="queries",
        tests=[f"{H}/aggregations.rs::aggregations_answer_like_the_official_emulator"],
        counted="each query; " + OVER_REST,
        compares="an error in full (status and message); results in part: every aggregate value "
                 "with its type, not `done`, which gRPC does not carry",
        count=aggregations),
    f"{H}/fixtures/aggregations_enterprise.json": dict(
        area="queries",
        tests=[f"{H}/aggregations.rs::enterprise_aggregations_answer_like_the_official_emulator"],
        counted="each query, on an enterprise database; " + OVER_REST,
        compares="an error in full (status and message); results in part: every aggregate value "
                 "with its type, not `done`, which gRPC does not carry, and only the counts where "
                 "a limit or an offset picks documents in no defined order; sums that overflow "
                 "in some orders of addition only, not at all",
        count=enterprise_aggregations),
    f"{H}/fixtures/auth.json": dict(
        area="admin", tests=[f"{H}/auth.rs::authorization_is_read_like_the_official_emulator"],
        counted="each case",
        compares="in full: the outcome as the recording summarises it (status or code and "
                 "message)",
        count=lambda f: (each(f["classify"], REST, FULL) + each(f["grpc"], GRPC, FULL)
                         + each(f["rest"], REST, FULL))),
    f"{H}/fixtures/base_versions.json": dict(
        area="documents",
        tests=[f"{H}/base_versions.rs::update_time_preconditions_like_the_official_emulator"],
        counted="each call",
        compares="an error in full (code and message, its versions those of hidane's own "
                 "commits), but a missing document on its code (its message prints the official "
                 "emulator's internal key, an intentional difference); a success on its status; "
                 "REST's `currentDocument.updateTime` parameter as intentionally different",
        count=base_versions),
    f"{H}/fixtures/bulk_delete.json": dict(
        area="admin",
        tests=[f"{H}/rest.rs::bulk_delete_answers_like_the_official_emulator"],
        counted="each request (the writes before a bulk delete and the query after it too)",
        compares="in full: status, content type and body byte for byte, server times masked",
        count=lambda f: rest(f["cases"])),
    f"{H}/fixtures/index_usage.json": dict(
        area="queries",
        tests=[f"{H}/index_usage.rs::index_usage_answers_like_the_official_emulator"],
        counted="each query of `queries.json` and `aggregations.json` that runs, each sequence "
                "of requests and each request to the endpoint",
        compares="in full: what `:indexUsage` answers after each, a database's reports as a set "
                 "(the official emulator's order changes from run to run), a sequence's answers "
                 "too, and an endpoint's status, content type and body byte for byte",
        count=lambda f: (each(f["cases"], REST, FULL) + each(f["sequences"], REST, FULL)
                         + each(f["endpoints"], REST, FULL))),
    f"{H}/fixtures/index_usage_enterprise.json": dict(
        area="queries",
        tests=[f"{H}/index_usage.rs::enterprise_index_usage_answers_like_the_official_emulator"],
        counted="each sequence of requests, on an enterprise database",
        compares="in full: the sequence's answers and what `:indexUsage` answers after it, the "
                 "reports as a set",
        count=lambda f: each(f["sequences"], REST, FULL)),
    f"{H}/fixtures/commit_checks.json": dict(
        area="documents",
        tests=[f"{H}/commit_checks.rs::the_first_error_is_the_official_one",
               f"{H}/commit_checks.rs::writes_to_another_database_are_refused"],
        counted="each commit over each transport it was recorded over, and each step that "
                "writes to or reads after another database",
        compares="in full: the first error's code and message, the number of write results or "
                 "`:batchWrite`'s statuses, a read's status, names found or missing; a failed "
                 "precondition without its message (it prints the official emulator's internal "
                 "key); the cases the test lists in `KNOWN` (array unions of arrays and of vectors with NaN) and the "
                 "steps in `DIFFERS` (writes to another database, which hidane refuses, and the "
                 "reads that follow) as intentionally different",
        count=commit_checks),
    f"{H}/fixtures/commit_checks_enterprise.json": dict(
        area="documents",
        tests=[f"{H}/commit_checks.rs::the_first_error_is_the_official_one_on_an_enterprise_database",
               f"{H}/commit_checks.rs::writes_to_another_database_are_refused_on_an_enterprise_database"],
        counted="each commit over each transport it was recorded over, and each step that "
                "writes to or reads after another database, on an enterprise database",
        compares="as `commit_checks.json`; the cases the test lists in `KNOWN_ENTERPRISE` (an "
                 "array union of a vector with NaN) and the steps in `DIFFERS` as intentionally "
                 "different",
        count=lambda f: commit_checks(f, "KNOWN_ENTERPRISE")),
    f"{H}/fixtures/conformance.json": dict(
        area="rest",
        tests=[f"{H}/rest.rs::conformance_requests_answer_like_the_official_emulator"],
        counted="each request (the conformance tests' requests, and a create before each "
                "update or delete)",
        compares="in full: status, content type and body byte for byte, server times, IDs and "
                 "tokens masked",
        count=lambda f: rest(f["cases"])),
    f"{H}/fixtures/cors.json": dict(
        area="rest", tests=[f"{H}/cors.rs::cors_answers_like_the_official_emulator"],
        counted="each request",
        compares="in full: status, every header but `date`, `content-length` and `connection`, "
                 "and the body",
        count=lambda f: each(f["cases"], REST, FULL)),
    f"{H}/fixtures/datastore_entities.json": dict(
        area="datastore",
        tests=[f"{H}/datastore.rs::datastore_mode_answers_like_the_official_emulator"],
        counted="each request, to a server started with `--database-mode datastore-mode`; "
                + OVER_REST,
        compares="in full: status, content type and body byte for byte, server times, versions, "
                 "transaction IDs and cursors masked; the requests the test lists in `KNOWN` "
                 "as intentionally different (D025)",
        count=lambda f: [(REST, DIFFERS if case["name"] in rust_strings(f"{H}/datastore.rs", "KNOWN")
                          else FULL) for case in f["cases"]]),
    f"{H}/fixtures/datastore_export.json": dict(
        area="datastore",
        tests=[f"{H}/datastore.rs::export_and_import_like_the_official_emulator"],
        counted="each request of each run, to a server started with `--database-mode "
                "datastore-mode` (and `--import-data`, where the run says so), and each export "
                "written; " + OVER_REST,
        compares="in full: status, content type and body byte for byte, server times, versions "
                 "and cursors masked; an export's overall metadata byte for byte, its output's "
                 "records as a set (D093), its other files by size",
        count=lambda f: [(REST, FULL) for run in f["runs"]
                         for _ in [*run["cases"], *run.get("exports", {})]]),
    f"{H}/fixtures/datastore_firestore_api.json": dict(
        area="datastore",
        tests=[f"{H}/datastore.rs::the_firestore_api_is_refused_as_on_the_official_emulator"],
        counted="each request to the Firestore API and to the emulator's own endpoints, to a "
                "server started with `--database-mode datastore-mode`, over REST and gRPC",
        compares="in full: REST status, content type and body byte for byte; gRPC code and "
                 "message; the requests the test lists in `KNOWN_CONTENT_TYPES` as "
                 "intentionally different (D086)",
        count=lambda f: [(REST, DIFFERS if case["name"] in rust_strings(f"{H}/datastore.rs",
                                                                        "KNOWN_CONTENT_TYPES")
                          else FULL) for case in f["cases"]]
                        + [(GRPC, FULL) for _ in f["grpc"]]),
    f"{H}/fixtures/datastore_gql.json": dict(
        area="datastore",
        tests=[f"{H}/datastore.rs::gql_answers_like_the_official_emulator"],
        counted="each GQL query and aggregation query, to a server started with "
                "`--database-mode datastore-mode` over the dataset of `datastore_queries.json`; "
                + OVER_REST,
        compares="in full: status, content type and body byte for byte (the structured query "
                 "the text reads as, the results, the parser's messages), server times, "
                 "versions, transaction IDs and cursors masked",
        count=lambda f: [(REST, FULL) for _ in f["cases"]]),
    f"{H}/fixtures/datastore_indexes.json": dict(
        area="datastore",
        tests=[f"{H}/datastore.rs::indexes_answer_like_the_official_emulator"],
        counted="each request of each run, to a server started with `--database-mode "
                "datastore-mode` and the run's flags (`--require_indexes`, `--index_file`) and "
                "index file; " + OVER_REST,
        compares="in full: status, content type and body byte for byte, server times, versions, "
                 "transaction IDs and cursors masked",
        count=lambda f: [(REST, FULL) for run in f["runs"] for case in run["cases"]
                         if "outcome" in case]),
    f"{H}/fixtures/datastore_projects.json": dict(
        area="datastore",
        tests=[f"{H}/datastore.rs::projects_are_read_as_on_the_official_emulator"],
        counted="each Datastore request, over gRPC and REST, to a server in Firestore Native "
                "mode and to one in Datastore mode",
        compares="in full: gRPC code and message (an answer's content not); REST status, content "
                 "type and body byte for byte, times, versions and transaction IDs masked",
        count=lambda f: [(GRPC, FULL) for run in f["runs"] for _ in run["grpc"]]
                        + [(REST, FULL) for run in f["runs"] for _ in run["cases"]]),
    f"{H}/fixtures/datastore_protobuf.json": dict(
        area="datastore",
        tests=[f"{H}/datastore.rs::protobuf_answers_like_the_official_emulator"],
        counted="each request, to a server started with `--database-mode datastore-mode`, its "
                "body in protobuf or JSON as its `Content-Type` says; " + OVER_REST,
        compares="in full: status, content type and body byte for byte, where the answer holds no "
                 "time or version; status and content type where it does; the requests the test "
                 "lists in `KNOWN_CONTENT_TYPES` as intentionally different (D086)",
        count=lambda f: [(REST, DIFFERS if case["name"] in rust_strings(f"{H}/datastore.rs",
                                                                         "KNOWN_CONTENT_TYPES")
                          else FULL if case["compare"] == "exact" else STATUS)
                         for case in f["cases"]]),
    f"{H}/fixtures/datastore_queries.json": dict(
        area="datastore",
        tests=[f"{H}/datastore.rs::datastore_queries_answer_like_the_official_emulator"],
        counted="each query and aggregation query, to a server started with "
                "`--database-mode datastore-mode` over the recording's dataset; " + OVER_REST,
        compares="in full: status, content type and body byte for byte, server times, versions, "
                 "transaction IDs and cursors masked (each cursor sent back as the server gave it)",
        count=lambda f: [(REST, FULL) for _ in f["cases"]]),
    f"{H}/fixtures/document_writes.json": dict(
        area="documents",
        tests=[f"{H}/documents.rs::reads_and_their_errors", f"{H}/documents.rs::preconditions",
               f"{H}/documents.rs::invalid_writes_use_the_official_messages",
               f"{H}/documents.rs::listing_documents_and_collections",
               f"{H}/documents.rs::batch_write_reports_each_write"],
        counted="each recorded message the tests check hidane's against (they replay no case "
                "as a whole); " + OVER_REST,
        compares="in full: the code and the message (a stored version's timestamps masked)",
        count=document_writes),
    f"{H}/fixtures/export_import.json": dict(
        area="export",
        tests=[f"{H}/export_import.rs::exports_match_the_official_files",
               f"{H}/export_import.rs::imports_write_what_the_official_emulator_writes",
               f"{H}/export_import.rs::production_shaped_exports_import_like_the_official_emulator",
               f"{H}/export_import.rs::the_endpoints_answer_like_the_official_emulator",
               f"{H}/export_import.rs::databases_are_seeded_like_the_official_emulator",
               f"{H}/export_import.rs::start_up_like_the_official_emulator"],
        counted="each export, import, endpoint answer, seeding step and start-up",
        compares="an export in part: its files, metadata byte for byte and every record, in "
                 "name order (an intentional difference) and without times; the rest in full: "
                 "imported documents, the endpoints' status, content type and body, seeding, "
                 "exit codes and messages",
        count=export_import),
    f"{H}/fixtures/field_order.json": dict(
        area="documents", tests=[f"{H}/field_order.rs::fields_come_back_in_the_official_order"],
        counted="each call",
        compares="an error in full (status and message); a commit on its status; a read in "
                 "part: the order of every field, nested ones included, and which commit its "
                 "`updateTime` names, not the values",
        count=lambda f: field_order(f)),
    f"{H}/fixtures/find_nearest.json": dict(
        area="queries", tests=[f"{H}/find_nearest.rs::find_nearest_like_the_official_emulator"],
        counted="each query, aggregation and write",
        compares="in full: the documents' names and fields but the vector (as the recording "
                 "keeps them), the aggregate values, or the status and message",
        count=lambda f: each(f["queries"] + f["aggregations"] + f["writes"], REST, FULL)),
    f"{H}/fixtures/function_triggers.json": dict(
        area="admin",
        tests=[f"{H}/function_triggers.rs::registrations_answer_like_the_official_emulator"],
        counted="each registration, in both runs",
        compares="in full: status, content type and body",
        count=lambda f: [a for run in f["runs"] for a in each(run["cases"], REST, FULL)]),
    f"{H}/fixtures/startup_flags.json": dict(
        area="admin",
        tests=[f"{H}/launch.rs::starts_and_refusals_like_the_official_emulator",
               f"{H}/launch.rs::queries_are_refused_with_require_indexes_in_firestore_mode"],
        counted="each start of the emulator with its flags, and each request to the one started "
                "with `--require_indexes` in Firestore Native mode",
        compares="a start: the banner's edition and mode, or the exit code and the message it "
                 "stops with; a request: the status and the error's message",
        count=lambda f: [(CLI, FULL) for _ in f["starts"]]
                        + [(REST, PART) for _ in f["index_enforcement_in_firestore_mode"]["requests"]]),
    f"{H}/fixtures/function_events.json": dict(
        area="admin",
        tests=[f"{H}/function_events.rs::events_reach_the_functions_emulator_as_on_the_official_emulator"],
        counted="each step of each case (registrations and writes over REST, gRPC and the Write "
                "stream), in the three runs: with a Functions emulator, without, and with one "
                "that does not listen",
        compares="in full: the status, every request the Functions emulator received (method, "
                 "path, content type and `ce-*` headers, the v1 body byte for byte and the v2 "
                 "body decoded, times and IDs as labels), and every `WARNING` about events. "
                 "Intentionally different: steps with v2 events (`ce-source` names the project "
                 "once), events for writes that change nothing, after `POST /reset`, and a v2 "
                 "trigger without a database; in part (in any order): a deleted path or a "
                 "cleared database",
        count=function_events),
    f"{H}/fixtures/grpc_settings.json": dict(
        area="values",
        tests=[f"{H}/grpc_settings.rs::v1beta1_is_v1_under_another_name",
               f"{H}/grpc_settings.rs::large_grpc_requests_pass",
               f"{H}/grpc_settings.rs::rest_bodies_up_to_16_mib"],
        counted="each v1beta1 call, gRPC size up to 20 MB and REST size (not the larger gRPC "
                "sizes nor the reflection listing)",
        compares="in full: the outcome as the recording summarises it",
        count=grpc_settings),
    f"{H}/fixtures/limits.json": dict(
        area="values", tests=[f"{H}/limits.rs::limits_like_the_official_emulator"],
        counted="each commit", compares="in full: status and message",
        count=lambda f: each(f["cases"], REST, FULL)),
    f"{H}/fixtures/list_documents.json": dict(
        area="documents",
        tests=[f"{H}/list_documents.rs::list_documents_like_the_official_emulator"],
        counted="each case (a paged case is one)",
        compares="in full: the names of every page, or the code and message; a page token next "
                 "to an order as intentionally different",
        count=list_documents),
    f"{H}/fixtures/negative_zero.json": dict(
        area="values",
        tests=[f"{H}/negative_zero.rs::every_recorded_step_gives_the_official_answer"],
        counted="each step",
        compares="in full: the answer as the recording summarises it, each double spelled out; "
                 "the steps that write `-1e-400` as intentionally different",
        count=negative_zero),
    f"{H}/fixtures/pipelines.json": dict(
        area="queries",
        tests=[f"{H}/pipelines.rs::pipelines_run_like_the_official_emulator"],
        counted="each pipeline, on an enterprise database, and each sequence of requests "
                "(transactions, read times, requests without a pipeline); " + OVER_REST,
        compares="in full: an error's status and message; the results' names, fields and "
                 "whether they have times, in order after a sort and as a set otherwise "
                 "(D092); whether the response has an execution time and a transaction",
        count=lambda f: each(f["cases"], REST, FULL) + each(f["sequences"], REST, FULL)),
    f"{H}/fixtures/project_ids.json": dict(
        area="rest",
        tests=[f"{H}/rest.rs::project_ids_answer_like_the_official_emulator"],
        counted="each request",
        compares="in full: status, content type and body byte for byte, server times and IDs "
                 "masked",
        count=lambda f: rest(f["cases"])),
    f"{H}/fixtures/single_project_mode.json": dict(
        area="admin",
        tests=[f"{H}/single_project_mode.rs::requests_for_another_project_like_the_official_emulator"],
        counted="each step of both runs (`--single_project_mode`, then with "
                "`--single_project_mode_error`)",
        compares="in full: the answer (REST status and body with server times masked, the gRPC "
                 "code and message, the WebChannel handshake's first frame or that none came) and "
                 "every line printed on standard output",
        count=lambda f: [({"rest": REST, "grpc": GRPC, "webchannel": WEBCHANNEL}[s["kind"]], FULL)
                         for run in f["runs"] for s in run["steps"]]),
    f"{H}/fixtures/queries.json": dict(
        area="queries", tests=[f"{H}/queries.rs::queries_answer_like_the_official_emulator"],
        counted="each query but the `findNearest` one; " + OVER_REST,
        compares="in full: each response's document, `skippedResults` and `done`, the fields of "
                 "projected documents, or the status and message",
        count=queries),
    f"{H}/fixtures/listen_queries.json": dict(
        area="streams",
        tests=[f"{H}/listen_queries.rs::listen_targets_answer_like_the_official_emulator"],
        counted="each target (the cases of `queries.json` as Listen targets), recorded over "
                "WebChannel, replayed over gRPC",
        compares="in full: the documents the target holds when current, as a set, or the cause "
                 "of its removal; a target that ends the stream (D006) on whether it does",
        count=listen_queries()),
    f"{H}/fixtures/listen_queries_enterprise.json": dict(
        area="streams",
        tests=[f"{H}/listen_queries.rs::enterprise_listen_targets_answer_like_the_official_emulator"],
        counted="each target, on an enterprise database, recorded over WebChannel, replayed "
                "over gRPC",
        compares="as `listen_queries.json`; the targets the test lists in `KNOWN_ENTERPRISE` "
                 "as intentionally different (D091)",
        count=listen_queries("KNOWN_ENTERPRISE")),
    f"{H}/fixtures/queries_enterprise.json": dict(
        area="queries",
        tests=[f"{H}/queries.rs::enterprise_queries_answer_like_the_official_emulator"],
        counted="each query but the `findNearest` one, on an enterprise database; " + OVER_REST,
        compares="an error in full but the type an `array_contains` error names; the documents "
                 "of the responses, `skippedResults`, `done` and the fields of projected "
                 "documents, in the order of the values the query orders by (documents that tie "
                 "come in no defined order, and a limit or an offset picks any of them)",
        count=enterprise_queries),
    f"{H}/fixtures/read_time_and_databases.json": dict(
        area="documents",
        tests=[f"{H}/read_time_and_databases.rs::read_times_up_to_the_window",
               f"{H}/read_time_and_databases.rs::databases_without_a_limit"],
        counted="each read at each age, the statuses of the writes to every database as one, "
                "and each read back",
        compares="in full: the code, the message and the names of the documents (gRPC), or the "
                 "status and the body with server times and transaction IDs masked (REST); the "
                 "reads from further back than hidane keeps versions, and the REST `?readTime=` "
                 "parameter, as intentionally different (docs/parity-exceptions.md)",
        count=read_time_and_databases),
    f"{H}/fixtures/json_escapes.json": dict(
        area="values",
        tests=[f"{H}/rest.rs::strings_escape_like_the_official_emulator",
               f"{H}/webchannel.rs::strings_escape_like_the_official_emulator"],
        counted="each REST request and each WebChannel step",
        compares="in full: REST as `rest.json` (its `compare` marks), WebChannel messages as "
                 "their exact JSON text",
        count=lambda f: rest(f["rest"]) + webchannel(f["webchannel"]["steps"])),
    f"{H}/fixtures/rest.json": dict(
        area="rest", tests=[f"{H}/rest.rs::rest_answers_like_the_official_emulator"],
        counted="each request",
        compares="as each case's `compare` marks it: `exact` in full (status, content type and "
                 "body byte for byte, server times, IDs and tokens masked), `status` on its "
                 "status and content type",
        count=lambda f: rest(f["cases"])),
    f"{H}/fixtures/rules_compile.json": dict(
        area="rules", tests=[f"{H}/rules_compile.rs::compiles_like_the_official_emulator"],
        counted="each ruleset loaded",
        compares="in full: status and body byte for byte; the cases the test lists in `CRASHES` "
                 "and `FIRST_LINE_ONLY` as intentionally different",
        count=rules_compile),
    f"{H}/fixtures/rules_streams.json": dict(
        area="rules", tests=[f"{H}/rules_streams.rs::streams_under_rules"],
        counted="each stream response, and each error that ended a stream",
        compares="a target change in full (type, targets, resume tokens and times masked); a "
                 "denied target's cause and an error that ended the stream as decision and "
                 "reasons; a document change in part (its name and targets, not its fields) and "
                 "a write response in part (its number of results)",
        count=rules_streams),
    f"{H}/fixtures/rules_expressions_grpc.json": dict(
        area="rules", tests=[f"{H}/rules_streams.rs::the_limit_of_expressions"],
        counted="each response, and each error that ended a call",
        compares="a stream's responses as `rules_streams.json`'s; the responses of the other "
                 "calls in part (their number); an error that ended a call as decision and "
                 "reasons",
        count=rules_streams),
    f"{H}/fixtures/transactions.json": dict(
        area="transactions", tests=[f"{H}/transactions.rs::replay"],
        counted="each step with an outcome, except those of the scenario marked `slow`",
        compares="as each step's `compare` marks it; without a mark in full (status, message, "
                 "elapsed time to 0.5 s and the result, not transaction IDs)",
        count=transactions),
    f"{H}/fixtures/same_document_writes.json": dict(
        area="documents",
        tests=[f"{H}/same_document_writes.rs::every_recorded_case_gives_the_official_answer"],
        counted="each call; " + OVER_REST,
        compares="an error in full (code and message), but a failed precondition on its code (its "
                 "message prints the official emulator's internal key, an intentional "
                 "difference); a commit in full (its number of write results and the commit "
                 "each one's update time comes from, or that it has none; the times themselves "
                 "differ from run to run), in part where they hold transform results, which are "
                 "not compared; a read in part (the fields and their order), a missing document "
                 "on its code",
        count=same_document_writes),
    f"{H}/fixtures/transforms.json": dict(
        area="documents",
        tests=[f"{H}/transforms.rs::every_recorded_case_gives_the_official_answer"],
        counted="each call; " + OVER_REST,
        compares="a commit in full (its transform results, server times as the request time, "
                 "or the code and message); a read in part (the document's fields), a missing "
                 "document on its code",
        count=transforms),
    f"{H}/fixtures/unsupported.json": dict(
        area="queries",
        tests=[f"{H}/unsupported.rs::ignored_and_refused_like_the_official_emulator"],
        counted="each call", compares="in full: the outcome as the recording summarises it",
        count=lambda f: each(f["grpc"], GRPC, FULL) + each(f["rest"], REST, FULL)),
    f"{H}/fixtures/webchannel.json": dict(
        area="webchannel", tests=[f"{H}/webchannel.rs::webchannel_like_the_official_emulator"],
        counted="each step",
        compares="in full: status, frames and messages (session IDs, times and tokens masked); a "
                 "long-polling read in part (its first data and whether it ended: where the "
                 "`noop`s fall depends on timing)",
        count=lambda f: webchannel(f["steps"])),
    f"{H}/fixtures/websocket_requests.json": dict(
        area="rules",
        tests=[f"{H}/websocket_requests.rs::records_as_the_official_emulator_sends_them"],
        counted="each step: the request monitor's records of a call, or a WebSocket exchange",
        compares="in full: every record field for field (times, request IDs and the release "
                 "key's number masked, a list filter's fields in any order); in part where an "
                 "official record shows `resource` undefined (its first pass), which accepts "
                 "hidane's stored document; a step marked `differs` (method and path only) as "
                 "intentionally different",
        count=websocket_requests),
    f"{H}/fixtures/websocket_escapes.json": dict(
        area="rules",
        tests=[f"{H}/websocket_requests.rs::strings_written_as_the_official_emulator_writes_them"],
        counted="each step: a call, and the request monitor's records of it as text",
        compares="text for text, times, request IDs and the release key's number masked; in part "
                 "where an official record shows `resource` undefined (its first pass), which is "
                 "left out",
        count=websocket_escapes),
    f"{H}/fixtures/rules_expressions_monitor.json": dict(
        area="rules",
        tests=[f"{H}/websocket_requests.rs::a_decision_past_the_limit_of_expressions"],
        counted="each step: the request monitor's records of a call, or a WebSocket exchange",
        compares="as `websocket_requests.json`'s; the steps marked `differs` (the official "
                 "emulator records nothing from a decision past the limit on, to a client that "
                 "connects after it) as intentionally different",
        count=websocket_requests),
    f"{H}/fixtures/write_stream.json": dict(
        area="streams", tests=[f"{H}/write_stream.rs::replay"],
        counted="each step with an outcome",
        compares="as each step's `compare` marks it; without a mark in full (the response's "
                 "token, write results and commit time as the recording summarises them, the "
                 "error, the elapsed time)",
        count=write_stream),
}
for _name in ("rules_auth", "rules_denials", "rules_expressions", "rules_list", "rules_lookups",
              "rules_request", "rules_same_document", "rules_writes"):
    RECORDINGS[f"{H}/fixtures/{_name}.json"] = dict(
        area="rules", tests=[f"{H}/rules_server.rs::replay"], counted="each call",
        compares="a denial as decision and reasons (`same_denial`: hidane's reasons are among "
                 "the official ones); every other answer on its status",
        count=rules_server(f"{_name}.json", "ALLOWED_THEN_UNMATCHED" if _name == "rules_writes" else None))
for _name in ("rules_coverage", "rules_coverage_flag", "rules_expressions_report", "rules_request_fields",
              "rules_verify"):
    RECORDINGS[f"{H}/fixtures/{_name}.json"] = dict(
        area="rules", tests=[f"{H}/rules_coverage.rs::replay"], counted="each call",
        compares="every status and what `debug()` printed (without `request.time`); the rules "
                 "and coverage reports in full (byte for byte, without `request.time`); a denial "
                 "as decision and reasons (`same_denial`); a call that printed in part, the "
                 "others on their status; the coverage page and the cases marked `differs` as "
                 "intentionally different",
        count=rules_coverage)

# Where the documentation explains why a transcript pair differs: (file, a phrase it holds).
SDK_DIFFERENCES = {
    "browser-rules": ("results/README.md", "the same 13 denial messages differ in the same way"),
    "flutter-web": ("results/README.md", "Identical except the message of the three `NOT_FOUND`"),
    "functions-triggers": ("docs/parity-exceptions.md",
                           "A write that changes nothing sends v2 `updated` and `written` events"),
    "sdk-clear": ("results/README.md", "reaches the web SDK in one snapshot on hidane and two"),
    "sdk-documents": ("docs/parity-exceptions.md",
                      "Precondition failures print the emulator's internal Datastore key"),
    "sdk-listen": ("results/README.md", "a batch reaches the web SDK's limit query as two"),
    "sdk-listen-resume": ("results/README.md", "the official emulator gets there in three"),
    "sdk-rules-unit-testing": ("results/README.md", "13 denial messages differ"),
    "nodejs-system-tests": ("results/README.md",
                            "fails on the official emulator alone, which reads the vector field path"),
    "nodejs-system-tests-rest": ("results/README.md", "and the same one more (D033)"),
    "sdk-ios-listen": ("results/README.md", "reaches the iOS SDK's query listeners as two snapshots"),
    "sdk-android-listen": ("results/README.md",
                           "reaches the Android SDK's query listeners as two snapshots"),
}
# The Go, Python and Java SDKs' transcripts (tools/oracle/sdk_go/ and the others) differ in the
# same messages.
for _sdk in ("go", "java", "python"):
    for _section in ("documents", "transactions"):
        SDK_DIFFERENCES[f"sdk-{_sdk}-{_section}"] = SDK_DIFFERENCES["sdk-documents"]


def suite_outline(data):
    """What a test suite's transcript holds: its counts of outcomes."""
    counts = data["counts"]
    return (f"the outcome of each test: {n(counts['passed'])} passed, "
            f"{n(counts['skipped'])} skipped, {n(counts['failed'])} failed")


# SDK transcripts in results/: name -> area, the client, its transport, and other transcripts
# compared with the official one besides `<name>-hidane.json`; `ignore` lists top-level keys
# left out of the comparison, `outline` says what the official transcript holds.
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
    "functions-triggers": dict(area="admin",
                               client="firebase-tools Functions emulator (firebase-functions v1 "
                                      "and v2), Admin SDK",
                               transport="CLI, gRPC"),
    # Each test's outcome; `emulator` names the emulator the suite ran against.
    "js-sdk-browser": dict(area="webchannel", client="firebase-js-sdk browser suite in Chromium",
                           transport="WebChannel, REST", ignore=("emulator",),
                           outline=suite_outline),
    # Each test's outcome; `emulator` names the emulator the suite ran against.
    "nodejs-system-tests": dict(area="admin", client="nodejs-firestore system tests (Admin SDK)",
                                transport="gRPC", ignore=("emulator",), outline=suite_outline),
    "nodejs-system-tests-rest": dict(area="rest",
                                     client="nodejs-firestore system tests (Admin SDK) over REST",
                                     transport="REST", ignore=("emulator",),
                                     outline=suite_outline),
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
    "sdk-web-aggregations": dict(area="queries", client="web SDK in Node", transport="gRPC"),
    "sdk-web-transactions": dict(area="transactions", client="web SDK in Node", transport="gRPC"),
    "sdk-web-writes": dict(area="streams", client="web SDK in Node", transport="gRPC"),
}
# One transcript per section of tools/oracle/sdk_go/, sdk_python/ and sdk_java/.
for _sdk, _client in (("go", "Go SDK"), ("java", "Java SDK"), ("python", "Python SDK"),
                      ("ios", "iOS SDK (FirebaseFirestore, on macOS)"),
                      ("android", "Android SDK (firebase-firestore, in an Android emulator)")):
    for _section, _area in (("documents", "documents"), ("listen", "streams"),
                            ("queries", "queries"), ("transactions", "transactions")):
        TRANSCRIPTS[f"sdk-{_sdk}-{_section}"] = dict(area=_area, client=_client, transport="gRPC")

# One transcript per section of tools/oracle/sdk_datastore_python/ and sdk_datastore_java/, over
# gRPC and over HTTP, and of sdk_datastore_go/ and sdk_datastore_node.mjs, over gRPC.
for _sdk, _client in (("python", "Datastore Python client (google-cloud-datastore)"),
                      ("java", "Datastore Java client (google-cloud-datastore)")):
    for _section in ("entities", "queries", "transactions"):
        TRANSCRIPTS[f"sdk-datastore-{_sdk}-{_section}"] = dict(
            area="datastore", client=_client, transport="gRPC")
        TRANSCRIPTS[f"sdk-datastore-{_sdk}-http-{_section}"] = dict(
            area="datastore", client=f"{_client} over HTTP", transport="REST")
for _section in ("entities", "queries", "transactions"):
    TRANSCRIPTS[f"sdk-datastore-go-{_section}"] = dict(
        area="datastore", client="Datastore Go client (cloud.google.com/go/datastore)",
        transport="gRPC")
    TRANSCRIPTS[f"sdk-datastore-node-{_section}"] = dict(
        area="datastore", client="Datastore Node.js client (@google-cloud/datastore)",
        transport="gRPC")

# JSON files in results/ that are neither a transcript nor a replayed recording.
IGNORED = {
    "results/issues-firebase-tools-metadata.json": "metadata of firebase-tools issues",
    "results/differential-fuzzing.json": "differential fuzzing runs (tools/fuzz/), reported by compatibility.py",
}

IDENTICAL, DOCUMENTED, UNEXPLAINED = "identical", "documented", "unexplained"


def load(rel):
    with open(ROOT / rel, encoding="utf-8") as f:
        return json.load(f)


def oracle_version(rel, fixture):
    """The official emulator version a recording states: its `oracle` header, or its name."""
    header = fixture.get("oracle") if isinstance(fixture, dict) else None
    match = re.search(r"\bv(\d+(?:\.\d+)+)\b", header or "") or re.search(
        r"-official-v(\d+(?:\.\d+)+)\.json$", rel)
    return f"v{match.group(1)}" if match else None


def check_tests(rel, tests):
    for test in tests:
        path, _, function = test.partition("::")
        if not (ROOT / path).is_file() or not re.search(
                rf"\bfn {re.escape(function.split('::')[-1])}\b", source(path)):
            raise Stop(f"{rel}: its test {test} was not found; update RECORDINGS.")


def recordings():
    found = {p.relative_to(ROOT).as_posix() for p in ROOT.glob("crates/*/tests/fixtures/*.json")}
    unknown = sorted(found - set(RECORDINGS))
    if unknown:
        raise Stop(f"{', '.join(unknown)}: not in RECORDINGS. Add each with its area, the tests "
                   "that replay it, what they compare and how to count its answers, then run "
                   "the script.")
    missing = sorted(rel for rel in RECORDINGS if not (ROOT / rel).is_file())
    if missing:
        raise Stop(f"{', '.join(missing)}: in RECORDINGS but not found; remove the entries.")
    rows = []
    for rel, entry in RECORDINGS.items():
        check_tests(rel, entry["tests"])
        fixture = load(rel)
        try:
            answers = Counter(entry["count"](fixture))
        except (KeyError, TypeError, AttributeError) as err:
            raise Stop(f"{rel}: its format changed ({err!r}); update its count in RECORDINGS.")
        if not answers:
            raise Stop(f"{rel}: no answers counted; check its count in RECORDINGS.")
        rows.append(dict(entry, path=rel, answers=answers, oracle=oracle_version(rel, fixture)))
    return sorted(rows, key=lambda r: (AREA_ORDER.index(r["area"]), r["path"]))


def normalized(rel, ignore=()):
    """A transcript as compared: keys sorted, without the top-level keys in `ignore`."""
    data = load(rel)
    if isinstance(data, dict):
        data = {key: value for key, value in data.items() if key not in ignore}
    return json.dumps(data, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def explained(name):
    """Where the documentation explains a pair's difference, checked to say so; or None."""
    if name not in SDK_DIFFERENCES:
        return None
    rel, phrase = SDK_DIFFERENCES[name]
    if phrase not in source(rel):
        raise Stop(f"SDK_DIFFERENCES[{name!r}]: {rel} no longer says {phrase!r}.")
    return rel


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
        ignore = entry.get("ignore", ())
        others = [(hidane, "hidane")] + [(f"results/{variant}", what)
                                         for variant, what in entry.get("variants", {}).items()]
        for rel, what in others:
            if rel not in files:
                raise Stop(f"TRANSCRIPTS[{name!r}]: {rel} not found.")
            accounted.add(rel)
            if ignore:
                what += ", without " + ", ".join(f"`{key}`" for key in ignore)
            if "outline" in entry:
                what += "; " + entry["outline"](load(official[0]))
            where = None
            if normalized(official[0], ignore) == normalized(rel, ignore):
                result = IDENTICAL
            else:
                where = explained(name)
                result = DOCUMENTED if where else UNEXPLAINED
            rows.append(dict(entry, name=name, official=official[0], other=rel, what=what,
                             result=result, where=where))
        accounted.add(official[0])
        if name in SDK_DIFFERENCES and all(row["result"] == IDENTICAL
                                           for row in rows if row["name"] == name):
            raise Stop(f"{name}: the transcripts are identical now; remove it from "
                       "SDK_DIFFERENCES.")
    unknown = sorted(files - accounted)
    if unknown:
        raise Stop(f"{', '.join(unknown)}: not in the mapping. Add a transcript pair to "
                   "TRANSCRIPTS, a replayed recording to RECORDINGS, or the file to IGNORED.")
    return sorted(rows, key=lambda r: (AREA_ORDER.index(r["area"]), r["name"], r["other"]))


def intentional_differences():
    """The rows of the table under "## Intentional differences" in docs/parity-exceptions.md."""
    match = re.search(r"^## Intentional differences.*?$(.*?)(?=^## |\Z)",
                      source("docs/parity-exceptions.md"), re.M | re.S)
    if not match:
        raise Stop("docs/parity-exceptions.md: no \"## Intentional differences\" section.")
    rows = [line for line in match.group(1).splitlines() if line.startswith("|")]
    if len(rows) < 3:
        raise Stop("docs/parity-exceptions.md: the intentional differences table is empty.")
    return len(rows) - 2  # the header and its separator


def n(number):
    return f"{number:,}"


def total(counter, level=None):
    return sum(count for (_, lv), count in counter.items() if level is None or lv == level)


def summary(answers):
    """`53 in full, 4 status only`: how some answers are compared, in LEVELS order."""
    return ", ".join(f"{n(total(answers, key))} {label}"
                     for key, label in LEVELS if total(answers, key))


def transports_of(answers, lang="en"):
    used = {transport for transport, _ in answers}
    return [TRANSPORT_JA.get(t, t) if lang == "ja" else t for t in TRANSPORTS if t in used]


def by_area(rows, pairs):
    areas = {}
    for key in AREA_ORDER:
        answers = Counter()
        for row in rows:
            if row["area"] == key:
                answers.update(row["answers"])
        results = Counter(pair["result"] for pair in pairs if pair["area"] == key)
        if answers or results:
            areas[key] = dict(answers=answers, results=results)
    return areas


def link(rel, base="../"):
    return f"[`{Path(rel).name}`]({base}{rel})"


def test_links(tests):
    """`[`rest.rs`](…) `replay`` for each file, with its functions."""
    by_file = {}
    for test in tests:
        path, _, function = test.partition("::")
        by_file.setdefault(path, []).append(f"`{function}`")
    return "; ".join(f"{link(path)} {', '.join(functions)}" for path, functions in by_file.items())


def sdk_summary(results, ja=False):
    labels = ({IDENTICAL: "一致 {}", DOCUMENTED: "既知の差 {}", UNEXPLAINED: "説明なし {}"} if ja
              else {IDENTICAL: "{} identical", DOCUMENTED: "{} documented difference{}",
                    UNEXPLAINED: "{} unexplained"})
    parts = []
    for key in (IDENTICAL, DOCUMENTED, UNEXPLAINED):
        if results[key]:
            plural = "" if results[key] == 1 else "s"
            parts.append(labels[key].format(results[key], plural))
    return parts


def markdown(rows, pairs, exceptions):
    everything = Counter()
    for row in rows:
        everything.update(row["answers"])
    versions = Counter(row["oracle"] for row in rows if row["oracle"])
    stated = sum(versions.values())
    results = Counter(pair["result"] for pair in pairs)
    out = [START, "<!-- Generated by tools/parity/table.py; edit the script, not this. -->", ""]
    out.append(
        f"**{n(total(everything))} recorded answers** in {len(rows)} recordings, replayed against "
        f"hidane by `cargo test`: {summary(everything)}. Oracle: "
        + ", ".join(f"{v} ({count} recordings)" for v, count in sorted(versions.items()))
        + f"; {len(rows) - stated} recordings do not state a version. SDK transcripts: "
        f"{len(pairs)} comparisons, {', '.join(sdk_summary(results))}. Intentional differences: "
        f"{exceptions} ([parity-exceptions.md](parity-exceptions.md)).")
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
            "| Recording | Area | Transports | Answers | Compared | Oracle |",
            "|---|---|---|--:|---|---|"]
    for row in rows:
        out.append(
            f"| {link(row['path'])} | {AREA_NAME['en'][row['area']]} | "
            f"{', '.join(transports_of(row['answers']))} | {n(total(row['answers']))} | "
            f"{summary(row['answers'])} | {row['oracle'] or 'not stated'} |")
    out += ["", "### What each test counts and compares", ""]
    for row in rows:
        out.append(f"- {link(row['path'])}, replayed by {test_links(row['tests'])}. Counted: "
                   f"{row['counted']}. Compared: {row['compares']}.")
    out += ["", "### SDK transcripts", "",
            "| Transcript | Area | Client | Transport | Compared with the official one | Result |",
            "|---|---|---|---|---|---|"]
    for pair in pairs:
        if pair["result"] == DOCUMENTED:
            result = f"documented difference ({link(pair['where'])})"
        else:
            result = pair["result"]
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
    results = Counter(pair["result"] for pair in pairs)
    docs = f"<a href=\"{REPO}/blob/main/docs/parity.md\">docs/parity.md</a>"
    readme = f"<a href=\"{REPO}/blob/main/results/README.md\">results/README.md</a>"
    exceptions_link = (f"<a href=\"{REPO}/blob/main/docs/parity-exceptions.md\">"
                       "parity exceptions</a>")

    def nowrap(parts, sep):
        return sep.join(f"<span class=\"nw\">{part}</span>" for part in parts)

    def transports(answers):
        return nowrap(transports_of(answers, lang), ", ") or "—"

    def sdk(counts):
        # One line per kind of result (style.css).
        parts = "".join(f"<span class=\"sdk-part\">{part}</span>"
                        for part in sdk_summary(counts, ja))
        return f"<td class=\"sdk\">{parts or '—'}</td>"

    if ja:
        intro = (f"<p class=\"fine\">現時点で、公式エミュレータの応答 {n(total(everything))} 件を "
                 f"{len(rows)} の記録に収め、CI がそのすべてを hidane に対して再生しています。"
                 "SDK の記録も両方のエミュレータで突き合わせています"
                 f"（<a href=\"{REPO}/tree/main/results\">results</a>）。数え方は {docs}"
                 "（英語）にあります。</p>")
        head = ["領域", "トランスポート", "記録した<wbr>応答", "全体を比較", "SDK の記録"]
        foot = "合計"
        notes = ("<p class=\"fine\">全体を比較: 記録が保持する応答の全体（ステータス、メッセージや"
                 "本文、値）を、実行ごとに変わる値（時刻や ID など）だけを除いて比べている応答の数"
                 "です。そのほかは Security Rules の判定と理由、応答の一部、またはステータスで"
                 f"比べています。内訳と比べ方はテストごとに {docs} にあります。"
                 "SDK の記録: 同じ手順を SDK から両方のエミュレータに対して実行した記録の組です。"
                 f"既知の差は、{readme} または parity exceptions に理由が書かれている差です。"
                 f"意図的に公式と違える点（{exceptions} 件）は {exceptions_link}（英語）に"
                 "一覧があります。</p>")
    else:
        intro = (f"<p class=\"fine\">So far: {n(total(everything))} answers of the official "
                 f"emulator in {len(rows)} recordings, each replayed against hidane in CI, and SDK "
                 "transcripts from both emulators compared side by side "
                 f"(<a href=\"{REPO}/tree/main/results\">results</a>). How they are counted: "
                 f"{docs}.</p>")
        head = ["Area", "Transports", "Recorded answers", "Compared in full", "SDK transcripts"]
        foot = "Total"
        notes = ("<p class=\"fine\">Compared in full: answers whose test compares everything the "
                 "recording keeps (status, message or body, values), masking only what changes "
                 "from run to run, such as times and IDs. The others are compared on the "
                 "Security Rules decision and its reasons, in part, or on their status; "
                 f"{docs} breaks them down, test by test. SDK transcripts: the same steps run "
                 "through an SDK against both emulators; a documented difference is one "
                 f"{readme} or the parity exceptions explain. Known, intentional differences "
                 f"({exceptions}) are listed in {exceptions_link}.</p>")
    num = ' class="num"'
    # On a narrow screen the transports go under the area's name (style.css).
    classes = {1: ' class="parity-tr"', 2: num, 3: num}
    out = [START, intro, "<div class=\"parity-scroll\">", "<table class=\"parity-table\">",
           "<thead>",
           "<tr>" + "".join(f"<th scope=\"col\"{classes.get(i, '')}>{h}</th>"
                            for i, h in enumerate(head)) + "</tr>",
           "</thead>", "<tbody>"]
    for key, area in by_area(rows, pairs).items():
        name = AREA_NAME[lang][key].replace("&", "&amp;")
        used = transports(area["answers"])
        out.append(f"<tr><th scope=\"row\">{name}<span class=\"parity-sub\">{used}</span></th>"
                   f"<td class=\"parity-tr\">{used}</td>"
                   f"<td{num}>{n(total(area['answers']))}</td>"
                   f"<td{num}>{n(total(area['answers'], FULL))}</td>{sdk(area['results'])}</tr>")
    out += ["</tbody>", "<tfoot>",
            f"<tr><th scope=\"row\">{foot}</th><td class=\"parity-tr\"></td>"
            f"<td{num}>{n(total(everything))}</td><td{num}>{n(total(everything, FULL))}</td>"
            f"{sdk(results)}</tr>",
            "</tfoot>", "</table>", "</div>", notes, END]
    return "\n".join(out)


def badge(rows):
    answers = sum(total(row["answers"]) for row in rows)
    return json.dumps({"schemaVersion": 1, "label": "parity",
                       "message": f"{n(answers)} recorded answers", "color": BADGE_COLOR},
                      indent=2) + "\n"


def between_markers(rel, block):
    text = source(rel)
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
