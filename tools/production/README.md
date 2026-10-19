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
  double without `.0`, so numbers compare by value), and a recording's placeholders (`<time>`,
  `<now>` and the like) match any value, alone or within a string (`the stored version (<version>) does not match ...`). Where an error says at which
  character of a resource name it went wrong (`lacks "/" at index 53`), the index is not
  compared: it counts the characters of a project ID that is not the recording's.

A difference `docs/parity-exceptions.md` already records counts as known rather than as a
difference. The rules for this are in `replay.py`:

- a pair of messages (`KNOWN_MESSAGES`);
- a pair of errors (`KNOWN_ERRORS`);
- the shapes of an answer that D097 to D099 describe: `details` in an error, an error inside an
  array, no `done`;
- a case about such a difference, by its name (`KNOWN_CASES`).

The report counts the known cases, says how many each entry explains, and lists the rest. An
entry that settles a new difference adds its rule there.

A query production refuses for want of a composite index, which the emulators never ask for
([D070](../../docs/parity-exceptions.md)), is counted apart as needing an index rather than
compared. The report lists the index, read from the link production gives to create it, and
masks the link: it names the project in base64.

A recording's uses of the official emulator's own delete (`DELETE /emulator/v1/…/documents`, of
the whole database or of a document or collection and what is below it) are not sent: the
replay deletes the same documents in production, so the cases after it see the same data.

The database is emptied before each recording and at the end. What differs goes to `report.json`
and `report.md` in `--out`, and to the job summary in GitHub Actions, with where each answer
first differs from the official emulator's. A run stops at `--budget`
requests (10,000 by default), within Firestore's free daily quota.

```sh
PRODUCTION_ACCESS_TOKEN=… python3 -I -B tools/production/replay.py --project <id> \
    --out production-parity crates/hidane/tests/fixtures/rest.json
```

Recordings it reads:

- those in the format of `rest.json`, a list of cases with a method, path, headers, body and
  outcome (`rest.json`, `conformance.json`);
- recordings of queries (`queries.json`, `aggregations.json`): a dataset, then structured queries
  with their outcomes. The dataset is written, each query is run over REST as its recorder
  ([`tools/oracle/queries.py`](../oracle/queries.py), [`aggregations.py`](../oracle/aggregations.py))
  ran it, and the outcome compared as the recorder put it: the documents in order, what an
  offset skipped, a projection's fields, an aggregation's results, or the error's status and
  message. `done`, which production does not send (D099), is not compared.

- recordings of steps (`same_document_writes.json`, `document_writes.json`, `transforms.json`,
  `base_versions.json`): on the official emulator each case ran in a project of its own; here
  each runs on the database emptied. Every step's status and answer are compared, the times in it
  read against the case's commits (`<commit 2>`, `<near commit 2>`), and page tokens and IDs the
  server chose left out. A time a step sends near a commit is moved with production's commit,
  and a page token or a transaction an earlier answer handed out is replaced by production's.
  Cases that set rules are not run, nor steps the emulator refused for want of admin credentials
  (their recorder sent none);
- the recording of a document's limits (`limits.json`): each case's document, built from the
  recording's specs as its recorder ([`tools/oracle/limits.py`](../oracle/limits.py)) built it,
  is committed, and the status and the error's message compared;
- the recording of how values and document names are ordered (`value_order.json`, in
  `crates/hidane-core/tests/fixtures/`), redone as its recorder
  ([`tools/oracle/value_order.py`](../oracle/value_order.py)) did it: the values are written to
  two collections, named forwards in one and backwards in the other, and read back ordered by
  value, so that neighbours that swap places are equal; the document names are written and read
  back with a collection group query. The groups of equal values in order, the names in order,
  and the values and names refused, with their messages, are compared;
- the recording of ListDocuments (`list_documents.json`): its seed is written as its recorder
  wrote it, each gRPC request is sent as its REST form (`GET …/{parent}/{collection ID}` with
  the other fields in the query; a listing read at the first commit reads at production's), and
  each REST path as it is. A request without a collection ID, which lists every collection, is
  not sent: REST has no form of it. The documents listed, page by page where the recorder
  followed the pages, or the error, are compared;
- the read times of `read_time_and_databases.json`: a document is written, then each read is
  made at each age before now, gRPC reads in their REST form. Production keeps an hour of the
  database's history, the replays' before this one included, so the reads go to a collection of
  the run's own, and the collection IDs listed are its alone. Its named databases are not sent;
- recordings of Security Rules conditions (`rules_eval.json`, in
  `crates/hidane-rules/tests/fixtures/`): each condition, in the recording's ruleset, or each whole
  ruleset, is tested with the Rules API (`projects.test`) for a `get` of `rooms/r1` without
  `auth`, and the outcome (allowed, denied, or not compiled) and the warnings are compared. A
  denial's message, the emulator's own, is not. Conditions that read the document, the request or
  other documents are not tested: the API takes the document as JSON, whose numbers are all
  floats, and runs lookups only on mocks. With a user's credentials, the Rules API needs a quota
  project: `--quota-project <id>`.

The workflow replays all thirteen nightly. Testing rules takes the permission
`firebaserules.rulesets.test`: the workflow's service account has it through a custom role that
holds that one permission (`rulesTester`), so it may test rulesets but not deploy them. The Rules
API takes seconds to compile each ruleset, so tests are sent eight at a time, and at most 240 a
minute: it allows a project 300.

[`test_replay.py`](test_replay.py) tests it offline, against a stand-in for production on
`127.0.0.1`; CI runs it.

## `indexes.py` and `indexes.json`

Production runs many of the recorded queries only with indexes the emulators never ask for:
composite ones, a single field's for collection group queries, vector indexes. A replay's report
lists those production asked for (`indexes`, read from the link or the gcloud command in its
error). [`indexes.json`](indexes.json) keeps them, in the format of `firestore.indexes.json`,
except that a field override lists only the indexes to add to the field's own; and
[`indexes.py`](indexes.py) adds to it and creates them:

```sh
python3 -I -B tools/production/indexes.py add production-parity/report.json
PRODUCTION_ACCESS_TOKEN=… python3 -I -B tools/production/indexes.py create --project <id> --wait
```

`create` creates what the project does not have and waits until it is ready. It takes the token
of a maintainer who may change indexes: the workflow's service account may only read and write
documents, so a new index waits for a maintainer, and its queries are counted as needing an
index until then.

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
