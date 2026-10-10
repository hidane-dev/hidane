# results/

Measurements and transcripts that the documentation cites. Scripts that produce new numbers live
in `tools/` and in the crates; the oracle scripts and their fixtures are described in
[`tools/oracle/README.md`](../tools/oracle/README.md).

## Official emulator baseline (Phase 0)

Measured against `cloud-firestore-emulator-v1.22.0.jar`. The summary and its interpretation are in
[`docs/why.md`](../docs/why.md). The raw logs and the measurement scripts were tied to one
workstation and are not published; a re-measurement with a public harness on a quiet runner is
tracked in #50.

| File | Contents |
|---|---|
| `official-v1.22.0-baseline.md` | Environment, startup times, memory, single-request latency, gRPC reflection, and the batch-write degradation runs, with every number the documentation cites |
| `batch-write-degradation-official-v1.22.0.csv` | Every batch of the degradation runs. Columns: `run,batch,docs_start,docs_end,elapsed_sec,rate_per_sec,rss_kb,t_rel_sec` (`rss_kb` is the emulator RSS when the row was written, `t_rel_sec` the seconds since the first write) |
| `issues-firebase-tools-metadata.json` | Metadata (title, state, dates, labels, comment count, URL) of firebase-tools #3477 / #2277 / #1624 / #4578. No issue bodies |

## hidane and SDK differentials

| File | Contents |
|---|---|
| `storage-memory-rss-hidane.csv` | RSS of hidane's in-memory store after writing 1k / 100k / 500k / 1M documents shaped like the baseline above (`users/{i}` = `{mykey, myid}`, 500 per commit), in-process, release build, macOS arm64. Produced by `cargo run --release -p hidane-core --example rss`. See ADR 0002 |
| `sdk-documents-official-v1.22.0.json`, `sdk-documents-hidane.json` | Transcripts of `tools/oracle/sdk_documents.mjs` (`@google-cloud/firestore` 9.3.1) against the official emulator and hidane, for #16 and #23 |
| `sdk-aggregations-official-v1.22.0.json`, `sdk-aggregations-hidane.json` | Transcripts of `tools/oracle/sdk_aggregations.mjs` (`@google-cloud/firestore` 9.3.1) against both, for #22. Identical |
| `sdk-queries-official-v1.22.0.json`, `sdk-queries-hidane.json` | Transcripts of `tools/oracle/sdk_queries.mjs` (`@google-cloud/firestore` 9.3.1) against both, for #21. Identical |
| `sdk-transactions-official-v1.22.0.json`, `sdk-transactions-hidane.json` | Transcripts of `tools/oracle/sdk_transactions.mjs` (`@google-cloud/firestore` 9.3.1, `runTransaction`) against both, for #17. Identical |
| `sdk-web-writes-official-v1.22.0.json`, `sdk-web-writes-hidane.json` | Transcripts of `tools/oracle/sdk_web_writes.mjs` (`firebase` 13.0.0: `setDoc`, `updateDoc` with transforms, `deleteDoc`, `writeBatch`, 50 concurrent writes) against both, for #20. Identical (keys sorted, see #108) |
| `sdk-web-transactions-official-v1.22.0.json`, `sdk-web-transactions-hidane.json` | Transcripts of `tools/oracle/sdk_web_transactions.mjs` (`firebase` 13.0.0, `runTransaction` in Node) against both, for #17. Identical |
