# Oracle scripts

Small scripts that ask the **official** Firestore emulator how it behaves, so hidane can
reproduce the answer. Each one writes a fixture that a hidane test reads; the fixture is
committed, the official jar is not (it may not be redistributed, see `docs/parity-exceptions.md`).

Python 3 standard library only; `harness.py` holds the shared request and recording helpers. Run them against a freshly started official emulator:

```sh
java -jar cloud-firestore-emulator-v1.22.0.jar --host 127.0.0.1 --port 8086 &
python3 -I tools/oracle/value_order.py 127.0.0.1:8086 > crates/hidane-core/tests/fixtures/value_order.json
curl -X POST http://127.0.0.1:8086/shutdown
```

| Script | Question | Fixture | Test |
|---|---|---|---|
| `value_order.py` + `value_order_cases.json` | How are values ordered, which values are equal, and how are document names ordered in a collection group? | `crates/hidane-core/tests/fixtures/value_order.json` | `crates/hidane-core/tests/official_order.rs` |
| `transforms.py` | How are field transforms applied (server time, increment, maximum / minimum, array union / remove), in which order, with which results? | `crates/hidane/tests/fixtures/transforms.json` | `crates/hidane/tests/transforms.rs` |
| `document_writes.py` | How do document reads and writes answer, including errors, preconditions, masks, paging and admin checks? | `crates/hidane/tests/fixtures/document_writes.json` | `crates/hidane/tests/documents.rs` |
| `queries.py` | What does RunQuery return for every filter operator on every value type, composite filters, implicit ordering, cursors, offsets, projections, collection groups and queries over every collection, and with which errors? | `crates/hidane/tests/fixtures/queries.json` | `crates/hidane/tests/queries.rs` |
| `aggregations.py` | What do `count`, `sum` and `avg` return at the edges (integer overflow, NaN, infinities, precision, non-numbers, empty sets), how do they interact with `offset` / `limit`, and which aggregation lists are rejected? | `crates/hidane/tests/fixtures/aggregations.json` | `crates/hidane/tests/aggregations.rs` |
| `write_stream.mjs` | How does the Write stream answer: handshake, tokens, pipelined batches, empty requests, errors, resumption, locks? Node.js with `@grpc/grpc-js` and `@grpc/proto-loader`, run like the SDK scripts below with the repository's `proto/` directory as its second argument | `crates/hidane/tests/fixtures/write_stream.json` | `crates/hidane/tests/write_stream.rs` |
| `rest.py` | What does every REST binding answer, byte for byte: JSON layout, ProtoJSON values, streams as arrays, errors, 404s, lenient input? | `crates/hidane/tests/fixtures/rest.json` | `crates/hidane/tests/rest.rs` |
| `list_documents.py` | What does ListDocuments list without a collection ID, how do page tokens continue across collections, which checks come first, and what do REST paths with a trailing slash mean? Needs `grpcurl` on `PATH` | `crates/hidane/tests/fixtures/list_documents.json` | `crates/hidane/tests/list_documents.rs` |
| `limits.py` | Which document limits are enforced (value, name, ID, depth, size), with which messages, in which order? | `crates/hidane/tests/fixtures/limits.json` | `crates/hidane/tests/limits.rs` |
| `grpc_settings.py` | Which gRPC services answer, is v1beta1 the same service as v1, and how large may gRPC and REST requests be? Needs `grpcurl` on `PATH`; sends requests of up to 105 MB | `crates/hidane/tests/fixtures/grpc_settings.json` | `crates/hidane/tests/grpc_settings.rs` |
| `find_nearest.py` | How does `find_nearest` rank, filter, tie-break and validate, how does it combine with aggregations, and which vector values can be stored? | `crates/hidane/tests/fixtures/find_nearest.json` | `crates/hidane/tests/find_nearest.rs` |
| `unsupported.py` | What does the official emulator ignore or refuse: `explain_options`, ExecutePipeline on a standard database, PartitionQuery? Needs `grpcurl` on `PATH` | `crates/hidane/tests/fixtures/unsupported.json` | `crates/hidane/tests/unsupported.rs` |
| `auth.py` | How is the `Authorization` header read: administrators, users and anonymous callers, which tokens fail and how, and where each RPC, stream and REST endpoint reads it among its other checks? Needs `grpcurl` on `PATH` | `crates/hidane/tests/fixtures/auth.json` | `crates/hidane/tests/auth.rs` |
| `cors.py` | How does every HTTP path answer CORS: `Origin` reflection, credentials, methods, preflights on any path, requested headers, Private Network Access? | `crates/hidane/tests/fixtures/cors.json` | `crates/hidane/tests/cors.rs` |
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
with their tokens) while the Admin SDK writes; its transcripts are in
`results/sdk-listen-resume-*.json`.

`sdk_clear.mjs` clears data the ways test suites and the Emulator UI do (recursive delete,
rules-unit-testing's `clearFirestore()`, `POST /reset`; needs `npm i @firebase/rules-unit-testing`)
with listeners attached; its transcripts are in `results/sdk-clear-*.json`.

`sdk_auth.mjs` reads and writes as a rules-unit-testing user (unsigned mock token), a signed-out
user, with rules disabled (`Bearer owner` from the web SDK) and through the Admin SDK; its
transcripts are in `results/sdk-auth-*.json`.

`sdk_find_nearest.mjs` writes vectors through the Admin SDK and the web SDK and runs the Admin
SDK's `findNearest` with each distance measure; its transcripts are in
`results/sdk-find-nearest-*.json`.

`keepalive.mjs` pings an idle gRPC connection every 10 s and prints whether the server answers
with `GOAWAY`; its output is in `results/keepalive-*.txt`.

`sdk_rest.mjs` drives the REST surface through the web SDK's Lite build (`firebase/firestore/lite`);
its transcripts are in `results/sdk-rest-*.json`.

`sdk_transactions.mjs` does the same for `runTransaction` (server transactions, retries on
`ABORTED`, lock waits), and `sdk_web_transactions.mjs` for the web SDK (`npm i firebase`), whose
transactions use preconditions and `verify` writes instead. Their transcripts are in
`results/sdk-transactions-*.json` and `results/sdk-web-transactions-*.json`.
