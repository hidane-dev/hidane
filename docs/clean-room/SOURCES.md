# Allowed sources

Under this policy, every behaviour hidane implements comes from one of the sources below. Each
section says what the source is, what hidane takes from it, and where it is used. A source that is
not on this page is not used until an issue has agreed to add it here; the sources that must never
be used are in [CONTRIBUTOR_GUIDE.md](CONTRIBUTOR_GUIDE.md). The kinds of facts that may instead
have come from the inspections of the jar made before the policy are in
[section 7](#7-before-this-policy).

The licenses named are the ones each project publishes. Naming them records where the material
came from; it is not a legal opinion on what they permit.

| Source | Published under | What hidane takes from it |
|---|---|---|
| [Public protobuf definitions](#1-public-protobuf-definitions) | Apache-2.0 | Services, messages, field numbers, REST bindings, documented semantics |
| [Public Firebase and Google Cloud documentation](#2-public-firebase-and-google-cloud-documentation) | The terms each page states | Documented semantics and limits, the Rules language, production behaviour |
| [The officially distributed SDKs and firebase-tools](#3-the-officially-distributed-sdks-and-firebase-tools) | Apache-2.0 (the SDKs), MIT (firebase-tools) | What clients send and expect, how firebase-tools starts and calls the emulator; their behaviour against both emulators |
| [Black-box observation of the official emulator](#4-black-box-observation-of-the-official-emulator) | (observed, not copied) | The emulator's answers, output and files |
| [CEL's specification and conformance tests](#5-cels-specification-and-conformance-tests) | Apache-2.0 | CEL semantics where Rules follows CEL, test cases |
| [Other public specifications and inputs](#6-other-public-specifications-and-inputs) | Each as published | File formats, protocols, platform behaviour the official emulator depends on, sample inputs, known bugs |

## 1. Public protobuf definitions

- **What**: the `google.firestore.v1` definitions from
  [googleapis/googleapis](https://github.com/googleapis/googleapis) and the files they import
  (`google/api`, `google/rpc`, `google/type`), Apache-2.0.
- **Where they are**: vendored in [`crates/hidane-proto/proto/`](../../crates/hidane-proto/proto/)
  at a pinned commit, with their license (`LICENSE-googleapis`) and a README that names the commit
  and the one local change: `Write.verify`, a field the SDKs' own published copies of
  `write.proto` define and send in transactions, which googleapis does not publish.
- **What hidane takes**: the services and messages it serves over gRPC, their field numbers, the
  `google.api.http` bindings its REST layer follows (`crates/hidane/src/rest.rs`), and the
  semantics their comments document (Listen's consistent snapshots and resume tokens, for
  example).
- **Used by**: `crates/hidane-proto/build.rs`, which compiles them for the other crates; the gRPC,
  REST and WebChannel layers of `crates/hidane/`; the oracle scripts that talk gRPC
  (`tools/oracle/write_stream.mjs`, `tools/oracle/rules_streams.py` and others through
  `grpcurl`).
- They are third-party code, kept with their license and notices; the CLA does not cover them
  ([`docs/cla/README.md`](../cla/README.md#what-the-cla-does-not-cover)).
- **Datastore mode** (#81, added by #356): the `google.datastore.v1` definitions
  (`datastore.proto`, `entity.proto`, `query.proto`, `aggregation_result.proto`,
  `query_profile.proto`) from googleapis at the same commit, Apache-2.0, vendored next to the
  others. hidane takes the `Datastore` service, its messages and field numbers, the
  `google.api.http` bindings of its REST API (`POST /v1/projects/{project_id}:lookup` and the
  others), and the semantics their comments document. Compiled by
  `crates/hidane-proto/build.rs` into `hidane_proto::google::datastore::v1`; not served yet.
- **Also**: `google.events.cloud.firestore.v1.DocumentEventData` from
  [googleapis/google-cloudevents](https://github.com/googleapis/google-cloudevents) (Apache-2.0),
  the data of the CloudEvents a Firestore trigger receives, declared by hand in
  `crates/hidane/src/triggers/events.rs` after the published `data.proto` (#70).

## 2. Public Firebase and Google Cloud documentation

- **What**: [firebase.google.com/docs](https://firebase.google.com/docs) (Cloud Firestore,
  Security Rules, the Emulator Suite) and the Cloud Firestore documentation on
  [cloud.google.com](https://cloud.google.com/firestore/docs) (the API reference, the emulator,
  limits), under the terms each page states. For Datastore mode (#81, added by #356), the
  documentation of Firestore in Datastore mode and Cloud Datastore on
  [cloud.google.com](https://cloud.google.com/datastore/docs): the API reference, entities and
  keys, queries and GQL, transactions, indexes and `index.yaml`, limits, and the emulator.
- **What hidane takes**: documented behaviour and limits; the Security Rules language (its
  statements, types, functions, `request` and `resource`, the call limits of `get()` and
  `exists()`); in Datastore mode, the GQL grammar and the `index.yaml` format; and, where the
  official emulator differs from production Firestore, what production does.
- **Used by**: [ADR 0005](../adr/0005-rules-engine.md) (the Rules language and what its
  documentation leaves open); the production column of
  [`docs/parity-exceptions.md`](../parity-exceptions.md); `crates/hidane/tests/fixtures/rules_compile.json`,
  whose compile cases include the rulesets shown on the Firebase documentation's pages, each
  with its page's URL in `source`.

## 3. The officially distributed SDKs and firebase-tools

Their public source code is read to learn the protocols they speak and how they call the
emulator. hidane takes facts from it (which transport a client uses, which RPCs, which fields
and headers it sends, which flags firebase-tools passes, which phrase it looks for on standard
error), not code: no code of theirs is copied into hidane. Their behaviour is also observed:
each is run against both emulators and the results compared.

| Project | Published under | Read to learn | Where |
|---|---|---|---|
| [firebase-tools](https://github.com/firebase/firebase-tools) | MIT | How it downloads, checks and starts the emulator (the Java probe, `FIRESTORE_EMULATOR_BINARY_PATH`, the flags it passes, SIGINT to stop it); the jar version and checksum it pins (`src/emulator/downloadableEmulatorInfo.json`); how `emulators:export` and `--import` call the emulator; what its Functions emulator sends to register Firestore triggers, and how it reads the events it receives; which emulators `emulators:start` starts from `firebase.json` (`filterEmulatorTargets` in `src/emulator/controller.ts`) or, without one, from its default configuration (`DEFAULT_CONFIG` and `beforeEmulatorCommand` in `src/emulator/commandUtils.ts`), and which of them run on Java (`hidane doctor`, `hidane migrate`); where it caches the emulators it downloads and what it names their files (`CACHE_DIR` and `FIREBASE_EMULATORS_PATH` in `src/emulator/downloadableEmulators.ts`, `downloadableEmulatorInfo.json`), and the environment variables through which the emulators are found (`src/emulator/constants.ts`) (`hidane report`) | [ADR 0006](../adr/0006-launch-under-firebase-tools.md), `crates/hidane/src/launch.rs`, `crates/hidane/src/cli.rs`, `crates/hidane/src/main.rs`, [`docs/export-format.md`](../export-format.md), `crates/hidane/src/triggers.rs`, `crates/hidane/src/triggers/`, `tools/oracle/function_triggers.py`, `tools/oracle/function_events.py`, `.github/actions/official-emulator/pinned.sh`, `crates/hidane/src/project/firebase.rs`, `tools/upstream/watch.py` (the emulator each firebase-tools release pins), `crates/hidane/src/diagnostic/mod.rs` |
| [firebase-js-sdk](https://github.com/firebase/firebase-js-sdk) | Apache-2.0 | Which transport it uses in the browser, in Node and in Lite; the WebChannel requests it makes; the mock tokens rules-unit-testing builds (`createMockUserToken`); its copy of `write.proto` with `verify` | [`docs/compatibility.md`](../compatibility.md), [`docs/webchannel.md`](../webchannel.md), `crates/hidane/src/webchannel.rs`, `crates/hidane/src/firestore/auth.rs`, `crates/hidane-proto/proto/README.md` |
| Server SDKs: [nodejs-firestore](https://github.com/googleapis/nodejs-firestore), [google-cloud-go](https://github.com/googleapis/google-cloud-go), [python-firestore](https://github.com/googleapis/python-firestore), [java-firestore](https://github.com/googleapis/java-firestore) | Apache-2.0 | Which transport and RPCs each uses against an emulator, and how each finds it | [`docs/compatibility.md`](../compatibility.md) |
| Mobile SDKs: [firebase-ios-sdk](https://github.com/firebase/firebase-ios-sdk), [firebase-android-sdk](https://github.com/firebase/firebase-android-sdk) | Apache-2.0 | Which transport they use (gRPC, the Listen and Write streams) | [`docs/compatibility.md`](../compatibility.md) |
| Datastore client libraries (#81, added by #356): [python-datastore](https://github.com/googleapis/python-datastore), [google-cloud-go](https://github.com/googleapis/google-cloud-go) `datastore`, [java-datastore](https://github.com/googleapis/java-datastore), [nodejs-datastore](https://github.com/googleapis/nodejs-datastore) | Apache-2.0 | Which transport and RPCs each uses against an emulator (`DATASTORE_EMULATOR_HOST`), and what it sends | `tools/oracle/sdk_datastore_python/` (python-datastore), `tools/oracle/sdk_datastore_java/` (java-datastore), `tools/oracle/sdk_datastore_go/` (google-cloud-go `datastore`), `tools/oracle/sdk_datastore_node.mjs` (nodejs-datastore) |

Their behaviour against both emulators:

- `tools/oracle/sdk_*.mjs`, `tools/oracle/sdk_go/`, `tools/oracle/sdk_python/`,
  `tools/oracle/sdk_java/`, `tools/oracle/webchannel/` and `tools/oracle/flutter_web/` run the
  same steps through an SDK against each emulator, and `tools/oracle/sdk_datastore_python/`,
  `tools/oracle/sdk_datastore_java/`, `tools/oracle/sdk_datastore_go/` and
  `tools/oracle/sdk_datastore_node.mjs` through the client libraries of Datastore against each in
  Datastore mode; the transcripts are in `results/`.
- `tools/js-sdk-browser/` runs firebase-js-sdk's own Firestore browser test suite against each.
- `hidane exec -- firebase emulators:start` runs firebase-tools with hidane in place of the jar,
  and the Emulator UI is used against it ([`docs/compatibility.md`](../compatibility.md));
  `tools/oracle/functions/` runs Firestore-triggered functions
  ([firebase-functions](https://github.com/firebase/firebase-functions), MIT) under it and under
  the official emulator.

If code from one of these projects were ever needed in hidane, it would come in as third-party
code under its own license, with its notices, and the pull request would say so, as for the
vendored protos ([`docs/cla/README.md`](../cla/README.md#what-the-cla-does-not-cover)).

## 4. Black-box observation of the official emulator

- **What**: the official Cloud Firestore emulator, `cloud-firestore-emulator` v1.22.0 (jar
  SHA-256 `9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c`), the release
  firebase-tools pins, run as a program and talked to from the outside.
- **What hidane takes**: its answers over gRPC, REST, WebChannel, the WebSocket feed on
  `--websocket_port` and its own endpoints (`/emulator/v1/…`, `/reset`, `:securityRules`,
  `:ruleCoverage`); what it prints while it runs (`debug()` output, warnings); the files it writes
  (exports); its start-up output, exit codes, timing and memory; its `--help` output and what it
  publishes while it runs (the services gRPC server reflection lists). Nothing inside the jar.
- **How**: [OBSERVATION_POLICY.md](OBSERVATION_POLICY.md). The scripts that record it are in
  [`tools/oracle/`](../../tools/oracle/), and what each recording says about its provenance is in
  [ORACLE_PROVENANCE.md](ORACLE_PROVENANCE.md). The differential fuzzer
  ([`tools/fuzz/`](../../tools/fuzz/)) observes it the same way: it sends generated REST
  requests to it and to hidane and compares the answers, and what it finds becomes a recording
  only through a recorder, after a person has looked at it.
- **Used by**: every recording in `crates/*/tests/fixtures/` and `results/`, replayed or compared
  by the tests and listed in [`docs/parity.md`](../parity.md); the baseline in
  `results/official-v1.22.0-baseline.md`.

## 5. CEL's specification and conformance tests

- **What**: [google/cel-spec](https://github.com/google/cel-spec), Apache-2.0: the language
  definition and the conformance tests in `tests/simple/testdata`, tag v0.25.3.
- **What hidane takes**: CEL's semantics where the Rules language follows CEL (how `&&` and `||`
  absorb errors, operators, conversions), and test cases. The official emulator decides where
  Rules differs from CEL ([ADR 0005](../adr/0005-rules-engine.md)).
- **Used by**: `spikes/rules-eval/extract_cel_spec.py`, which keeps the 580 cases that only use
  what Rules has (`spikes/rules-eval/cases/cel-spec.json`); the official emulator's decision on
  each of them in `results/rules-conditions-official-v1.22.0.json`, which
  `crates/hidane-rules/tests/conditions.rs` replays. The cases the official emulator rejects or
  evaluates differently are kept as Rules expectations.

## 6. Other public specifications and inputs

| Source | What hidane takes from it | Where |
|---|---|---|
| [LevelDB's log format](https://github.com/google/leveldb/blob/main/doc/log_format.md) (BSD-3-Clause) | The record framing of export files | `crates/hidane-core/src/export/log.rs`, [`docs/export-format.md`](../export-format.md) |
| Published protocol and format specifications: the protobuf encoding and ProtoJSON, gRPC over HTTP/2, HTTP/1.1, WebSocket (RFC 6455), JSON (RFC 8259), RFC 3339 timestamps, CloudEvents' HTTP binding | The wire formats the emulator's interfaces use | `crates/hidane-proto/`, `crates/hidane/src/` |
| The Java platform's specified behaviour, where the official emulator's answers depend on it: `String.hashCode` and `Double.toString` in the Java SE API specification | The order a field mask's fields come back in and the order triggers receive events (checked against `field_order.json` and `function_events.json`), how REST and WebChannel print doubles (checked against `rest.json` and others) | `crates/hidane-core/src/hash_order.rs`, `crates/hidane/src/rest.rs` |
| Rulesets of [firebase/quickstart-testing](https://github.com/firebase/quickstart-testing) (Apache-2.0) | Inputs for compiling rules, each with its source URL | `crates/hidane/tests/fixtures/rules_compile.json` |
| [googleapis/conformance-tests](https://github.com/googleapis/conformance-tests) (Apache-2.0), `firestore/v1` | The request shapes the client libraries' shared tests send | Planned (#44) |
| Testcontainers' Firestore modules: [testcontainers-java](https://github.com/testcontainers/testcontainers-java) `modules/gcloud`, [testcontainers-node](https://github.com/testcontainers/testcontainers-node) `packages/modules/gcloud`, [testcontainers-go](https://github.com/testcontainers/testcontainers-go) `modules/gcloud/firestore`, [testcontainers-dotnet](https://github.com/testcontainers/testcontainers-dotnet) `src/Testcontainers.Firestore` (MIT); and the reference of [`gcloud emulators firestore start`](https://cloud.google.com/sdk/gcloud/reference/emulators/firestore/start) | The command, port and wait strategy each module starts an emulator image with, and the flags of the gcloud command they run, which hidane reads in its image | `crates/hidane/src/gcloud.rs`, [`docs/testcontainers.md`](../testcontainers.md), `testcontainers/` |
| firebase-tools' public issue tracker and release notes | Known bugs of the official emulator and their status (#6252, #11241, #3477 and others) | [`docs/parity-exceptions.md`](../parity-exceptions.md), [`docs/why.md`](../why.md) |
| The public metadata of package registries: npm's dist-tags and version documents, PyPI's JSON API, the Go module proxy, Maven Central's `maven-metadata.xml`, pub.dev's API | Which releases exist of the packages hidane is verified against, on which channel, and the firebase-tools commit each of its releases was published from (`gitHead`); version numbers only, no behaviour | `tools/upstream/watch.py` ([`tools/upstream/README.md`](../../tools/upstream/README.md)) |
| Production Cloud Firestore and the production Rules API (`projects:test`), observed from the outside with a project of the maintainers' own | Production behaviour, where the documentation does not settle it (#82); the rules of [OBSERVATION_POLICY.md](OBSERVATION_POLICY.md#observing-production) apply | Cloud Firestore: [`tools/production/`](../../tools/production/README.md) and the Production parity workflow, whose findings are workflow artifacts. The Rules API: planned |

## 7. Before this policy

Before 2026-10-13 the jar's contents were inspected statically, without a decompiler or a
disassembler: its entries were listed and `strings` run over files unpacked from it in Phase 0
research, and on 2026-10-10 the field names of the emulator's import and export requests were read
from the string constants of its classes
([OBSERVATION_POLICY.md](OBSERVATION_POLICY.md#history-before-this-policy)). Those outputs informed
internal research notes, and through them the roadmap and the issues. The kinds of facts that may
have come from them:

| Kind of fact | Examples | Where the same kind of fact can also be seen |
|---|---|---|
| Flag and environment variable names | The emulator's command-line flags (`--host`, `--port`, `--websocket_port`, `--seed_from_export`, `--functions_emulator` and the others `crates/hidane/src/cli.rs` accepts) | The emulator's `--help`; the flags firebase-tools passes, in its public source ([ADR 0006](../adr/0006-launch-under-firebase-tools.md)) |
| Endpoint and service names | The paths of the emulator's own endpoints (`/emulator/v1/…`), the gRPC services it serves | Its answers to requests; gRPC server reflection; firebase-tools' public source |
| Message and field names | The fields of the emulator's import and export requests (`ImportDataRequest`, `ExportDataRequest`: `database`, `export_directory`, `export_name`), read from the jar on 2026-10-10 | The export request firebase-tools sends, in its public source ([`docs/export-format.md`](../export-format.md)); the emulator's error messages, which name some fields |
| Names in the WebChannel server | Phase 0 `strings` output of its WebChannel server classes | firebase-js-sdk's public source and the wire captured between it and the emulator ([`docs/webchannel.md`](../webchannel.md)) |

Most of these facts can also be observed at runtime or read in firebase-tools' public source, as
the last column says. The project has not re-verified each of them from those sources.
