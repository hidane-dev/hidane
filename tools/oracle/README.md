# Oracle scripts

Small scripts that ask the **official** Firestore emulator how it behaves, so hidane can
reproduce the answer. Each one writes a fixture that a hidane test reads; the fixture is
committed, the official jar is not (it may not be redistributed, see `docs/parity-exceptions.md`).

Python 3 standard library only; `harness.py` holds the shared request and recording helpers. Run them against a freshly started official emulator:

```sh
java -jar cloud-firestore-emulator-v1.22.0.jar --host 127.0.0.1 --port 8086 &
python3 -I tools/oracle/value_order.py 127.0.0.1:8086 > crates/hidane-core/tests/fixtures/value_order.json
curl -X POST http://127.0.0.1:8086/shutdown
```

| Script | Question | Fixture | Test |
|---|---|---|---|
| `value_order.py` + `value_order_cases.json` | How are values ordered, which values are equal, and how are document names ordered in a collection group? | `crates/hidane-core/tests/fixtures/value_order.json` | `crates/hidane-core/tests/official_order.rs` |
| `transforms.py` | How are field transforms applied (server time, increment, maximum / minimum, array union / remove), in which order, with which results? | `crates/hidane/tests/fixtures/transforms.json` | `crates/hidane/tests/transforms.rs` |
| `document_writes.py` | How do document reads and writes answer, including errors, preconditions, masks, paging and admin checks? | `crates/hidane/tests/fixtures/document_writes.json` | `crates/hidane/tests/documents.rs` |
| `transactions.py` | What do transactions lock, how long do writes wait, when do transactions end, and how are `verify` writes checked? Scenarios with concurrent steps and timing; needs `grpcurl` on `PATH` and takes about two minutes | `crates/hidane/tests/fixtures/transactions.json` | `crates/hidane/tests/transactions.rs` |

`sdk_documents.mjs` drives the same operations through `@google-cloud/firestore` (the engine of
firebase-admin) and prints a transcript without absolute timestamps. Run it from a directory
outside the repository where the package is installed, once per emulator, and diff the two:

```sh
cd "$(mktemp -d)" && npm i @google-cloud/firestore && cp <repo>/tools/oracle/sdk_documents.mjs .
FIRESTORE_EMULATOR_HOST=127.0.0.1:8089 node sdk_documents.mjs > official.json   # official jar
FIRESTORE_EMULATOR_HOST=127.0.0.1:8189 node sdk_documents.mjs > hidane.json     # hidane
```

The transcripts of the last run are in `results/sdk-documents-*.json`.

`sdk_transactions.mjs` does the same for `runTransaction` (server transactions, retries on
`ABORTED`, lock waits), and `sdk_web_transactions.mjs` for the web SDK (`npm i firebase`), whose
transactions use preconditions and `verify` writes instead. Their transcripts are in
`results/sdk-transactions-*.json` and `results/sdk-web-transactions-*.json`.
