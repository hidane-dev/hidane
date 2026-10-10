# hidane (火種, *the seed of fire*) — a Firestore emulator without Java.

[![status: phase 0](https://img.shields.io/badge/status-phase%200%20%C2%B7%20research-E2553D)](docs/ROADMAP.md)
[![license: MIT](https://img.shields.io/badge/license-MIT-1E1B18)](LICENSE)
[![website](https://img.shields.io/badge/web-hidane.dev-1E1B18)](https://hidane.dev)

hidane (pronounced *hi-da-ne*) aims to be a drop-in replacement for the official Cloud Firestore
emulator: a single Rust binary that speaks the same gRPC, REST and WebChannel wire protocols,
evaluates the same Security Rules, reads and writes the same import / export directories, and runs
under `firebase-tools`. No JDK to install. Starts in milliseconds. Tested for parity against the
official emulator as the oracle.

> **Phase 0 — research in progress.** There is nothing to run yet. The findings that shape the
> design are in [`docs/`](docs/), the plan in [`docs/ROADMAP.md`](docs/ROADMAP.md), and the work
> in [issues](https://github.com/hidane-dev/hidane/issues) grouped by
> [milestone](https://github.com/hidane-dev/hidane/milestones). The `hidane` packages on
> crates.io, npm and pub.dev are 0.0.1 name reservations and do nothing.

## Why hidane?

The official emulator is a closed-source Java application shipped as a jar. Three things about
it hurt everyday local development and CI (details and raw logs: [`docs/why.md`](docs/why.md)):

| | Official emulator v1.22.0 | hidane target |
|---|---|---|
| Runtime | Java 21 or newer ([required since firebase-tools 15.0.0](https://github.com/firebase/firebase-tools/releases/tag/v15.0.0)) | none, single static binary |
| Time until the port accepts | 0.74 s directly, 2.5 s through `firebase emulators:start` | < 100 ms |
| Resident memory, idle | 95 MiB (456 MiB after 1,000 documents) | < 50 MiB |
| Batch writes with a listener attached | slow down in proportion to the number of stored documents, 11× at 100k ([firebase-tools#3477](https://github.com/firebase/firebase-tools/issues/3477)) | cost proportional to the changed documents only |

Official-emulator numbers are Phase 0 measurements taken under load (see the caveat in
`docs/why.md`); hidane numbers are targets, not results.

## What will work, and when

Which clients can use hidane depends on the transport their SDK speaks to an emulator. Phase 0
read the SDK sources to settle this ([`docs/compatibility.md`](docs/compatibility.md)):

| Milestone | Clients |
|---|---|
| **v0.1** gRPC core, no rules | firebase-admin (Node) / `@google-cloud/firestore`, Go, Python, Java, iOS, Android, Flutter (mobile and desktop), C++ / Unity, firebase-js-sdk **in Node** |
| **v0.2** rules | Security Rules, `@firebase/rules-unit-testing`, the Emulator UI request monitor |
| **v0.3** webchannel | firebase-js-sdk **in the browser**, Flutter Web |

The Web SDK in the browser does almost everything over WebChannel and the rest over REST, so a
gRPC-only server does nothing for it. WebChannel landed on `main` ahead of v0.3
([`docs/webchannel.md`](docs/webchannel.md)); Flutter Web is not verified yet.

## Parity

hidane treats the official emulator as the oracle. The same test suites run against both, and the
results will be published as a parity table with a badge in this README. No claim of
compatibility without a test behind it. Differences that are intentional (performance bugs and
known defects of the official emulator that hidane will not reproduce) are listed in
[`docs/parity-exceptions.md`](docs/parity-exceptions.md).

## Prior art

Two open-source emulators came closest in our survey. Both document a gRPC endpoint for the
server SDKs; neither documents REST, WebChannel, Security Rules or running under
`firebase-tools`, which is the gap hidane is built to close.

| | hidane (planned) | [skunkteam/rust-firestore-emulator](https://github.com/skunkteam/rust-firestore-emulator) | [YutaUra/firestore-emulator](https://github.com/YutaUra/firestore-emulator) |
|---|---|---|---|
| Language | Rust | Rust | TypeScript |
| Runtime dependency | none | none | Node.js |
| Documented transports | gRPC (v0.1), REST (v0.1), WebChannel (v0.3) | gRPC | gRPC |
| Security Rules | v0.2 | not documented | not documented |
| Verification | parity against the official emulator | own test suite, also runnable against real Cloud Firestore | tested against the official emulator |

As read from their READMEs in October 2026; corrections welcome.

## Install (planned)

None of these exist yet. They show the intended shape:

```sh
curl -fsSL https://hidane.dev/install.sh | sh      # prebuilt binary
brew install hidane-dev/tap/hidane                  # Homebrew tap
cargo binstall hidane                               # prebuilt via cargo-binstall
npm install --save-dev hidane                       # npm (binary as an optional dependency)
dart pub global activate hidane                     # pub.dev launcher
docker run --rm -p 8080:8080 ghcr.io/hidane-dev/hidane
```

Then `export FIRESTORE_EMULATOR_HOST=127.0.0.1:8080`, or let `firebase emulators:start` launch it.

## Documentation

- [Why hidane](docs/why.md) — the measurements behind the table above, with sources
- [Compatibility](docs/compatibility.md) — transport per SDK, how each finds the emulator, which RPCs they use
- [Parity exceptions](docs/parity-exceptions.md) — where hidane and the official emulator intentionally differ
- [Roadmap](docs/ROADMAP.md) — scope of v0.1 / v0.2 / v0.3, targets, risks
- [Decision records](docs/adr/) — storage engine, gRPC stack, WebChannel timing, Rules engine, license (all drafts)
- [Benchmark results](results/) — the official emulator baseline, hidane's measurements and the SDK differentials

## Contributing

Phase 0 is research. Please open an issue before sending a pull request; the code you would touch
may be about to change shape. Research findings and parity-gap reports are especially welcome.
See [CONTRIBUTING.md](CONTRIBUTING.md), the [issue templates](.github/ISSUE_TEMPLATE/) and
[Discussions](https://github.com/hidane-dev/hidane/discussions). Security issues go through
[private vulnerability reporting](SECURITY.md).

## Limitations and non-goals

- Local development and CI only. hidane is not a production database and must not be exposed on
  a network without authentication.
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

[MIT](LICENSE). The license may change to `MIT OR Apache-2.0` before the first release
([ADR 0001](docs/adr/0001-license.md)).

## Trademarks

Firebase, Cloud Firestore and Google Cloud are trademarks of Google LLC. hidane is an independent
open-source project and is not affiliated with, endorsed by, or sponsored by Google.
