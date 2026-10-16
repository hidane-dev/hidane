# hidane roadmap (Phase 0 draft)

- Date: 2026-10-09
- Status: **tentative**. Based on the Phase 0 research; nothing here is decided. Each item is tracked as a
  GitHub issue, grouped by [milestone](https://github.com/hidane-dev/hidane/milestones).
- Decisions that still need to be made are drafted as ADRs in [`adr/`](adr/).
- Phase 0 research was done as internal working notes. This page carries the conclusions and their
  primary sources; the notes themselves are not published.

## 0. Facts the plan rests on

| Fact | Consequence | Evidence |
|---|---|---|
| The official emulator serves HTTP/1.1 REST, HTTP/2 gRPC and WebChannel on **one port**; the only other external port is `--websocket_port` for the Emulator UI | v0.1 must multiplex one port | Startup log and `lsof` of v1.22.0; `results/` |
| Server SDKs (Node admin, Go, Python, Java), iOS, Android, Flutter native and firebase-js-sdk **in Node** work over **gRPC alone** | v0.1 = gRPC covers them | SDK sources, see [compatibility.md](compatibility.md) |
| The browser firebase-js-sdk does reads and writes over **WebChannel** and transactions / aggregations over **REST**; nothing works over gRPC | Web support needs both; v0.1 states "browser: not yet" | [compatibility.md](compatibility.md) |
| firebase-tools has no supported way to replace the `java` invocation. A `java` shim on `PATH` works end to end (verified) | Launch strategy must be decided | `src/emulator/downloadableEmulators.ts` (v15.33.0) |
| Listen must emit an empty-`target_ids` `TargetChange` with a monotonically increasing `read_time` per consistent snapshot; `resume_token` design couples to storage | Storage design constraint | `google/firestore/v1/firestore.proto` (Listen docs), js-sdk `remote_store.ts` |
| Rules `list` evaluation (query constraints vs rule conditions) is unspecified for a closed-source engine | Planned: a conservative subset, deny-by-default. Measuring showed the official model, now reproduced whole ([ADR 0005](adr/0005-rules-engine.md)) | [rules-query docs](https://firebase.google.com/docs/firestore/security/rules-query) |
| Known official-emulator divergences: simple-lock transactions, `PartitionQuery` 501, `ExecutePipeline` enterprise-only, #6252, #11241 | A parity-exception list is needed | [parity-exceptions.md](parity-exceptions.md) |
| Official baseline (under load): 0.74 s to port (2.5 s via CLI), 95 MiB idle, 456 MiB after 1k docs, superlinear writes with a listener | Targets below | [why.md](why.md), `results/` |
| The jar grants no redistribution rights | Never committed or shipped: its answers are recorded into fixtures that CI replays without Java, and a workflow that needs the jar downloads it from Google's URL and checks the SHA-256 (#40) | jar `FIRESTORE_EMULATOR_LICENSES` |

## 1. Targets (proposed)

| Metric | Official (measured) | hidane target | hidane v0.5.1 (measured) |
|---|---|---|---|
| Time until the port accepts | 852 ms | < 100 ms | 1.53 ms |
| RSS idle | 148 MiB | < 50 MiB | 9.3 MiB |
| RSS after 1,000 documents | 276 MiB | < 100 MiB | 11.9 MiB |
| A commit of 500 writes | 65.4 ms | < 25 ms | 10.2 ms |
| Write cost with a listener attached | grows with total document count | O(changed documents), independent of total | flat per batch: 100k documents in 4.9 s |
| Java | 21+ required | not required (single binary) | not required |

The first four rows are medians from the canonical benchmark environment, both emulators on one
quiet machine ([result](https://github.com/hidane-dev/benchmark-results/blob/main/results/release/2026/10/2026-10-13-release-150328.md)). The listener row comes from Phase 0 and a later run on another
machine, not side by side (`docs/why.md`); #50 re-measures it. A workflow checks hidane's first
four rows against their targets on a GitHub-hosted runner on every push to `main` and nightly
([`bench/`](../bench/README.md#targets-in-ci), #47).

## 2. v0.1 — gRPC core, no rules

**Goal**: usable from server SDKs, mobile SDKs and Node-based firebase-js-sdk, under
`firebase emulators:start`, with Security Rules fixed to allow-all.

v0.1.0 was released on 2026-10-11. Its milestone was closed on 2026-10-12: the parity and
benchmark work it still held moved to the "Quality: parity & benchmarks" milestone (#5, #6), and
#14, #36, #44 and #50 to the backlog.

**Supported clients**: firebase-admin (Node) / `@google-cloud/firestore`, Go, Python, Java, iOS, Android,
Flutter (Android / iOS / macOS / Windows), C++ / Unity, firebase-js-sdk in Node.
**Not supported (stated up front)**: browser firebase-js-sdk, Flutter Web, firebase-js-sdk Lite (REST only;
works once R1 lands).

| Priority | Area | Item |
|---|---|---|
| P1 | cli | Accept the official 22 CLI flags and the exact argument set firebase-tools passes; bind within 60 s; exit within 4 s of SIGINT with code 0 / 130; startup log lines |
| P1 | cli | Decide how to launch under firebase-tools (`java` shim vs upstream PR) |
| P1 | grpc | Multiplex HTTP/1.1 and HTTP/2 (h2c) on one port (ADR 0003) |
| P1 | grpc | The 16 `google.firestore.v1.Firestore` RPCs (`ExecutePipeline` → UNIMPLEMENTED, `PartitionQuery` → 501 like the official emulator) |
| P1 | grpc | Listen: consistent-snapshot delivery, ADD / CURRENT / RESET / REMOVE ordering, client-assigned target ids, `ExistenceFilter.count` |
| P1 | grpc | Write stream: handshake (`stream_token` only), FIFO 1:1, accept empty `writes` |
| P1 | storage | In-memory store (ADR 0002), `__name__` ordering, value type ordering, monotonically increasing commit timestamps, snapshot reads |
| P1 | storage | Change notification in O(changed documents) per commit |
| P1 | rest | Admin HTTP (`GET /`, `POST /reset`, `POST /shutdown`) and the `/emulator/v1` endpoints firebase-tools calls (`:securityRules` accepted and ignored, `DELETE …/documents`) |
| P1 | parity | The official emulator's answers recorded by `tools/oracle/` and replayed in CI without Java (done). Drift checks, the method's conventions, the parity table and the nodejs-firestore system tests moved to the Quality milestone (#5) |
| P1 | bench | Startup, RSS and listener-degradation measurements (done by hand, `results/`); running them in CI moved to the Quality milestone (#6) |
| P1 | distribution | hidane.dev DNS / HTTPS (currently a dead link), GitHub Releases naming |
| P2 | rest | REST transcoding of the documents API (enables js-sdk Lite, `curl`, Emulator UI, `preferRest`) |
| P2 | storage | Export / import (LevelDB log + EntityProto, `--seed_from_export`, `POST :export`) — done, [export-format.md](export-format.md) |
| P2 | rest | Emulator UI compatibility (`:listCollectionIds` admin check, `showMissing=true`) |
| P2 | grpc | gRPC reflection, `v1beta1` alias, 17 MB message limit, keepalive |

Parity exceptions planned for v0.1: the listener-induced write slowdown and the #11241 regression
(see [parity-exceptions.md](parity-exceptions.md)).

## 3. v0.2 — rules

**Goal**: evaluate `firestore.rules`; `@firebase/rules-unit-testing` and the Emulator UI request monitor work.

v0.2.0 (2026-10-12) has the P1 items, v0.2.1 adds the request monitor (#69) and the remaining
`request` fields (#166), v0.2.2 checks a commit's `verify` writes (#173), and v0.2.3 verifies
rules-unit-testing end to end (#186) and accepts the Functions emulator's trigger registrations
(#185). v0.2.4 shows constraints in coverage reports and `debug()` as the official emulator does
(#167) and escapes JSON strings as it does (#191). Functions events (#70) moved to v0.4.

| Priority | Item | Status (2026-10-12) |
|---|---|---|
| P1 | Grammar (v1 / v2, `let`, operator precedence) and diagnostics (`issues[]` / `Severity`) | In v0.2.0 (#59, #60): the official type checker's warnings |
| P1 | Type system (14 types, 70+ methods) and built-in functions | In v0.2.0 (#60, #62) |
| P1 | `request` / `resource` / `request.query`, `request.resource.data` after FieldValue transforms | In v0.2.0 (#63, #64); in v0.2.1 (#166): every key of the official `request` and `request.query` (`inTransaction`, `fields`, `writeFields`, `transforms`, `readFields`, `autoId`, `kind`, `parent`, …), in its order; in v0.2.2 (#173): a commit's `verify` writes, checked as a `get` of their document, before the preconditions of the whole commit |
| P1 | `get` / `exists` / `getAfter` / `existsAfter`, 10 / 20 call limits, cached calls not counted | In v0.2.0 (#65); after v0.5.1, the limit of 1,000 evaluated expressions per decision, counted as the official emulator counts them (#293) |
| P1 | `list` implication, measured against the official emulator | In v0.2.0 (#66): the official model, not a subset |
| P1 | Real diagnostics from `:securityRules`, `:ruleCoverage` (JSON), denial message `%s for '%s' @ L%d` | In v0.2.0: denial messages, `GET :securityRules` (#155), `:ruleCoverage` as JSON and as a page, and `debug()` output (#67) |
| P1 | Mock tokens (`alg: none`) and `Bearer owner` | In v0.2.0 (#68) |
| P2 | WebSocket `/requests` feed for the Emulator UI | In v0.2.1 (#69): the official records, the last 50 on connect; a client that falls behind is disconnected instead of holding up requests |
| P2 | Golden results from the production Rules API `projects:test` | #82 (backlog, needs a GCP project) |
| P2 | Event delivery to `--functions_emulator` (v1 and v2 CloudEvents) | Moved to v0.4 (#70); the trigger registrations are accepted since v0.2.3 (#185), so `emulators:start` comes up with Firestore-triggered functions |

Parity exception: #6252 (the official emulator evaluates conditions in two passes and lists the reasons of both) is not reproduced; hidane evaluates each condition once.

## 4. v0.3 — webchannel

**Goal**: browser firebase-js-sdk and Flutter Web work. Together with REST (v0.1 P2) this completes Web support.

v0.3.0 (2026-10-13) completes it. WebChannel itself landed early, in v0.1.0 (#71–#73); v0.2.4
verified Flutter Web (#75); and in v0.3.0 firebase-js-sdk's own browser suite passes on hidane as
on the official emulator, 1,183 tests in Chromium, run in CI (#74). Two differences it found were
fixed from new recordings: commits that write one document more than once, and an equality on
`__name__` next to inequalities on other fields. The parity table (#43) is generated from the
recordings ([parity.md](parity.md)).

| Priority | Item |
|---|---|
| P1 | WebChannel v8: handshake, forward channel, back channel (chunked), `noop`, `terminate` (landed early: #71, [webchannel.md](webchannel.md)) |
| P1 | Authorization extracted from the `headers=` body field; CORS expose-headers |
| P1 | `forceLongPolling` (`CI=1`) and `detectBufferingProxy` must keep working |
| P1 | firebase-js-sdk browser integration tests (Playwright) run against hidane (done: #74, `tools/js-sdk-browser/`) |
| prerequisite | Listen / Write core designed transport-agnostic from v0.1 (ADR 0004) |

## 5. v0.4 — functions

**Goal**: Firestore-triggered Cloud Functions fire under `firebase emulators:start` with the
Functions emulator, as they do with the official emulator.

Released in v0.4.0 (2026-10-13), with the commit checks in the official emulator's order
(#202, #203) and the Go, Python and Java server SDKs verified against both emulators (#187).

The Functions emulator registers its triggers (accepted since v0.2.3, #185), and every commit
sends an event to each trigger that watches a document it changed (#70), recorded from the
official emulator by `tools/oracle/function_events.py` and replayed by
`crates/hidane/tests/function_events.rs`: v1 JSON events, v2 CloudEvents with `ce-*` headers and
a `DocumentEventData` in protobuf (`withAuthContext` too), path patterns, named databases, batches,
transactions, the Write stream, deleted paths and cleared databases, none for a write the Security
Rules deny (#288), one event at a time in the
background, the official emulator's order and `WARNING`s. Without `--functions_emulator`, each
event is a `WARNING`. End to end, v1 and v2 functions receive the same data under `hidane exec`
as with the official emulator (`results/functions-triggers-*.json`).

| Priority | Item | Status |
|---|---|---|
| P1 | v1 events (`providers/cloud.firestore/eventTypes/document.*`): JSON, the documents before and after | Done (#70) |
| P1 | v2 events (`google.cloud.firestore.document.v1.*`, `withAuthContext`): binary CloudEvents, `application/protobuf` in base64 | Done (#70) |
| P1 | Matching: v1 resources, v2 `EXACT` and `PATH_PATTERN` documents, databases | Done (#70) |
| P1 | Delivery: asynchronous and in order, the official emulator's timeouts and `WARNING`s | Done (#70) |

Parity exceptions (#70): `ce-source` names the project once, a write that changes nothing and
the delete of a missing document send no v2 event, `POST /reset` keeps the triggers, and a v2
trigger without a database watches `(default)` ([parity-exceptions.md](parity-exceptions.md)).

## 6. v0.5 — tooling

**Goal**: the tools around the emulator — checking a project before it moves, moving it,
measuring what that saves, and reporting a bug — and the quality work behind them.

Released in v0.5.0 (2026-10-13):

| Priority | Item | Status |
|---|---|---|
| P1 | `hidane doctor`: a project's `firebase.json`, rules, SDK and firebase-tools versions and CI, against what was verified | Done (#217, #221) |
| P1 | `hidane migrate`: the changes as a diff, applied with `--apply` | Done (#219, #222) |
| P1 | `hidane benchmark` (both emulators, your command) and `hidane benchmark savings` | Done (#213, #238) |
| P1 | `hidane report`: a redacted diagnostic bundle | Done (#253) |
| P1 | Testcontainers: the gcloud modules (Java, Node, Go) and generic containers (Python) | Done (#240, #264, #268) |
| P2 | Differential fuzzing against the official emulator, first two batches of fixes | Done (#259, #273, #276); the rest in #262 |
| P2 | Reproducible builds, SBOMs and image attestations | Done (#246, #254) |

## 7. Enterprise edition

**Goal**: answer a database started with `--database-edition enterprise` (firebase-tools'
`firestore.edition: "enterprise"`) as the official emulator does.

| Priority | Item | Status |
|---|---|---|
| P1 | Queries and aggregation queries with the pipeline semantics | Done in v0.6.0 (#341, #342) |
| P1 | Listeners' queries, checked with null and NaN as values | Done (#343, #338) |
| P1 | Pipelines (`ExecutePipeline`): every stage and function the SDKs send, in transactions too | Done (#347) |
| P1 | Writes: values as an enterprise database checks them (arrays in arrays, no size limits, 20 levels) | Done (#338) |
| P2 | The BSON types (Int32, Decimal128, ObjectId, regular expressions, MinKey and MaxKey, BSON timestamps and binary data): checked, stored, ordered and compared in queries and pipelines | Done (#338) |
| P2 | `:setDatabaseEdition`: one database's edition, until `/reset` | Done (#36) |

## 8. Backlog

- Datastore mode (`--database-mode datastore-mode`), `--index_file` / `--require_indexes` (#81)
- `google.firestore.admin.v1` (not in the official emulator either; stays UNIMPLEMENTED)
- The gRPC `FirestoreEmulator` service (#36); its HTTP endpoints are served
- Runs against production Firestore and the production Rules API (needs a GCP project, #82)

The distribution channels and the license decision listed here before shipped in v0.1.0.

## 9. Out of scope

- Realtime Database, Cloud Storage and Auth emulators (no maintained RTDB-compatible server has existed since 2021; a separate project's problem)
- Production use. hidane is a local-development and CI emulator and must not be exposed without authentication (`SECURITY.md`)
- Decompiling or reusing the official jar. Clean-room only: public protos, public docs, black-box observation

## 10. Biggest risks (as of Phase 0)

1. **No Web support until v0.3.** Browser development is the most common local-development case and a gRPC-only v0.1 does nothing for it. Mitigation: pull REST unary into v0.1, keep the Listen / Write core transport-agnostic, and revisit the WebChannel timing in ADR 0004.
2. **No supported replacement hook in firebase-tools.** The `java` shim works but is an awkward distribution story; an upstream PR may or may not be accepted.
3. **Rules `list` implication is unspecified.** Parity can only be measured by differential testing, and the evaluation semantics (errors, overflow) are undocumented. Since resolved by measuring: the official model is reproduced (#66, ADR 0005).

Runners-up: the `resume_token` design is hard to change later; the jar cannot be redistributed so CI depends on an external download; the baseline numbers were measured under load.
