# nodejs-firestore's system tests

Runs the system tests of the Node.js server SDK, `@google-cloud/firestore`, against an emulator,
and compares hidane's outcomes with the official emulator's (#42). The tests are the SDK's own,
from [google-cloud-node](https://github.com/googleapis/google-cloud-node)'s
`handwritten/firestore/dev/system-test`. They create documents and run queries, aggregations,
listeners, transactions, bulk writes, bundles and recursive deletes against `127.0.0.1:<port>`,
over gRPC as `pnpm run system-test:emulator:grpc` does there, and over REST as
`system-test:emulator:rest` does (neither starts an emulator).

The suite runs at the commit pinned in `.github/workflows/nodejs-system-tests.yml`
(`GOOGLE_CLOUD_NODE_COMMIT`), which runs it against hidane weekly and for changes to the server.

| File | What it does |
|---|---|
| `setup.sh` | Installs the Firestore package of a checkout with its own package manager (pnpm 10), without install scripts, and compiles it and the tests to `build/` |
| `run.sh` | Runs `build/system-test` with Mocha against `127.0.0.1:<port>`, over gRPC or REST, and writes Mocha's JSON report |
| `metadata.py` | Over REST, stands in for the metadata server: the SDK asks google-auth-library for credentials there too, and gets the token `owner`, which both emulators read as the administrator's (nothing reaches Google) |
| `results.mjs` | Turns a report into `results/nodejs-system-tests-*.json`: each test's ID and outcome, nothing else |
| `compare.mjs` | Fails on a test that passes on the official emulator and not on hidane, unless `expected-failures.txt` lists it; on a listed test that passes; and on runs of different commits, transports or tests |
| `expected-failures.txt` | The tests expected to fail on hidane, each with its reason (none now) |

## Running it

```sh
git clone --filter=blob:none --sparse https://github.com/googleapis/google-cloud-node.git /path/to/gcn
git -C /path/to/gcn sparse-checkout set handwritten/firestore bin
git -C /path/to/gcn checkout <GOOGLE_CLOUD_NODE_COMMIT>
tools/nodejs-system-tests/setup.sh /path/to/gcn

hidane --host 127.0.0.1 --port 8181 &          # or the official emulator, freshly started
tools/nodejs-system-tests/run.sh /path/to/gcn 8181 /tmp/report.json
node tools/nodejs-system-tests/results.mjs <GOOGLE_CLOUD_NODE_COMMIT> hidane /tmp/report.json > hidane.json
node tools/nodejs-system-tests/compare.mjs results/nodejs-system-tests-official-v1.22.0.json \
  hidane.json tools/nodejs-system-tests/expected-failures.txt
```

Over REST, pass `rest` to `run.sh` and `results.mjs`, start the emulator afresh, and compare with
`results/nodejs-system-tests-rest-official-v1.22.0.json`.

The tests take about 15 seconds against either emulator; setting up the checkout takes a few
minutes. To record the official emulator again, run the workflow by hand with `official`: it also
runs the suite against a freshly started official emulator, the pinned release, and uploads
`nodejs-system-tests-official` and `nodejs-system-tests-rest-official`, whose results go in
`results/` as they are. By hand, start the jar with an English locale (see
`tools/oracle/README.md`) and pass `"official v1.22.0"` as the emulator's name.

## What the results hold

At the pinned commit, 937 tests, with the same outcomes over gRPC and REST:

| | Official emulator v1.22.0 | hidane |
|---|--:|--:|
| Passed | 328 | 329 |
| Failed | 13 | 12 |
| Skipped | 596 | 596 |

- **Skipped.** 596 tests skip themselves against an emulator or a Standard edition database:
  - 338 test pipelines, which need an Enterprise edition database.
  - The rest are marked `skipEmulator`, or need production features: indexes, `CollectionGroup`
    partitions, and the tracing exporters.
- **Failed on both.** The 12 tests of query explain (`explain()`, `explainStream()`) fail on both
  emulators: neither returns explain metrics (`docs/parity-exceptions.md`, D079).
- **Failed on the official emulator alone.** `vector search supports findNearest on vector nested
  in a map` fails there: the official emulator reads the vector field path `embedding.vector` as
  one field named `embedding.vector` (D033). hidane reads it as a path, as production does.

## What is left out

- **c8.** `bin/run-test.cjs` runs Mocha under c8 for coverage; `run.sh` runs Mocha alone, with a
  timeout of a minute per test in place of 20 minutes, so a hanging test fails rather than stalls
  the run.
