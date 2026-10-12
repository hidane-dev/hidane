# firebase-js-sdk's browser suite

Runs the Firestore browser integration tests of
[firebase-js-sdk](https://github.com/firebase/firebase-js-sdk) against an emulator, and compares
hidane's outcomes with the official emulator's (#74). The tests are the SDK's own: Vitest in
Playwright's Chromium, the SDK built from its sources, talking WebChannel (listeners, writes,
`getDoc`, `getDocs`) and REST (transactions, aggregations) to `localhost:<port>`, as
`yarn test:browser:emulator` in `packages/firestore` does
(`FIRESTORE_TARGET_BACKEND=emulator vitest run --project=browser`; it starts no emulator).

The suite runs at the commit pinned in `.github/workflows/js-sdk-browser.yml`
(`FIREBASE_JS_SDK_COMMIT`), which runs it against hidane weekly and for changes to the server.

| File | What it does |
|---|---|
| `setup.sh` | Installs a checkout with its own package manager (Yarn 1, Node.js 24) and builds the packages the tests import from `dist/` (`@firebase/app`, `auth`, `component`, `logger`, `util`, `webchannel-wrapper`) |
| `run.sh` | Runs `test/integration` of `test:browser:emulator` against `localhost:<port>` and writes Vitest's JSON report. It adds `vitest.config.mjs` and `cleanup.ts` to the checkout |
| `cleanup.ts` | A setup file that terminates, after each test, the Firestore instances it created (below) |
| `results.mjs` | Turns a report into `results/js-sdk-browser-*.json`: each test's ID and outcome, nothing else |
| `compare.mjs` | Fails on a test that passes on the official emulator and not on hidane, unless `expected-failures.txt` lists it; on a listed test that passes; and on runs of different commits or tests |
| `expected-failures.txt` | The tests expected to fail on hidane, each with its reason (none now) |

## Running it

```sh
git clone https://github.com/firebase/firebase-js-sdk.git /path/to/firebase-js-sdk
git -C /path/to/firebase-js-sdk checkout <FIREBASE_JS_SDK_COMMIT>
tools/js-sdk-browser/setup.sh /path/to/firebase-js-sdk
(cd /path/to/firebase-js-sdk && npx playwright install chromium chromium-headless-shell)

hidane --host 127.0.0.1 --port 8181 &          # or the official emulator, freshly started
tools/js-sdk-browser/run.sh /path/to/firebase-js-sdk 8181 /tmp/report.json
node tools/js-sdk-browser/results.mjs <FIREBASE_JS_SDK_COMMIT> hidane /tmp/report.json > hidane.json
node tools/js-sdk-browser/compare.mjs results/js-sdk-browser-official-v1.22.0.json hidane.json \
  tools/js-sdk-browser/expected-failures.txt
```

Each run takes about a minute on an M1 Max, against either emulator. To record the official
emulator again, start the jar with an English locale (see `tools/oracle/README.md`) and pass
`"official v1.22.0"` as the emulator's name.

## What is left out, and what is added

- **The unit tests.** `test:browser:emulator` also runs `test/unit`, which never reaches an
  emulator; `run.sh` passes `test/integration`.
- **Skipped tests.** 739 of the 1,922 integration tests skip themselves against an emulator or a
  Standard edition database: 614 test pipelines, the others BSON types, large documents (behind a
  flag) and cases the suite marks as not for the emulator. The results list them as `skipped`.
- **`cleanup.ts`.** Without it the suite is not green against either emulator, and its failures
  depend on timing. Some tests leave their Firestore instances running (they were written against
  production, where it costs nothing), and one loses track of an instance it starts. Against an
  emulator, each running instance holds two of the six HTTP/1.1 connections Chromium opens to
  one host and port (its Listen and Write streams), so after three of them every test that needs
  a new connection waits until it times out. Run unchanged against the official emulator, the
  suite had failed tests in 9 of its first 15 files when it was stopped after 54 minutes. The
  setup file terminates, after each test, every Firestore instance the test created; it changes
  no test.
