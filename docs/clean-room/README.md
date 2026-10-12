# Clean-room provenance

hidane reproduces the behaviour of a closed-source program, the official Cloud Firestore emulator,
which Google distributes as a jar. It does so without using that program's code: every behaviour
hidane implements comes from a source anyone can read or observe, and the project keeps a record
of which source it came from. This directory states the policy and how it is carried out.

| Document | What it says |
|---|---|
| [SOURCES.md](SOURCES.md) | The sources behaviour may come from, and where each is used |
| [OBSERVATION_POLICY.md](OBSERVATION_POLICY.md) | How the official emulator is observed, what a recording may hold, and how a difference is decided |
| [ORACLE_PROVENANCE.md](ORACLE_PROVENANCE.md) | What each recording says about where it came from |
| [CONTRIBUTOR_GUIDE.md](CONTRIBUTOR_GUIDE.md) | What contributors must not use, what to do if they have seen it, and how to cite a source in a pull request |

These pages describe the project's policy and practice. They are not legal advice, and they do
not decide any legal question.

## What clean-room means for hidane

The official emulator is a black box. hidane learns what it does by sending it requests and
recording its answers, and implements those answers from scratch in Rust. Nobody working on
hidane decompiles or disassembles the jar or reads the code and resources inside it, and nothing
from it (code, classes, resources, pages) is copied, ported or translated into hidane.

What the emulator does is one thing; what it is made of is another. hidane reproduces the first:
the messages on the wire, the order of fields, the text of an error, the bytes of an export, as
observed. It does not use the second.

## Why it matters

- **Contributors** know what they may look at and what they may not, and that work built on an
  allowed source will not have to be thrown away later.
- **Users** run and redistribute hidane under `MIT OR Apache-2.0`. The jar's terms grant no right
  to redistribute it, and hidane ships none of it: no jar, no class, no resource of it.
- **A future acquirer** (or anyone doing due diligence on the code) can trace each behaviour to
  the source it came from and to the recording or document that holds it. The
  [Contributor License Agreement](../cla/README.md) records who wrote the code and under which
  grant; these pages record where its behaviour came from.

## The rule

**Behaviour comes only from allowed sources** ([SOURCES.md](SOURCES.md)): public protobuf
definitions, public documentation, the public source and behaviour of the officially distributed
SDKs and firebase-tools, black-box observation of the official emulator, and public
specifications such as CEL's. A behaviour that cannot be traced to one of them is not implemented
until it can be. Sources that must never be used are listed in
[CONTRIBUTOR_GUIDE.md](CONTRIBUTOR_GUIDE.md).

## How each part of hidane traces back

| Part of hidane | Behaviour comes from | Where the record is |
|---|---|---|
| Wire messages and services (gRPC, and REST's paths and JSON) | The `google.firestore.v1` protobuf definitions from googleapis, vendored with one documented local change | `crates/hidane-proto/proto/` and its README |
| Documents, writes, field transforms, queries, aggregations, vector search, transactions | Firebase and Google Cloud documentation, the comments in the protos, and the official emulator's answers | `crates/hidane/tests/fixtures/*.json` ([`docs/parity.md`](../parity.md) lists them), `results/sdk-*.json` |
| Listen and Write streams | The protos and their comments, the SDKs' public source (what they send and expect), and the official emulator's answers; resuming a target follows production as the protos document it | `write_stream.json`, `rules_streams.json`, `results/sdk-listen-*.json`, `results/sdk-web-writes-*.json` |
| REST byte for byte, CORS, JSON escaping, limits | The official emulator's answers | `rest.json`, `cors.json`, `json_escapes.json`, `limits.json` |
| WebChannel | firebase-js-sdk's public source (the requests it makes) and the wire between it and the official emulator, captured from the outside | [`docs/webchannel.md`](../webchannel.md), `webchannel.json`, `tools/oracle/webchannel/` |
| Security Rules language and evaluation | The Firebase documentation of the Rules language, CEL's specification and conformance tests, and the official emulator's decisions, messages and coverage reports | [ADR 0005](../adr/0005-rules-engine.md), `crates/hidane-rules/tests/fixtures/*.json`, `crates/hidane/tests/fixtures/rules_*.json`, `results/rules-conditions-official-v1.22.0.json` |
| Export and import files | Exports the official emulator wrote, read byte by byte, and LevelDB's published log format | [`docs/export-format.md`](../export-format.md), `export_import.json` |
| Launching under firebase-tools, flags, process behaviour | firebase-tools' public source (how it starts and stops the emulator, what it passes) and the official emulator's start-up output and exit codes | [ADR 0006](../adr/0006-launch-under-firebase-tools.md), `export_import.json` (`startup`), `crates/hidane/src/cli.rs` |
| The Functions emulator's trigger registrations and the events sent to it | firebase-tools' public source (what its Functions emulator sends and how it reads events), the published `DocumentEventData` definition, and the official emulator's answers and events | `function_triggers.json`, `function_events.json`, `results/functions-triggers-*.json`, `crates/hidane/src/triggers/` |
| The Emulator UI's request monitor | The WebSocket feed the official emulator serves, observed | `websocket_requests.json` |
| Where hidane differs on purpose | A documented bug of the official emulator, with production behaviour followed where it is known, or a reason given | [`docs/parity-exceptions.md`](../parity-exceptions.md) |

Each recording names the script that made it and the official emulator it was made against
([ORACLE_PROVENANCE.md](ORACLE_PROVENANCE.md)); each pull request that adds or changes behaviour
says where the behaviour came from ([CONTRIBUTOR_GUIDE.md](CONTRIBUTOR_GUIDE.md)); and the test
suite replays every recording against hidane, so a behaviour with a recording behind it is checked
on every change ([`docs/parity.md`](../parity.md)).

Where the official emulator serves something that is its own work rather than an answer, hidane
writes its own instead: the `:ruleCoverage.html` page is hidane's page around the same data
([`docs/parity-exceptions.md`](../parity-exceptions.md)).
