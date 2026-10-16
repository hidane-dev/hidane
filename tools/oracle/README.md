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
| `transforms.py` | How are field transforms applied (server time, increment, maximum / minimum, array union / remove), in which order, with which results, and which are refused (a write may not transform a field and a field nested in it)? Which field does a transform of `__name__` write (`__key__`, at any depth)? | `crates/hidane/tests/fixtures/transforms.json` | `crates/hidane/tests/transforms.rs` |
| `negative_zero.py` | What becomes of `-0.0` written over gRPC, REST and WebChannel, at the top level, in maps and in arrays, and read back over each; how do field transforms, filters, orderings, cursors, aggregations, Security Rules, writes that change nothing and an export and import treat it? (The store keeps it and REST prints `-0.0`; ProtoJSON reads every literal zero as `0.0`; queries take it as equal to `0.0` and `0`.) Needs `grpcurl` on `PATH` | `crates/hidane/tests/fixtures/negative_zero.json` | `crates/hidane/tests/negative_zero.rs` |
| `document_writes.py` | How do document reads and writes answer, including errors, preconditions, masks, paging and admin checks? | `crates/hidane/tests/fixtures/document_writes.json` | `crates/hidane/tests/documents.rs` |
| `same_document_writes.py` | Which commits that write one document more than once are refused, and with which message? (The official emulator collapses them into one Datastore mutation: an insert (an `update_time` of version 0 is one) may only follow a delete, an update may not follow a delete, nor may a delete that needs the document, a delete that needs none may follow only a delete, and only the first write may carry an `update_time`; checked over the whole commit before any precondition.) Every pair of writes on an existing and a missing document, longer sequences, preconditions on later writes, and which error comes first; and what the writes of a commit that is not refused report (each the document as the whole commit leaves it: no update time once deleted; the time it had when the writes leave it as it was, none of them deleting it) | `crates/hidane/tests/fixtures/same_document_writes.json` | `crates/hidane/tests/same_document_writes.rs` |
| `base_versions.py` | How is an `update_time` precondition compared with a document's version? (Its update time, the time a delete left, or 0. The time must be whole microseconds and not before the epoch, checked with each write's names; a version past the stored one is an `INVALID_ARGUMENT` that prints the Datastore key; the deleted version lets only a delete through; a later write repeating the first one's precondition is not checked again; Security Rules see version 0 as a create and any other as an update.) Each kind of write against the stored, deleted and missing versions, the key for names that need escaping, numeric IDs and a named database, and REST's `currentDocument.updateTime` parameter (read as version 0) | `crates/hidane/tests/fixtures/base_versions.json` | `crates/hidane/tests/base_versions.rs` |
| `commit_checks.py` | Which problem of a commit is reported first, over REST `:commit`, gRPC `Commit`, `:batchWrite` and the Write stream? (The verifies of the whole commit, then the names of every write, then the sequences of writes to one document, then the values of every write, then each write as it is applied; the Write stream checks no verify, `:batchWrite` refuses a document written twice first.) Every pair of kinds of invalid write, in two writes and in one, and what a write to a document of another database becomes (acknowledged, kept where no read finds it). Takes a directory the jar exports to; needs `grpcurl` on `PATH`. With `enterprise`, against a jar started with `--database-edition enterprise`: the same cases, where an enterprise database checks values otherwise (no size limit on a value or a document, arrays in arrays, 20 levels of maps and arrays counted alike, a problem inside a map reported as at the top) | `crates/hidane/tests/fixtures/commit_checks.json`, `crates/hidane/tests/fixtures/commit_checks_enterprise.json` | `crates/hidane/tests/commit_checks.rs` |
| `field_order.py` | In which order do a document's fields come back after sets, masked updates, transforms and nested writes, and in which order do read masks, query projections and aggregations return them? (Written order; masked and transformed fields that change move to the end in the order of a Java `HashMap`) | `crates/hidane/tests/fixtures/field_order.json` | `crates/hidane/tests/field_order.rs` |
| `queries.py` | What does RunQuery return for every filter operator on every value type, composite filters, implicit ordering, cursors, offsets, projections, collection groups and queries over every collection, and with which errors, in which order (field paths in filters, orders and projections among them; values over 1500 bytes by Firestore's storage size rules; filters that match nothing, which add no implicit order)? With `enterprise` as its second argument, against a jar started with `--database-edition enterprise`: the same cases as a pipeline answers them (#338: null and NaN equal themselves, negations true of a missing field, no restriction from indexes, array offsets read, `array-contains` on a value that is not an array an error, no order but the query's own) | `crates/hidane/tests/fixtures/queries.json`, `crates/hidane/tests/fixtures/queries_enterprise.json` | `crates/hidane/tests/queries.rs` |
| `listen_queries.py` | Which documents does a listener's query target hold when it becomes current, and which queries does it refuse? (The dataset and the queries of `queries.json` as Listen targets, over WebChannel: on a standard database as RunQuery answers them, but a query of two collections ends the stream with `UNKNOWN`; with `enterprise` as its second argument, on an enterprise database, checked as on a standard one, but null and NaN matched as values, #338) | `crates/hidane/tests/fixtures/listen_queries.json`, `crates/hidane/tests/fixtures/listen_queries_enterprise.json` | `crates/hidane/tests/listen_queries.rs` |
| `aggregations.py` | What do `count`, `sum` and `avg` return at the edges (integer overflow, NaN, infinities, precision, non-numbers, empty sets), how do they interact with `offset` / `limit` (the fields `sum` and `avg` read order the query), which aggregation lists are rejected, which queries `sum` and `avg` need (a collection; the aggregations are checked first; no projection; their fields ordered before the name), and which paths they may read? With `enterprise` as its second argument, against a jar started with `--database-edition enterprise`: the same cases as a pipeline answers them (#338: integers added as 64-bit integers until a double, an overflow an error; doubles added as decimals, `0.1 + 0.2` being `0.3`; `count` of every document; no number a null sum; the projected documents aggregated; `count` with `up_to` an error) | `crates/hidane/tests/fixtures/aggregations.json`, `crates/hidane/tests/fixtures/aggregations_enterprise.json` | `crates/hidane/tests/aggregations.rs` |
| `write_stream.mjs` | How does the Write stream answer: handshake, tokens, pipelined batches, empty requests, errors, resumption, locks? Node.js with `@grpc/grpc-js` and `@grpc/proto-loader`, run like the SDK scripts below with `crates/hidane-proto/proto/` as its second argument | `crates/hidane/tests/fixtures/write_stream.json` | `crates/hidane/tests/write_stream.rs` |
| `rest.py` | What does every REST binding answer, byte for byte: JSON layout, ProtoJSON values, streams as arrays, errors, 404s, lenient input (bodies as Gson reads them in lenient mode: single quotes, comments, unquoted names and strings), whitespace around a path's segments (trimmed), field paths in masks and transforms (array offsets, `__key__`, `__type__` and reserved names, in which pass)? | `crates/hidane/tests/fixtures/rest.json` | `crates/hidane/tests/rest.rs` |
| `conformance.py` | How does the official emulator answer the requests client SDKs send, as googleapis/conformance-tests gives them (Apache-2.0, pinned to a commit): commits for `create`, `set`, `update` and `delete` (preconditions, masks, server timestamps and the other transforms), GetDocument, and RunQuery for queries built clause by clause? The tests a client refuses before sending (`isError`) and `listen` are left out. Takes the conformance tests' `firestore/v1` directory as its second argument | `crates/hidane/tests/fixtures/conformance.json` | `crates/hidane/tests/rest.rs` |
| `project_ids.py` | Which project IDs are accepted, and where are they checked? (A Cloud project ID, after an optional domain: lowercase letters, digits and hyphens; `Invalid project ID(P) or database ID(D)` once a name's database ID is read, before the rest of the name, in every name and every reference of a request, in the order it is read; `.` and `..` refused as project and database IDs; database IDs not checked; a `500` without a message for `:securityRules` and `:ruleCoverage` without a database) | `crates/hidane/tests/fixtures/project_ids.json` | `crates/hidane/tests/rest.rs` |
| `single_project_mode.py` | What do `--single_project_mode` and `--single_project_mode_error` do to a request for another project than `--project_id`? (The project a request is addressed to: a REST path's, a gRPC call's `google-cloud-resource-prefix` header, else its `x-goog-request-params` database, else its name, a WebChannel URL's `database`, an export's body; checked once the name is read and again once the `Authorization` header is, each check printing the warning on standard output; with the error flag, an `INVALID_ARGUMENT` with that message, and a WebChannel handshake never answered.) Starts the jar twice; needs `grpcurl` on `PATH` | `crates/hidane/tests/fixtures/single_project_mode.json` | `crates/hidane/tests/single_project_mode.rs` |
| `list_documents.py` | What does ListDocuments list without a collection ID, how do page tokens continue across collections, how does `order_by` order a collection (and its next page, an error without a message: D089), which checks come first, and what do REST paths with a trailing slash mean? Needs `grpcurl` on `PATH` | `crates/hidane/tests/fixtures/list_documents.json` | `crates/hidane/tests/list_documents.rs` |
| `webchannel.py` | How does the browser SDK's transport behave: handshake, back channel (streaming and long polling), forward channel, stream errors, terminate, unknown sessions? Speaks the protocol directly, no browser | `crates/hidane/tests/fixtures/webchannel.json` | `crates/hidane/tests/webchannel.rs` |
| `json_escapes.py` | Which characters are escaped in JSON strings, and how: control characters, `"`, `\`, `/`, `<`, `>`, `&`, `=`, `'`, U+007F–U+009F, U+2000–U+20FF (U+2028, U+2029, zero widths, bidirectional controls), the byte order mark, noncharacters, as a field name, a map key, a string, a document ID, a collection ID and an alias, read back over REST (bodies and error messages) and WebChannel (messages and stream errors), byte for byte? (Three writers: REST bodies escape control characters, U+2028 and U+2029; errors also `/`, U+007F–U+009F and U+2000–U+20FF, in uppercase hex; WebChannel messages also `<`, `>`, `&`, `=` and `'`) | `crates/hidane/tests/fixtures/json_escapes.json` | `crates/hidane/tests/rest.rs`, `crates/hidane/tests/webchannel.rs` |
| `limits.py` | Which document limits are enforced (value, name, ID, depth, size), with which messages, in which order? | `crates/hidane/tests/fixtures/limits.json` | `crates/hidane/tests/limits.rs` |
| `read_time_and_databases.py` | How old may a `read_time` be, and how many databases may a project have? (Up to seven days, 604,800 s before now, over every read, however long the emulator has run and whenever the data was written; a time older than the data reads its absence (recorded seconds after the write; the script says what was seen an hour later), an older one `FAILED_PRECONDITION` "The requested 'read_time' is too old.". No limit on databases: the 1,000 named databases written, `(default)` besides, are all created.) Needs `grpcurl` on `PATH`; takes the number of databases as its second argument | `crates/hidane/tests/fixtures/read_time_and_databases.json` | `crates/hidane/tests/read_time_and_databases.rs` |
| `grpc_settings.py` | Which gRPC services answer, is v1beta1 the same service as v1, and how large may gRPC and REST requests be? Needs `grpcurl` on `PATH`; sends requests of up to 105 MB | `crates/hidane/tests/fixtures/grpc_settings.json` | `crates/hidane/tests/grpc_settings.rs` |
| `find_nearest.py` | How does `find_nearest` rank, filter, tie-break and validate, how does it combine with aggregations, and which vector values can be stored? | `crates/hidane/tests/fixtures/find_nearest.json` | `crates/hidane/tests/find_nearest.rs` |
| `unsupported.py` | What does the official emulator ignore or refuse: `explain_options`, ExecutePipeline on a standard database, PartitionQuery? Needs `grpcurl` on `PATH` | `crates/hidane/tests/fixtures/unsupported.json` | `crates/hidane/tests/unsupported.rs` |
| `auth.py` | How is the `Authorization` header read: administrators, users and anonymous callers, which tokens fail and how, and where each RPC, stream and REST endpoint reads it among its other checks? Needs `grpcurl` on `PATH` | `crates/hidane/tests/fixtures/auth.json` | `crates/hidane/tests/auth.rs` |
| `cors.py` | How does every HTTP path answer CORS: `Origin` reflection, credentials, methods, preflights on any path, requested headers, Private Network Access? | `crates/hidane/tests/fixtures/cors.json` | `crates/hidane/tests/cors.rs` |
| `datastore_checks.py` | Which problem of a Datastore query does the official emulator in `--database-mode datastore-mode` report first (#81)? Every problem of a query (`:runQuery`) and of an aggregation query (`:runAggregationQuery`) alone and every ordered pair of them, and several problems in one filter, one projection and one query of kinds or namespaces; found by the differential fuzzer. Takes a jar started with `--database-mode datastore-mode` | `crates/hidane/tests/fixtures/datastore_checks.json` | `crates/hidane/tests/datastore.rs` |
| `datastore_entities.py` | How does the official emulator in `--database-mode datastore-mode` store and read entities (#81)? The Firestore API refused; `allocateIds` (scattered IDs, one counter per database) and `reserveIds`; `commit` with each mutation, each kind of value read back, and each invalid key, value and request (with the official messages); `lookup` and its read options; transactions (begin, commit, rollback, read-only, a lock held by another). Takes a jar started with `--database-mode datastore-mode` | `crates/hidane/tests/fixtures/datastore_entities.json` | `crates/hidane/tests/datastore.rs` |
| `datastore_export.py` | How does the official emulator in `--database-mode datastore-mode` export and import (#81)? Starts the jar three times (the jar and `java` as arguments): `:export` of entities of every value type, meaning, namespace and ancestry, named and not, and the requests it refuses (in this mode the default database is an empty ID, not `(default)`); `--import-data` of that export; `:import` of it, twice, and its refusals. Each export is recorded as its files, its output's records sorted | `crates/hidane/tests/fixtures/datastore_export.json` | `crates/hidane/tests/datastore.rs` |
| `datastore_firestore_api.py` | How does the official emulator in `--database-mode datastore-mode` answer the Firestore API (#81)? Over REST, each Firestore route with each method, valid and invalid bodies and names and several content types (routed, read and its names checked, then refused), and the emulator's own endpoints (which fail); over gRPC, calls with good and bad names and the RPC that is not implemented. Takes a jar started with `--database-mode datastore-mode` | `crates/hidane/tests/fixtures/datastore_firestore_api.json` | `crates/hidane/tests/datastore.rs` |
| `datastore_gql.py` | How does the official emulator in `--database-mode datastore-mode` read GQL (#81)? Each clause of the GQL reference's grammar (`SELECT`, `DISTINCT ON`, `FROM`, `WHERE` with every operator and both directions, `ORDER BY`, `LIMIT`, `OFFSET` with cursors), each kind of literal (strings and escapes, numbers, `KEY`, `DATETIME`, `BLOB`, `ARRAY`) and binding, literals refused, aggregations (`SELECT COUNT(*)`, `AGGREGATE … OVER`), and the lexical and syntax errors of its parser; each answer holds the structured query the text reads as. Takes a jar started with `--database-mode datastore-mode` | `crates/hidane/tests/fixtures/datastore_gql.json` | `crates/hidane/tests/datastore.rs` |
| `datastore_indexes.py` | Which queries does the official emulator in `--database-mode datastore-mode` serve with `--require_indexes` (#81)? Starts the jar once per run (the jar and `java` as arguments), each with its flags and an `index.yaml` of its own: queries that need no index, one the file defines or one it does not (kinds and ancestors, the key, equalities in their order, inequalities, orders and directions, projections, `distinctOn`, `OR`, sums and averages, GQL, namespaces, projects and transactions), which refusals come before the index and which after, `--index_file` without `--require_indexes` and the reverse, when the file is read, and how each spelling or mistake in the file is read | `crates/hidane/tests/fixtures/datastore_indexes.json` | `crates/hidane/tests/datastore.rs` |
| `datastore_projects.py` | How does the official emulator read a Datastore request's project, in each mode (#81)? Starts the jar in Firestore Native mode and in `--database-mode datastore-mode` (the jar and `java` as arguments): `Lookup` with each kind of project ID (App Engine's app IDs, with a domain or a partition, and invalid ones), every RPC with an empty project and a valid one, the REST verbs, and the services reflection lists. Firestore Native mode serves the Datastore API and refuses it once the project is read | `crates/hidane/tests/fixtures/datastore_projects.json` | `crates/hidane/tests/datastore.rs` |
| `datastore_protobuf.py` | How does the official emulator in `--database-mode datastore-mode` answer REST requests in protobuf (#81), as the Java and Python client libraries send them over HTTP? Which `Content-Type` is read as protobuf (exactly `application/x-protobuf`), which as JSON and which is not found; each verb in protobuf, and errors as a `google.rpc.Status`. Takes a jar started with `--database-mode datastore-mode` | `crates/hidane/tests/fixtures/datastore_protobuf.json` | `crates/hidane/tests/datastore.rs` |
| `datastore_queries.py` | How does the official emulator in `--database-mode datastore-mode` answer queries (#81)? Over a dataset of every value type, arrays, entity values, an ancestry and namespaces: kinds and orders (values of every type ordered together), every filter operator against values of every type, composite filters, keys and ancestors, projections, keys-only and `distinctOn` (one result per index value of the properties ordered, compared or projected), limits, offsets and cursors (each cursor named, so a replay sends its own), `__kind__`, `__namespace__` and `__property__`, the queries it refuses, and `runAggregationQuery` (count, sum, avg, aliases, limits). Takes a jar started with `--database-mode datastore-mode` | `crates/hidane/tests/fixtures/datastore_queries.json` | `crates/hidane/tests/datastore.rs` |
| `export_import.py` | What do exports hold, byte for byte (LevelDB logs of App Engine entities), what does an import write where, how do `:export` and `:import` answer bad requests and damaged files, when does `--seed_from_export` seed which database, and how does a managed export's layout import? Takes the jar and `java` as arguments to start emulators of its own | `crates/hidane/tests/fixtures/export_import.json` | `crates/hidane/tests/export_import.rs` |
| `bulk_delete.py` | What does `POST /emulator/v1/projects/{p}:bulkDelete` delete, and how does it refuse a request? (Datastore's bulk delete: the documents of every collection with one of `collection_ids`, at any depth, not those below them; the namespace `""` is every document, another none; the body's database whatever the path; the body, its database, the `Authorization` header and the IDs checked in that order) | `crates/hidane/tests/fixtures/bulk_delete.json` | `crates/hidane/tests/rest.rs` |
| `pipelines.py` | How does the official emulator run pipelines (`ExecutePipeline`, `:executePipeline`) on an enterprise database? Each stage the SDKs send (`collection`, `collection_group`, `database`, `documents`, `where`, `select`, `add_fields`, `remove_fields`, `replace_with`, `sort`, `limit`, `offset`, `aggregate`, `distinct`, `find_nearest`, `sample`, `union`, `unnest`, `let`, `update`, `delete`) and how malformed stages, arguments and options are refused; each function the SDKs know, evaluated on a document holding a value of every type, with arguments that make sense and then each argument replaced by every type and by a missing field; subpipelines and variables; `update` and `delete` in transactions (applied by the commit); read times. Takes a jar started with `--database-edition enterprise` | `crates/hidane/tests/fixtures/pipelines.json` | `crates/hidane/tests/pipelines.rs` |
| `index_usage.py` | Which composite indexes does `GET /emulator/v1/projects/{p}:indexUsage?database=…` report the queries run on a database would need, and how often? (For each query of `queries.json` and `aggregations.json` in a database of its own: one index per conjunction of the filter in disjunctive normal form, its equality fields by path, then array-contains, then the orders with `__name__` last, none where a single field or a collection's array-contains with `__name__` ascending would do; counted by RunQuery, RunAggregationQuery, a transaction's query, a query at a read time and a listener's first run, not by a query a check refuses, and forgotten by `POST /reset`; the reports in an order of the emulator's own, which changes from run to run; the project of the path not read). With `enterprise`, against a jar started with `--database-edition enterprise`, the sequences alone: only a listener's query counts | `crates/hidane/tests/fixtures/index_usage.json`, `crates/hidane/tests/fixtures/index_usage_enterprise.json` | `crates/hidane/tests/index_usage.rs` |
| `function_triggers.py` | How does the official emulator answer the Functions emulator's Firestore trigger registrations, v1 (`PUT /emulator/v1/projects/{p}/triggers/{key}`) and v2 (`POST …/eventarcTrigger?eventarcTriggerId={key}`): as firebase-tools sends them, then other event types, projects, databases, keys, bodies, query parameters, methods, paths, content types and tokens, with and without `--functions_emulator`? (`{}` for v1 and the trigger as read for v2; `400` for a body that does not parse; `500` without a message for a name or a trigger it refuses, with the reason in its log, which the fixture keeps.) Takes the jar and `java` as arguments to start emulators of its own, on one port (8131 by default) | `crates/hidane/tests/fixtures/function_triggers.json` | `crates/hidane/tests/function_triggers.rs` |
| `startup_flags.py` | Which flags does the official emulator start with, and which does it refuse? Starts the jar once per flag set (the jar and `java` as arguments): `--database-mode` and `--database-edition` in every spelling (only `firestore-native`, `datastore-mode`, `standard` and `enterprise` start), the checks between flags and their order, each start as its banner's edition and mode or its exit code and message; and requests to the emulator started with `--require_indexes` in Firestore Native mode, which refuses every query and nothing else | `crates/hidane/tests/fixtures/startup_flags.json` | `crates/hidane/tests/launch.rs` |
| `function_events.py` | What does the official emulator send to `--functions_emulator` when a commit changes documents that registered triggers watch: v1 events (`POST /functions/projects/{p}/triggers/{key}`, a JSON body) and v2 CloudEvents (`ce-*` headers and a `DocumentEventData` as `application/protobuf` in base64, decoded in the fixture), for every event type and `withAuthContext`, path patterns, named databases and projects, values of every type, batches, transactions, `:batchWrite`, gRPC Commit and the Write stream, deleted paths, cleared databases, imports and `POST /reset`, and writes the Security Rules deny (over each transport, in a batch with an allowed write and in a transaction; the owner's credential for contrast); in which order (all v1 events of a commit, then the v2 ones, document by document, triggers in the order of a Java `HashMap` sized for them), how the next event waits for the answer to the last, and which `WARNING`s it prints for a Functions emulator that answers an error, does not listen, or is not configured? Takes the jar and `java` as arguments to start emulators of its own, on a port (8200 by default) and with a recording server on another (8201) | `crates/hidane/tests/fixtures/function_events.json` | `crates/hidane/tests/function_events.rs` |
| `rules_compile.py` | How does `PUT :securityRules` answer: which rulesets compile, with which warnings (`issues[]` with positions) or errors (`400` "Error compiling rules:" with `L<line>:<col>` lines), for the samples of the Firebase documentation and firebase/quickstart-testing (Apache-2.0, sources in the fixture) and cases for versions, paths, statements, functions, conditions, lexical rules and several files? To record again, pass the fixture's corpus entries (those with a `source`) back as the corpus | `crates/hidane/tests/fixtures/rules_compile.json` | `crates/hidane/tests/rules_compile.rs` |
| `rules_paths.py` | Which documents does a `match` path cover in rules versions 1 and 2 (single and recursive wildcards, globs in the middle, nested matches), and what do the wildcards hold (`is path`, segments)? | `crates/hidane-rules/tests/fixtures/rules_paths.json` | `crates/hidane-rules/tests/paths.rs` |
| `rules_request.py` | What do `request` and `resource` hold for a get, lists, queries under a document and of collection groups, a create with and without a document ID, an update with and without a mask, transforms and a delete (method, path, `resource.data`, `request.resource.data` after merges and transforms, `request.query`, `inTransaction`, `fields`, `writeFields`, `transforms`, `autoId`, and the keys `request` does not have)? One probe condition per load, allowed or denied | `crates/hidane/tests/fixtures/rules_request.json` | `crates/hidane/tests/rules_server.rs` |
| `rules_request_fields.py` | What does `request` hold, key by key and in which order, for gets, batchGets, lists, queries of every shape, aggregations, reads and a write in a transaction, creates with and without a document ID, upserts, updates, deletes and batches (`debug(request)` and the coverage report's values of `request`), and what are `writeFields` and `transforms` for writes with update masks and transforms? Takes the file the jar's standard output goes to, as `rules_coverage.py` does | `crates/hidane/tests/fixtures/rules_request_fields.json` | `crates/hidane/tests/rules_coverage.rs` |
| `rules_streams.py` | How do the Listen and Write streams answer under rules: a denied target (`ADD`, then `REMOVE` with a `PERMISSION_DENIED` cause), a target of documents with one denied, a denied query, a listened document that a write makes denied, a denied write on the stream, verifies on it (allowed, denied, after an allowed write)? Needs `grpcurl`; uses the repository's protos | `crates/hidane/tests/fixtures/rules_streams.json` | (#63, #67) |
| `rules_writes.py` | Which `allow` method decides each kind of write (by whether the document exists, or by an `exists` precondition; deletes always `delete`), and how do a batchGet with a denied document (checked in order, stopping at the first denied one, those before listed as `false`), a read in a transaction, an aggregation, a list and listCollectionIds answer under rules? | `crates/hidane/tests/fixtures/rules_writes.json` | `crates/hidane/tests/rules_server.rs` |
| `rules_verify.py` | How are the `verify` writes of a commit (the SDKs' transactions send one for each document read but not written) checked: as a `get` of the document, with `request.inTransaction` true, before the precondition, alone and next to other writes (in order, the first denied write decides; the rules of the whole commit come before any precondition), in transactions, with `request` and `resource` as `debug()` prints them (in the second pass, `request` has the keys of a write and `request.resource` is the stored document), and against the lookup limits a commit's writes share (the verified document costs the other writes' `exists()` a lookup, the written ones cost the verify's, `existsAfter()` of either is free)? Takes the file the jar's standard output goes to, as `rules_coverage.py` does | `crates/hidane/tests/fixtures/rules_verify.json` | `crates/hidane/tests/rules_coverage.rs` |
| `rules_same_document.py` | How do Security Rules check a commit that writes one document more than once? Once, as one write where the first comes: `resource` the document before the commit, `request.resource` what the last write leaves; a delete last makes a `delete`, else the first write decides the method; `request.writeFields` the transforms, then the masks joined as one mask, a delete starting over; two documents in turn, the first one's check first | `crates/hidane/tests/fixtures/rules_same_document.json` | `crates/hidane/tests/rules_server.rs` |
| `rules_lookups.py` | How do `get`, `exists`, `getAfter` and `existsAfter` behave, and how many calls does a request allow? (10 per single read or write, 20 per query and per batch, repeated calls to one document counted once; `get` sees the state before the writes, `getAfter` after the whole batch) | `crates/hidane/tests/fixtures/rules_lookups.json` | `crates/hidane/tests/rules_server.rs` |
| `rules_expressions.py` | How many expressions may a Security Rules decision evaluate, and what counts as one? (1,000; past them, "Unable to evaluate the expression as the maximum of 1000 expressions to evaluate has been reached."). For each kind of expression, the most that fit in a list, recorded at that number and one more; what costs more than one (`&&` and `\|\|` evaluating their right side, conditionals, `is`, path segments, `let`s) counted last; and what counts together: the statements of a decision, but not the writes of a batch, the two methods of an upsert, the disjunctions of a query or the documents of a batchGet. Then, at the most that fit and one more: conditions that read `resource`, `request.resource`, `request.auth`, wildcards and documents (`get()`, `exists()`, `getAfter()`, `existsAfter()`, stored and missing), with the cost before, after, between them, in functions, `let`s, `\|\|` and conditionals; that come to false, an error, or an error `&&` or `\|\|` absorbs (the limit's error wins over the left side's); in `rules_version` 1, 2 and none, with recursive wildcards; for gets, lists, queries and aggregations (filters on what the rules read, `__name__`, collection groups), commits (creates, updates, upserts, deletes, transforms), batchGets, and reads and commits in transactions (a batchGet that begins a transaction and is denied leaves nothing locked) | `crates/hidane/tests/fixtures/rules_expressions.json` | `crates/hidane/tests/rules_server.rs` |
| `rules_expressions_report.py` | What does the official emulator report of a decision near the limit of 1,000 expressions? For each place of a `debug()` call (operands, `&&`, `\|\|`, conditionals, `is`, indexes, slices, methods, functions, `let`s, paths), the most a condition may cost before it with the decision within the limit, the most with the call still printing, and one more: an expression counts once what is in it is evaluated, a call before its function runs. Coverage reports of decisions past the limit at different places: nothing after the expression that goes past it has a value. Takes the file the jar's standard output goes to, as `rules_coverage.py` does | `crates/hidane/tests/fixtures/rules_expressions_report.json` | `crates/hidane/tests/rules_coverage.rs` |
| `rules_expressions_grpc.py` | Is the limit of 1,000 expressions the same over gRPC? GetDocument, BatchGetDocuments (each document from zero), RunQuery and RunAggregationQuery (each disjunction from zero), Commit (each write from zero), Listen targets of documents, of a missing one and of queries, and the Write stream, each at the most a condition may cost and one more. Needs `grpcurl`; uses the repository's protos | `crates/hidane/tests/fixtures/rules_expressions_grpc.json` | `crates/hidane/tests/rules_streams.rs` |
| `rules_expressions_monitor.py` | What does the request monitor (`--websocket_port`) send around a decision past the limit of 1,000 expressions? Nothing for it; a client connected before still gets the records after it, a client that connects after it nothing at all (no backlog, no record). Needs a fresh jar | `crates/hidane/tests/fixtures/rules_expressions_monitor.json` | `crates/hidane/tests/websocket_requests.rs` |
| `rules_list.py` | Which queries does an `allow list` condition allow, and what does a denial say? Rules are not filters: a table of conditions against queries with each kind of filter (equality, ranges, `in`, `not-in`, `!=`, null and NaN, array membership, `OR`, names, nested fields, collection groups, limits, offsets, aggregations, listings), then pairs that show how a field the filter does not pin down (a `constraint`) compares, what a list sees of documents named by the filter and of path wildcards, and `request.query`; then a table of filters with null and NaN (unary, and field filters that match nothing or every value but null), alone and next to other filters of the same field, and filters that repeat, against conditions that compare the field with a value of each type | `crates/hidane/tests/fixtures/rules_list.json` | `crates/hidane/tests/rules_server.rs` |
| `rules_denials.py` | What does a denial say for a false or failing condition, no matching `allow`, several statements and matches, each method, a write without a precondition (both its `create` and `update` statements are listed, in that order; the one that does not apply as `false` or its error), queries and batches, and what does `GET :ruleCoverage` answer? | `crates/hidane/tests/fixtures/rules_denials.json` | `crates/hidane/tests/rules_server.rs` |
| `rules_coverage.py` + `rules_coverage.rules` | What do `GET :securityRules` and `GET :ruleCoverage` answer (no rules, after a `PUT` that compiled or not, per database, fingerprints, after `POST /reset`; with `--rules`, the file's rules with a report of their own in each database), on which methods, paths, content types and query strings, what does a coverage report hold (expressions and their positions in code points, values of every type, counts and order, statements counted as a growing `\|\|`, expressions not evaluated, errors and where they are placed, upserts, lists, filters with null and NaN, ways of a filter that repeat), and what does `debug()` print for each type? Takes the file the jar's standard output goes to, and records what each call printed; run again with `--rules` against a jar started in a directory holding `rules_coverage.rules` with `--rules rules_coverage.rules` | `crates/hidane/tests/fixtures/rules_coverage.json`, `rules_coverage_flag.json` | `crates/hidane/tests/rules_coverage.rs` |
| `websocket_escapes.py` | How does the request monitor write strings that JSON may escape, in the ruleset, the path, keys, values, a reference, bytes and a token's claims? `"`, `\` and the control characters escaped, U+2028 and U+2029 as `\u2028` and `\u2029`, the HTML characters (`<` `>` `&` `=` `'`, which the WebChannel escapes), `/`, U+007F, U+0080 and the rest as they are. Keeps each record as text | `crates/hidane/tests/fixtures/websocket_escapes.json` | `crates/hidane/tests/websocket_requests.rs` |
| `websocket_requests.py` | What does the WebSocket feed on `--websocket_port` send, the one the Emulator UI's request monitor (its Firestore "Requests" tab) reads: the handshake at `/requests` and the answers to requests that are not one, the backlog (the last 50 records, oldest first) as one JSON-array frame, then one frame per evaluation, with every field of a record in order (`rules`, `time`, `rulesReleaseKey`, `requestId`, `outcome`, `granularAllowOutcomes`, `rulesContext` with `request` and `resource` as value messages) for reads, lists, writes, batches, transactions, Listen targets, the owner (not recorded) and databases without rules; pongs, closes, several clients, `PUT` and `POST /reset` (which keep the backlog), and a client that does not read (writes wait for it)? Takes the WebSocket port as its second argument; needs `grpcurl` on `PATH` | `crates/hidane/tests/fixtures/websocket_requests.json` | `crates/hidane/tests/websocket_requests.rs` |
| `rules_auth.py` | What does `request.auth` hold for unsigned (`alg: none`) tokens with claims present or missing, expired or for another project, for `Bearer owner` and for no header? (`uid` from `sub`, `null` without one; `token` with every claim; nothing verified) | `crates/hidane/tests/fixtures/rules_auth.json` | `crates/hidane/tests/rules_server.rs` |
| `rules_conditions.py` | What does the official emulator decide for a Security Rules condition (allow, false, evaluation error, compile error) on a fixed document? Takes a JSON list of conditions; `spikes/rules-eval/conditions.py` builds the list the #61 spike used | `results/rules-conditions-official-v1.22.0.json` | `crates/hidane-rules/tests/conditions.rs` |
| `rules_eval.py` | How are Security Rules conditions evaluated: operators and their errors, literals, indexes and slices, conversions, every function and method of every type (with the forms each takes), lookups and which documents cost nothing, scoping of functions and wildcards, and the entries of a denial? Records the full message of each denial; the two batches whose lookup of a document another write writes is free in some projects and runs and costs one in others (#210) under 20 projects, with both answers | `crates/hidane-rules/tests/fixtures/rules_eval.json` | `crates/hidane-rules/tests/library.rs` |
| `rules_warnings.py` | Which warnings does the compiler give, with which positions: the type checker's (`Invalid type. Received one of […]. Expected one of […].`, conditions expecting a boolean, comparisons that can never be true, conditionals with results of different types, the types of wildcards, parameters and `let`s inferred from their uses and checked at each call)? Records the `issues[]` of every condition and ruleset of `rules_eval.py`, `rules_conditions.py` and `rules_paths.py` (whose fixtures keep only the descriptions) with positions, and cases of its own; nothing is read | `crates/hidane-rules/tests/fixtures/rules_warnings.json` | `crates/hidane-rules/tests/warnings.rs` |
| `transactions.py` | What do transactions lock, how long do writes wait, when do transactions end, how are `verify` writes checked, and is a query's transaction checked before the query, and a commit's before its writes? Scenarios with concurrent steps and timing; needs `grpcurl` on `PATH` and takes about two minutes | `crates/hidane/tests/fixtures/transactions.json` | `crates/hidane/tests/transactions.rs` |

`tools/parity/table.py` counts the answers the tests replay from these fixtures and writes the
parity table ([`docs/parity.md`](../../docs/parity.md)), the Parity section of hidane.dev and its
badge. When you add a fixture, add it to `RECORDINGS` in that script (its area, the test
functions that replay it, what they compare and how to count its answers, each at the level the
test compares it) and run `python3 tools/parity/table.py`; a new pair of SDK transcripts in
`results/` goes in its `TRANSCRIPTS`, and a pair that differs in `SDK_DIFFERENCES` with where the
documentation explains it. CI fails while a fixture or a transcript is missing from them, or
while the table is stale.

`corpus.py` lists every recording, fixtures and official transcripts, in the manifest of the
[Hidane Oracle Corpus](../../docs/oracle-corpus.md) (`docs/oracle-corpus.json`): the script that
made each, the client it used, the official emulator release it was made against and how that is
known, when it was recorded, its SHA-256, and how the tests compare it. Run
`python3 tools/oracle/corpus.py` after `table.py`; CI fails while the manifest is stale. A new
pair of SDK transcripts then needs an entry in `EVIDENCE` of `tools/parity/sdk_certification.py`,
which writes the [SDK certification matrix](../../docs/sdk-certification.md) from the transcripts
(run it after `corpus.py`; CI fails while it is stale). Where
these recordings may come from and how they are made is in
[`docs/clean-room/`](../../docs/clean-room/README.md).

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

`sdk_web_aggregations.mjs` does the same for aggregation queries through the web SDK in Node
(`npm i firebase@13.0.0`: `getCountFromServer`, and `getAggregateFromServer` with `count`, `sum`
and `average`), which sends them as `RunAggregationQuery` over gRPC, and records the SDK's version
in its transcript; its transcripts are in `results/sdk-web-aggregations-*.json`.

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

`sdk_rules_unit_testing.mjs` runs a rules-unit-testing suite under Security Rules
(`initializeTestEnvironment` with a ruleset that reads `request.auth` and its claims,
`resource.data`, `request.resource.data` and a `get()` lookup, and a list rule): `assertSucceeds` /
`assertFails` for a signed-in user, a signed-out user and `withSecurityRulesDisabled`, on gets,
lists, listeners, creates, updates, deletes, batches and transactions, then `clearFirestore()`.
It prints which assertions passed with each error's code and message, and the versions of
`firebase` and `@firebase/rules-unit-testing` (`npm i firebase @firebase/rules-unit-testing`); its
transcripts are in `results/sdk-rules-unit-testing-*.json`.

`sdk_export_import.mjs` writes documents of every kind through the Admin SDK under
`firebase emulators:exec --export-on-exit` and reads them back under `--import`. Writing with one
emulator and reading with the other shows that each reads the other's exports; its transcripts
are in `results/export-import-*.json`.

`sdk_find_nearest.mjs` writes vectors through the Admin SDK and the web SDK and runs the Admin
SDK's `findNearest` with each distance measure; its transcripts are in
`results/sdk-find-nearest-*.json`.

`functions/` is a firebase project whose functions (`firebase-functions` v1 and v2, `onDocument*`
with and without `withAuthContext`, a subcollection and a named database) write down what each
event gave them; `functions/drive.mjs` writes the documents they watch under
`firebase emulators:exec --only functions,firestore` and prints what the functions received. Its
header says how to run it under each emulator (`hidane exec -- firebase …` for hidane); its
transcripts are in `results/functions-triggers-*.json`.

`keepalive.mjs` pings an idle gRPC connection every 10 s and prints whether the server answers
with `GOAWAY`; its output is in `results/keepalive-*.txt`.

`webchannel/page.js` runs firebase-js-sdk scenarios in a browser (listeners, writes, queries,
batches, long polling, mock tokens, a refused write); its transcripts are in
`results/browser-webchannel-*.json`. `webchannel/rules_page.js` runs the steps of
`sdk_rules_unit_testing.mjs` in a browser (it imports them; its header says how to bundle it), where
the contexts' tokens travel in the handshake's `headers` field, a denied write ends the Write
stream and a denied listener's target is removed with a cause; its transcripts are in
`results/browser-rules-*.json`. `webchannel/tap.py` records the wire between a browser and an
emulator, which is how `docs/webchannel.md` was written.

`flutter_web/` is a Flutter Web app (`cloud_firestore` with `useFirestoreEmulator`, which drives
the firebase-js-sdk that `firebase_core_web` loads from gstatic.com). It writes and reads
documents of every value type, updates them with transforms and merges, runs queries (filters,
`Filter.or`, cursors, `limitToLast`, a collection group, `count` / `sum` / `average`), listens to a
query and a document while a document is added, changed, moved, taken out of the query and
deleted, commits a batch and transactions, and records the errors of updates of a missing
document. The header of `lib/main.dart` says how to build and serve it; the page takes the
emulator's port as `?port=` and leaves the transcript in `window.hidaneTranscript`, with the
firebase-js-sdk version and the URLs it was loaded from. Its transcripts are in
`results/flutter-web-*.json`.

`tools/js-sdk-browser/` runs firebase-js-sdk's own Firestore browser suite (Vitest in Chromium)
against either emulator and compares the outcome of each test; its README says how, and its
results are in `results/js-sdk-browser-*.json`.

`sdk_rest.mjs` drives the REST surface through the web SDK's Lite build (`firebase/firestore/lite`);
its transcripts are in `results/sdk-rest-*.json`.

`sdk_transactions.mjs` does the same for `runTransaction` (server transactions, retries on
`ABORTED`, lock waits), and `sdk_web_transactions.mjs` for the web SDK (`npm i firebase`), whose
transactions use preconditions and `verify` writes instead. Their transcripts are in
`results/sdk-transactions-*.json` and `results/sdk-web-transactions-*.json`.

`tools/fuzz/` asks the questions nobody wrote down: it sends generated REST requests to both
emulators through `harness.py`'s `send`, compares the answers after its `mask` (the
normalisation `rest.py` records with), and writes each difference it cannot explain, minimised,
in `rest.py`'s fixture format for a person to turn into a case of a recorder here. Its README says
how.

## The official emulator in CI

CI replays the fixtures without Java. To run the official emulator on a runner, the composite
action `.github/actions/official-emulator` downloads its jar from the URL firebase-tools uses
(`https://storage.googleapis.com/firebase-preview-drop/emulator/cloud-firestore-emulator-v<version>.jar`),
checks the jar's SHA-256, caches it with `actions/cache` (outside the workspace, so it is never
committed), sets up Temurin 21 with `actions/setup-java` (leaving `JAVA_HOME` and `PATH` as they
were), starts the emulator with `-Duser.language=en -Duser.country=US` and waits until `GET /`
answers, for two minutes at most (on `ubuntu-24.04` it answers 1 to 4 seconds after `java`
starts, and in the drift jobs below its resident memory peaks at about 280 to 360 MiB).
Without a `version` it runs the one in `.github/actions/official-emulator/pinned.sh`, which the
fixtures were recorded from:

```yaml
- id: emulator
  uses: ./.github/actions/official-emulator
  with:
    port: "8080" # also host, args (more emulator flags), java-options, timeout
- env:
    ADDRESS: ${{ steps.emulator.outputs.address }}
  run: |
    python3 -I tools/oracle/value_order.py "$ADDRESS" > "$RUNNER_TEMP/value_order.json"
    curl -X POST "http://$ADDRESS/shutdown"
```

Its outputs are the emulator's `address`, `pid`, `log` and `startup-seconds`, the `jar` and the
`java` it runs with. Stop the emulator with `POST /shutdown` (or its `pid`) before starting
another on the same port; the runner ends it with the job otherwise. `start: "false"` only
fetches the jar and sets up Java. Another release runs with `version` and `sha256` together, as
firebase-tools' `src/emulator/downloadableEmulatorInfo.json` gives them (`firestore`).

The Official emulator workflow (`.github/workflows/official-emulator.yml`) uses the action
weekly, by hand, and on pull requests that change the action or the workflow:

- **drift** re-records `value_order`, `queries`, `aggregations`, `rest`, `limits`,
  `rules_paths` and `rules_eval`, each against a freshly started emulator, and fails when a
  recording differs from the committed fixture by a byte. They cover values, queries,
  aggregations, the REST surface, document limits, rules paths and the evaluation of rules, take
  seconds, need nothing beyond the Python standard library, and come out the same on every run:
  they wait on no stream, and the server times and IDs the emulator chooses are masked or absent
  in what they record. (Two batches of `rules_eval.py` are allowed in some projects and denied in
  others, and which ones changes from one start of the official emulator to the next, on Java 21
  and 24 alike; the script records them under 20 projects with both answers, #210.) A
  recording that differs is made again against a second fresh emulator, which tells a confirmed
  difference (both recordings agree) from an unstable one. Each recording, a `result.json` and
  the emulator's output are uploaded, and each job's summary gives the emulator's start-up time
  and peak resident memory. Run by hand with a `version` and `sha256`, it records against that
  release instead and shows which answers it changes; the recordings' `oracle` field still names
  the pinned version, since the scripts write it. The jobs live in the reusable
  `.github/workflows/official-recordings.yml`, which the Upstream workflow's compatibility canary
  calls for new releases ([`tools/upstream/README.md`](../upstream/README.md)).
- **newer** reads which Firestore emulator firebase-tools' latest release pins and fails when it
  is not the one in `pinned.sh`. Its job summary says what to do: run the workflow by hand with
  the new version and SHA-256 to see which recordings change, then re-record every fixture
  against the new jar, update the `oracle` field the scripts write and `pinned.sh`, and change
  hidane where the answers changed.

`sdk_go/`, `sdk_python/` and `sdk_java/` run the same steps through the Go, Python and Java
server SDKs: `cloud.google.com/go/firestore` 1.26.0 (pinned in `go.mod` and `go.sum`; Go 1.26 or
newer), `google-cloud-firestore` 2.34.1 (`requirements.txt`; run with Python 3.13) and
`com.google.cloud:google-cloud-firestore` 3.49.0 (`pom.xml`; Java 21 and Maven). Each takes a
section as its argument, prints its transcript, and wants a fresh emulator per section (each
section uses a project of its own, `demo-<sdk>-<section>`):

- `documents`: set, merge and `mergeFields`, dotted and quoted field paths, create, update and
  delete with and without preconditions (`exists`, `lastUpdateTime`), a write that changes
  nothing, `getAll`, a field mask, batches (one that fails writes nothing), `listDocuments`,
  `listCollections`, the BulkWriter (BatchWrite), generated IDs, every field transform and a
  value of each type;
- `queries`: the filters, `or` / `and`, orderings, cursors, offsets, limits and `limitToLast`,
  projections, document-ID filters, collection groups, streaming, `explain` options, queries in
  transactions, a partition query (`UNIMPLEMENTED` on both emulators), `count` / `sum` /
  `average`, `findNearest` and `recursiveDelete` (not in the Go SDK);
- `transactions`: read-write, write-only and read-only transactions (also at a past read time),
  a callback that fails, a precondition that fails at the commit, an outside write that waits
  for a transaction's lock or gives up after 2 s, and two transactions that read one
  document and write it 300 ms apart, so that the first lock wait times out and one of them is
  retried after `ABORTED` (two commits that start waiting at the same instant can both time
  out on the official emulator, and the attempts then depend on the SDK's backoff);
- `listen`: query, limit, collection-group and document listeners while documents enter,
  change in, move in and leave them, a write that changes nothing, a batch, a server timestamp
  and a delete; within each step the snapshots are grouped by listener.

A step an SDK does not offer is left out (the Go SDK has no field mask on a get and no
`recursiveDelete`; the Java SDK has no `exists` precondition for deletes). Keep build output and
downloaded dependencies out of the repository:

```sh
tmp=$(mktemp -d); export FIRESTORE_EMULATOR_HOST=127.0.0.1:8089
# Go
(cd tools/oracle/sdk_go && go build -o "$tmp/sdk_go" .) && "$tmp/sdk_go" documents > go-documents.json
# Python
python3 -m venv "$tmp/venv" && "$tmp/venv/bin/pip" install -r tools/oracle/sdk_python/requirements.txt
"$tmp/venv/bin/python" -I tools/oracle/sdk_python/transcript.py documents > python-documents.json
# Java
cp -R tools/oracle/sdk_java "$tmp/" && (cd "$tmp/sdk_java" && mvn -q compile exec:java -Dexec.args=documents) > java-documents.json
```

Their transcripts are in `results/sdk-go-*.json`, `results/sdk-python-*.json` and
`results/sdk-java-*.json`, one pair per section.

`sdk_datastore_python/` runs steps through the Python client library of Datastore,
`google-cloud-datastore` 2.27.0 (`requirements.txt`; run with Python 3.13), against an emulator
started with `--database-mode datastore-mode` (#81), named by `DATASTORE_EMULATOR_HOST`. It takes
a section, `entities` (every value type, keys and IDs, namespaces, the official messages of what
is refused), `queries` (filters, orders, projections, `distinct_on`, cursors, ancestors, metadata
kinds, aggregations) or `transactions`, and wants a fresh emulator per section. The client speaks
gRPC; with `GOOGLE_CLOUD_DISABLE_GRPC=true` it speaks HTTP, REST with protobuf bodies:

```sh
python3 -m venv "$tmp/venv" && "$tmp/venv/bin/pip" install -r tools/oracle/sdk_datastore_python/requirements.txt
DATASTORE_EMULATOR_HOST=127.0.0.1:8086 "$tmp/venv/bin/python" -I tools/oracle/sdk_datastore_python/transcript.py entities > entities.json
DATASTORE_EMULATOR_HOST=127.0.0.1:8087 GOOGLE_CLOUD_DISABLE_GRPC=true "$tmp/venv/bin/python" -I tools/oracle/sdk_datastore_python/transcript.py entities > http-entities.json
```

Their transcripts are in `results/sdk-datastore-python-*.json` (over gRPC) and
`results/sdk-datastore-python-http-*.json` (over HTTP), one pair per section.

`sdk_datastore_java/` runs the same sections through the Java client library of Datastore,
`com.google.cloud:google-cloud-datastore` 3.7.0 (`pom.xml`; Java 21 and Maven), with GQL steps
besides. It speaks gRPC, its default, or HTTP (REST with protobuf bodies) with `TRANSPORT=http`:

```sh
cp -R tools/oracle/sdk_datastore_java "$tmp/" && (cd "$tmp/sdk_datastore_java" && \
  DATASTORE_EMULATOR_HOST=127.0.0.1:8086 mvn -q compile exec:java -Dexec.args=entities) > java-entities.json
```

Its transcripts are in `results/sdk-datastore-java-*.json` and `results/sdk-datastore-java-http-*.json`.

`sdk_datastore_go/` runs the same sections through the Go client library of Datastore,
`cloud.google.com/go/datastore` 1.27.0 (pinned in `go.mod` and `go.sum`; Go 1.26 or newer),
over gRPC, the only transport it has:

```sh
(cd tools/oracle/sdk_datastore_go && go build -o "$tmp/sdk_datastore_go" .) && \
  DATASTORE_EMULATOR_HOST=127.0.0.1:8086 "$tmp/sdk_datastore_go" entities > go-entities.json
```

Its transcripts are in `results/sdk-datastore-go-*.json`.

`sdk_datastore_node.mjs` runs the same sections through the Node.js client library of Datastore,
`@google-cloud/datastore` (11.3.0 for the transcripts in `results/`), over gRPC: its REST
fallback looks for credentials even against an emulator.

```sh
cd "$(mktemp -d)" && npm i @google-cloud/datastore && cp <repo>/tools/oracle/sdk_datastore_node.mjs .
DATASTORE_EMULATOR_HOST=127.0.0.1:8086 node sdk_datastore_node.mjs entities > node-entities.json
```

Its transcripts are in `results/sdk-datastore-node-*.json`.

`sdk_ios/` runs the same four sections through the client SDK of Apple platforms,
`FirebaseFirestore` 13.0.1 from firebase-ios-sdk (pinned in `Package.swift` and
`Package.resolved`), built for macOS with Swift 5.9 or newer: the same client as on iOS, over the
same gRPC core (#267). Reads go to the server and the cache stays in memory, so a transcript
holds what the emulator answered; with the steps a client SDK offers (no create, no
preconditions, no `listDocuments`), a value of each type, the filters and aggregations, a
transaction that meets another client's write and is retried, and query and document listeners
fed by a second client, left idle for 95 seconds once (firebase-tools#11238). Build it outside
the repository:

```sh
cp -R tools/oracle/sdk_ios "$tmp/" && (cd "$tmp/sdk_ios" && swift build -c release)
"$tmp/sdk_ios/.build/release/sdk_ios" documents > ios-documents.json
```

Its transcripts are in `results/sdk-ios-*.json`. `.github/workflows/ios-sdk.yml` runs it against
hidane monthly and compares with the committed hidane transcripts.

`sdk_android/` runs the same steps through the Android SDK, `com.google.firebase:firebase-firestore`
27.0.0 (pinned in `app/build.gradle.kts`), as an instrumentation test in an Android emulator
(#266). It builds with Gradle 9.8 and JDK 21, reaches the emulator under test on the host as
10.0.2.2, and takes the section and the port as instrumentation arguments; the transcript is
written to the app's external files directory:

```sh
cp -R tools/oracle/sdk_android "$tmp/" && (cd "$tmp/sdk_android" && gradle :app:assembleDebug :app:assembleDebugAndroidTest)
adb install -r -t "$tmp/sdk_android/app/build/outputs/apk/debug/app-debug.apk"
adb install -r -t "$tmp/sdk_android/app/build/outputs/apk/androidTest/debug/app-debug-androidTest.apk"
adb shell am instrument -w -e section documents -e port 8089 dev.hidane.sdkandroid.test/androidx.test.runner.AndroidJUnitRunner
adb pull /sdcard/Android/data/dev.hidane.sdkandroid/files/documents.json android-documents.json
```

Its transcripts are in `results/sdk-android-*.json`. `.github/workflows/android-sdk.yml` runs it
against hidane monthly in an Android emulator (`ci.sh`) and compares with the committed hidane
transcripts.
