# Parity

hidane treats the official Cloud Firestore emulator as the oracle. The scripts in
[`tools/oracle/`](../tools/oracle/) ask the official emulator how it answers and record its
answers in fixtures (`crates/*/tests/fixtures/*.json`, and one recording in `results/`);
`cargo test` sends hidane the same requests and compares its answers with the recorded ones. CI
runs the tests on every pull request and on `main`, and an answer that differs fails the build:
passing CI is what makes the recorded answers below matching ones.

The tables are generated from the fixtures and `results/` by
[`tools/parity/table.py`](../tools/parity/table.py), which also writes the Parity section of
hidane.dev and the badge's data (`website/public/badge/parity.json`). CI runs it with `--check`
and fails when they are stale: after recording or adding a fixture, run
`python3 tools/parity/table.py` and commit the result.

How to read them:

- **Recorded answers**: answers of the official emulator that a test compares with hidane's,
  counted as the list at the end says for each recording: the answer to a call, a stream's
  response, a Security Rules decision, a compiled ruleset, a value's place in an order. Answers a
  fixture holds but no test replays are not counted.
- **Compared**: how the test that replays a recording compares each answer. The script encodes
  what each test asserts, and the list at the end names the test functions and says what they
  compare; where a recording marks an answer itself (`compare`, `differs`), the mark decides, and
  the lists a test keeps (`KNOWN`, `FIRST_LINE_ONLY`, `CRASHES`, the cases it skips) are read
  from its source.
  - *in full*: everything the recording keeps of the answer (status, message or body, values),
    masking only what changes from run to run: project IDs, times, transaction IDs, page and
    resume tokens, generated document IDs. Some recordings keep a summary rather than the whole
    message (document names, whether a field is present, counts); "in full" is then the whole
    summary.
  - *decision and reasons*: a Security Rules decision and, for a denial, its reasons as
    `same_denial` (`crates/hidane/tests/common/rules.rs`) accepts them: each of hidane's reasons
    must be one of the official emulator's. The official emulator evaluates a condition in two
    passes and lists the reasons of both; hidane evaluates once
    ([parity-exceptions.md](parity-exceptions.md)), so its denial lists a subset. The rest of a
    denial's body is not compared.
  - *in part*: some of the answer, as the list says: the values without the rest of the
    message, the order of fields without their values, a decision without its message, a
    request monitor record whose `resource` the official emulator left undefined.
  - *status only*: the status (or the gRPC code); a successful answer's body is not compared.
    Most are Security Rules calls whose decision is the status, and the calls that set up a
    case (loading rules, writing documents as the owner); the rest are errors compared on their
    code, such as a failed precondition, whose official message prints an internal key.
  - *without messages* and *not compared*: steps of `transactions.json` marked `codes` and
    `none`, where hidane intentionally answers differently.
  - *intentionally different*: answers where hidane does not do what the official emulator does,
    marked `differs` in the recording or listed by the test; the test compares them in part (or
    checks hidane's own answer), and [parity-exceptions.md](parity-exceptions.md) says why.
  - *not stated*: for a recording whose test nobody has read yet; none at present.
- **Transport**: how the replay reaches hidane: gRPC, REST, WebChannel, WebSocket (the Emulator
  UI's request monitor), CLI (starting the process with flags), or in-process (the test calls
  hidane's crates directly, as for value ordering and the Security Rules engine). Where a
  recording was made over another transport, the list says so.
- **Oracle**: the version of the official emulator a recording names (its `oracle` header, or
  its file name in `results/`); "not stated" where it names none. The
  [Oracle Corpus manifest](oracle-corpus.md) gives the version of those too, from git history.
- **SDK transcripts**: `tools/oracle/sdk_*.mjs` and `tools/oracle/webchannel/*.js` run the same
  steps through an SDK against both emulators and print a transcript, kept in `results/` as
  `<name>-official-v<version>.json` and `<name>-hidane.json` (for export and import, also each
  emulator reading the other's export; for firebase-js-sdk's browser suite, run by
  `tools/js-sdk-browser/`, the outcome of each test). A pair is *identical* when the two files
  hold the same JSON (keys sorted), leaving out a field that only names the emulator. Otherwise it is a *documented difference* when
  [`results/README.md`](../results/README.md) or [parity-exceptions.md](parity-exceptions.md)
  explains it (the script checks that the cited file still says so), and *unexplained* if not.
  Transcripts are recorded by hand, not in CI; the script compares the committed files.
- **Intentional differences**: the rows of the first table of
  [parity-exceptions.md](parity-exceptions.md).

<!-- parity:start -->
<!-- Generated by tools/parity/table.py; edit the script, not this. -->

**20,543 recorded answers** in 41 recordings, replayed against hidane by `cargo test`: 12,325 in full, 3,912 decision and reasons, 751 in part, 3,404 status only, 30 without messages, 1 not compared, 120 intentionally different. Oracle: v1.22.0 (23 recordings); 18 recordings do not state a version. SDK transcripts: 33 comparisons, 19 identical, 14 documented differences. Intentional differences: 68 ([parity-exceptions.md](parity-exceptions.md)).

### By area and transport

| Area | Transport | Recorded answers | Compared |
|---|---|--:|---|
| Documents & writes | gRPC | 2,630 | 2,422 in full, 110 in part, 70 status only, 22 without messages, 6 intentionally different |
|  | REST | 2,616 | 2,452 in full, 79 in part, 65 status only, 7 without messages, 13 intentionally different |
| Queries & aggregations | gRPC | 474 | 419 in full, 55 in part |
|  | REST | 94 | 94 in full |
| Transactions | gRPC | 206 | 201 in full, 3 status only, 1 without messages, 1 not compared |
|  | REST | 1 | 1 in full |
| Listen & Write streams | gRPC | 49 | 48 in full, 1 status only |
| REST | REST | 206 | 202 in full, 4 status only |
| WebChannel | REST | 2 | 2 in full |
|  | WebChannel | 32 | 29 in full, 3 in part |
| Security Rules | gRPC | 23 | 9 in full, 7 decision and reasons, 7 in part |
|  | REST | 6,487 | 329 in full, 2,748 decision and reasons, 133 in part, 3,261 status only, 16 intentionally different |
|  | WebSocket | 149 | 45 in full, 90 in part, 14 intentionally different |
|  | in-process | 6,605 | 5,158 in full, 1,157 decision and reasons, 271 in part, 19 intentionally different |
| Export & import | REST | 64 | 61 in full, 3 in part |
|  | CLI | 6 | 6 in full |
| Auth & admin endpoints | gRPC | 73 | 70 in full, 3 intentionally different |
|  | REST | 548 | 503 in full, 45 intentionally different |
| Values & limits | gRPC | 87 | 85 in full, 2 intentionally different |
|  | REST | 70 | 68 in full, 2 intentionally different |
|  | WebChannel | 18 | 18 in full |
|  | in-process | 103 | 103 in full |
| **Total** | | **20,543** | 12,325 in full, 3,912 decision and reasons, 751 in part, 3,404 status only, 30 without messages, 1 not compared, 120 intentionally different |

### By recording

| Recording | Area | Transports | Answers | Compared | Oracle |
|---|---|---|--:|---|---|
| [`commit_checks.json`](../crates/hidane/tests/fixtures/commit_checks.json) | Documents & writes | gRPC, REST | 4,664 | 4,616 in full, 29 without messages, 19 intentionally different | v1.22.0 |
| [`document_writes.json`](../crates/hidane/tests/fixtures/document_writes.json) | Documents & writes | gRPC | 16 | 16 in full | v1.22.0 |
| [`field_order.json`](../crates/hidane/tests/fixtures/field_order.json) | Documents & writes | REST | 145 | 1 in full, 79 in part, 65 status only | not stated |
| [`list_documents.json`](../crates/hidane/tests/fixtures/list_documents.json) | Documents & writes | gRPC, REST | 35 | 35 in full | v1.22.0 |
| [`same_document_writes.json`](../crates/hidane/tests/fixtures/same_document_writes.json) | Documents & writes | gRPC | 324 | 160 in full, 96 in part, 68 status only | v1.22.0 |
| [`transforms.json`](../crates/hidane/tests/fixtures/transforms.json) | Documents & writes | gRPC | 62 | 46 in full, 14 in part, 2 status only | v1.22.0 |
| [`aggregations.json`](../crates/hidane/tests/fixtures/aggregations.json) | Queries & aggregations | gRPC | 89 | 34 in full, 55 in part | v1.22.0 |
| [`find_nearest.json`](../crates/hidane/tests/fixtures/find_nearest.json) | Queries & aggregations | REST | 90 | 90 in full | v1.22.0 |
| [`queries.json`](../crates/hidane/tests/fixtures/queries.json) | Queries & aggregations | gRPC | 375 | 375 in full | v1.22.0 |
| [`unsupported.json`](../crates/hidane/tests/fixtures/unsupported.json) | Queries & aggregations | gRPC, REST | 14 | 14 in full | v1.22.0 |
| [`transactions.json`](../crates/hidane/tests/fixtures/transactions.json) | Transactions | gRPC, REST | 207 | 202 in full, 3 status only, 1 without messages, 1 not compared | v1.22.0 |
| [`write_stream.json`](../crates/hidane/tests/fixtures/write_stream.json) | Listen & Write streams | gRPC | 49 | 48 in full, 1 status only | v1.22.0 |
| [`cors.json`](../crates/hidane/tests/fixtures/cors.json) | REST | REST | 22 | 22 in full | v1.22.0 |
| [`rest.json`](../crates/hidane/tests/fixtures/rest.json) | REST | REST | 184 | 180 in full, 4 status only | v1.22.0 |
| [`webchannel.json`](../crates/hidane/tests/fixtures/webchannel.json) | WebChannel | REST, WebChannel | 34 | 31 in full, 3 in part | v1.22.0 |
| [`rules_eval.json`](../crates/hidane-rules/tests/fixtures/rules_eval.json) | Security Rules | in-process | 1,278 | 384 in full, 889 decision and reasons, 2 in part, 3 intentionally different | not stated |
| [`rules_paths.json`](../crates/hidane-rules/tests/fixtures/rules_paths.json) | Security Rules | in-process | 182 | 180 in full, 2 in part | not stated |
| [`rules_warnings.json`](../crates/hidane-rules/tests/fixtures/rules_warnings.json) | Security Rules | in-process | 3,694 | 3,678 in full, 16 intentionally different | not stated |
| [`rules_auth.json`](../crates/hidane/tests/fixtures/rules_auth.json) | Security Rules | REST | 247 | 88 decision and reasons, 159 status only | not stated |
| [`rules_compile.json`](../crates/hidane/tests/fixtures/rules_compile.json) | Security Rules | REST | 250 | 248 in full, 2 intentionally different | not stated |
| [`rules_coverage.json`](../crates/hidane/tests/fixtures/rules_coverage.json) | Security Rules | REST | 250 | 35 in full, 4 decision and reasons, 75 in part, 126 status only, 10 intentionally different | not stated |
| [`rules_coverage_flag.json`](../crates/hidane/tests/fixtures/rules_coverage_flag.json) | Security Rules | REST | 17 | 9 in full, 2 decision and reasons, 4 in part, 2 status only | not stated |
| [`rules_denials.json`](../crates/hidane/tests/fixtures/rules_denials.json) | Security Rules | REST | 65 | 21 decision and reasons, 44 status only | not stated |
| [`rules_list.json`](../crates/hidane/tests/fixtures/rules_list.json) | Security Rules | REST | 3,451 | 2,395 decision and reasons, 1,056 status only | not stated |
| [`rules_lookups.json`](../crates/hidane/tests/fixtures/rules_lookups.json) | Security Rules | REST | 304 | 15 decision and reasons, 289 status only | not stated |
| [`rules_request.json`](../crates/hidane/tests/fixtures/rules_request.json) | Security Rules | REST | 1,018 | 118 decision and reasons, 900 status only | not stated |
| [`rules_request_fields.json`](../crates/hidane/tests/fixtures/rules_request_fields.json) | Security Rules | REST | 182 | 37 in full, 50 in part, 95 status only | not stated |
| [`rules_streams.json`](../crates/hidane/tests/fixtures/rules_streams.json) | Security Rules | gRPC | 23 | 9 in full, 7 decision and reasons, 7 in part | not stated |
| [`rules_verify.json`](../crates/hidane/tests/fixtures/rules_verify.json) | Security Rules | REST | 337 | 39 decision and reasons, 4 in part, 290 status only, 4 intentionally different | not stated |
| [`rules_writes.json`](../crates/hidane/tests/fixtures/rules_writes.json) | Security Rules | REST | 366 | 66 decision and reasons, 300 status only | not stated |
| [`websocket_requests.json`](../crates/hidane/tests/fixtures/websocket_requests.json) | Security Rules | WebSocket | 149 | 45 in full, 90 in part, 14 intentionally different | not stated |
| [`rules-conditions-official-v1.22.0.json`](../results/rules-conditions-official-v1.22.0.json) | Security Rules | in-process | 1,451 | 916 in full, 268 decision and reasons, 267 in part | v1.22.0 |
| [`export_import.json`](../crates/hidane/tests/fixtures/export_import.json) | Export & import | REST, CLI | 70 | 67 in full, 3 in part | not stated |
| [`auth.json`](../crates/hidane/tests/fixtures/auth.json) | Auth & admin endpoints | gRPC, REST | 146 | 146 in full | v1.22.0 |
| [`function_events.json`](../crates/hidane/tests/fixtures/function_events.json) | Auth & admin endpoints | gRPC, REST | 133 | 85 in full, 48 intentionally different | v1.22.0 |
| [`function_triggers.json`](../crates/hidane/tests/fixtures/function_triggers.json) | Auth & admin endpoints | REST | 342 | 342 in full | v1.22.0 |
| [`value_order.json`](../crates/hidane-core/tests/fixtures/value_order.json) | Values & limits | in-process | 103 | 103 in full | v1.22.0 |
| [`grpc_settings.json`](../crates/hidane/tests/fixtures/grpc_settings.json) | Values & limits | gRPC, REST | 11 | 11 in full | v1.22.0 |
| [`json_escapes.json`](../crates/hidane/tests/fixtures/json_escapes.json) | Values & limits | REST, WebChannel | 30 | 30 in full | v1.22.0 |
| [`limits.json`](../crates/hidane/tests/fixtures/limits.json) | Values & limits | REST | 37 | 37 in full | v1.22.0 |
| [`negative_zero.json`](../crates/hidane/tests/fixtures/negative_zero.json) | Values & limits | gRPC, REST, WebChannel | 97 | 93 in full, 4 intentionally different | v1.22.0 |

### What each test counts and compares

- [`commit_checks.json`](../crates/hidane/tests/fixtures/commit_checks.json), replayed by [`commit_checks.rs`](../crates/hidane/tests/commit_checks.rs) `the_first_error_is_the_official_one`, `writes_to_another_database_are_refused`. Counted: each commit over each transport it was recorded over, and each step that writes to or reads after another database. Compared: in full: the first error's code and message, the number of write results or `:batchWrite`'s statuses, a read's status, names found or missing; a failed precondition without its message (it prints the official emulator's internal key); the cases the test lists in `KNOWN` (array unions of arrays and of vectors with NaN) and the steps in `DIFFERS` (writes to another database, which hidane refuses, and the reads that follow) as intentionally different.
- [`document_writes.json`](../crates/hidane/tests/fixtures/document_writes.json), replayed by [`documents.rs`](../crates/hidane/tests/documents.rs) `reads_and_their_errors`, `preconditions`, `invalid_writes_use_the_official_messages`, `listing_documents_and_collections`, `batch_write_reports_each_write`. Counted: each recorded message the tests check hidane's against (they replay no case as a whole); recorded over REST. Compared: in full: the code and the message (a stored version's timestamps masked).
- [`field_order.json`](../crates/hidane/tests/fixtures/field_order.json), replayed by [`field_order.rs`](../crates/hidane/tests/field_order.rs) `fields_come_back_in_the_official_order`. Counted: each call. Compared: an error in full (status and message); a commit on its status; a read in part: the order of every field, nested ones included, and which commit its `updateTime` names, not the values.
- [`list_documents.json`](../crates/hidane/tests/fixtures/list_documents.json), replayed by [`list_documents.rs`](../crates/hidane/tests/list_documents.rs) `list_documents_like_the_official_emulator`. Counted: each case (a paged case is one). Compared: in full: the names of every page, or the code and message.
- [`same_document_writes.json`](../crates/hidane/tests/fixtures/same_document_writes.json), replayed by [`same_document_writes.rs`](../crates/hidane/tests/same_document_writes.rs) `every_recorded_case_gives_the_official_answer`. Counted: each call; recorded over REST. Compared: an error in full (code and message), but a failed precondition on its code (its message prints the official emulator's internal key, an intentional difference); a commit in full (its number of write results and the commit each one's update time comes from, or that it has none; the times themselves differ from run to run), in part where they hold transform results, which are not compared; a read in part (the fields and their order), a missing document on its code.
- [`transforms.json`](../crates/hidane/tests/fixtures/transforms.json), replayed by [`transforms.rs`](../crates/hidane/tests/transforms.rs) `every_recorded_case_gives_the_official_answer`. Counted: each call; recorded over REST. Compared: a commit in full (its transform results, server times as the request time, or the code and message); a read in part (the document's fields), a missing document on its code.
- [`aggregations.json`](../crates/hidane/tests/fixtures/aggregations.json), replayed by [`aggregations.rs`](../crates/hidane/tests/aggregations.rs) `aggregations_answer_like_the_official_emulator`. Counted: each query; recorded over REST. Compared: an error in full (status and message); results in part: every aggregate value with its type, not `done`, which gRPC does not carry.
- [`find_nearest.json`](../crates/hidane/tests/fixtures/find_nearest.json), replayed by [`find_nearest.rs`](../crates/hidane/tests/find_nearest.rs) `find_nearest_like_the_official_emulator`. Counted: each query, aggregation and write. Compared: in full: the documents' names and fields but the vector (as the recording keeps them), the aggregate values, or the status and message.
- [`queries.json`](../crates/hidane/tests/fixtures/queries.json), replayed by [`queries.rs`](../crates/hidane/tests/queries.rs) `queries_answer_like_the_official_emulator`. Counted: each query but the `findNearest` one; recorded over REST. Compared: in full: each response's document, `skippedResults` and `done`, the fields of projected documents, or the status and message.
- [`unsupported.json`](../crates/hidane/tests/fixtures/unsupported.json), replayed by [`unsupported.rs`](../crates/hidane/tests/unsupported.rs) `ignored_and_refused_like_the_official_emulator`. Counted: each call. Compared: in full: the outcome as the recording summarises it.
- [`transactions.json`](../crates/hidane/tests/fixtures/transactions.json), replayed by [`transactions.rs`](../crates/hidane/tests/transactions.rs) `replay`. Counted: each step with an outcome, except those of the scenario marked `slow`. Compared: as each step's `compare` marks it; without a mark in full (status, message, elapsed time to 0.5 s and the result, not transaction IDs).
- [`write_stream.json`](../crates/hidane/tests/fixtures/write_stream.json), replayed by [`write_stream.rs`](../crates/hidane/tests/write_stream.rs) `replay`. Counted: each step with an outcome. Compared: as each step's `compare` marks it; without a mark in full (the response's token, write results and commit time as the recording summarises them, the error, the elapsed time).
- [`cors.json`](../crates/hidane/tests/fixtures/cors.json), replayed by [`cors.rs`](../crates/hidane/tests/cors.rs) `cors_answers_like_the_official_emulator`. Counted: each request. Compared: in full: status, every header but `date`, `content-length` and `connection`, and the body.
- [`rest.json`](../crates/hidane/tests/fixtures/rest.json), replayed by [`rest.rs`](../crates/hidane/tests/rest.rs) `rest_answers_like_the_official_emulator`. Counted: each request. Compared: as each case's `compare` marks it: `exact` in full (status, content type and body byte for byte, server times, IDs and tokens masked), `status` on its status and content type.
- [`webchannel.json`](../crates/hidane/tests/fixtures/webchannel.json), replayed by [`webchannel.rs`](../crates/hidane/tests/webchannel.rs) `webchannel_like_the_official_emulator`. Counted: each step. Compared: in full: status, frames and messages (session IDs, times and tokens masked); a long-polling read in part (its first data and whether it ended: where the `noop`s fall depends on timing).
- [`rules_eval.json`](../crates/hidane-rules/tests/fixtures/rules_eval.json), replayed by [`library.rs`](../crates/hidane-rules/tests/library.rs) `evaluation_matches_the_official_emulator`. Counted: each condition or ruleset decided; recorded over REST. Compared: an allow and a compile error in full; a denial as decision and reasons (the entries of one evaluation where the official emulator lists two); a denied batch on its decision only (in part); the official 500s, which hidane answers with an evaluation error, as intentionally different.
- [`rules_paths.json`](../crates/hidane-rules/tests/fixtures/rules_paths.json), replayed by [`paths.rs`](../crates/hidane-rules/tests/paths.rs) `matches_cover_the_documents_the_official_emulator_allows`. Counted: each read under `allow read: if true`, and each `match` path refused; recorded over REST. Compared: each read's decision in full; a refused path on the refusal only (in part; `rules_warnings.json` holds the message).
- [`rules_warnings.json`](../crates/hidane-rules/tests/fixtures/rules_warnings.json), replayed by [`warnings.rs`](../crates/hidane-rules/tests/warnings.rs) `warnings_are_the_official_emulators`. Counted: each ruleset compiled, its `issues[]` or error; recorded over REST. Compared: in full: every issue with its position and severity, or the error message; the cases the test lists in `FIRST_LINE_ONLY` (the first line) and `KNOWN` as intentionally different.
- [`rules_auth.json`](../crates/hidane/tests/fixtures/rules_auth.json), replayed by [`rules_server.rs`](../crates/hidane/tests/rules_server.rs) `replay`. Counted: each call. Compared: a denial as decision and reasons (`same_denial`: hidane's reasons are among the official ones); every other answer on its status.
- [`rules_compile.json`](../crates/hidane/tests/fixtures/rules_compile.json), replayed by [`rules_compile.rs`](../crates/hidane/tests/rules_compile.rs) `compiles_like_the_official_emulator`. Counted: each ruleset loaded. Compared: in full: status and body byte for byte; the cases the test lists in `CRASHES` and `FIRST_LINE_ONLY` as intentionally different.
- [`rules_coverage.json`](../crates/hidane/tests/fixtures/rules_coverage.json), replayed by [`rules_coverage.rs`](../crates/hidane/tests/rules_coverage.rs) `replay`. Counted: each call. Compared: every status and what `debug()` printed (without `request.time`); the rules and coverage reports in full (byte for byte, without `request.time`); a denial as decision and reasons (`same_denial`); a call that printed in part, the others on their status; the coverage page and the cases marked `differs` as intentionally different.
- [`rules_coverage_flag.json`](../crates/hidane/tests/fixtures/rules_coverage_flag.json), replayed by [`rules_coverage.rs`](../crates/hidane/tests/rules_coverage.rs) `replay`. Counted: each call. Compared: every status and what `debug()` printed (without `request.time`); the rules and coverage reports in full (byte for byte, without `request.time`); a denial as decision and reasons (`same_denial`); a call that printed in part, the others on their status; the coverage page and the cases marked `differs` as intentionally different.
- [`rules_denials.json`](../crates/hidane/tests/fixtures/rules_denials.json), replayed by [`rules_server.rs`](../crates/hidane/tests/rules_server.rs) `replay`. Counted: each call. Compared: a denial as decision and reasons (`same_denial`: hidane's reasons are among the official ones); every other answer on its status.
- [`rules_list.json`](../crates/hidane/tests/fixtures/rules_list.json), replayed by [`rules_server.rs`](../crates/hidane/tests/rules_server.rs) `replay`. Counted: each call. Compared: a denial as decision and reasons (`same_denial`: hidane's reasons are among the official ones); every other answer on its status.
- [`rules_lookups.json`](../crates/hidane/tests/fixtures/rules_lookups.json), replayed by [`rules_server.rs`](../crates/hidane/tests/rules_server.rs) `replay`. Counted: each call. Compared: a denial as decision and reasons (`same_denial`: hidane's reasons are among the official ones); every other answer on its status.
- [`rules_request.json`](../crates/hidane/tests/fixtures/rules_request.json), replayed by [`rules_server.rs`](../crates/hidane/tests/rules_server.rs) `replay`. Counted: each call. Compared: a denial as decision and reasons (`same_denial`: hidane's reasons are among the official ones); every other answer on its status.
- [`rules_request_fields.json`](../crates/hidane/tests/fixtures/rules_request_fields.json), replayed by [`rules_coverage.rs`](../crates/hidane/tests/rules_coverage.rs) `replay`. Counted: each call. Compared: every status and what `debug()` printed (without `request.time`); the rules and coverage reports in full (byte for byte, without `request.time`); a denial as decision and reasons (`same_denial`); a call that printed in part, the others on their status; the coverage page and the cases marked `differs` as intentionally different.
- [`rules_streams.json`](../crates/hidane/tests/fixtures/rules_streams.json), replayed by [`rules_streams.rs`](../crates/hidane/tests/rules_streams.rs) `streams_under_rules`. Counted: each stream response, and each error that ended a stream. Compared: a target change in full (type, targets, resume tokens and times masked); a denied target's cause and an error that ended the stream as decision and reasons; a document change in part (its name and targets, not its fields) and a write response in part (its number of results).
- [`rules_verify.json`](../crates/hidane/tests/fixtures/rules_verify.json), replayed by [`rules_coverage.rs`](../crates/hidane/tests/rules_coverage.rs) `replay`. Counted: each call. Compared: every status and what `debug()` printed (without `request.time`); the rules and coverage reports in full (byte for byte, without `request.time`); a denial as decision and reasons (`same_denial`); a call that printed in part, the others on their status; the coverage page and the cases marked `differs` as intentionally different.
- [`rules_writes.json`](../crates/hidane/tests/fixtures/rules_writes.json), replayed by [`rules_server.rs`](../crates/hidane/tests/rules_server.rs) `replay`. Counted: each call. Compared: a denial as decision and reasons (`same_denial`: hidane's reasons are among the official ones); every other answer on its status.
- [`websocket_requests.json`](../crates/hidane/tests/fixtures/websocket_requests.json), replayed by [`websocket_requests.rs`](../crates/hidane/tests/websocket_requests.rs) `records_as_the_official_emulator_sends_them`. Counted: each step: the request monitor's records of a call, or a WebSocket exchange. Compared: in full: every record field for field (times, request IDs and the release key's number masked, a list filter's fields in any order); in part where an official record shows `resource` undefined (its first pass), which accepts hidane's stored document; a step marked `differs` (method and path only) as intentionally different.
- [`rules-conditions-official-v1.22.0.json`](../results/rules-conditions-official-v1.22.0.json), replayed by [`conditions.rs`](../crates/hidane-rules/tests/conditions.rs) `conditions_decide_as_on_the_official_emulator`. Counted: each condition decided; recorded over REST. Compared: an allow in full; a denial (`false` or an error) as decision and reasons; a ruleset that does not compile on the refusal only (in part; `rules_warnings.json` holds the message).
- [`export_import.json`](../crates/hidane/tests/fixtures/export_import.json), replayed by [`export_import.rs`](../crates/hidane/tests/export_import.rs) `exports_match_the_official_files`, `imports_write_what_the_official_emulator_writes`, `production_shaped_exports_import_like_the_official_emulator`, `the_endpoints_answer_like_the_official_emulator`, `databases_are_seeded_like_the_official_emulator`, `start_up_like_the_official_emulator`. Counted: each export, import, endpoint answer, seeding step and start-up. Compared: an export in part: its files, metadata byte for byte and every record, in name order (an intentional difference) and without times; the rest in full: imported documents, the endpoints' status, content type and body, seeding, exit codes and messages.
- [`auth.json`](../crates/hidane/tests/fixtures/auth.json), replayed by [`auth.rs`](../crates/hidane/tests/auth.rs) `authorization_is_read_like_the_official_emulator`. Counted: each case. Compared: in full: the outcome as the recording summarises it (status or code and message).
- [`function_events.json`](../crates/hidane/tests/fixtures/function_events.json), replayed by [`function_events.rs`](../crates/hidane/tests/function_events.rs) `events_reach_the_functions_emulator_as_on_the_official_emulator`. Counted: each step of each case (registrations and writes over REST, gRPC and the Write stream), in the three runs: with a Functions emulator, without, and with one that does not listen. Compared: in full: the status, every request the Functions emulator received (method, path, content type and `ce-*` headers, the v1 body byte for byte and the v2 body decoded, times and IDs as labels), and every `WARNING` about events. Intentionally different: steps with v2 events (`ce-source` names the project once), events for writes that change nothing, after `POST /reset`, and a v2 trigger without a database; in part (in any order): a deleted path or a cleared database.
- [`function_triggers.json`](../crates/hidane/tests/fixtures/function_triggers.json), replayed by [`function_triggers.rs`](../crates/hidane/tests/function_triggers.rs) `registrations_answer_like_the_official_emulator`. Counted: each registration, in both runs. Compared: in full: status, content type and body.
- [`value_order.json`](../crates/hidane-core/tests/fixtures/value_order.json), replayed by [`official_order.rs`](../crates/hidane-core/tests/official_order.rs) `values_within_a_group_are_equal`, `groups_are_strictly_ascending`, `document_names_sort_like_a_collection_group_query`. Counted: each value placed in the official order, and each document name; recorded over REST. Compared: in full: each value's place and its ties, the names' order.
- [`grpc_settings.json`](../crates/hidane/tests/fixtures/grpc_settings.json), replayed by [`grpc_settings.rs`](../crates/hidane/tests/grpc_settings.rs) `v1beta1_is_v1_under_another_name`, `large_grpc_requests_pass`, `rest_bodies_up_to_16_mib`. Counted: each v1beta1 call, gRPC size up to 20 MB and REST size (not the larger gRPC sizes nor the reflection listing). Compared: in full: the outcome as the recording summarises it.
- [`json_escapes.json`](../crates/hidane/tests/fixtures/json_escapes.json), replayed by [`rest.rs`](../crates/hidane/tests/rest.rs) `strings_escape_like_the_official_emulator`; [`webchannel.rs`](../crates/hidane/tests/webchannel.rs) `strings_escape_like_the_official_emulator`. Counted: each REST request and each WebChannel step. Compared: in full: REST as `rest.json` (its `compare` marks), WebChannel messages as their exact JSON text.
- [`limits.json`](../crates/hidane/tests/fixtures/limits.json), replayed by [`limits.rs`](../crates/hidane/tests/limits.rs) `limits_like_the_official_emulator`. Counted: each commit. Compared: in full: status and message.
- [`negative_zero.json`](../crates/hidane/tests/fixtures/negative_zero.json), replayed by [`negative_zero.rs`](../crates/hidane/tests/negative_zero.rs) `every_recorded_step_gives_the_official_answer`. Counted: each step. Compared: in full: the answer as the recording summarises it, each double spelled out; the steps that write `-1e-400` as intentionally different.

### SDK transcripts

| Transcript | Area | Client | Transport | Compared with the official one | Result |
|---|---|---|---|---|---|
| [`sdk-documents-official-v1.22.0.json`](../results/sdk-documents-official-v1.22.0.json) | Documents & writes | Admin SDK | gRPC | [`sdk-documents-hidane.json`](../results/sdk-documents-hidane.json) (hidane) | documented difference ([`parity-exceptions.md`](../docs/parity-exceptions.md)) |
| [`sdk-go-documents-official-v1.22.0.json`](../results/sdk-go-documents-official-v1.22.0.json) | Documents & writes | Go SDK | gRPC | [`sdk-go-documents-hidane.json`](../results/sdk-go-documents-hidane.json) (hidane) | documented difference ([`parity-exceptions.md`](../docs/parity-exceptions.md)) |
| [`sdk-java-documents-official-v1.22.0.json`](../results/sdk-java-documents-official-v1.22.0.json) | Documents & writes | Java SDK | gRPC | [`sdk-java-documents-hidane.json`](../results/sdk-java-documents-hidane.json) (hidane) | documented difference ([`parity-exceptions.md`](../docs/parity-exceptions.md)) |
| [`sdk-python-documents-official-v1.22.0.json`](../results/sdk-python-documents-official-v1.22.0.json) | Documents & writes | Python SDK | gRPC | [`sdk-python-documents-hidane.json`](../results/sdk-python-documents-hidane.json) (hidane) | documented difference ([`parity-exceptions.md`](../docs/parity-exceptions.md)) |
| [`sdk-aggregations-official-v1.22.0.json`](../results/sdk-aggregations-official-v1.22.0.json) | Queries & aggregations | Admin SDK | gRPC | [`sdk-aggregations-hidane.json`](../results/sdk-aggregations-hidane.json) (hidane) | identical |
| [`sdk-find-nearest-official-v1.22.0.json`](../results/sdk-find-nearest-official-v1.22.0.json) | Queries & aggregations | Admin SDK, web SDK | gRPC | [`sdk-find-nearest-hidane.json`](../results/sdk-find-nearest-hidane.json) (hidane) | identical |
| [`sdk-go-queries-official-v1.22.0.json`](../results/sdk-go-queries-official-v1.22.0.json) | Queries & aggregations | Go SDK | gRPC | [`sdk-go-queries-hidane.json`](../results/sdk-go-queries-hidane.json) (hidane) | identical |
| [`sdk-java-queries-official-v1.22.0.json`](../results/sdk-java-queries-official-v1.22.0.json) | Queries & aggregations | Java SDK | gRPC | [`sdk-java-queries-hidane.json`](../results/sdk-java-queries-hidane.json) (hidane) | identical |
| [`sdk-python-queries-official-v1.22.0.json`](../results/sdk-python-queries-official-v1.22.0.json) | Queries & aggregations | Python SDK | gRPC | [`sdk-python-queries-hidane.json`](../results/sdk-python-queries-hidane.json) (hidane) | identical |
| [`sdk-queries-official-v1.22.0.json`](../results/sdk-queries-official-v1.22.0.json) | Queries & aggregations | Admin SDK | gRPC | [`sdk-queries-hidane.json`](../results/sdk-queries-hidane.json) (hidane) | identical |
| [`sdk-go-transactions-official-v1.22.0.json`](../results/sdk-go-transactions-official-v1.22.0.json) | Transactions | Go SDK | gRPC | [`sdk-go-transactions-hidane.json`](../results/sdk-go-transactions-hidane.json) (hidane) | documented difference ([`parity-exceptions.md`](../docs/parity-exceptions.md)) |
| [`sdk-java-transactions-official-v1.22.0.json`](../results/sdk-java-transactions-official-v1.22.0.json) | Transactions | Java SDK | gRPC | [`sdk-java-transactions-hidane.json`](../results/sdk-java-transactions-hidane.json) (hidane) | documented difference ([`parity-exceptions.md`](../docs/parity-exceptions.md)) |
| [`sdk-python-transactions-official-v1.22.0.json`](../results/sdk-python-transactions-official-v1.22.0.json) | Transactions | Python SDK | gRPC | [`sdk-python-transactions-hidane.json`](../results/sdk-python-transactions-hidane.json) (hidane) | documented difference ([`parity-exceptions.md`](../docs/parity-exceptions.md)) |
| [`sdk-transactions-official-v1.22.0.json`](../results/sdk-transactions-official-v1.22.0.json) | Transactions | Admin SDK | gRPC | [`sdk-transactions-hidane.json`](../results/sdk-transactions-hidane.json) (hidane) | identical |
| [`sdk-web-transactions-official-v1.22.0.json`](../results/sdk-web-transactions-official-v1.22.0.json) | Transactions | web SDK in Node | gRPC | [`sdk-web-transactions-hidane.json`](../results/sdk-web-transactions-hidane.json) (hidane) | identical |
| [`sdk-go-listen-official-v1.22.0.json`](../results/sdk-go-listen-official-v1.22.0.json) | Listen & Write streams | Go SDK | gRPC | [`sdk-go-listen-hidane.json`](../results/sdk-go-listen-hidane.json) (hidane) | identical |
| [`sdk-java-listen-official-v1.22.0.json`](../results/sdk-java-listen-official-v1.22.0.json) | Listen & Write streams | Java SDK | gRPC | [`sdk-java-listen-hidane.json`](../results/sdk-java-listen-hidane.json) (hidane) | identical |
| [`sdk-listen-official-v1.22.0.json`](../results/sdk-listen-official-v1.22.0.json) | Listen & Write streams | Admin SDK, web SDK | gRPC | [`sdk-listen-hidane.json`](../results/sdk-listen-hidane.json) (hidane) | documented difference ([`README.md`](../results/README.md)) |
| [`sdk-listen-resume-official-v1.22.0.json`](../results/sdk-listen-resume-official-v1.22.0.json) | Listen & Write streams | web SDK, Admin SDK | gRPC | [`sdk-listen-resume-hidane.json`](../results/sdk-listen-resume-hidane.json) (hidane) | documented difference ([`README.md`](../results/README.md)) |
| [`sdk-python-listen-official-v1.22.0.json`](../results/sdk-python-listen-official-v1.22.0.json) | Listen & Write streams | Python SDK | gRPC | [`sdk-python-listen-hidane.json`](../results/sdk-python-listen-hidane.json) (hidane) | identical |
| [`sdk-web-writes-official-v1.22.0.json`](../results/sdk-web-writes-official-v1.22.0.json) | Listen & Write streams | web SDK in Node | gRPC | [`sdk-web-writes-hidane.json`](../results/sdk-web-writes-hidane.json) (hidane) | identical |
| [`sdk-rest-official-v1.22.0.json`](../results/sdk-rest-official-v1.22.0.json) | REST | web SDK Lite | REST | [`sdk-rest-hidane.json`](../results/sdk-rest-hidane.json) (hidane) | identical |
| [`browser-webchannel-official-v1.22.0.json`](../results/browser-webchannel-official-v1.22.0.json) | WebChannel | firebase in Chromium | WebChannel | [`browser-webchannel-hidane.json`](../results/browser-webchannel-hidane.json) (hidane) | identical |
| [`flutter-web-official-v1.22.0.json`](../results/flutter-web-official-v1.22.0.json) | WebChannel | Flutter Web (cloud_firestore) in Chromium | WebChannel, REST | [`flutter-web-hidane.json`](../results/flutter-web-hidane.json) (hidane) | documented difference ([`README.md`](../results/README.md)) |
| [`js-sdk-browser-official-v1.22.0.json`](../results/js-sdk-browser-official-v1.22.0.json) | WebChannel | firebase-js-sdk browser suite in Chromium | WebChannel, REST | [`js-sdk-browser-hidane.json`](../results/js-sdk-browser-hidane.json) (hidane, without `emulator`; the outcome of each test: 1,183 passed, 739 skipped, 0 failed) | identical |
| [`browser-rules-official-v1.22.0.json`](../results/browser-rules-official-v1.22.0.json) | Security Rules | firebase + rules-unit-testing in Chromium | WebChannel, REST | [`browser-rules-hidane.json`](../results/browser-rules-hidane.json) (hidane) | documented difference ([`README.md`](../results/README.md)) |
| [`sdk-rules-unit-testing-official-v1.22.0.json`](../results/sdk-rules-unit-testing-official-v1.22.0.json) | Security Rules | firebase + rules-unit-testing in Node | gRPC | [`sdk-rules-unit-testing-hidane.json`](../results/sdk-rules-unit-testing-hidane.json) (hidane) | documented difference ([`README.md`](../results/README.md)) |
| [`export-import-official-v1.22.0.json`](../results/export-import-official-v1.22.0.json) | Export & import | firebase-tools + Admin SDK | CLI, gRPC | [`export-import-hidane-to-official.json`](../results/export-import-hidane-to-official.json) (exported by hidane, read by the official emulator) | identical |
| [`export-import-official-v1.22.0.json`](../results/export-import-official-v1.22.0.json) | Export & import | firebase-tools + Admin SDK | CLI, gRPC | [`export-import-hidane.json`](../results/export-import-hidane.json) (hidane) | identical |
| [`export-import-official-v1.22.0.json`](../results/export-import-official-v1.22.0.json) | Export & import | firebase-tools + Admin SDK | CLI, gRPC | [`export-import-official-to-hidane.json`](../results/export-import-official-to-hidane.json) (exported by the official emulator, read by hidane) | identical |
| [`functions-triggers-official-v1.22.0.json`](../results/functions-triggers-official-v1.22.0.json) | Auth & admin endpoints | firebase-tools Functions emulator (firebase-functions v1 and v2), Admin SDK | CLI, gRPC | [`functions-triggers-hidane.json`](../results/functions-triggers-hidane.json) (hidane) | documented difference ([`parity-exceptions.md`](../docs/parity-exceptions.md)) |
| [`sdk-auth-official-v1.22.0.json`](../results/sdk-auth-official-v1.22.0.json) | Auth & admin endpoints | rules-unit-testing, web SDK, Admin SDK | gRPC | [`sdk-auth-hidane.json`](../results/sdk-auth-hidane.json) (hidane) | identical |
| [`sdk-clear-official-v1.22.0.json`](../results/sdk-clear-official-v1.22.0.json) | Auth & admin endpoints | rules-unit-testing, web SDK, Admin SDK | gRPC, REST | [`sdk-clear-hidane.json`](../results/sdk-clear-hidane.json) (hidane) | documented difference ([`README.md`](../results/README.md)) |

<!-- parity:end -->
