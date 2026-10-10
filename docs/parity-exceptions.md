# Parity exceptions

hidane treats the official Cloud Firestore emulator as the oracle: the same test suites run
against both and the differences are published. This page lists the places where hidane
**intentionally** behaves differently, and the places where the official emulator itself
diverges from production Firestore so that a parity target has to be chosen.

Status: Phase 0 draft. Entries will be tagged in the conformance suite (issue #45).

## Intentional differences (hidane will not reproduce these)

| Official emulator behaviour | hidane | Why |
|---|---|---|
| Batch writes slow down in proportion to the number of stored documents while a listener is attached (reproduced in Phase 0; firebase-tools [#3477](https://github.com/firebase/firebase-tools/issues/3477)). The cause shows on the Listen stream: after its first answer, every write to a watched collection, even one that changes nothing, is answered with `RESET` and the whole result again | A commit costs O(number of changed documents): Listen sends only the documents that entered, changed in or left each target, then one `NO_CHANGE`, as production does, and nothing for writes that change nothing. 100,000 documents with a whole-collection listener: 4.9 s, flat per batch (`results/listen-batch-writes-hidane.csv`). The SDKs build the same snapshots (`results/sdk-listen-*.json`) | Performance bug, not a semantic contract. See [why.md](why.md) §3 |
| A batch that changes a limit query can reach a web SDK listener as two snapshots, the first one showing a document that the same commit deleted | One snapshot per commit | Bug (snapshots must be atomic) |
| `POST /reset` tells attached listeners nothing, and they receive no later changes either | Listeners see every document go, then later writes as usual | Bug (listeners keep stale data) |
| A cleared database (`DELETE /emulator/v1/…/documents`, rules-unit-testing's `clearFirestore()`) reaches a web SDK query listener as several snapshots | One snapshot with every document gone | Follows from answering with `RESET` |
| After a reconnect, the web SDK sees a resumed query in several snapshots: the first still shows the documents deleted or changed away while it was offline, later ones drop them one by one | One snapshot with everything that changed since the token | Follows from resuming with `RESET` (see the production table below); the SDK then looks up each document it can no longer place |
| Adding a Listen target with an ID that is in use ends the stream with `UNKNOWN` and no message | Ends it with `INVALID_ARGUMENT` "Target ID … is already in use." | Bug (an internal error) |
| A Listen query whose parent names another database reads the stream's database | Refuses the target (`REMOVE` with an `INVALID_ARGUMENT` cause) | Bug |
| Rules that reference `resource.data` are evaluated twice and the first evaluation always errors (firebase-tools [#6252](https://github.com/firebase/firebase-tools/issues/6252), open since 2023) | Evaluate once | Bug |
| Transaction conflict detection regressed in v1.22.0 (firebase-tools [#11241](https://github.com/firebase/firebase-tools/issues/11241)) | Detect conflicts: every document a transaction reads and every collection ID it lists stays locked until it ends; queries will lock the same way (#21) | Bug; parity target is the documented contract |
| A transaction ID is spent on every non-transactional request too, and the count restarts at 1 on `POST /reset` | Same 9-byte encoding, counted per database, but only transactions take an ID and the count continues across a reset | IDs are opaque to clients. Continuing the count means an ID from before a reset reads as expired instead of naming a new transaction |
| A commit or read with a well-formed transaction ID that was never issued (or was forgotten by a reset) fails with `UNKNOWN` and no message; a rollback answers `INVALID_ARGUMENT` "Transaction is invalid or expired." | `INVALID_ARGUMENT` "Transaction is invalid or expired." everywhere; an ID that has ended answers `ABORTED` "The referenced transaction has expired or is no longer valid.", as on the official emulator | Bug (an internal error) |
| A write that starts waiting for a lock shortly before the holding transaction expires still times out after 2 s: the expiry is only noticed at the next request | The write proceeds when the holding transaction expires | Timing detail with the same outcome a moment later |
| The Write stream fails with `UNKNOWN` and no message unless the call carries `google-cloud-resource-prefix` or `x-goog-request-params` metadata (every SDK sends one) | Accepts the stream either way; the database comes from the first request | Bug (an internal error) |
| The Write stream handshake accepts any database name | `INVALID_ARGUMENT` for a malformed name, as other RPCs answer | Bug |
| A Write stream write to a document of another database is acknowledged, with a write result and a commit time, and stored nowhere | `INVALID_ARGUMENT` "Document "…" is not in database "…".", as Commit answers | Bug (silent data loss) |
| Document fields come back in the order they were written, nested maps included | In name order | Open, #108 |
| An export lists its documents in no particular order, and their fields in the order they were written | Documents in name order, fields in name order | The same documents either way ([export-format.md](export-format.md)) |
| A seed's documents of another database are kept in the seeded database, where no read finds them, and written into its exports | Left out | Bug (unreadable data that resurfaces in exports) |
| `find_nearest` reads a vector field path such as `m.v` as one field named `m.v` | Resolves the path (the field `v` of the map `m`), as production and the SDKs do; `` `m.v` `` names the field `m.v` | Bug (nested vector fields could not be searched) |
| A gRPC message over 100 MiB fails with `RESOURCE_EXHAUSTED` "gRPC message exceeds maximum size 104857600: …" | Same limit, but tonic answers `OUT_OF_RANGE` "Error, decoded message length too large: …" | Wording of a transport error |
| Server reflection lists `google.firestore.v1beta1.Firestore` (and cannot describe it), `google.firestore.emulator.v1.FirestoreEmulator` and `google.datastore.v1.Datastore` | Lists `google.firestore.v1.Firestore` and reflection; v1beta1 answers without being listed; the emulator admin API (#36) and Datastore mode (#81) are not served yet | Reflection is for tools; v1beta1 has no descriptors here |
| A document's size is its Datastore entity's encoded size: two string fields pass up to 1,048,486 bytes together | Firestore's storage size rules (name, field names and values, plus 32 bytes), which allow a few tens of bytes more | Same limit and message; the exact boundary is an internal encoding |
| WebChannel handshakes and forward-channel answers are chunked and the connection is closed after each | A content length, and the connection stays open | Transport detail; the client reads the same frames |
| A long-polling back channel puts `noop`s around its first data depending on timing, and ends after one message | A `noop` while waiting; ends after the messages queued when data comes | Timing; each message arrives once, in order |
| WebChannel sessions are never dropped: each one keeps a thread (firebase-tools [#11124](https://github.com/firebase/firebase-tools/issues/11124)) | A session with no back channel and no request for 5 minutes is dropped | Bug (resource leak) |
| WebChannel answers carry no `Access-Control-Allow-Methods` | The CORS headers of every HTTP path, that one included | Harmless |
| JWT segments in `Authorization` are read with a lenient JSON parser (an unquoted key such as `{a:1}` passes) | Strict JSON: such a token is `invalid jwt` | Clients only send JSON |
| `DELETE /emulator/v1/…/documents/{path}` with an empty path segment names the parent in its error (`Document parent name "…/c/" has invalid trailing "/".`) | Names the whole path (`Resource name "…" lacks a resource id at index ….`), same status | Wording only |
| RunAggregationQuery responses carry `done: true`, a field the published protos do not define (visible over REST) | Not sent over gRPC, where clients built from the published protos would drop it; the REST layer (#34) will add it | Not part of the published API |
| REST `GET` of a document or collection with `?transaction=` never answers (gRPC works) | Follows the gRPC behaviour (#34) | Bug |
| A REST query parameter that does not parse (`pageSize=abc`, `showMissing=maybe`) leaves the request unanswered | `400` "Payload isn't valid for request.", the answer to a body that does not parse | Bug |
| A `:listCollectionIds` body that names a `parent` as the path does leaves the request unanswered | The path wins | Bug |
| REST page tokens (`nextPageToken`) are an encoded internal message | The last name returned, encoded; both are opaque to clients | Internal format |
| Masked fields come back in an internal hash order, and aggregation results in request order | In name order | Open, #108 |
| iOS clients receive `GOAWAY too_many_pings` after about 90 s (firebase-tools [#11238](https://github.com/firebase/firebase-tools/issues/11238)) | Keepalive settings that do not trip the client | Bug |
| The `issues[].severity` returned by `:securityRules` is a string (`"ERROR"`) while firebase-tools compares it against a numeric enum, so invalid rules still print "Rules updated." | hidane returns the same string the official emulator does; the CLI-side comparison is an upstream bug | Keep wire parity; report upstream |
| gRPC reflection lists services but `grpcurl describe` fails on a `google.api.api_visibility` extension | Full reflection that works with `grpcurl` | Developer convenience |
| `GET //` never answers (the connection hangs) | `404 Not Found` | Bug |
| Precondition failures print the emulator's internal Datastore key: `entity already exists: EntityRef[partitionRef=dev~p, path=/c/d]`, and a protobuf text dump of the key for `no entity to update` | Production Firestore's wording, `Document already exists: <name>` and `No document to update: <name>`; same status codes (`ALREADY_EXISTS`, `NOT_FOUND`) | Internal detail; applications see the production wording in production |
| `read_time` older than at least two hours is still served (the exact limit is #86) | Up to one hour old, like production Firestore; older answers `FAILED_PRECONDITION` "The requested 'read_time' is too old." with the official message | Bounded memory for old versions (ADR 0002) |
| The REST `readTime` parameter always fails with "Only timestamps past epoch are supported.", even for the latest commit time (gRPC works) | Follows the gRPC behaviour (#34) | Bug |
| `serverTimestamp()` stores the time the request was received, truncated to milliseconds, a few milliseconds before the commit time | The commit time truncated to milliseconds | Same precision and the same value for every field of a commit; tests cannot depend on which instant inside the request is used |

## Official-emulator behaviour that differs from production (parity target to be decided)

| Area | Official emulator | Production Firestore | Decision |
|---|---|---|---|
| Transactions | Locks, measured in #17: a read-write transaction holds a shared lock on every document it reads (found or not) and on every collection with a collection ID it lists or queries, whatever the parent. A write, in a transaction or not, waits for other transactions' locks for up to 2 s, then fails with `ABORTED` "Transaction lock timeout." (which ends a waiting transaction). An unused transaction expires 60 s after its last request (the [docs](https://docs.cloud.google.com/firestore/native/docs/emulator) say 30 s). `concurrency_mode: OPTIMISTIC` is accepted and ignored. Read-only transactions lock nothing and read the snapshot of their start | Contention is resolved by aborting a transaction with `ABORTED` rather than by a fixed wait, and optimistic concurrency can be selected | Follow the official emulator: same lock scope, waits, expiry and messages (`tests/fixtures/transactions.json`, replayed by `crates/hidane/tests/transactions.rs`) |
| Composite indexes | Not tracked or required | Required for many queries | Follow the emulator (no index enforcement) |
| Listen `once` | Ignored: the target stays after `CURRENT` | The target is removed once it is current | Follow the emulator, measured in #18 (no SDK sets it) |
| Resuming a Listen target (`resume_token` / `read_time`) | Starts over: `ADD`, `RESET`, the whole result | Sends only what changed since the token | Follow production (#19): what entered, changed in or left the target since the token. From before the versions kept (one hour), the documents updated since then and an `ExistenceFilter` with the count. Tokens from before the process started or the store was reset, or that do not parse, start over with `RESET` |
| Query cursors | At most one value per explicit `order_by`; values for the implicit orderings (inequality fields, `__name__`) fail with "Cursor has too many values." | Cursors are positions in the query's full ordering | Follow the emulator, measured in #21. The SDKs add explicit orderings when they build a cursor from a snapshot, so they are not affected |
| Descending document order | `order_by __name__ desc` as the only ordering, without a filter on another field, fails with `FAILED_PRECONDITION` "Firestore does not support descending key scans" | Served | Follow the emulator, measured in #21 |
| Limits | Per document, with Datastore's messages: 1,048,487 bytes for a string or bytes value, 1,500 bytes for a field name (nested names count their dotted path, an array as `array`) and for a collection or document ID, 20 levels of maps and arrays, 1 MiB for the document. Not per request: a 64 MB gRPC commit is accepted (firebase-tools [#8649](https://github.com/firebase/firebase-tools/issues/8649)); gRPC messages up to 100 MiB, REST bodies up to 16 MiB | Enforced, per document and per request | Follow the emulator (#121, #25), except the document-size boundary below |
| `PartitionQuery` | 501 `UNIMPLEMENTED` | Supported | Follow the emulator (issue #26) |
| HTTP/2 keepalive pings | A client pinging an idle connection every 10 s is sent `GOAWAY` (`ENHANCE_YOUR_CALM`, `too_many_pings`) after 30 s, likely the iOS SDK's 90 s GOAWAY (firebase-tools [#11238](https://github.com/firebase/firebase-tools/issues/11238)) | Pinging clients stay connected | Follow production (#25): pings are answered and never close the connection (`results/keepalive-*.txt`) |
| `ExecutePipeline` | Only with `--database-edition enterprise`; on a standard database `INVALID_ARGUMENT` "ExecutePipeline requires the Database Edition to be \`enterprise\`." | Enterprise edition only | Follow the emulator on standard databases (#26); enterprise pipelines are not implemented in hidane yet (`UNIMPLEMENTED`, #80) |
| `explain_options` (RunQuery, RunAggregationQuery) | Ignored: the same results, no explain metrics | Explain metrics (plan summary, and execution stats with `analyze`) | Follow the emulator (#26) |
| `find_nearest` | Runs vector search: the query's results nearest first (largest dot product first), ties in the query's order, NaN distances last; the distance field is a literal name (`a.b` is one field); a NaN in the query vector is accepted | Runs vector search | Follow the emulator (#118, `tests/fixtures/find_nearest.json`) |
| Persistence | In-memory; export / import only | Durable | Follow the emulator (ADR 0002) |
| Import | `createTime` / `updateTime` are overwritten with the import time; `--seed_from_export` seeds every project's databases on first access, again after `POST /reset` | n/a | Follow the emulator ([export-format.md](export-format.md)) |

## Not in scope (neither emulator nor hidane)

- `google.firestore.admin.v1` (index, field, database, backup management): the official emulator does not expose it. hidane answers `UNIMPLEMENTED`.
- TTL, point-in-time recovery, bundles: no emulator support.
- Realtime Database, Cloud Storage, Auth emulators.

## How to read the parity table (once it exists)

The conformance suite will publish, per RPC and scenario, whether hidane and the official emulator
returned the same result at three levels: L0 status code, L1 status code + HTTP status, L2 exact
error message (only required for a small Tier 3 set). Entries listed on this page are tagged so they
do not count as regressions.
