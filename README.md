# hidane (火種, *the seed of fire*) — a Firestore emulator without Java.

[![release](https://img.shields.io/github/v/release/hidane-dev/hidane?color=E2553D)](https://github.com/hidane-dev/hidane/releases)
[![license: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-1E1B18)](#license)
[![website](https://img.shields.io/badge/web-hidane.dev-1E1B18)](https://hidane.dev)

hidane (pronounced *hi-da-ne*) aims to be a drop-in replacement for the official Cloud Firestore
emulator: a single Rust binary that speaks the same gRPC, REST and WebChannel wire protocols,
evaluates the same Security Rules, reads and writes the same import / export directories, and runs
under `firebase-tools`. No JDK to install. Starts in milliseconds. Tested for parity against the
official emulator as the oracle.

> **v0.1.0, the first release.** hidane serves the Firestore API over gRPC, REST and WebChannel:
> documents, queries, aggregations, vector search, transactions, listeners, exports and imports,
> and the emulator's own endpoints, each checked against the official emulator; `firebase-tools`
> launches it in place of the official one. **Security Rules are not there yet** (v0.2): every
> request is allowed. The plan is in [`docs/ROADMAP.md`](docs/ROADMAP.md), the work in
> [issues](https://github.com/hidane-dev/hidane/issues).

```sh
curl -fsSL https://hidane.dev/install.sh | sh             # or Homebrew, npm, cargo: see Install
hidane exec -- firebase emulators:start --only firestore  # under firebase-tools, no Java needed
hidane --host 127.0.0.1 --port 8080                       # or on its own:
export FIRESTORE_EMULATOR_HOST=127.0.0.1:8080
```

`hidane exec` runs the command with hidane standing in for `java` and the official jar, for that
command only ([ADR 0006](docs/adr/0006-launch-under-firebase-tools.md)).

## Why hidane?

The official emulator is a closed-source Java application shipped as a jar. Three things about
it hurt everyday local development and CI (details and raw logs: [`docs/why.md`](docs/why.md)):

| | Official emulator v1.22.0 | hidane on `main` |
|---|---|---|
| Runtime | Java 21 or newer ([required since firebase-tools 15.0.0](https://github.com/firebase/firebase-tools/releases/tag/v15.0.0)) | none, single binary |
| Time until the port accepts | 0.74 s directly, 2.5 s through `firebase emulators:start` | 5.5 ms (median of 10) |
| Resident memory | 95 MiB idle, 456 MiB after 1,000 documents | 7.4 MiB idle, 21 MiB after 1,000 documents |
| 100,000 documents in batches of 500, a listener on the collection | 121 s, each batch slower as documents accumulate ([firebase-tools#3477](https://github.com/firebase/firebase-tools/issues/3477)) | 4.9 s, flat per batch |

All numbers are measurements, but not side by side: the official emulator's were taken in Phase 0
on a loaded workstation (see the caveat in `docs/why.md`), hidane's later on a quiet one
(`results/startup-memory-hidane.txt`, `results/listen-batch-writes-hidane.csv`). A same-machine
comparison is #50.

## What works today

Which clients can use hidane depends on the transport their SDK speaks to an emulator
([`docs/compatibility.md`](docs/compatibility.md)). "Verified" means the client's results were
compared with the official emulator's (`results/`):

| Client | Transport | In v0.1.0 |
|---|---|---|
| firebase-admin (Node) / `@google-cloud/firestore` | gRPC | verified: documents, queries, aggregations, vector search, transactions, listeners |
| firebase-js-sdk in Node | gRPC | verified: writes, transactions, listeners, offline and resume |
| firebase-js-sdk Lite | REST | verified |
| firebase-js-sdk in the browser | WebChannel and REST | verified in Chromium: listeners, writes, queries, long polling, mock tokens ([`docs/webchannel.md`](docs/webchannel.md)) |
| `@firebase/rules-unit-testing` | the above | verified without rules: contexts, `clearFirestore()` |
| Go, Python, Java, iOS, Android, Flutter (all platforms), C++ / Unity | gRPC or WebChannel | same transports, not verified yet |
| Security Rules, the Emulator UI request monitor | — | not yet (v0.2): every request is allowed |
| `firebase emulators:start` / `emulators:exec` | `hidane exec -- firebase …` | verified with firebase-tools 15.33.0, no Java installed |
| Emulator UI | REST and the emulator's endpoints | verified: browse, create and delete documents, Clear all data; not the request monitor (#69) |
| Export / import (`--import`, `--export-on-exit`, `emulators:export`) | the official format | verified: hidane and the official emulator read each other's exports, byte for byte the same files ([`docs/export-format.md`](docs/export-format.md)) |

## Parity

hidane treats the official emulator as the oracle. Scripts in [`tools/oracle/`](tools/oracle/)
ask the official emulator how it answers — 17 recorded fixtures so far, from value ordering to
WebChannel framing and export files — and the test suite replays every recording against hidane; SDK transcripts
from both emulators are compared in [`results/`](results/). A parity table with a badge will
follow (#43). No claim of compatibility without a test behind it. Differences that are intentional (performance bugs and
known defects of the official emulator that hidane will not reproduce) are listed in
[`docs/parity-exceptions.md`](docs/parity-exceptions.md).

## Prior art

Two open-source emulators came closest in our survey. Both document a gRPC endpoint for the
server SDKs; neither documents REST, WebChannel, Security Rules or running under
`firebase-tools`, which is the gap hidane is built to close.

| | hidane | [skunkteam/rust-firestore-emulator](https://github.com/skunkteam/rust-firestore-emulator) | [YutaUra/firestore-emulator](https://github.com/YutaUra/firestore-emulator) |
|---|---|---|---|
| Language | Rust | Rust | TypeScript |
| Runtime dependency | none | none | Node.js |
| Documented transports | gRPC, REST, WebChannel | gRPC | gRPC |
| Security Rules | planned (v0.2) | not documented | not documented |
| Verification | the official emulator's answers recorded and replayed, SDK transcripts compared | own test suite, also runnable against real Cloud Firestore | tested against the official emulator |

As read from their READMEs in October 2026; corrections welcome.

## Install

Every channel installs the same release binaries: macOS (arm64, x86_64), Linux (arm64, x86_64;
static, any distribution) and Windows (x64).

```sh
curl -fsSL https://hidane.dev/install.sh | sh   # macOS, Linux: into ~/.local/bin, SHA-256 checked
brew install hidane-dev/tap/hidane               # Homebrew (macOS, Linux)
npm install --save-dev hidane                    # npm, every platform: the binary is an optional dependency
cargo binstall hidane                            # the release archive, through cargo-binstall
cargo install hidane                             # from source, Rust 1.88 or newer
dart pub global activate hidane                  # pub.dev: a launcher that downloads the binary on first run
docker run --rm -p 8080:8080 ghcr.io/hidane-dev/hidane
```

Then `export FIRESTORE_EMULATOR_HOST=127.0.0.1:8080`, or let `firebase emulators:start` launch it
through `hidane exec`. The archives, their checksums and build provenance are on
[GitHub Releases](https://github.com/hidane-dev/hidane/releases)
([how to verify them](docs/releasing.md#assets)). The macOS binaries are not signed yet: one
downloaded with a browser needs `xattr -d com.apple.quarantine hidane`.

## Documentation

- [Why hidane](docs/why.md) — the measurements behind the table above, with sources
- [Compatibility](docs/compatibility.md) — transport per SDK, how each finds the emulator, which RPCs they use
- [Parity exceptions](docs/parity-exceptions.md) — where hidane and the official emulator intentionally differ
- [Roadmap](docs/ROADMAP.md) — scope of v0.1 / v0.2 / v0.3, targets, risks
- [Decision records](docs/adr/) — storage engine, gRPC stack, WebChannel timing, Rules engine, license (all drafts)
- [Benchmark results](results/) — the official emulator baseline, hidane's measurements and the SDK differentials
- [Releasing](docs/releasing.md) — release assets, checksums, provenance and how a release is cut

## Contributing

hidane is built issue by issue, each one measured against the official emulator first. Please
open an issue before sending a pull request; the code you would touch may be about to change
shape. Parity-gap reports (a request whose answer differs from the official emulator's) are
especially welcome.
See [CONTRIBUTING.md](CONTRIBUTING.md), the [issue templates](.github/ISSUE_TEMPLATE/) and
[Discussions](https://github.com/hidane-dev/hidane/discussions). Security issues go through
[private vulnerability reporting](SECURITY.md).

## Limitations and non-goals

- Local development and CI only. hidane is not a production database and must not be exposed on
  a network without authentication.
- Until Security Rules land (v0.2), every request is allowed, whatever `firestore.rules` says.
- Cloud Firestore only. The Realtime Database, Cloud Storage and Auth emulators are out of scope.
- Features the official emulator does not implement either (the Admin API, TTL, point-in-time
  recovery, bundles) stay unimplemented. See `docs/parity-exceptions.md`.

## Acknowledgements

hidane is a clean-room implementation written from the public protobuf definitions
([googleapis](https://github.com/googleapis/googleapis)), the public Firebase and Google Cloud
documentation, and black-box observation of the official emulator. No code was decompiled or
ported from the official jar. Thanks to the authors of skunkteam/rust-firestore-emulator and
YutaUra/firestore-emulator, whose public work helped us map the landscape, and to the model
projects whose approach to parity testing we follow: [dynoxide](https://github.com/nubo-db/dynoxide)
and [zerobrew](https://github.com/zerobrewhq/zerobrew).

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option ([ADR 0001](docs/adr/0001-license.md)).

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in
the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without
any additional terms or conditions.

## Trademarks

Firebase, Cloud Firestore and Google Cloud are trademarks of Google LLC. hidane is an independent
open-source project and is not affiliated with, endorsed by, or sponsored by Google.
