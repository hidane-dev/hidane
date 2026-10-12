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
        outcome = result["outcome"]
        if result["name"] in known:
            level = DIFFERS
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


def queries(f):
    # queries.rs leaves out the `findNearest` cases (find_nearest.json covers them).
    return [(GRPC, FULL) for case in f["cases"] if "findNearest" not in case["query"]]


def rest(cases):
    """Cases in the format of `tools/oracle/rest.py`'s fixture."""
    return [(REST, compare_mark(case)) for case in cases]


def rules_server(name):
    """rules_server.rs's replay: every status, and a denial's reasons."""
    def count(f):
        skip = rust_strings(f"{H}/rules_server.rs", f"fixtures/{name}")
        return [(REST, REASONS if denial(step) else STATUS)
                for case in f if case["name"] not in skip for step in case["steps"]]
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
            # A read back: its fields. A commit: its transform results, or its error.
            answers.append((GRPC, PART if step["method"] == "GET" else FULL))
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
                 "with an evaluation error, as intentionally different",
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
    f"{H}/fixtures/auth.json": dict(
        area="admin", tests=[f"{H}/auth.rs::authorization_is_read_like_the_official_emulator"],
        counted="each case",
        compares="in full: the outcome as the recording summarises it (status or code and "
                 "message)",
        count=lambda f: (each(f["classify"], REST, FULL) + each(f["grpc"], GRPC, FULL)
                         + each(f["rest"], REST, FULL))),
    f"{H}/fixtures/cors.json": dict(
        area="rest", tests=[f"{H}/cors.rs::cors_answers_like_the_official_emulator"],
        counted="each request",
        compares="in full: status, every header but `date`, `content-length` and `connection`, "
                 "and the body",
        count=lambda f: each(f["cases"], REST, FULL)),
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
        compares="in full: the names of every page, or the code and message",
        count=lambda f: each(f["grpc"], GRPC, FULL) + each(f["rest"], REST, FULL)),
    f"{H}/fixtures/negative_zero.json": dict(
        area="values",
        tests=[f"{H}/negative_zero.rs::every_recorded_step_gives_the_official_answer"],
        counted="each step",
        compares="in full: the answer as the recording summarises it, each double spelled out; "
                 "the steps that write `-1e-400` as intentionally different",
        count=negative_zero),
    f"{H}/fixtures/queries.json": dict(
        area="queries", tests=[f"{H}/queries.rs::queries_answer_like_the_official_emulator"],
        counted="each query but the `findNearest` one; " + OVER_REST,
        compares="in full: each response's document, `skippedResults` and `done`, the fields of "
                 "projected documents, or the status and message",
        count=queries),
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
    f"{H}/fixtures/transactions.json": dict(
        area="transactions", tests=[f"{H}/transactions.rs::replay"],
        counted="each step with an outcome, except those of the scenario marked `slow`",
        compares="as each step's `compare` marks it; without a mark in full (status, message, "
                 "elapsed time to 0.5 s and the result, not transaction IDs)",
        count=transactions),
    f"{H}/fixtures/transforms.json": dict(
        area="documents",
        tests=[f"{H}/transforms.rs::every_recorded_case_gives_the_official_answer"],
        counted="each call; " + OVER_REST,
        compares="a commit in full (its transform results, server times as the request time, "
                 "or the code and message); a read in part (the document's fields)",
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
    f"{H}/fixtures/write_stream.json": dict(
        area="streams", tests=[f"{H}/write_stream.rs::replay"],
        counted="each step with an outcome",
        compares="as each step's `compare` marks it; without a mark in full (the response's "
                 "token, write results and commit time as the recording summarises them, the "
                 "error, the elapsed time)",
        count=write_stream),
}
for _name in ("rules_auth", "rules_denials", "rules_list", "rules_lookups", "rules_request",
              "rules_writes"):
    RECORDINGS[f"{H}/fixtures/{_name}.json"] = dict(
        area="rules", tests=[f"{H}/rules_server.rs::replay"], counted="each call",
        compares="a denial as decision and reasons (`same_denial`: hidane's reasons are among "
                 "the official ones); every other answer on its status",
        count=rules_server(f"{_name}.json"))
for _name in ("rules_coverage", "rules_coverage_flag", "rules_request_fields", "rules_verify"):
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
    "sdk-clear": ("results/README.md", "reaches the web SDK in one snapshot on hidane and two"),
    "sdk-documents": ("docs/parity-exceptions.md",
                      "Precondition failures print the emulator's internal Datastore key"),
    "sdk-listen": ("results/README.md", "a batch reaches the web SDK's limit query as two"),
    "sdk-listen-resume": ("results/README.md", "the official emulator gets there in three"),
    "sdk-rules-unit-testing": ("results/README.md", "13 denial messages differ"),
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


def normalized(rel):
    return json.dumps(load(rel), sort_keys=True, separators=(",", ":"), ensure_ascii=False)


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
        others = [(hidane, "hidane")] + [(f"results/{variant}", what)
                                         for variant, what in entry.get("variants", {}).items()]
        for rel, what in others:
            if rel not in files:
                raise Stop(f"TRANSCRIPTS[{name!r}]: {rel} not found.")
            accounted.add(rel)
            where = None
            if normalized(official[0]) == normalized(rel):
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
