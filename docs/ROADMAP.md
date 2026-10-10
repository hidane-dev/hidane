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
| Rules `list` evaluation (query constraints vs rule conditions) is unspecified for a closed-source engine | v0.2 ships a conservative subset, deny-by-default | [rules-query docs](https://firebase.google.com/docs/firestore/security/rules-query) |
| Known official-emulator divergences: simple-lock transactions, `PartitionQuery` 501, `ExecutePipeline` enterprise-only, #6252, #11241 | A parity-exception list is needed | [parity-exceptions.md](parity-exceptions.md) |
| Official baseline (under load): 0.74 s to port (2.5 s via CLI), 95 MiB idle, 456 MiB after 1k docs, superlinear writes with a listener | Targets below | [why.md](why.md), `results/` |
| The jar grants no redistribution rights | CI downloads it each run and verifies SHA256 | jar `FIRESTORE_EMULATOR_LICENSES` |

## 1. Targets (proposed)

| Metric | Official (measured) | hidane target |
|---|---|---|
| Time until the port accepts | 0.74 s (2.5 s via firebase-tools) | < 100 ms |
| RSS idle | 95 MiB | < 50 MiB |
| RSS after 1,000 documents | 456 MiB | < 100 MiB |
| Write cost with a listener attached | grows with total document count | O(changed documents), independent of total |
| Java | 21+ required | not required (single binary) |

## 2. v0.1 — gRPC core, no rules

**Goal**: usable from server SDKs, mobile SDKs and Node-based firebase-js-sdk, under
`firebase emulators:start`, with Security Rules fixed to allow-all.

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
| P1 | parity | Oracle jar in CI, conformance-suite format, nodejs-firestore system tests (gRPC) |
| P1 | bench | Startup, RSS and degradation regression benchmarks in CI |
| P1 | distribution | hidane.dev DNS / HTTPS (currently a dead link), GitHub Releases naming |
| P2 | rest | REST transcoding of the documents API (enables js-sdk Lite, `curl`, Emulator UI, `preferRest`) |
| P2 | storage | Export / import (LevelDB log + EntityProto, `--seed_from_export`, `POST :export`). May slip to v0.2 |
| P2 | rest | Emulator UI compatibility (`:listCollectionIds` admin check, `showMissing=true`) |
| P2 | grpc | gRPC reflection, `v1beta1` alias, 17 MB message limit, keepalive |

Parity exceptions planned for v0.1: the listener-induced write slowdown and the #11241 regression
(see [parity-exceptions.md](parity-exceptions.md)).

## 3. v0.2 — rules

**Goal**: evaluate `firestore.rules`; `@firebase/rules-unit-testing` and the Emulator UI request monitor work.

| Priority | Item |
|---|---|
| P1 | Grammar (v1 / v2, `let`, operator precedence) and diagnostics (`issues[]` / `Severity`) |
| P1 | Type system (14 types, 70+ methods) and built-in functions |
| P1 | `request` / `resource` / `request.query`, `request.resource.data` after FieldValue transforms |
| P1 | `get` / `exists` / `getAfter` / `existsAfter`, 10 / 20 call limits, cached calls not counted |
| P1 | `list` implication: conservative subset, deny when undecidable, with a diagnostic log |
| P1 | Real diagnostics from `:securityRules`, `:ruleCoverage` (JSON), denial message `%s for '%s' @ L%d` |
| P1 | Mock tokens (`alg: none`) and `Bearer owner` |
| P2 | WebSocket `/requests` feed for the Emulator UI |
| P2 | Golden results from the production Rules API `projects:test` |
| P2 | Event delivery to `--functions_emulator` (v1 and v2 CloudEvents) |

Parity exception: #6252 (first evaluation of rules that use `resource.data` errors) is not reproduced.

## 4. v0.3 — webchannel

**Goal**: browser firebase-js-sdk and Flutter Web work. Together with REST (v0.1 P2) this completes Web support.

| Priority | Item |
|---|---|
| P1 | WebChannel v8: handshake, forward channel, back channel (chunked), `noop`, `terminate` |
| P1 | Authorization extracted from the `headers=` body field; CORS expose-headers |
| P1 | `forceLongPolling` (`CI=1`) and `detectBufferingProxy` must keep working |
| P1 | firebase-js-sdk browser integration tests (Playwright) run against hidane |
| prerequisite | Listen / Write core designed transport-agnostic from v0.1 (ADR 0004) |

## 5. Backlog

- Enterprise edition pipelines (`ExecutePipeline`, #80)
- Datastore mode (`--database-mode datastore-mode`), `--index_file` / `--require_indexes`
- `google.firestore.admin.v1` (not in the official emulator either; stays UNIMPLEMENTED)
- Distribution channels: Homebrew tap, cargo binstall, ghcr multi-arch, `curl | sh`, npm optionalDependencies, pub.dev launcher, mise
- License decision (ADR 0001)
- Registry metadata alignment (npm keywords, `0.0.0-stage`, pub.dev description)
- Nightly against production Firestore (needs a GCP project)
- iOS / Android integration tests (monthly, manual)

## 6. Out of scope

- Realtime Database, Cloud Storage and Auth emulators (no maintained RTDB-compatible server has existed since 2021; a separate project's problem)
- Production use. hidane is a local-development and CI emulator and must not be exposed without authentication (`SECURITY.md`)
- Decompiling or reusing the official jar. Clean-room only: public protos, public docs, black-box observation

## 7. Biggest risks (as of Phase 0)

1. **No Web support until v0.3.** Browser development is the most common local-development case and a gRPC-only v0.1 does nothing for it. Mitigation: pull REST unary into v0.1, keep the Listen / Write core transport-agnostic, and revisit the WebChannel timing in ADR 0004.
2. **No supported replacement hook in firebase-tools.** The `java` shim works but is an awkward distribution story; an upstream PR may or may not be accepted.
3. **Rules `list` implication is unspecified.** Parity can only be measured by differential testing, and the evaluation semantics (errors, overflow) are undocumented.

Runners-up: the `resume_token` design is hard to change later; the jar cannot be redistributed so CI depends on an external download; the baseline numbers were measured under load.
