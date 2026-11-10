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

A database refuses requests with 429 while it is written faster than it allows ("This database
has exceeded their maximum bandwidth for writes"), which the official emulator never does: the
replay waits and sends such a request again, a few times, before it takes the answer. So it does
a `:batchWrite` answered 200 with every write refused so (`commits.py`): none was written.

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
  and the values and names refused, with their messages, are compared. A value written to the
  first collection and refused by the second is reported, as it would read as equal to its
  neighbour; an array in an array, which production stores and the official emulator refuses
  (D119), is left out of the groups and counts as known;
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
- the recording of how a commit's writes are checked (`commit_checks.json`), in
  [`commits.py`](commits.py): each commit with problems is sent over REST `:commit` and
  `:batchWrite` as its recorder ([`tools/oracle/commit_checks.py`](../oracle/commit_checks.py))
  sent it, and compared as it summed the answer up: the first error's code and message (a failed
  precondition's code alone), the number of write results, or `:batchWrite`'s status for each
  write. Its gRPC `Commit` and Write stream are not sent, nor its writes to another database or
  project. The database is emptied after a write goes through, as each commit met an empty
  project. Where the official emulator refused an array in an array first (D119), or production
  refused a document over 1 MiB before the official emulator's first error (D075), production's
  answer counts as known;
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

- the recordings of rules whose steps are sent as the owner, without credentials, or, with
  `--id-tokens`, as a user (`rules_request.json`, `rules_writes.json`, `rules_verify.json`,
  `rules_lookups.json`, `rules_same_document.json`, `rules_denials.json`,
  `rules_expressions.json`, `rules_list.json`). The cases that load
  one ruleset are put together by it; each ruleset is released, and each of its cases sent on the
  database emptied, as the recordings of steps are. A case that loads two
  (`rules_expressions.json`: a limit reached, then not) is sent on its own, each ruleset released
  as the case comes to its load. A step the owner sent is sent with the token, which rules do not
  decide, as they do not decide the emulator's owner; a step sent without credentials is sent
  without, and a user's with a real ID token for the user (below). The emulator's load of the
  rules is compared, on its status, with whether production made the ruleset. A case that sends a
  token no real one stands for is not sent, nor, without `--id-tokens`, one that sends a user's
  token. `rules_auth.json` stays out: each of its cases sends, besides tokens no real one stands
  for (expired, of another project, without a `sub`), a token of a `sub` alone and one of a full
  set of claims, and a real ID token is neither: it holds more than a `sub` (`user_id`, `aud`,
  `exp`), and its `aud` and `sign_in_provider` (`custom`) are not the recording's. What it could
  compare, conditions of `request.auth.uid` and of `sub`, `rules_list.json` does. Nor is a case
  that uses the emulator's own API but the load. A denial's message is the emulator's own (D118),
  and production puts a streamed method's in an array (D098). On the official emulator the cases
  of a recording ran one after another in one project, and a few rely on what an earlier case
  wrote: the documents their answers show created before
  their own first write are written first, and a case that rewrote such a document, or counted
  them, is not sent. A case that only deletes and writes documents as the owner, keeping none of
  its answers (`rules_expressions.json`'s `documents`), has what it left written before each case
  after it; of a read that began a transaction, that recording keeps the transaction alone, and
  production's answer is compared on whether it began one. Production will not begin a
  transaction without credentials (D127): such a
  case ends there. A verify of a missing document is given the commit's time by production, a
  time just before it by the emulator (D128). A condition of a long chain of members, which the
  emulator denies, fails in production (D134). A list without a page size shows the rules a limit
  of 30 in the emulator, none in production (D135), and production refuses a verify alone under a
  condition of `request.resource.data` that the emulator lets through (D136). A query production
  asks an index for is counted apart, and the index kept (`indexes.py`);
- the conditions of `rules_eval.json` that read: `rooms/r1` and `rooms/r2` are written as the
  recorder wrote them, and each condition decides a read of `rooms/r1` without credentials,
  allowed or denied. Conditions that do not depend on where the document is are deployed in one
  ruleset, each on a collection of its own that holds the same document; the others, and whole
  rulesets, one at a time. Whether each compiles, and its warnings, are compared with
  `projects.test`, as above.

A release decides requests seconds after it is made (from 1.3 to 2.2 s, measured on 2026-10-30),
and takes minutes to reach every server. The replay waits 5 s after each release; a case that
differs is sent again two minutes later, its rulesets released again. Sent again, a case comes
seconds after the release of another case's rules, which can still answer it: one that differs
again is sent a third time, each of its rulesets released again and given a minute, and differs
only if it does then (on 2026-11-10, nine cases of `rules_list.json` differed on both sends, and
each was the same or known given a minute). The report counts those that did not.

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

The recordings' tokens are not signed (`alg: none`), which the emulator takes and production does
not. With `--id-tokens`, [`id_tokens.py`](id_tokens.py) sends a recording's token whose payload is
a `sub` alone (or a `sub` and a `user_id` the same) as a real ID token for that uid: it signs a
custom token as the service account `--signer` with IAM's `signJwt`, and exchanges it with
Identity Toolkit's `accounts:signInWithCustomToken`, which takes an API key of the project, in
`PRODUCTION_FIREBASE_API_KEY`, restricted to Identity Toolkit. The project's Identity Platform has
no sign-in method, so that only who may sign as the service account can sign in; signing takes
`iam.serviceAccounts.signJwt` on the service account itself, which a custom role holds alone
(`tokenSigner`). An ID token is kept for its hour, and made again five minutes before it ends.
Neither it nor the key is written anywhere.

```sh
PRODUCTION_ACCESS_TOKEN=… python3 -I -B tools/production/replay.py --project <id> --out production-rules \
    --deploy-rules --shard 0/7 crates/hidane/tests/fixtures/rules_request.json
PRODUCTION_ACCESS_TOKEN=… PRODUCTION_FIREBASE_API_KEY=… python3 -I -B tools/production/replay.py --project <id> \
    --out production-rules-users --deploy-rules --shard 0/7 --id-tokens --signer <service account> \
    crates/hidane/tests/fixtures/rules_list.json
PRODUCTION_ACCESS_TOKEN=… python3 -I -B tools/production/release.py --project <id>
```

[`test_replay.py`](test_replay.py) tests it offline, against a stand-in for production on
`127.0.0.1`; CI runs it.

## Datastore mode

A database has one mode, and the project's `(default)` is in Firestore Native mode. The
recordings in Datastore mode (`datastore_entities.json`, `datastore_queries.json`,
`datastore_gql.json`, `datastore_firestore_api.json`, `datastore_gql_errors.json`,
`datastore_sequences.json`, `datastore_protobuf.json`, `datastore_locks.json`, and which fault
of a request is reported, `datastore_checks.json` and `datastore_commit_checks.json`) go, with
`--datastore-database <id>`, to a named database of the project in Datastore mode, in
[`datastore.py`](datastore.py). The last two hold about 14,400 requests: the nightly workflow
sends a seventh of their cases a night, the day of the week's (`--datastore-shard i/7`, the
cases whose place is `i` modulo 7). A recording of `tools/oracle/datastore_*.py` is one, though it
may not say its mode, and what its `every_case` gives goes into each case. Each is sent in
order, after the database is emptied and the recording's dataset written, and compared as
above; a run of one character the recorder wrote `{repeat:<c>:<n>}` is written out. A recording
whose recorder sent each case to a project of its own after a commit (`seed`, as
`datastore_commit_checks.json`) has it before its first case, and again, the database emptied,
after each commit production did not refuse as a bad request; a case of steps of its own goes
from the seed, `{tx}` the transaction its last step to begin one began. Production does some things its
own way, and the replay puts them as the recording put
the official emulator's:

- a request names its database in its body (`databaseId`) and in the routing header
  `x-goog-request-params`: without the header production refuses it ("Missing routing header"),
  without the body's it goes to the `(default)` database. Production's answers name the database
  in each key's `partitionId`, which the emulator's, of its default database, do not: it is taken
  out, as is the database a message names with a key (`database_id: "<id>"` in a key printed as
  protobuf text, `<project>#<id>` in "mismatched databases within request");
- a request of the Firestore API goes to the same database, and production's answer names
  `(default)` again;
- the IDs production allocates are not the emulator's: those of an answer to `allocateIds` or
  `commit` take the emulator's place in the requests that follow, and are read back as the
  emulator's in the answers;
- a query production refuses for want of a composite index is counted apart (D070), and the
  indexes it recommends ("recommended index is", "One possible index to serve the query is", "The
  recommended indexes are") are kept in the report (`datastoreIndexes`), for `indexes.py` (below).
  An index of a reserved kind (`__x__`) production refuses to make, so a query that asks for one
  is compared, and its refusal is known (D132).

What D119 to D133 record of Datastore mode is set aside before an answer is compared, by the
rules of `datastore.py`, and counts as known: the fields only one side's answers have, where in
an answer (`KNOWN_FIELDS`, as `commitTime` and `indexUpdates` of a commit); values each side
gives (`KNOWN_VALUES`, as `moreResults`); answers that differ as a whole, an error on one side or
both (`KNOWN_ERRORS`); a batch production cut at 300 entities, or that gives an entity once where
the emulator gives it for each value of an array its query matched; an array whose values
excluded from indexes production gives last; and a lookup's results in production's order. The
cases about the rest are known by their names (`KNOWN_CASES` in `replay.py`).

The recorder of `datastore_sequences.json` sent each of its cases, steps in turn, to a project of
its own: the database is emptied before each, and a case with a request to another database, or
whose client gives up waiting for an answer, is not sent. A step names a time an earlier step
answered (`{time:<step>:<path>}`) and a transaction one began (`{tx}`, `{tx:<name>}`): production's
take their place, and a commit's time outside a transaction, which production does not answer
(D120), is its last mutation's, as it is to the emulator. The recorder labelled the official
emulator's times and versions `<t1>`, `<t2>`, ... in the order they first appear in the case's
answers: each of production's is put as the label in its place, as long as a label stands for
one time and a time for one label all along the case (`timed` in `datastore.py`). Production
gives some times its own way (D120, D121, `compared`): a lookup reads at the time of an earlier
write, and gives a missing entity that time as its version; the deletion of a key that is not
there answers the commit's version in a transaction; a query's snapshot version is its read
time; and `__kind__` and `__namespace__` list only what is stored. Those times are taken for the
labels, and count as known.

The recorder of `datastore_protobuf.json` sent REST in protobuf, as the Java and Python client
libraries send it over HTTP, and bodies of other types, each with its own `Content-Type`, to a
fixed project (`demo-protobuf`). Each request goes as recorded, to the project's path, its body
naming the database: a JSON object's `databaseId`, or the `database_id` of a message sent as
protobuf. One without a `Content-Type` is not sent: urllib would give it a form's. An answer in
protobuf is read as JSON gives it, an error as its `google.rpc.Status` with its code's name, the
keys `allocateIds` answers with production's project put as `demo-protobuf` and its IDs read back
as the emulator's; the recorder compared some answers by their status and media type alone
(`compare`), and so does the replay.

The recorder of `datastore_locks.json` timed a write to what another transaction had read, in
it or outside one, while the reader stayed in use (a lookup in it every `keep_alive_seconds`); it
sent its cases at once, each to a project of its own. They go in turn, the database emptied before
each: its steps in order, the reader kept in use while the write waits, and the case's
transactions rolled back after it. The write is sent once, and given 600 s to answer: sent again,
it would be another write, and production keeps one waiting up to 180 s (D130). How long the
write waited counts as the same within 5 s of the official emulator's.

A request to another database or project (the recorders send some to projects of their own, to
allocate IDs from the start), a GQL key that names the project (production reads it as one of
`(default)`, and GQL cannot name another database), or a request of the emulator's own API, is
not sent. The database is
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

The Datastore-mode database's indexes are kept apart, in
[`datastore_indexes.json`](datastore_indexes.json), as index.yaml has them (a kind, whether the
query has an ancestor, and the properties with their directions); `add` puts them there.
`create --datastore-database datastore` creates them through the Firestore Admin API, each as an
index of the kind's collection group in Datastore mode's API scope: of the collection group for a
query without an ancestor (`COLLECTION_GROUP`), of the tree below the ancestor for one with
(`COLLECTION_RECURSIVE`), `__key__` as `__name__`. A user's token needs `--quota-project`.

```sh
PRODUCTION_ACCESS_TOKEN=… python3 -I -B tools/production/indexes.py create --project <id> \
    --datastore-database datastore --wait
```

## The Production parity workflow

[`.github/workflows/production-parity.yml`](../../.github/workflows/production-parity.yml) runs
nightly and on demand, on `main` only. It signs in through Workload Identity Federation as a
service account that may read and write Firestore, test rulesets, and deploy them, with the
project, the provider and the service account named by the variables of the `production-parity`
environment. No key exists. Deploying rules takes `firebaserules.rulesets.create` and `delete`,
and `firebaserules.releases.create`, `update` and `delete`: the service account has them through
a custom role that holds those alone (`rulesDeployer`), and it may sign as itself for the ID
tokens (`tokenSigner`, above), with the API key in the environment's secret
`PRODUCTION_FIREBASE_API_KEY`. The report is the run's
`production-parity` artifact, kept 30 days.

Its second job, after the first, replays with rules deployed a seventh of what deploys rules,
by the day of the week (a run on demand takes the seventh as its `rules-shard`), and takes the
rules down after its replay, whatever happened. Its report is the run's `production-rules`
artifact.

A job after it does the same for `rules_list.json`, whose requests go as a user, with the same
seventh and `--id-tokens`. It signs in again, as the two together would outlast the token's hour.
Its report is the run's `production-rules-users` artifact.

Another job replays the recordings in Datastore mode against the database `datastore`, beside the
others, as it empties only that database. Its report is the run's `production-datastore` artifact.

A difference it reports is a lead, not a fact about hidane: production can differ from the
official emulator by design. A person reads it, and where it settles how production answers, it
goes into the production column of [`docs/parity-exceptions.md`](../../docs/parity-exceptions.md)
with the run that showed it.
