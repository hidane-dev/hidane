# Observing the official emulator

hidane learns the official emulator's behaviour from the outside: a script sends it requests and
records its answers, and hidane's tests replay the recording. This page says how the emulator is
observed, what a recording may hold, and how a difference between the two is decided.

## What is observed

The official Cloud Firestore emulator, `cloud-firestore-emulator` v1.22.0, the release
firebase-tools pins (`src/emulator/downloadableEmulatorInfo.json`: its version and its jar's
SHA-256, `9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c`), downloaded from the
URL firebase-tools downloads it from and checked against that SHA-256
([`.github/actions/official-emulator/pinned.sh`](../../.github/actions/official-emulator/pinned.sh)).

It is observed only as a running program, through what it offers any user:

- its answers over gRPC, REST and WebChannel on its port, over the WebSocket feed on
  `--websocket_port`, and on its own endpoints (`/emulator/v1/…`, `/reset`, `/shutdown`,
  `:securityRules`, `:ruleCoverage`);
- what it prints to standard output while it runs (`debug()` values, warnings, start-up lines),
  as firebase-tools writes it to `firestore-debug.log`;
- the files it writes (exports), and the exit code and output of a start;
- how long it takes and how much memory it uses.

Nothing inside the jar is used. It is not decompiled, disassembled or patched, its classes and
resources are not read, and no debugger, profiler or agent is attached to read its internals.
The one exception is its license notice (`FIRESTORE_EMULATOR_LICENSES`), read only to learn the
terms it comes under ([`docs/ROADMAP.md`](../ROADMAP.md), "The jar grants no redistribution
rights").

## How it is observed

| Rule | Why |
|---|---|
| Every recording is made by a script in [`tools/oracle/`](../../tools/oracle/), committed, which anyone can run again; its README says which script writes which recording and which test replays it | A recording can be checked and remade by someone else |
| A freshly started jar for each recording (a second port is fine) | Earlier requests change later answers: documents already written turn creates into `409`s, and transaction IDs are counted per database |
| The jar runs under an English locale: `java -Duser.language=en -Duser.country=US -jar …` | Its messages for Security Rules warnings and evaluation errors exist in English only; under another locale each becomes a `500 UNKNOWN` |
| It listens on `127.0.0.1` only | It has no authentication |
| Synthetic data only: project IDs the script chooses (`demo-…`, or a name per case), documents the script writes, unsigned mock tokens (`alg: none`) and `Bearer owner`, `apiKey: 'demo'` | A recording is published; it must hold nothing about real users, projects or systems |
| No credentials: no service account, API key, OAuth token or real Firebase project is used or recorded | Same |
| What changes from run to run is masked or replaced by placeholders (`{project}`, `<time>`, `<id>`, `<token>`, `{dir}`) | Recordings stay the same across runs, and hold no path of the machine they were made on |
| The jar is never committed or redistributed. `.gitignore` excludes `*.jar` and `/research/`, where local copies and notes stay; CI downloads it from Google's URL at the pinned SHA-256 and caches it outside the workspace ([`tools/oracle/README.md`](../../tools/oracle/README.md#the-official-emulator-in-ci)) | Its terms grant no right to redistribute it |

The Official emulator workflow (`.github/workflows/official-emulator.yml`) re-records a set of
fixtures against a freshly downloaded jar every week and fails when one differs from the
committed recording by a byte, so the recordings stay reproducible.

## What a recording may hold

- The requests the script sent (method, path, body, the headers that matter) and the answers it
  received: status or gRPC code, headers, body or message, stream frames, as they came or as a
  summary the replaying test compares (document names, counts, the order of fields).
- What the emulator printed for a request (`debug()` lines, `WARNING` lines), next to that request.
- The files the emulator wrote from the script's data (exports, in base64).
- A header naming the emulator and the script ([ORACLE_PROVENANCE.md](ORACLE_PROVENANCE.md)).
- Inputs from allowed sources, such as rulesets from the Firebase documentation, with their source
  ([SOURCES.md](SOURCES.md)).

It must not hold:

- credentials, real project IDs, personal data, or anything from a real application;
- paths, user names or other details of the machine it was made on;
- anything taken from inside the jar;
- more of a page, script or stylesheet the emulator serves than the replaying test compares. Such
  assets are the emulator's own work, not answers about the request. Known exception:
  `crates/hidane/tests/fixtures/rules_coverage.json` keeps the official `:ruleCoverage.html`
  page (and the HTML page of a `500`) whole, while its test compares only the page's data
  (`const data = …`); it is to be cut down to that data when it is next recorded.

## How a difference is decided

hidane follows the official emulator. An answer that differs from the recorded one fails the
tests, and is fixed in hidane. hidane differs on purpose only where a row of
[`docs/parity-exceptions.md`](../parity-exceptions.md) says why, for one of these reasons:

- **A bug of the official emulator** (an internal error, a request left unanswered, data lost, a
  leak, a slowdown, a message that misleads). hidane then follows production Cloud Firestore,
  where its behaviour is documented or known, and otherwise answers as the emulator answers a
  similar request.
- **A cost hidane will not carry**: shipping the emulator's own work (its coverage page), keeping
  memory without bound, or holding what a Rust string cannot (half a surrogate pair). hidane then
  does the closest thing that avoids it.
- **A detail no client can rely on**, where hidane's design gives another answer that clients
  read the same way: the wording of a transport error, how a response is chunked, the contents
  of an opaque token.
- **A choice between the official emulator and production**, where they differ and neither is a
  bug, made once and recorded with the decision.

[`docs/parity-exceptions.md`](../parity-exceptions.md) is the database of these differences. Its
tables:

| Table | Columns | What a row records |
|---|---|---|
| Intentional differences | **Official emulator behaviour**: what the official emulator does, as recorded, often with the issue that reports it. **hidane**: what hidane does instead. **Why**: the reason, usually "Bug", or the cost the official behaviour would carry | A difference hidane keeps on purpose. A recording marks the answers it covers (`differs`, a `compare` of `status`, `codes` or `none`) or the test lists them (`KNOWN`, `FIRST_LINE_ONLY`, `CRASHES`); the tests compare those answers as marked, and [`docs/parity.md`](../parity.md) counts them by how they are compared |
| Official-emulator behaviour that differs from production | **Area**. **Official emulator**: its behaviour. **Production Firestore**: what production does, from the documentation. **Decision**: which one hidane follows, and the recording or issue behind it | A choice between the two parity targets |
| Not in scope | (a list) | What neither emulator offers and hidane does not either |

A new difference gets its row in the pull request that introduces it, and the pull request's
"Parity impact" section says so.
