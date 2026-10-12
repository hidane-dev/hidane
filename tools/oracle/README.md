# Oracle scripts

Small scripts that ask the **official** Firestore emulator how it behaves, so hidane can
reproduce the answer. Each one writes a fixture that a hidane test reads; the fixture is
committed, the official jar is not (it may not be redistributed, see `docs/parity-exceptions.md`).

Python 3 standard library only; `harness.py` holds the shared request and recording helpers, and
`steps.py` records calls one at a time for the Security Rules scripts, which choose the next call
from an answer. Run them against a freshly started official emulator:

```sh
java -Duser.language=en -Duser.country=US -jar cloud-firestore-emulator-v1.22.0.jar --host 127.0.0.1 --port 8086 &
python3 -I tools/oracle/value_order.py 127.0.0.1:8086 > crates/hidane-core/tests/fixtures/value_order.json
curl -X POST http://127.0.0.1:8086/shutdown
```

Start the jar with an English locale, as above: its messages for Security Rules warnings and
evaluation errors exist in English only, and under another locale each of them becomes a
`500 UNKNOWN` (a `MissingResourceException` in its log).

| Script | Question | Fixture | Test |
|---|---|---|---|
| `value_order.py` + `value_order_cases.json` | How are values ordered, which values are equal, and how are document names ordered in a collection group? | `crates/hidane-core/tests/fixtures/value_order.json` | `crates/hidane-core/tests/official_order.rs` |
| `transforms.py` | How are field transforms applied (server time, increment, maximum / minimum, array union / remove), in which order, with which results? | `crates/hidane/tests/fixtures/transforms.json` | `crates/hidane/tests/transforms.rs` |
| `document_writes.py` | How do document reads and writes answer, including errors, preconditions, masks, paging and admin checks? | `crates/hidane/tests/fixtures/document_writes.json` | `crates/hidane/tests/documents.rs` |
| `field_order.py` | In which order do a document's fields come back after sets, masked updates, transforms and nested writes, and in which order do read masks, query projections and aggregations return them? (Written order; masked and transformed fields that change move to the end in the order of a Java `HashMap`) | `crates/hidane/tests/fixtures/field_order.json` | `crates/hidane/tests/field_order.rs` |
| `queries.py` | What does RunQuery return for every filter operator on every value type, composite filters, implicit ordering, cursors, offsets, projections, collection groups and queries over every collection, and with which errors? | `crates/hidane/tests/fixtures/queries.json` | `crates/hidane/tests/queries.rs` |
| `aggregations.py` | What do `count`, `sum` and `avg` return at the edges (integer overflow, NaN, infinities, precision, non-numbers, empty sets), how do they interact with `offset` / `limit`, and which aggregation lists are rejected? | `crates/hidane/tests/fixtures/aggregations.json` | `crates/hidane/tests/aggregations.rs` |
| `write_stream.mjs` | How does the Write stream answer: handshake, tokens, pipelined batches, empty requests, errors, resumption, locks? Node.js with `@grpc/grpc-js` and `@grpc/proto-loader`, run like the SDK scripts below with `crates/hidane-proto/proto/` as its second argument | `crates/hidane/tests/fixtures/write_stream.json` | `crates/hidane/tests/write_stream.rs` |
| `rest.py` | What does every REST binding answer, byte for byte: JSON layout, ProtoJSON values, streams as arrays, errors, 404s, lenient input? | `crates/hidane/tests/fixtures/rest.json` | `crates/hidane/tests/rest.rs` |
| `list_documents.py` | What does ListDocuments list without a collection ID, how do page tokens continue across collections, which checks come first, and what do REST paths with a trailing slash mean? Needs `grpcurl` on `PATH` | `crates/hidane/tests/fixtures/list_documents.json` | `crates/hidane/tests/list_documents.rs` |
| `webchannel.py` | How does the browser SDK's transport behave: handshake, back channel (streaming and long polling), forward channel, stream errors, terminate, unknown sessions? Speaks the protocol directly, no browser | `crates/hidane/tests/fixtures/webchannel.json` | `crates/hidane/tests/webchannel.rs` |
| `limits.py` | Which document limits are enforced (value, name, ID, depth, size), with which messages, in which order? | `crates/hidane/tests/fixtures/limits.json` | `crates/hidane/tests/limits.rs` |
| `grpc_settings.py` | Which gRPC services answer, is v1beta1 the same service as v1, and how large may gRPC and REST requests be? Needs `grpcurl` on `PATH`; sends requests of up to 105 MB | `crates/hidane/tests/fixtures/grpc_settings.json` | `crates/hidane/tests/grpc_settings.rs` |
| `find_nearest.py` | How does `find_nearest` rank, filter, tie-break and validate, how does it combine with aggregations, and which vector values can be stored? | `crates/hidane/tests/fixtures/find_nearest.json` | `crates/hidane/tests/find_nearest.rs` |
| `unsupported.py` | What does the official emulator ignore or refuse: `explain_options`, ExecutePipeline on a standard database, PartitionQuery? Needs `grpcurl` on `PATH` | `crates/hidane/tests/fixtures/unsupported.json` | `crates/hidane/tests/unsupported.rs` |
| `auth.py` | How is the `Authorization` header read: administrators, users and anonymous callers, which tokens fail and how, and where each RPC, stream and REST endpoint reads it among its other checks? Needs `grpcurl` on `PATH` | `crates/hidane/tests/fixtures/auth.json` | `crates/hidane/tests/auth.rs` |
| `cors.py` | How does every HTTP path answer CORS: `Origin` reflection, credentials, methods, preflights on any path, requested headers, Private Network Access? | `crates/hidane/tests/fixtures/cors.json` | `crates/hidane/tests/cors.rs` |
| `export_import.py` | What do exports hold, byte for byte (LevelDB logs of App Engine entities), what does an import write where, how do `:export` and `:import` answer bad requests and damaged files, when does `--seed_from_export` seed which database, and how does a managed export's layout import? Takes the jar and `java` as arguments to start emulators of its own | `crates/hidane/tests/fixtures/export_import.json` | `crates/hidane/tests/export_import.rs` |
| `rules_compile.py` | How does `PUT :securityRules` answer: which rulesets compile, with which warnings (`issues[]` with positions) or errors (`400` "Error compiling rules:" with `L<line>:<col>` lines), for the samples of the Firebase documentation and firebase/quickstart-testing (Apache-2.0, sources in the fixture) and cases for versions, paths, statements, functions, conditions, lexical rules and several files? To record again, pass the fixture's corpus entries (those with a `source`) back as the corpus | `crates/hidane/tests/fixtures/rules_compile.json` | `crates/hidane/tests/rules_compile.rs` |
| `rules_paths.py` | Which documents does a `match` path cover in rules versions 1 and 2 (single and recursive wildcards, globs in the middle, nested matches), and what do the wildcards hold (`is path`, segments)? | `crates/hidane-rules/tests/fixtures/rules_paths.json` | `crates/hidane-rules/tests/paths.rs` |
| `rules_request.py` | What do `request` and `resource` hold for a get, lists, queries under a document and of collection groups, a create with and without a document ID, an update with and without a mask, transforms and a delete (method, path, `resource.data`, `request.resource.data` after merges and transforms, `request.query`, `inTransaction`, `fields`, `writeFields`, `transforms`, `autoId`, and the keys `request` does not have)? One probe condition per load, allowed or denied | `crates/hidane/tests/fixtures/rules_request.json` | `crates/hidane/tests/rules_server.rs` |
| `rules_request_fields.py` | What does `request` hold, key by key and in which order, for gets, batchGets, lists, queries of every shape, aggregations, reads and a write in a transaction, creates with and without a document ID, upserts, updates, deletes and batches (`debug(request)` and the coverage report's values of `request`), and what are `writeFields` and `transforms` for writes with update masks and transforms? Takes the file the jar's standard output goes to, as `rules_coverage.py` does | `crates/hidane/tests/fixtures/rules_request_fields.json` | `crates/hidane/tests/rules_coverage.rs` |
| `rules_streams.py` | How do the Listen and Write streams answer under rules: a denied target (`ADD`, then `REMOVE` with a `PERMISSION_DENIED` cause), a target of documents with one denied, a denied query, a listened document that a write makes denied, a denied write on the stream? Needs `grpcurl`; uses the repository's protos | `crates/hidane/tests/fixtures/rules_streams.json` | (#63, #67) |
| `rules_writes.py` | Which `allow` method decides each kind of write (by whether the document exists, or by an `exists` precondition; deletes always `delete`), and how do a batchGet with a denied document, a read in a transaction, an aggregation, a list and listCollectionIds answer under rules? | `crates/hidane/tests/fixtures/rules_writes.json` | `crates/hidane/tests/rules_server.rs` |
| `rules_lookups.py` | How do `get`, `exists`, `getAfter` and `existsAfter` behave, and how many calls does a request allow? (10 per single read or write, 20 per query and per batch, repeated calls to one document counted once; `get` sees the state before the writes, `getAfter` after the whole batch) | `crates/hidane/tests/fixtures/rules_lookups.json` | `crates/hidane/tests/rules_server.rs` |
| `rules_list.py` | Which queries does an `allow list` condition allow, and what does a denial say? Rules are not filters: a table of conditions against queries with each kind of filter (equality, ranges, `in`, `not-in`, `!=`, null and NaN, array membership, `OR`, names, nested fields, collection groups, limits, offsets, aggregations, listings), then pairs that show how a field the filter does not pin down (a `constraint`) compares, what a list sees of documents named by the filter and of path wildcards, and `request.query` | `crates/hidane/tests/fixtures/rules_list.json` | `crates/hidane/tests/rules_server.rs` |
| `rules_denials.py` | What does a denial say for a false or failing condition, no matching `allow`, several statements and matches, each method, a write without a precondition (both its `create` and `update` statements are listed, in that order; the one that does not apply as `false` or its error), queries and batches, and what does `GET :ruleCoverage` answer? | `crates/hidane/tests/fixtures/rules_denials.json` | `crates/hidane/tests/rules_server.rs` |
| `rules_coverage.py` + `rules_coverage.rules` | What do `GET :securityRules` and `GET :ruleCoverage` answer (no rules, after a `PUT` that compiled or not, per database, fingerprints, after `POST /reset`; with `--rules`, the file's rules with a report of their own in each database), on which methods, paths, content types and query strings, what does a coverage report hold (expressions and their positions in code points, values of every type, counts and order, statements counted as a growing `\|\|`, expressions not evaluated, errors and where they are placed, upserts, lists), and what does `debug()` print for each type? Takes the file the jar's standard output goes to, and records what each call printed; run again with `--rules` against a jar started in a directory holding `rules_coverage.rules` with `--rules rules_coverage.rules` | `crates/hidane/tests/fixtures/rules_coverage.json`, `rules_coverage_flag.json` | `crates/hidane/tests/rules_coverage.rs` |
| `websocket_requests.py` | What does the WebSocket feed on `--websocket_port` send, the one the Emulator UI's request monitor (its Firestore "Requests" tab) reads: the handshake at `/requests` and the answers to requests that are not one, the backlog (the last 50 records, oldest first) as one JSON-array frame, then one frame per evaluation, with every field of a record in order (`rules`, `time`, `rulesReleaseKey`, `requestId`, `outcome`, `granularAllowOutcomes`, `rulesContext` with `request` and `resource` as value messages) for reads, lists, writes, batches, transactions, Listen targets, the owner (not recorded) and databases without rules; pongs, closes, several clients, `PUT` and `POST /reset` (which keep the backlog), and a client that does not read (writes wait for it)? Takes the WebSocket port as its second argument; needs `grpcurl` on `PATH` | `crates/hidane/tests/fixtures/websocket_requests.json` | `crates/hidane/tests/websocket_requests.rs` |
| `rules_auth.py` | What does `request.auth` hold for unsigned (`alg: none`) tokens with claims present or missing, expired or for another project, for `Bearer owner` and for no header? (`uid` from `sub`, `null` without one; `token` with every claim; nothing verified) | `crates/hidane/tests/fixtures/rules_auth.json` | `crates/hidane/tests/rules_server.rs` |
| `rules_conditions.py` | What does the official emulator decide for a Security Rules condition (allow, false, evaluation error, compile error) on a fixed document? Takes a JSON list of conditions; `spikes/rules-eval/conditions.py` builds the list the #61 spike used | `results/rules-conditions-official-v1.22.0.json` | `crates/hidane-rules/tests/conditions.rs` |
| `rules_eval.py` | How are Security Rules conditions evaluated: operators and their errors, literals, indexes and slices, conversions, every function and method of every type (with the forms each takes), lookups and which documents cost nothing, scoping of functions and wildcards, and the entries of a denial? Records the full message of each denial | `crates/hidane-rules/tests/fixtures/rules_eval.json` | `crates/hidane-rules/tests/library.rs` |
| `rules_warnings.py` | Which warnings does the compiler give, with which positions: the type checker's (`Invalid type. Received one of […]. Expected one of […].`, conditions expecting a boolean, comparisons that can never be true, conditionals with results of different types, the types of wildcards, parameters and `let`s inferred from their uses and checked at each call)? Records the `issues[]` of every condition and ruleset of `rules_eval.py`, `rules_conditions.py` and `rules_paths.py` (whose fixtures keep only the descriptions) with positions, and cases of its own; nothing is read | `crates/hidane-rules/tests/fixtures/rules_warnings.json` | `crates/hidane-rules/tests/warnings.rs` |
| `transactions.py` | What do transactions lock, how long do writes wait, when do transactions end, and how are `verify` writes checked? Scenarios with concurrent steps and timing; needs `grpcurl` on `PATH` and takes about two minutes | `crates/hidane/tests/fixtures/transactions.json` | `crates/hidane/tests/transactions.rs` |

`sdk_documents.mjs` drives the same operations through `@google-cloud/firestore` (the engine of
firebase-admin) and prints a transcript without absolute timestamps. Run it from a directory
outside the repository where the package is installed, once per emulator, and diff the two:

```sh
cd "$(mktemp -d)" && npm i @google-cloud/firestore && cp <repo>/tools/oracle/sdk_documents.mjs .
FIRESTORE_EMULATOR_HOST=127.0.0.1:8089 node sdk_documents.mjs > official.json   # official jar
FIRESTORE_EMULATOR_HOST=127.0.0.1:8189 node sdk_documents.mjs > hidane.json     # hidane
```

The transcripts of the last run are in `results/sdk-documents-*.json`.

`sdk_queries.mjs` does the same for queries (filters, `Filter.or`, `limitToLast`, snapshot
cursors, `documentId`, collection groups, `stream`, queries in transactions and
`recursiveDelete`); its transcripts are in `results/sdk-queries-*.json`.

`sdk_aggregations.mjs` does the same for `count()` and `AggregateField.sum` / `average`;
its transcripts are in `results/sdk-aggregations-*.json`.

`sdk_web_writes.mjs` does the same for writes through the web SDK (`npm i firebase`), which
sends them over the Write stream; its transcripts are in `results/sdk-web-writes-*.json`.

`sdk_listen.mjs` listens through both the Admin SDK and the web SDK while writing through the
Admin SDK, and prints every snapshot each listener received; its transcripts are in
`results/sdk-listen-*.json`.

`sdk_listen_resume.mjs` takes the web SDK offline and back online (it then resumes its targets
with their tokens) while the Admin SDK writes, including writes that move two limit queries'
windows; its transcripts are in `results/sdk-listen-resume-*.json`.

`sdk_clear.mjs` clears data the ways test suites and the Emulator UI do (recursive delete,
rules-unit-testing's `clearFirestore()`, `POST /reset`; needs `npm i @firebase/rules-unit-testing`)
with listeners attached; its transcripts are in `results/sdk-clear-*.json`.

`sdk_auth.mjs` reads and writes as a rules-unit-testing user (unsigned mock token), a signed-out
user, with rules disabled (`Bearer owner` from the web SDK) and through the Admin SDK; its
transcripts are in `results/sdk-auth-*.json`.

`sdk_export_import.mjs` writes documents of every kind through the Admin SDK under
`firebase emulators:exec --export-on-exit` and reads them back under `--import`. Writing with one
emulator and reading with the other shows that each reads the other's exports; its transcripts
are in `results/export-import-*.json`.

`sdk_find_nearest.mjs` writes vectors through the Admin SDK and the web SDK and runs the Admin
SDK's `findNearest` with each distance measure; its transcripts are in
`results/sdk-find-nearest-*.json`.

`keepalive.mjs` pings an idle gRPC connection every 10 s and prints whether the server answers
with `GOAWAY`; its output is in `results/keepalive-*.txt`.

`webchannel/page.js` runs firebase-js-sdk scenarios in a browser (listeners, writes, queries,
batches, long polling, mock tokens, a refused write); its transcripts are in
`results/browser-webchannel-*.json`. `webchannel/tap.py` records the wire between a browser and an
emulator, which is how `docs/webchannel.md` was written.

`sdk_rest.mjs` drives the REST surface through the web SDK's Lite build (`firebase/firestore/lite`);
its transcripts are in `results/sdk-rest-*.json`.

`sdk_transactions.mjs` does the same for `runTransaction` (server transactions, retries on
`ABORTED`, lock waits), and `sdk_web_transactions.mjs` for the web SDK (`npm i firebase`), whose
transactions use preconditions and `verify` writes instead. Their transcripts are in
`results/sdk-transactions-*.json` and `results/sdk-web-transactions-*.json`.
