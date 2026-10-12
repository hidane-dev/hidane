# Benchmark: the official Firestore emulator and hidane, side by side

> **This run is not stable.** The stability checks failed (see Stability below), so its numbers are noisy and must not be cited.

> These parameters are not the canonical ones (bench/canonical.json), so the numbers are not comparable with canonical results.

## Environment

| | |
|---|---|
| Date | 2026-01-01T00:00:00Z |
| Run | nightly, benchmark definitions v1, non-canonical parameters, environment revision 1 |
| Machine | `example`, Example CPU, 2 cores, 4 threads, 8 GiB RAM |
| Cloud | instance type c7i.xlarge, zone us-east-1a, image ubuntu-24.04-amd64-server-example, AMI ami-00000000000000000, EBS gp3 30 GiB |
| OS | Ubuntu 24.04 LTS, kernel 6.8.0-example, x86_64, ext4 filesystem, virtualization: amazon |
| CPU frequency | cpufreq is not exposed (common on virtual machines such as EC2, where the hypervisor controls frequency) |
| hidane | 0.3.0, commit `000000000000`, release build, binary SHA-256 `000000000000…` |
| Engine | `hidane benchmark` 0.3.0, commit `000000000000`, release build |
| Official emulator | 1.22.0, jar SHA-256 `9b6498b7f627…` (the pinned jar), Java 21.0.8 (Eclipse Adoptium), `-Duser.language=en -Duser.country=US` |
| Tools | Node.js 22.22.3, firebase-tools 15.33.0, rustc 1.90.0 (example) |
| Stability | **not stable**: synthetic example: these numbers are not measurements. Load average before the run 0.10 / 0.10 / 0.10, 0.10 after; 7.0 GiB memory available; CPU steal 0.10 % over the run |

## Summary

- In this benchmark environment, median time from process start until the port accepted connections was 20.0 ms for the official emulator and 20.0 ms for hidane.
- In this benchmark environment, median time from process start until the first gRPC request was answered was 20.0 ms for the official emulator and 20.0 ms for hidane.
- In this benchmark environment, median resident memory when idle was 20.0 MiB for the official emulator and 20.0 MiB for hidane.
- In this benchmark environment, median resident memory after writing 1,000 documents was 20.0 MiB for the official emulator and 20.0 MiB for hidane.
- In this benchmark environment, median latency of a single-document read over gRPC was 20.0 ms for the official emulator and 20.0 ms for hidane.
- In this benchmark environment, median latency of a 500-write batch commit was 20.0 ms for the official emulator and 20.0 ms for hidane.
- In this benchmark environment, median time to write 100 documents in commits of 500 was 20.0 ms for the official emulator and 20.0 ms for hidane.
- In this benchmark environment, median wall-clock time of the synthetic CI workload (start, ready, seed, 2 tests, shutdown) was 20.0 ms for the official emulator and 20.0 ms for hidane.
- In this benchmark environment, median wall-clock time of `firebase emulators:exec` around the same workload (2 tests) was 20.0 ms for the official emulator and 20.0 ms for hidane.
- In this benchmark environment, median wall-clock time of the command from emulator start to shutdown was 20.0 ms for the official emulator and 20.0 ms for hidane.

Medians of the measured runs; every statistic and sample is in the JSON result.

## Startup

A fresh process per run: 0 warm-up and 3 measured runs per target, targets alternating.

| Metric | Target | Median | p95 | Min | Max | Mean | Std dev | n |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| `time_to_port` | official | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |
| `time_to_port` | hidane | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |
| `time_to_first_request` | official | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |
| `time_to_first_request` | hidane | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |
| `time_to_exit` | official | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |
| `time_to_exit` | hidane | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |

- `time_to_port`: Process start until a TCP connection to the port is accepted
- `time_to_first_request`: Process start until a gRPC GetDocument of a missing document is answered (NOT_FOUND)
- `time_to_exit`: POST /shutdown until the process has exited

## Memory and CPU time

A fresh process per run (0 warm-up, 3 measured per target): 1000 ms idle once the port accepts, then 1,000 documents written in commits of 500.

| Metric | Target | Median | p95 | Min | Max | Mean | Std dev | n |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| `rss_at_ready` | official | 20.0 MiB | 29.0 MiB | 10.0 MiB | 30.0 MiB | 20.0 MiB | 10.0 MiB | 3 |
| `rss_at_ready` | hidane | 20.0 MiB | 29.0 MiB | 10.0 MiB | 30.0 MiB | 20.0 MiB | 10.0 MiB | 3 |
| `rss_idle` | official | 20.0 MiB | 29.0 MiB | 10.0 MiB | 30.0 MiB | 20.0 MiB | 10.0 MiB | 3 |
| `rss_idle` | hidane | 20.0 MiB | 29.0 MiB | 10.0 MiB | 30.0 MiB | 20.0 MiB | 10.0 MiB | 3 |
| `rss_during_workload` | official | 20.0 MiB | 29.0 MiB | 10.0 MiB | 30.0 MiB | 20.0 MiB | 10.0 MiB | 3 |
| `rss_during_workload` | hidane | 20.0 MiB | 29.0 MiB | 10.0 MiB | 30.0 MiB | 20.0 MiB | 10.0 MiB | 3 |
| `rss_after_workload` | official | 20.0 MiB | 29.0 MiB | 10.0 MiB | 30.0 MiB | 20.0 MiB | 10.0 MiB | 3 |
| `rss_after_workload` | hidane | 20.0 MiB | 29.0 MiB | 10.0 MiB | 30.0 MiB | 20.0 MiB | 10.0 MiB | 3 |
| `rss_peak` | official | 20.0 MiB | 29.0 MiB | 10.0 MiB | 30.0 MiB | 20.0 MiB | 10.0 MiB | 3 |
| `rss_peak` | hidane | 20.0 MiB | 29.0 MiB | 10.0 MiB | 30.0 MiB | 20.0 MiB | 10.0 MiB | 3 |
| `cpu_time_to_ready` | official | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |
| `cpu_time_to_ready` | hidane | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |
| `cpu_time_idle` | official | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |
| `cpu_time_idle` | hidane | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |
| `cpu_time_workload` | official | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |
| `cpu_time_workload` | hidane | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |
| `workload_time` | official | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |
| `workload_time` | hidane | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |

- `rss_at_ready`: RSS when the port first accepts
- `rss_idle`: RSS after the port is ready and 1000 ms idle
- `rss_during_workload`: Highest RSS sampled while writing 1000 documents in commits of 500 over gRPC (sampled every 50 ms)
- `rss_after_workload`: RSS right after writing 1000 documents in commits of 500 over gRPC
- `rss_peak`: Peak RSS of the process up to the end of the workload (highest periodic sample)
- `cpu_time_to_ready`: CPU time (user + system) from process start until the port accepts
- `cpu_time_idle`: CPU time (user + system) during the 1000 ms idle period
- `cpu_time_workload`: CPU time (user + system) while writing 1000 documents in commits of 500 over gRPC
- `workload_time`: Wall-clock time of writing 1000 documents in commits of 500 over gRPC

## Operations

One process per target, 100 documents seeded; each operation runs 3 times, the first 0 discarded as warm-up.

| Metric | Target | Median | p95 | Min | Max | Mean | Std dev | n |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| `single_read` | official | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |
| `single_read` | hidane | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |
| `single_write` | official | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |
| `single_write` | hidane | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |
| `delete` | official | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |
| `delete` | hidane | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |
| `query` | official | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |
| `query` | hidane | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |
| `batch_write` | official | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |
| `batch_write` | hidane | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |
| `transaction` | official | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |
| `transaction` | hidane | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |

- `single_read`: GetDocument of an existing document
- `single_write`: Commit with one write (a new document)
- `delete`: Commit with one delete of an existing document
- `query`: RunQuery with an equality filter matching 10 of 100 documents, read to the end
- `batch_write`: Commit with 500 writes (the same 500 documents each time)
- `transaction`: Read-modify-write transaction: BeginTransaction, GetDocument in it, Commit

## Dataset sizes

A fresh process per run, documents written in commits of 500; 100 documents: 0 warm-up + 3 runs. Larger sizes run fewer times.

| Metric | Target | Median | p95 | Min | Max | Mean | Std dev | n |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| `seed_100` | official | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |
| `seed_100` | hidane | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |
| `rss_after_seed_100` | official | 20.0 MiB | 29.0 MiB | 10.0 MiB | 30.0 MiB | 20.0 MiB | 10.0 MiB | 3 |
| `rss_after_seed_100` | hidane | 20.0 MiB | 29.0 MiB | 10.0 MiB | 30.0 MiB | 20.0 MiB | 10.0 MiB | 3 |
| `query_100` | official | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |
| `query_100` | hidane | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |

- `seed_100`: Writing 100 documents in commits of 500 over gRPC
- `rss_after_seed_100`: RSS right after writing 100 documents
- `query_100`: RunQuery with an equality filter matching 1 of the 100 documents, read to the end (10 per process after 3 warm-up)

## CI workload

A fresh process per run (0 warm-up, 3 measured per target): start, wait for the port, seed 100 documents, 2 tests, POST /shutdown.

| Metric | Target | Median | p95 | Min | Max | Mean | Std dev | n |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| `total` | official | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |
| `total` | hidane | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |
| `ready` | official | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |
| `ready` | hidane | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |
| `seed` | official | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |
| `seed` | hidane | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |
| `tests` | official | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |
| `tests` | hidane | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |
| `shutdown` | official | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |
| `shutdown` | hidane | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |

- `total`: Process start until exit: ready, seed 100 documents, 2 tests, shutdown
- `ready`: Process start until the port accepts
- `seed`: Connecting and writing 100 documents
- `tests`: 2 tests, each: one commit of 3 writes, 2 reads, 1 query, then a reset (DELETE …/documents)
- `shutdown`: POST /shutdown until the process has exited

## CI workload through firebase-tools

`firebase emulators:exec --only firestore` per run (0 warm-up, 3 measured per target), around `hidane benchmark workload` (seed 100 documents, 2 tests). firebase-tools starts the official emulator from the same jar and Java; hidane runs it through `hidane exec`.

| Metric | Target | Median | p95 | Min | Max | Mean | Std dev | n |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| `total` | official | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |
| `total` | hidane | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |
| `workload` | official | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |
| `workload` | hidane | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |

- `total`: `firebase emulators:exec --only firestore` from start to exit: the emulator starts, 100 documents are seeded, 2 tests run, the emulator stops
- `workload`: The seeding and tests inside, as the workload times them

## Command

A fresh process per run (0 warm-up, 3 measured per target); the command runs with FIRESTORE_EMULATOR_HOST pointing at it.

| Metric | Target | Median | p95 | Min | Max | Mean | Std dev | n |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| `total` | official | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |
| `total` | hidane | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |
| `command` | official | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |
| `command` | hidane | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |
| `ready` | official | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |
| `ready` | hidane | 20.0 ms | 29.0 ms | 10.0 ms | 30.0 ms | 20.0 ms | 10.0 ms | 3 |

- `total`: Process start until exit: ready, the command, shutdown
- `command`: Wall-clock time of the command, with FIRESTORE_EMULATOR_HOST set to the target
- `ready`: Process start until the port accepts

Rendered by `hidane benchmark render` from the JSON result (schema version 1). Methodology and claim policy: `bench/README.md`.
