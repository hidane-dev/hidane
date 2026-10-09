# ADR 0002: Storage engine

- **Status**: Draft (undecided)
- **Date**: 2026-10-09

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
- Open: RSS at 1M documents

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

Undecided. Placeholder: **D (start with A behind a trait)**. `resume_token` carries the commit
sequence plus `read_time`; the change log keeps the last N commits (configurable) and anything
older is resynchronised through `ExistenceFilter.count`, which both the JS and Admin SDKs handle.

## Consequences

- The storage epic and the Listen `resume_token` issue reference this ADR
- The transaction parity target is decided in its own issue; A can implement either model
- Type-ordered key encoding is needed by A, B and C alike, so it is designed first
