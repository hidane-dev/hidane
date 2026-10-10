# Parity exceptions

hidane treats the official Cloud Firestore emulator as the oracle: the same test suites run
against both and the differences are published. This page lists the places where hidane
**intentionally** behaves differently, and the places where the official emulator itself
diverges from production Firestore so that a parity target has to be chosen.

Status: Phase 0 draft. Entries will be tagged in the conformance suite (issue #45).

## Intentional differences (hidane will not reproduce these)

| Official emulator behaviour | hidane | Why |
|---|---|---|
| Batch writes slow down in proportion to the number of stored documents while a listener is attached (reproduced in Phase 0; firebase-tools [#3477](https://github.com/firebase/firebase-tools/issues/3477)) | A commit costs O(number of changed documents) | Performance bug, not a semantic contract. See [why.md](why.md) §3 |
| Rules that reference `resource.data` are evaluated twice and the first evaluation always errors (firebase-tools [#6252](https://github.com/firebase/firebase-tools/issues/6252), open since 2023) | Evaluate once | Bug |
| Transaction conflict detection regressed in v1.22.0 (firebase-tools [#11241](https://github.com/firebase/firebase-tools/issues/11241)) | Detect conflicts | Bug; parity target is the documented contract |
| iOS clients receive `GOAWAY too_many_pings` after about 90 s (firebase-tools [#11238](https://github.com/firebase/firebase-tools/issues/11238)) | Keepalive settings that do not trip the client | Bug |
| The `issues[].severity` returned by `:securityRules` is a string (`"ERROR"`) while firebase-tools compares it against a numeric enum, so invalid rules still print "Rules updated." | hidane returns the same string the official emulator does; the CLI-side comparison is an upstream bug | Keep wire parity; report upstream |
| gRPC reflection lists services but `grpcurl describe` fails on a `google.api.api_visibility` extension | Full reflection that works with `grpcurl` | Developer convenience |
| `GET //` never answers (the connection hangs) | `404 Not Found` | Bug |
| Precondition failures print the emulator's internal Datastore key: `entity already exists: EntityRef[partitionRef=dev~p, path=/c/d]`, and a protobuf text dump of the key for `no entity to update` | Production Firestore's wording, `Document already exists: <name>` and `No document to update: <name>`; same status codes (`ALREADY_EXISTS`, `NOT_FOUND`) | Internal detail; applications see the production wording in production |
| `read_time` older than at least two hours is still served (the exact limit is #86) | Up to one hour old, like production Firestore; older answers `FAILED_PRECONDITION` "The requested 'read_time' is too old." with the official message | Bounded memory for old versions (ADR 0002) |
| The REST `readTime` parameter always fails with "Only timestamps past epoch are supported.", even for the latest commit time (gRPC works) | Will follow the gRPC behaviour when REST lands (#34) | Bug |
| `serverTimestamp()` stores the time the request was received, truncated to milliseconds, a few milliseconds before the commit time | The commit time truncated to milliseconds | Same precision and the same value for every field of a commit; tests cannot depend on which instant inside the request is used |

## Official-emulator behaviour that differs from production (parity target to be decided)

| Area | Official emulator | Production Firestore | Decision |
|---|---|---|---|
| Transactions | "Simple lock" model, locks released after at most 30 s ([docs](https://docs.cloud.google.com/firestore/native/docs/emulator)) | Optimistic concurrency, `ABORTED` on conflict | Open, issue #17 |
| Composite indexes | Not tracked or required | Required for many queries | Follow the emulator (no index enforcement) |
| Limits (document size, batch size, …) | Not enforced; a 12 MB batch is accepted (firebase-tools [#8649](https://github.com/firebase/firebase-tools/issues/8649)) | Enforced | Open; likely follow the emulator, maybe opt-in enforcement |
| `PartitionQuery` | 501 `UNIMPLEMENTED` | Supported | Follow the emulator (issue #26) |
| `ExecutePipeline` | Only with `--database-edition enterprise` | Enterprise edition only | Follow the emulator (`UNIMPLEMENTED` on standard) |
| Persistence | In-memory; export / import only | Durable | Follow the emulator (ADR 0002) |
| Import | `createTime` / `updateTime` are overwritten with the import time | n/a | Follow the emulator |

## Not in scope (neither emulator nor hidane)

- `google.firestore.admin.v1` (index, field, database, backup management): the official emulator does not expose it. hidane answers `UNIMPLEMENTED`.
- TTL, point-in-time recovery, bundles: no emulator support.
- Realtime Database, Cloud Storage, Auth emulators.

## How to read the parity table (once it exists)

The conformance suite will publish, per RPC and scenario, whether hidane and the official emulator
returned the same result at three levels: L0 status code, L1 status code + HTTP status, L2 exact
error message (only required for a small Tier 3 set). Entries listed on this page are tagged so they
do not count as regressions.
