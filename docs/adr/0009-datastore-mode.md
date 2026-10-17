# ADR 0009: Datastore mode, a second engine beside Firestore's

- **Status**: Accepted
- **Date**: 2026-10-16
- **Issue**: #81 (sources: #356)

## Context

`gcloud emulators firestore start --database-mode=datastore-mode` starts the official emulator
(`--database-mode datastore-mode`, which it reports as `CLOUD_FIRESTORE_IN_DATASTORE_MODE`) for
projects that use the Cloud Datastore client libraries. hidane refuses that flag today. A first
look at the official emulator v1.22.0 in that mode, over REST, showed this (black-box
observation; the sources are listed in [`clean-room/SOURCES.md`](../clean-room/SOURCES.md)):

- It serves `google.datastore.v1.Datastore` over gRPC and REST: `Lookup`, `RunQuery` (structured
  and GQL), `RunAggregationQuery`, `BeginTransaction`, `Commit`, `Rollback`, `AllocateIds`,
  `ReserveIds`. Reflection lists it beside the Firestore services.
- It refuses the Firestore API, every RPC: `INVALID_ARGUMENT` "Database mode not set to Firestore
  Mode. If using the gCloud CLI, please specify the --database-mode flag."
- Values and their order are Datastore's, not Firestore's. Ascending by a property: null,
  integers and timestamps together (a timestamp as its microseconds), booleans, strings and
  blobs together (as bytes), doubles (NaN last), geo points, keys. An integer never equals a
  double. Each element of an array is indexed on its own; an entity value, an empty array and
  a property marked `excludeFromIndexes` are not indexed, so no filter or order finds them.
  `!=` and `not-in` match null. Projections return index values (`meaning: 18`).
- Keys are paths of kind and name or numeric ID, in a namespace. Numeric IDs are allocated
  scattered: 2⁵² plus a counter's 51 low bits reversed, one counter shared by every kind.
  `__kind__` and `__namespace__` queries list the kinds and namespaces of the entities and of
  every key read or written, there or not.
- An entity's version is its last commit time in microseconds; outside a transaction each
  mutation is committed at a time of its own. Cursors are opaque bytes: a position in the
  values a query ordered by and the key, which another query reads in its own order.
- Transactions lock as Firestore's do in the official emulator: a conflicting commit waits, then
  fails with `ABORTED` "Transaction lock timeout." (after 2 s in a transaction, 60 s outside
  one, `tools/oracle/datastore_locks.py`). Read-only transactions refuse writes and read the
  entities as they were when they began; a read time goes back an hour, as in Firestore Native
  mode (D065), where the official emulator goes back seven days.
- Errors carry their own texts, some of them a key printed as protocol-buffer text
  ("no entity to update: app: …").

## Options

**A. Datastore on Firestore's engine.** Store entities as documents (a kind as a collection, a
numeric ID as a document ID), translate Datastore's queries to Firestore's.

- Reuses the store, the transactions and export.
- But almost nothing about values or queries carries over: Datastore's type order, its index
  model (array elements, unindexed properties), namespaces, numeric IDs, projections of index
  values. Each would be an exception inside the Firestore engine, which a change on either side
  could break. And in Datastore mode the official emulator serves no Firestore request, so the
  two never need to see the same data.

**B. A second engine.** A `datastore` module with its own entity store, value order, query
evaluation and transactions, and its own gRPC service and REST routes. It shares the server,
the port, the process model and the lock design with the Firestore engine, not their data.

- Datastore's semantics are written once, where they belong, and tested against recordings of
  the official emulator in Datastore mode.
- The Firestore engine is untouched: a mistake in Datastore mode cannot reach the Firestore
  mode, which almost every user runs.
- Export and import in Datastore mode would need their own path. Recorded first, as everything
  else, before deciding.

## Decision

**B.** `--database-mode datastore-mode` starts hidane with the Datastore engine in place of the
Firestore one: the Datastore service over gRPC and REST, the Firestore API refused as the
official emulator refuses it. As for Firestore (ADR 0008), each part is recorded from the
official emulator first, then implemented, then replayed in CI. In order:

1. Keys and values, `Lookup`, `Commit` (insert, update, upsert, delete; non-transactional and
   transactional), `BeginTransaction`, `Rollback`, `AllocateIds`, `ReserveIds`, and the refusal
   of the Firestore API.
2. Structured queries (`RunQuery`): kinds, filters (property and key, ancestors, composites),
   orders, projections and `distinctOn`, limits, offsets and cursors; `__kind__` and
   `__namespace__`; then `RunAggregationQuery`.
3. GQL.
4. `--index_file` and `--require_indexes`; how `gcloud emulators firestore start
   --database-mode=datastore-mode` can start hidane; the client libraries run against both.

Cursors are hidane's own bytes: clients pass them back unread, so recordings compare what a
cursor selects, not its bytes.

## Consequences

- `--database-mode datastore-mode` stops being refused once step 1 lands; until step 3 lands,
  `hidane doctor` and the docs say which parts of Datastore mode are served.
- The `DATASTORE_EMULATOR_HOST` the client libraries read points at hidane's port.
- `CLOUD_DATASTORE` (the other value `--database-mode` accepts) stays refused until it is
  recorded and found to differ, or not. Recorded since (`tools/oracle/startup_flags.py`): the
  official emulator refuses it at startup, as it refuses every value but `firestore-native` and
  `datastore-mode`, so hidane refuses it as it does.
