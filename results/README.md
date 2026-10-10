# results/ — raw benchmark logs

Raw logs captured during Phase 0 against the official Cloud Firestore emulator
(`cloud-firestore-emulator-v1.22.0.jar`, SHA256 `9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c`).
The summary and interpretation live in [`docs/why.md`](../docs/why.md). This directory holds evidence and scripts only.

**Caveat**: every number here was measured on macOS arm64 (M1 Max, 64 GiB) **while other heavy tasks were running
concurrently** (load average 12–58). Treat absolute values as indicative; relative comparisons are the point.
A re-measurement on a quiet runner is tracked in issue #50.

## Environment

| File | Contents |
|---|---|
| `env-official-v1.22.0.txt` | `sw_vers`, CPU, memory, `java -version`, `firebase --version`, jar SHA256, `uptime` before and after |

## Startup time

| File | Contents |
|---|---|
| `startup-official-v1.22.0.json` | `hyperfine --warmup 1 --runs 10 --export-json`. One run = start `java -jar` → poll until TCP 8090 accepts → kill → wait for the port to close, so wall time is longer than time-to-port |
| `startup-official-v1.22.0-port-open-ms.txt` | Per-run "milliseconds until the port opened" printed by the script (first line is the warmup) plus load averages |
| `startup-official-v1.22.0-hyperfine-output.log` | Full `hyperfine --show-output` log |
| `startup-firebase-tools-15.33.0.txt` | Five startups through `firebase emulators:start --only firestore`, and the exact `java` command line firebase-tools spawned |
| `startup-firebase-tools-15.33.0-run0.log` | firebase-tools stdout for run 0 (includes `lsof` warnings) |
| `firebase.json.bench` | `firebase.json` used for the firebase-tools runs (firestore.port=8091, hub.port=4409, UI disabled) |

## Resident memory

| File | Contents |
|---|---|
| `memory-official-v1.22.0.txt` | RSS right after start and after 1 / 5 / 10 s idle, `jcmd VM.flags` / `GC.heap_info`, JVM defaults (`-XX:+PrintFlagsFinal`), RSS after writing 1,000 documents through REST `:commit` |

## Batch-write degradation (reproduction of firebase-tools#3477)

| File | Contents |
|---|---|
| `batch-write-degradation-official-v1.22.0.csv` | All runs combined. Columns: `run,batch,docs_start,docs_end,elapsed_sec,rate_per_sec,rss_kb,t_rel_sec` (`rss_kb` is the emulator RSS when the row was written, `t_rel_sec` is seconds since the first write) |
| `degradation-logs/<run>.meta.txt` | Conditions, total time, final RSS and load averages for each run |
| `degradation-logs/<run>.listener.log` | Node `onSnapshot` listener log (runs B and C only) |
| `degradation-logs/<run>.emulator.log` | First 30 and last 20 lines of emulator stdout (truncated to keep the file small) |

Runs:

- `A-nolistener` — no listener, 100,000 documents (500 × 200 batches)
- `A2-nolistener-500k` — no listener, 500,000 documents (× 1,000 batches)
- `B-listener-unlimited` — `onSnapshot` on the whole `users` collection, 100,000 documents
- `C-listener-limit50` — `onSnapshot` with `limit(50)` on `users`, 100,000 documents

## Single-request latency

| File | Contents |
|---|---|
| `latency-official-v1.22.0.json` | `hyperfine --warmup 3 --runs 20 -N --export-json`: REST PATCH / GET, gRPC Commit / GetDocument (grpcurl), and the process-spawn baseline of each client |
| `latency-official-v1.22.0.txt` | Same, console output |
| `latency-official-v1.22.0-summary.txt` | mean / median / min / max (ms) computed from the JSON |
| `latency-grpc-reflection-vs-proto-official-v1.22.0.{json,txt}` | gRPC Commit re-measured with `-proto`, and the full error showing that grpcurl cannot invoke through the emulator's reflection alone |

## Reference

| File | Contents |
|---|---|
| `issues-firebase-tools-metadata.json` | Metadata (title, state, dates, labels, comment count, URL) of firebase-tools #3477 / #2277 / #1624 / #4578. No issue bodies |
| `scripts/` | The scripts that produced everything above. Paths point at the scratch directory used during measurement and need editing before reuse |

## Notes for re-running

- Java 21 or newer is required (the jar is compiled to class-file version 65). Through `mise` shims the resolved `java` or `firebase` may be a different version or architecture, so the scripts use absolute paths (`openjdk-24.0.2`, `node/22.22.3/bin/firebase`).
- Ports 8090–8098 are used to avoid clashing with other emulator instances.
- The write client for the degradation runs is `demo.go` from williamhaley/firestore-emulator-slow, built with `go build` and its pinned `cloud.google.com/go/firestore v1.5.0`.

## hidane

| File | Contents |
|---|---|
| `storage-memory-rss-hidane.csv` | RSS of hidane's in-memory store after writing 1k / 100k / 500k / 1M documents shaped like the baseline above (`users/{i}` = `{mykey, myid}`, 500 per commit), in-process, release build, macOS arm64. Produced by `cargo run --release -p hidane-core --example rss`. See ADR 0002 |
| `sdk-documents-official-v1.22.0.json`, `sdk-documents-hidane.json` | Transcripts of `tools/oracle/sdk_documents.mjs` (`@google-cloud/firestore` 9.3.1) against the official emulator and hidane, for #16 and #23 |
