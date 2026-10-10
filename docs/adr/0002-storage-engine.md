# ADR 0002: Storage engine

- **Status**: Accepted (option D, validated by #27)
- **Date**: 2026-10-10

## Context

The official emulator is in-memory; persistence exists only through export / import (a Cloud
Datastore-style tree: `*.overall_export_metadata` plus LevelDB-log files containing EntityProto
records). hidane's storage layer has these constraints:

| Constraint | Detail | Source |
|---|---|---|
| Consistent snapshots | Listen guarantees an empty-`target_ids` `TargetChange` with a monotonically increasing `read_time` per consistent snapshot, so changes must be collected per commit | `google/firestore/v1/firestore.proto` (Listen / TargetChange docs); firebase-js-sdk `remote/remote_store.ts` |
| `resume_token` | On reconnect the server sends only changes after the token. Without a deletion history it must fall back to `ExistenceFilter.count` | `firestore.proto` (`Target.resume_token`), `write.proto` (`ExistenceFilter`) |
| Type ordering | Queries order values by the 11-step type order (Null < Bool < numbers < Date < String < Bytes < Reference < GeoPoint < Array < Vector < Map); NaN sorts below -Infinity. Encoding this into keys keeps indexes simple | [Data types: value type ordering](https://firebase.google.com/docs/firestore/manage-data/data-types) |
| Transactions | Official emulator: "simple lock", released after at most 30 s. Production: optimistic concurrency. A parity target has to be chosen | [Emulator docs](https://docs.cloud.google.com/firestore/native/docs/emulator) |
| Performance | Official emulator writes degrade with total document count while a listener is attached; hidane targets O(changed documents) per commit | `results/degradation-logs/` |
| Memory | Official: 456 MiB after 1,000 documents, 2.18 GiB at 500k (JVM). hidane target: < 50 MiB idle | `results/memory-official-v1.22.0.txt` |
| Export / import | Read and write the official format; import overwrites `createTime` / `updateTime` with the import time | Observed on v1.22.0 |

## Options

### A. In-memory (ordered map + version counter)

- `BTreeMap<EncodedKey, Vec<Version>>`-style store; each commit gets a monotonically increasing sequence number (the source of `read_time`); keep the last N commits as a change log for `resume_token`
- Pro: fastest, no dependencies, same lifecycle as the official emulator (data gone on exit; export / import covers persistence)
- Con: everything lives in memory (as with the official emulator); change-log retention needs a policy
- Measured in #27: 473 MiB at 1M documents (see below)

### B. redb (pure-Rust embedded KV store, MVCC)

- Pro: persistence for free; MVCC snapshot reads; single file
- Con: introduces persistence the official emulator does not have, overlapping with `--import` / `--export-on-exit`; file open cost vs the < 100 ms startup target is unverified
- Open: Listen notifications still need A's change log on top

### C. SQLite (rusqlite, bundled)

- Pro: mature; parts of queries could map to SQL
- Con: Firestore type ordering, array operators and collection-group queries need custom key encoding or virtual tables; a C dependency weakens the single-binary story (static linking is possible)

### D. A behind a storage trait, B pluggable later

- Start with A; add B as a backend if persistence is wanted
- Con: up-front abstraction cost

## Decision

**Option D.** The in-memory engine (`hidane_core::store::MemoryStore`) sits behind the
`hidane_core::store::Store` trait; a persistent engine can be added later without touching the
RPC layers.

The model the trait fixes, and why:

- **Versions keyed by commit time.** Every document keeps its versions, each stamped with the
  commit time in microseconds (Firestore's timestamp precision, #28). A read names a read time
  and sees the newest version at or before it. Commit times are unique and strictly increasing
  per database (`max(clock, last commit + 1 µs)`), so a read time is also a position in history.
  This gives consistent snapshots for queries, read-only transactions, `read_time` reads and
  Listen.
- **Retention.** Overwritten versions stay readable for one hour by default (Firestore's limit
  for stale reads); older ones are dropped when the document is next written. The official
  emulator's own limit is still to be measured (#86).
- **Commits are closures over a `WriteBatch`.** The commit time is fixed before the closure runs
  (server timestamps and `update_time` need it), reads inside the batch see the batch's writes,
  and an error from the closure applies nothing. Preconditions, update masks and transforms are
  computed by the caller inside the same critical section.
- **Commits return their changes** (old and new version of each touched document), so change
  notification is O(changed documents) and never re-reads the database (#30).
- **Keys** are the order-preserving path encoding from #28: the document map iterates in
  `__name__` order, a collection is one key range (descendants are skipped by seeking past
  their subtree), and a collection-group index (`collection id ++ path`) makes every group one
  key range in full-path order.
- **Fields are stored protobuf-encoded** and decoded on read.

`resume_token` and the change log for Listen are designed with #19 on top of this.

## Memory (#27)

Release build, macOS arm64, documents shaped like the Phase 0 baseline (`users/{i}` =
`{mykey: "my data", myid: i}`, 500 per commit), measured in-process with
`cargo run --release -p hidane-core --example rss` (raw output: `results/storage-memory-rss-hidane.csv`):

| Documents | hidane RSS | Official emulator RSS (`results/`, Phase 0) |
|---|---|---|
| 1,000 | 2.7 MiB | 456 MiB |
| 100,000 | 49 MiB | — |
| 500,000 | 238 MiB | 1,633 MiB at the end of the run |
| 1,000,000 | 473 MiB | not measured |

About 500 bytes per document. The first implementation used 1.9 GiB at 1M; three changes
brought it down: generated `Value` shrank from 72 to 32 bytes by boxing the pipeline-only
variants, fields are stored encoded (a decoded two-field map reserved room for eleven
entries, ~1 KB), and a document's path is shared by all its versions. What remains is mostly
the path (segment strings), the two map keys and per-allocation overhead; packing the path into
one allocation is the next step if memory matters again.

## Cost of adding redb later

| Part | Effort |
|---|---|
| Trait boundary | None. RPC layers only see `Store`. |
| Keys and values | None. Keys are already byte-comparable (redb orders `&[u8]` keys lexicographically) and fields are already encoded bytes. |
| Version history | redb's MVCC gives a snapshot of *the current* state per read transaction, not reads at an arbitrary past time. Versions need their own key layout (`path key ++ inverted commit time`) and pruning, re-implementing `Record::at` / `Record::prune` over a key range. |
| Collection-group index | A second table with the same keys. |
| Concurrency | redb has one writer and many readers, which matches the per-database write lock. |
| Startup | Opening a file must stay inside the < 100 ms startup target; persistence would be opt-in, since the official emulator is in-memory too. |
| Tests | The `MemoryStore` unit tests are written against the trait methods; turning them into a suite generic over `Store` lets a second engine reuse them. |

Estimate: a few hundred lines plus the shared test suite. Not planned for v0.1.

## Consequences

- The storage epic and the Listen `resume_token` issue (#19) build on this model
- The transaction parity target is decided in its own issue (#17); the commit closure supports either model. #17 chose the official emulator's lock model, implemented above the store (`crates/hidane/src/firestore/transactions.rs`); the store itself did not change
- Type-ordered key encoding (#28) is the key format of the engine
