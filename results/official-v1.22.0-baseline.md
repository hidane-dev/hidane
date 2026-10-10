# Official emulator baseline (Phase 0)

Numbers measured on 2026-10-09 against `cloud-firestore-emulator-v1.22.0.jar`
(SHA256 `9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c`). The summary and its
interpretation are in [`docs/why.md`](../docs/why.md).

The raw logs and the measurement scripts were tied to one workstation (absolute paths, local
tooling) and are not published; the values below are copied from them. A re-measurement with a
public harness on a quiet runner is tracked in #50, and the automated startup and memory
harnesses in #47 and #48.

**Caveat**: other heavy jobs were running during every run (load average 12–58). Treat absolute
values as indicative; relative comparisons are the point.

## Environment

| | |
|---|---|
| Machine | macOS on Apple silicon (M1 Max, 10 cores, 64 GiB) |
| Java | OpenJDK 24.0.2 (the jar needs Java 21 or newer: class-file version 65) |
| firebase-tools | 15.33.0 on Node.js 22.22.3 |
| Tools | hyperfine 2.0.0, grpcurl 1.9.4 |

## Startup

Time from starting the process until TCP connections to the port are accepted.

| How | Runs (ms) | Mean |
|---|---|---|
| `java -jar … --host 127.0.0.1 --port 8090` | 1167.2 (first start), then 801.5, 757.9, 788.5, 743.5, 755.0, 688.3, 688.6, 776.1, 696.1, 709.0 | 740.5 ms over the 10 runs after the first |
| `firebase emulators:start --only firestore` | 3140.2, 2249.1, 2370.9, 2689.1, 2236.0 (after one unrecorded warm-up run) | 2537.1 ms |

firebase-tools started the emulator with this command line (the jar lives in its cache
directory):

```
java -Dgoogle.cloud_firestore.debug_log_level=FINE -Duser.language=en -jar <cache>/cloud-firestore-emulator-v1.22.0.jar --host 127.0.0.1 --port 8091 --websocket_port 9150 --database-edition standard --project_id demo-hidane --single_project_mode true
```

## Memory

| When | RSS |
|---|---|
| The port opens | 134 MiB (137,520 KiB) |
| After 1 s idle | 139 MiB (141,840 KiB) |
| After 5 s idle | 133 MiB (136,464 KiB) |
| After 10 s idle | 95 MiB (97,264 KiB) |
| After writing 1,000 documents (2 commits of 500) | 456 MiB (467,168 KiB) |
| 5 s later | 423 MiB (432,960 KiB) |
| Peak during the 500,000-document run below | 2,229 MiB, 2.18 GiB (2,282,816 KiB) |
| At the end of that run | 1,633 MiB (1,671,984 KiB) |

JVM defaults on that machine: G1, initial heap 1 GiB, maximum heap 16 GiB (25 % of physical RAM),
37 threads when idle. The first commit of 500 writes took 1,715 ms, the second 259 ms.

## Single-request latency

hyperfine, 20 runs each, one client process per request (so each time includes starting the
client); the baselines show the client's own cost.

| Request | Mean | Median | Min | Max |
|---|---|---|---|---|
| REST PATCH, one document (`curl`) | 25.8 ms | 25.4 ms | 21.1 ms | 33.0 ms |
| REST GET, one document (`curl`) | 21.5 ms | 20.9 ms | 19.2 ms | 25.9 ms |
| gRPC Commit, one write (`grpcurl`) | 37.5 ms | 36.8 ms | 35.3 ms | 43.3 ms |
| gRPC GetDocument (`grpcurl`) | 35.9 ms | 35.2 ms | 33.2 ms | 43.4 ms |
| Baseline: `curl GET /` | 19.4 ms | 18.1 ms | 16.1 ms | 38.3 ms |
| Baseline: `grpcurl list` through reflection | 20.8 ms | 20.8 ms | 18.9 ms | 25.9 ms |
| Baseline: `curl --version` (process start only) | 15.1 ms | 14.9 ms | 13.5 ms | 16.8 ms |
| Baseline: `grpcurl -version` (process start only) | 15.9 ms | 16.1 ms | 14.2 ms | 16.9 ms |
| gRPC Commit with `-proto` (local proto files), separate run | 38.7 ms | 38.2 ms | 36.4 ms | 44.4 ms |

## gRPC reflection

`grpcurl list` works through the emulator's reflection service and lists
`google.datastore.v1.Datastore`, `google.firestore.emulator.v1.FirestoreEmulator` and
`google.firestore.v1.Firestore`. `grpcurl describe` and invoking a method through reflection fail
(grpcurl 1.9.4):

```
Error invoking method "google.firestore.v1.Firestore/Commit": failed to query for service descriptor "google.firestore.v1.Firestore": proto: extension field "google.api.api_visibility" cannot be declared in proto3 unless extended descriptor options
```

so clients must pass the proto files (`-proto`).

## Batch-write degradation (reproduction of firebase-tools#3477)

Batches of 500 writes through the Go client of
[williamhaley/firestore-emulator-slow](https://github.com/williamhaley/firestore-emulator-slow)
(`cloud.google.com/go/firestore` v1.5.0), with and without a Node.js `onSnapshot` listener on the
collection. Every batch is in
[`batch-write-degradation-official-v1.22.0.csv`](batch-write-degradation-official-v1.22.0.csv)
(`run,batch,docs_start,docs_end,elapsed_sec,rate_per_sec,rss_kb,t_rel_sec`).

| Run | Documents | Listener | Total write time | Peak RSS | RSS at the end |
|---|---|---|---|---|---|
| `A-nolistener` | 100,000 | none | 11.0 s | 593 MiB | 564 MiB |
| `A2-nolistener-500k` | 500,000 | none | 42.6 s | 2,229 MiB | 1,633 MiB |
| `B-listener-unlimited` | 100,000 | the whole collection | 121.0 s | 1,282 MiB | 1,196 MiB |
| `C-listener-limit50` | 100,000 | `limit(50)` | 65.5 s | 685 MiB | 685 MiB |

With the unlimited listener, the time per batch grows by about 168 ms per 10,000 documents
already written; with `limit(50)`, by about 70 ms. Without a listener it stays flat at 5–15 ms
per batch.
