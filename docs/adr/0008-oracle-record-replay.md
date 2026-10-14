# ADR 0008: Recording the official emulator and replaying it against hidane

- **Status**: Accepted
- **Date**: 2026-10-14
- **Issue**: #41

## Context

hidane's claim is that a test suite cannot tell it from the official Cloud Firestore emulator
(v1.22.0, the release firebase-tools 15.33.0 pins). That claim needs evidence that anyone can
check and that CI checks on every change. Four constraints shape how the evidence is made:

- The official jar is a black box. It may be run and observed over the wire, never unpacked,
  read or decompiled ([`clean-room/OBSERVATION_POLICY.md`](../clean-room/OBSERVATION_POLICY.md)),
  and it may not be redistributed.
- CI runs on every pull request, often without Java, and must not depend on downloading the jar
  to decide whether hidane is right.
- The questions differ in shape. Some are a request and its answer (most of REST and gRPC). Some
  are sequences with timing and concurrency (transaction locks, the Write and Listen streams,
  WebChannel sessions). Some need a counterpart (a Functions emulator that records events, a
  directory the jar exports to, the jar's standard output for `debug()` and coverage). Some are
  only visible through an SDK (snapshots, retries, offline and resume).
- A recording has to say where it came from, so that a new official release or a doubt about one
  answer leads to the files to make again.

Issue #41 asked for a case format (operations → expectations), normalisation rules, tiers,
tags and a result schema. Most of that has been built since, piece by piece. This ADR records how
the pieces fit and decides the one question still open: whether the recorders should share a
single declarative case format.

## Options

**A. One declarative case format and one runner.** Every case is data (operations, then
expectations), as in paritysuite/dynamodb-conformance. One runner sends the cases to either
emulator and compares.

- One format to learn and review; a case can run against any target, including production
  Firestore (#82).
- The questions above do not fit one format without it growing into a programming language:
  concurrent steps with timing, choosing the next request from an answer (the Security Rules
  recorders do), a recording server for Functions events, exports compared byte for byte.
- Replaying needs the runner to speak every transport hidane serves, outside hidane's own tests.

**B. A recorder per question, a fixture per recorder, a test per fixture.** A script asks the
official emulator one question over the wire and writes what it answered to a committed JSON
fixture. A Rust test replays the fixture against hidane, in process or over its servers, and
compares at a stated level. A manifest lists every recording and its provenance.

- Each recorder takes the shape its question needs, in the Python standard library (or Node.js
  for SDK transcripts), observing only the wire.
- Replaying needs no Java: CI runs it on every pull request, and a failing replay names the case.
- Each fixture's shape is its own, so a reviewer reads each recorder's README row and header.

**C. Live differential testing only.** Run both emulators in CI and compare on every change.

- Nothing to re-record; finds what nobody wrote down.
- Needs the jar and Java in every run, is slower, and flaky wherever the official emulator is
  (#210). A difference found has no recorded expectation to fix against.

## Decision

**B**, with **C** where a recording cannot do the job. There is no single shared case format:
the shared layer is the manifest, the masking rules and the comparison levels, not the fixture
shape.

1. **Recorders.** One script per question in `tools/oracle/`. Each runs against a freshly
   started official emulator (English locale, a fresh data directory) and writes a fixture under
   `crates/*/tests/fixtures/`. The table in [`tools/oracle/README.md`](../../tools/oracle/README.md)
   gives the question, the fixture and the test for each. SDK transcripts are recorded the same
   way through a pinned SDK, once per emulator, in `results/`.
2. **Normalisation.** What changes from run to run is masked when recorded, and the replay
   applies the same mask: the project ID, server times, transaction IDs, page tokens and generated
   document IDs (`harness.py`'s `mask`, which `crates/hidane/tests/rest.rs` mirrors). Recorders
   whose answers carry other run-dependent values (resume tokens, session and stream IDs) replace
   them with placeholders, as their headers say.
   Listen sequences are compared event by event where hidane answers as the official emulator
   does. Where it answers as production does (changes per commit rather than `RESET` and the
   whole result, D-rows in [`parity-exceptions.md`](../parity-exceptions.md)), they are compared
   through SDK transcripts, as the snapshots a client builds.
3. **Comparison levels and tags.** Each fixture is registered in `RECORDINGS` of
   `tools/parity/table.py` with its area, the transport its replay reaches hidane through, the
   test functions that replay it and the level each compares at: in full, decision and reasons,
   in part, status only, without messages, not compared, intentionally different. These take the
   place of the "tiers 1–3" #41 proposed. The table is published as
   [`docs/parity.md`](../parity.md), and CI fails while a fixture is unregistered or the table is
   stale.
4. **Known divergences.** A difference kept on purpose is a row of
   [`docs/parity-exceptions.json`](../parity-exceptions.json): a D-number, its kind
   (bug-for-bug, more-correct, equivalent, limitation), its area and its references.
   `tools/parity/differences.py` checks the rows in CI. The fuzzer's accepted differences name
   them, and the parity table counts them. #45 adds a link from every row to the test or fixture
   that pins it.
5. **Provenance and schemas.** `tools/oracle/corpus.py` writes the manifest,
   [`docs/oracle-corpus.json`](../oracle-corpus.json) (schema `docs/oracle-corpus.schema.json`):
   for each recording, its script, client, the official release and how that is known, when it
   was recorded, its SHA-256 and how it is compared ([`clean-room/ORACLE_PROVENANCE.md`](../clean-room/ORACLE_PROVENANCE.md)).
   The results for users are `docs/compatibility.json` and `docs/sdk-certification.json`, each
   with its schema.
6. **Live checks where recordings fall short.** The weekly drift job re-records the
   deterministic recorders against a fresh official emulator and fails on any byte of
   difference. It also checks whether firebase-tools pins a newer release. The daily
   differential fuzzer compares generated requests on both emulators and writes each unexplained
   difference minimised in `rest.py`'s fixture format, for a person to turn into a recorder's case
   ([`tools/fuzz/README.md`](../../tools/fuzz/README.md)). firebase-js-sdk's own browser suite
   runs against both on a schedule, and its outcomes are compared test by test.

## Consequences

- Adding a behaviour means four things in one pull request: a case in a recorder (or a new
  recorder), the fixture recorded against the pinned jar, a replay test that fails before the
  fix, and the fixture in `RECORDINGS`. `corpus.py` and the generated docs follow.
- A new official release changes the `oracle` constant in every recorder and `pinned.sh`
  together, and every recording is made again. The drift job's run against the new release
  shows which answers change first.
- An answer the official emulator gives differently from run to run cannot be a byte-compared
  fixture. Its recorder stays out of the drift job, and the answer is decided in an issue (#210).
- Each fixture's shape stays its own. Reviewers rely on the recorder's README row and header, and
  on `docs/parity.md`'s "What each test counts and compares".
- Running the cases against production Firestore (#82) would need a runner of its own for the
  recorders whose questions production can answer. That is out of scope here.
