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
| `sdk-clear-official-v1.22.0.json`, `sdk-clear-hidane.json` | Transcripts of `tools/oracle/sdk_clear.mjs` (`@firebase/rules-unit-testing` 6.0.0 `clearFirestore()`, the Emulator UI's recursive delete and `POST /reset`, with web SDK and Admin SDK listeners) against both, for #33. The recursive delete matches; `clearFirestore()` reaches the web SDK in one snapshot on hidane and two on the official emulator; after `POST /reset` the official emulator's listeners receive nothing, not even later writes (`docs/parity-exceptions.md`) |
| `sdk-documents-official-v1.22.0.json`, `sdk-documents-hidane.json` | Transcripts of `tools/oracle/sdk_documents.mjs` (`@google-cloud/firestore` 9.3.1) against the official emulator and hidane, for #16 and #23 |
| `sdk-aggregations-official-v1.22.0.json`, `sdk-aggregations-hidane.json` | Transcripts of `tools/oracle/sdk_aggregations.mjs` (`@google-cloud/firestore` 9.3.1) against both, for #22. Identical |
| `sdk-listen-official-v1.22.0.json`, `sdk-listen-hidane.json` | Transcripts of `tools/oracle/sdk_listen.mjs` (Admin SDK `onSnapshot` on queries, a limit query, documents and a collection group; web SDK `onSnapshot`, `getDoc`, `getDocs`) against both, for #18. Identical except one step: on the official emulator a batch reaches the web SDK's limit query as two snapshots, the first one not atomic (`docs/parity-exceptions.md`) |
| `sdk-listen-resume-official-v1.22.0.json`, `sdk-listen-resume-hidane.json` | Transcripts of `tools/oracle/sdk_listen_resume.mjs` (web SDK `disableNetwork` / `enableNetwork` around writes), for #19. The final states match; the official emulator gets there in three snapshots after reconnecting, hidane in one (`docs/parity-exceptions.md`) |
| `startup-memory-hidane.txt` | Time until a release build accepts on its port (10 runs) and its RSS idle and after 1,000 documents shaped like the baseline, from `tools/bench/startup_memory.py` (macOS arm64, M1 Max). Not side by side with the baseline (#50) |
| `sdk-auth-official-v1.22.0.json`, `sdk-auth-hidane.json` | Transcripts of `tools/oracle/sdk_auth.mjs` (rules-unit-testing user and signed-out contexts, `withSecurityRulesDisabled`, the Admin SDK), for #24. Identical |
| `sdk-find-nearest-official-v1.22.0.json`, `sdk-find-nearest-hidane.json` | Transcripts of `tools/oracle/sdk_find_nearest.mjs` (Admin SDK `findNearest` with each distance measure, web SDK `vector()`), for #118. Identical |
| `keepalive-official-v1.22.0.txt`, `keepalive-hidane.txt` | What `tools/oracle/keepalive.mjs` (a client pinging every 10 s) saw: `GOAWAY too_many_pings` after 30 s from the official emulator, nothing from hidane, for #25 |
| `browser-webchannel-official-v1.22.0.json`, `browser-webchannel-hidane.json` | Results of the browser scenarios in `tools/oracle/webchannel/page.js` (firebase 13.0.0 in Chromium over WebChannel: listeners, writes, queries and batches, forced and auto-detected long polling, mock tokens, a refused write), for #71. Identical |
| `listen-batch-writes-hidane.csv` | 100,000 documents written through the Admin SDK in batches of 500, with and without an `onSnapshot` listener on the whole collection, against a release build of hidane (macOS arm64, M1 Max), for #30. The time per batch stays flat with the listener |
| `sdk-queries-official-v1.22.0.json`, `sdk-queries-hidane.json` | Transcripts of `tools/oracle/sdk_queries.mjs` (`@google-cloud/firestore` 9.3.1) against both, for #21. Identical |
| `sdk-rest-official-v1.22.0.json`, `sdk-rest-hidane.json` | Transcripts of `tools/oracle/sdk_rest.mjs` (`firebase` 13.0.0 Lite: get, set, update with transforms, batches, queries, count / sum, transactions, a failing update) against both, for #34. Identical |
| `sdk-transactions-official-v1.22.0.json`, `sdk-transactions-hidane.json` | Transcripts of `tools/oracle/sdk_transactions.mjs` (`@google-cloud/firestore` 9.3.1, `runTransaction`) against both, for #17. Identical |
| `sdk-web-writes-official-v1.22.0.json`, `sdk-web-writes-hidane.json` | Transcripts of `tools/oracle/sdk_web_writes.mjs` (`firebase` 13.0.0: `setDoc`, `updateDoc` with transforms, `deleteDoc`, `writeBatch`, 50 concurrent writes) against both, for #20. Identical (keys sorted, see #108) |
| `sdk-web-transactions-official-v1.22.0.json`, `sdk-web-transactions-hidane.json` | Transcripts of `tools/oracle/sdk_web_transactions.mjs` (`firebase` 13.0.0, `runTransaction` in Node) against both, for #17. Identical |
