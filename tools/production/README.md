# Production parity

hidane follows the official Cloud Firestore emulator, and where the emulator has a bug, it
follows production Cloud Firestore ([OBSERVATION_POLICY.md](../../docs/clean-room/OBSERVATION_POLICY.md#how-a-difference-is-decided)).
What production does was known only from its documentation. These tools observe it from the
outside, in a project of the maintainers' own (#82), under the rules of
[Observing production](../../docs/clean-room/OBSERVATION_POLICY.md#observing-production).

## `replay.py`

[`replay.py`](replay.py) replays recordings of the official emulator against production. Each
recording's cases are sent in order, as hidane's tests send them, with an OAuth access token in
place of the emulator's `Bearer owner`. A case is sent only if it is for the project's
`(default)` database and names no other project. Each answer is masked as the recordings are
([`tools/oracle/harness.py`](../oracle/harness.py)) and compared with the official emulator's
recorded answer:

- the status;
- the media type;
- the body, where the recording compares it, as JSON. Production prints JSON its own way (a
  double without `.0`, so numbers compare by value), and a recording's placeholders (`<time>`, `<now>` and the like) match any value, alone or within a
  string (`the stored version (<version>) does not match ...`). Where an error says at which
  character of a resource name it went wrong (`lacks "/" at index 53`), the index is not
  compared: it counts the characters of a project ID that is not the recording's.

A query production refuses for want of a composite index, which the emulators never ask for
([D070](../../docs/parity-exceptions.md)), is counted apart as needing an index rather than
compared. The link production gives to create the index is masked: it names the project in
base64.

A recording's uses of the official emulator's own delete (`DELETE /emulator/v1/…/documents`, of
the whole database or of a document or collection and what is below it) are not sent: the
replay deletes the same documents in production, so the cases after it see the same data.

The database is emptied before each recording and at the end. What differs goes to `report.json`
and `report.md` in `--out`, and to the job summary in GitHub Actions, with where each answer
first differs from the official emulator's. A run stops at `--budget`
requests (5,000 by default), within Firestore's free daily quota.

```sh
PRODUCTION_ACCESS_TOKEN=… python3 -I -B tools/production/replay.py --project <id> \
    --out production-parity crates/hidane/tests/fixtures/rest.json
```

Recordings it reads: those in the format of `rest.json`, a list of cases with a method, path,
headers, body and outcome. `rest.json` and `conformance.json` are replayed nightly. Recordings
that need composite indexes in production (`queries.json`, `aggregations.json`) and the Rules
API (`projects.test`) come later.

[`test_replay.py`](test_replay.py) tests it offline, against a stand-in for production on
`127.0.0.1`; CI runs it.

## The Production parity workflow

[`.github/workflows/production-parity.yml`](../../.github/workflows/production-parity.yml) runs
nightly and on demand, on `main` only. It signs in through Workload Identity Federation as a
service account that may read and write Firestore and test rulesets, with the project, the
provider and the service account named by the variables of the `production-parity`
environment. No key exists. The report is the run's `production-parity` artifact, kept 30
days.

A difference it reports is a lead, not a fact about hidane: production can differ from the
official emulator by design. A person reads it, and where it settles how production answers, it
goes into the production column of [`docs/parity-exceptions.md`](../../docs/parity-exceptions.md)
with the run that showed it.
