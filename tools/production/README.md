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
- a case about such a difference, by its name (`KNOWN_CASES`);
- a test of rules whose ruleset calls what the Rules API answers differently (`KNOWN_RULES`):
  `debug()`, which it does not have (D113), or whose file it refuses: no content, or a name that
  is not text (D114).

The report counts the known cases, says how many each entry explains, and lists the rest. An
entry that settles a new difference adds its rule there.

A query production refuses for want of a composite index, which the emulators never ask for
([D070](../../docs/parity-exceptions.md)), is counted apart as needing an index rather than
compared. The report lists the index, read from the link production gives to create it, and
masks the link: it names the project in base64.

A recording's uses of the official emulator's own delete (`DELETE /emulator/v1/…/documents`, of
the whole database or of a document or collection and what is below it) are not sent: the
replay deletes the same documents in production, so the cases after it see the same data.

The database is emptied before each recording and at the end, with a query of every collection
(`allDescendants` without a collection ID), so that no document's name goes into a URL: production
serves no path with a NUL in it (D116). What differs goes to `report.json`
and `report.md` in `--out`, and to the job summary in GitHub Actions, with where each answer
first differs from the official emulator's. A run stops at `--budget`
requests to Firestore (10,000 by default), within its free daily quota; tests of rules are not
counted.

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
  Cases that set rules are not run (but with their rules deployed, below), nor steps the emulator
  refused for want of admin credentials (their recorder sent none);
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
- the recording of the order of fields (`field_order.json`), a list of cases of steps replayed as
  the recordings of steps are; the order of each answer's keys is compared too, and production's
  own order counts as known (D115);
- the recording of vector search (`find_nearest.json`): its seed is written, then its queries,
  aggregations and malformed vector values are sent as its recorder
  ([`tools/oracle/find_nearest.py`](../oracle/find_nearest.py)) sent them, and compared as it
  put them;
- the REST cases of `json_escapes.json`, as those of `rest.json`;
- the recording of what the official emulator ignores or refuses (`unsupported.json`): explain
  options, pipelines on a standard database and partitioned queries, each gRPC request in its
  REST form, compared as its recorder summed them up;
- the recording of what a listener's query targets hold (`listen_queries.json`), in
  [`listen.py`](listen.py): its dataset is written, and each query is added as the target of a
  Listen stream of its own over production's WebChannel, as its recorder
  ([`tools/oracle/listen_queries.py`](../oracle/listen_queries.py)) opened them, eight at a time.
  The documents the target holds when it becomes current, in the order sent, or why it was
  removed, are compared. Production's Listen checks targets on its own (D117), and
  `KNOWN_LISTEN` counts what the exceptions record as known;
- recordings of Security Rules conditions (`rules_eval.json`, in
  `crates/hidane-rules/tests/fixtures/`): each condition, in the recording's ruleset, or each whole
  ruleset, is tested with the Rules API (`projects.test`) for a `get` of `rooms/r1` without
  `auth`, and the outcome (allowed, denied, or not compiled) and the warnings are compared. A
  denial's message, the emulator's own, is not. Conditions that read the document, the request or
  other documents are not tested: the API takes the document as JSON, whose numbers are all
  floats, and runs lookups only on mocks. With a user's credentials, the Rules API needs a quota
  project: `--quota-project <id>`;
- recordings of how Security Rules compile, replayed by [`rules.py`](rules.py), each ruleset
  tested with the Rules API without a test suite, which compiles it:
  - `rules_compile.json` (in `crates/hidane/tests/fixtures/`), rulesets the official emulator
    loaded with its own `PUT /emulator/v1/projects/{p}:securityRules`
    ([`tools/oracle/rules_compile.py`](../oracle/rules_compile.py)). Where it refused one (`400`,
    "Error compiling rules:" and a line `L<line>:<column> <description>` for each issue),
    production's issues, one of which is an `ERROR`, are put in the same lines; where it compiled
    one, its warnings are compared with production's, positions and all. A case of the
    emulator's request is sent with the request's `rules` as the source, if they hold one file:
    the Rules API tests one, and refuses several ("Currently only one file source can be passed
    to firebase rules."). A request without files, or not JSON, is not sent;
  - `rules_warnings.json`, conditions and rulesets with the warnings the official emulator gave
    them, each `[line, column, currentOffset, endOffset, severity, description]`
    ([`rules_warnings.py`](../oracle/rules_warnings.py)), compared the same way;
- recordings of the documents `match` paths cover (`rules_paths.json`,
  [`rules_paths.py`](../oracle/rules_paths.py)): each ruleset is tested with a `get` of each of
  the recording's documents without `auth`, and whether each read is allowed, and the warnings,
  are compared.

The workflow replays all twenty-one nightly. Testing rules takes the permission
`firebaserules.rulesets.test`: the workflow's service account has it through a custom role that
holds that one permission (`rulesTester`). The Rules API takes seconds to compile each ruleset,
so tests are sent eight at a time, and at most 240 a minute: it allows a project 300.

## Rules deployed

`projects.test` does not show what rules decide about a request: conditions that read the
document, the request or other documents, and the requests of recordings of rules. With
`--deploy-rules`, the replay deploys those rules to the project's databases and sends the
requests:

- the recordings of rules whose steps are sent as the owner or without credentials
  (`rules_request.json`, `rules_writes.json`, `rules_verify.json`, `rules_lookups.json`,
  `rules_same_document.json`, `rules_denials.json`). The cases are put together by the ruleset
  they load; each ruleset is released, and each of its cases sent on the database emptied, as the
  recordings of steps are. A step the owner sent is sent with the token, which rules do not
  decide, as they do not decide the emulator's owner; a step sent without credentials is sent
  without. The emulator's load of the rules is compared, on its status, with whether production
  made the ruleset. A case that sends a user's token (`rules_list.json`, `rules_auth.json`) is
  not sent: production takes only tokens Firebase Authentication signed, and the recordings'
  tokens are not signed. Nor is a case that uses the emulator's own API but the load. A denial's
  message is the emulator's own (D118);
- the conditions of `rules_eval.json` that read: `rooms/r1` and `rooms/r2` are written as the
  recorder wrote them, and each condition decides a read of `rooms/r1` without credentials,
  allowed or denied. Conditions that do not depend on where the document is are deployed in one
  ruleset, each on a collection of its own that holds the same document; the others, and whole
  rulesets, one at a time. Whether each compiles, and its warnings, are compared with
  `projects.test`, as above.

A release decides requests seconds after it is made (from 1.3 to 2.2 s, measured on 2026-10-30),
and takes minutes to reach every server. The replay waits 5 s after each release; a case that
differs is sent again two minutes later, its ruleset released again, and differs only if it does
again. The report counts those that did not.

While it runs, the database is open as the rules deployed open it, to anyone. The project is
kept without a release of rules, so that it refuses every request without credentials, and
[`release.py`](release.py) leaves it so: the replay deletes the release and the rulesets it made
when it ends, whatever happened, and `release.py` does it again on its own. Each ruleset the
replay makes ends with the line `// hidane production parity`, so that those a stopped run left
are deleted too. A release of rules it did not make stops the run, and is left as it is. The
Rules API allows a project 72 requests a minute that make, change or delete rulesets and
releases: `release.py` sends at most 60, its reads of them included.

`--shard i/n` sends the i-th of n parts of what deploys rules, from 0: the rulesets and the
conditions are shared out by their SHA-256 digests.

```sh
PRODUCTION_ACCESS_TOKEN=… python3 -I -B tools/production/replay.py --project <id> --out production-rules \
    --deploy-rules --shard 0/7 crates/hidane/tests/fixtures/rules_request.json
PRODUCTION_ACCESS_TOKEN=… python3 -I -B tools/production/release.py --project <id>
```

[`test_replay.py`](test_replay.py) tests it offline, against a stand-in for production on
`127.0.0.1`; CI runs it.

## Datastore mode

A database has one mode, and the project's `(default)` is in Firestore Native mode. The
recordings in Datastore mode (`datastore_entities.json`, `datastore_queries.json`,
`datastore_gql.json`, `datastore_firestore_api.json`) go, with `--datastore-database <id>`, to a
named database of the project in Datastore mode, in [`datastore.py`](datastore.py). Each is sent
in order, after the database is emptied and the recording's dataset written, and compared as
above. Production does some things its own way, and the replay puts them as the recording put
the official emulator's:

- a request names its database in its body (`databaseId`) and in the routing header
  `x-goog-request-params`: without the header production refuses it ("Missing routing header"),
  without the body's it goes to the `(default)` database. Production's answers name the database
  in each key's `partitionId`, which the emulator's, of its default database, do not: it is taken
  out;
- a request of the Firestore API goes to the same database, and production's answer names
  `(default)` again;
- the IDs production allocates are not the emulator's: those of an answer to `allocateIds` or
  `commit` take the emulator's place in the requests that follow, and are read back as the
  emulator's in the answers;
- a query production refuses for want of a composite index is counted apart (D070), and the
  index it recommends is kept in the report (`datastoreIndexes`).

A request to another database or project (the recorders send some to projects of their own, to
allocate IDs from the start), or of the emulator's own API, is not sent. The database is
emptied with a query of no kind in each namespace `__namespace__` lists. A database in Datastore
mode other than `(default)` has no free quota: its requests count in `--budget`, and are billed.

```sh
PRODUCTION_ACCESS_TOKEN=… python3 -I -B tools/production/replay.py --project <id> --out production-datastore \
    --datastore-database datastore crates/hidane/tests/fixtures/datastore_entities.json
```

The database was made once, by a maintainer: `gcloud firestore databases create
--database=datastore --type=datastore-mode --location=us-central1 --project=<id>`.

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
service account that may read and write Firestore, test rulesets, and deploy them, with the
project, the provider and the service account named by the variables of the `production-parity`
environment. No key exists. Deploying rules takes `firebaserules.rulesets.create` and `delete`,
and `firebaserules.releases.create`, `update` and `delete`: the service account has them through
a custom role that holds those alone (`rulesDeployer`). The report is the run's
`production-parity` artifact, kept 30 days.

Its second job, after the first, replays with rules deployed a seventh of what deploys rules,
by the day of the week (a run on demand takes the seventh as its `rules-shard`), and takes the
rules down after its replay, whatever happened. Its report is the run's `production-rules`
artifact.

A third job replays the recordings in Datastore mode against the database `datastore`, beside the
others, as it empties only that database. Its report is the run's `production-datastore` artifact.

A difference it reports is a lead, not a fact about hidane: production can differ from the
official emulator by design. A person reads it, and where it settles how production answers, it
goes into the production column of [`docs/parity-exceptions.md`](../../docs/parity-exceptions.md)
with the run that showed it.
