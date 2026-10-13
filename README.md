<h1 align="center">hidane (火種, <em>the seed of fire</em>) — a Firestore emulator without Java.</h1>

<div align="center">

<p>
  <a href="https://hidane.dev">hidane.dev</a> ·
  <a href="https://hidane.dev/ja/">日本語</a>
</p>

[![release](https://img.shields.io/github/v/release/hidane-dev/hidane?color=E2553D)](https://github.com/hidane-dev/hidane/releases)
[![parity](https://img.shields.io/endpoint?url=https%3A%2F%2Fhidane.dev%2Fbadge%2Fparity.json)](docs/parity.md)
[![license: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-1E1B18)](#license)
[![website](https://img.shields.io/badge/web-hidane.dev-1E1B18)](https://hidane.dev)

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/hidane-dev/hidane/main/docs/assets/readme-banner-dark.png">
  <img alt="hidane. One binary. No JDK. Milliseconds to start. A Firestore emulator that speaks the official one's gRPC, REST and WebChannel protocols and Security Rules, tested against it as the oracle." src="https://raw.githubusercontent.com/hidane-dev/hidane/main/docs/assets/readme-banner-light.png" width="100%">
</picture>

<img alt="A terminal recording: the same 200 Firestore tests under firebase emulators:exec, first on the official emulator, then with hidane exec in front; both pass, and the hidane run finishes sooner." src="https://raw.githubusercontent.com/hidane-dev/hidane/main/docs/assets/demo.gif" width="100%">

<sub>Recorded on a laptop, so its timings are that machine's. The figures below are medians from the canonical benchmark.</sub>

<p><strong>Run your Firestore tests under <code>firebase-tools</code> without installing Java.</strong></p>

</div>

hidane (pronounced *hi-da-ne*) aims to be a drop-in replacement for the official Cloud Firestore
emulator: a single Rust binary that speaks the same gRPC, REST and WebChannel wire protocols,
evaluates the same Security Rules, reads and writes the same import / export directories, and runs
under `firebase-tools`. No JDK to install. Starts in milliseconds. Tested for parity against the
official emulator as the oracle.

> **v0.4.0 delivers Firestore events to Cloud Functions.** hidane serves the Firestore API over
> gRPC, REST and WebChannel: documents, queries, aggregations, vector search, transactions,
> listeners, exports and imports, and the emulator's own endpoints, each checked against the
> official emulator; `firebase-tools` launches it in place of the official one. It evaluates
> `firestore.rules` as the official emulator does (since v0.2.0), firebase-js-sdk's own browser
> test suite passes on it (v0.3.0), and in v0.4.0 Firestore-triggered functions, v1 and v2, fire
> under the Functions emulator with the same data as with the official one. The Go, Python and Java
> server SDKs are verified too.
> The plan is in
> [`docs/ROADMAP.md`](docs/ROADMAP.md), the work in
> [issues](https://github.com/hidane-dev/hidane/issues).

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

## Quick start

```sh
hidane exec -- firebase emulators:start --only firestore  # firebase-tools starts hidane, not Java and the jar
hidane exec -- firebase emulators:exec "npm test"         # the same around a test run, as in CI
hidane --host 127.0.0.1 --port 8080                       # or on its own,
export FIRESTORE_EMULATOR_HOST=127.0.0.1:8080             # with the SDKs pointed at it
```

`hidane exec` runs the command with hidane standing in for `java` and the official jar, for that
command only ([ADR 0006](docs/adr/0006-launch-under-firebase-tools.md)).

## Why hidane?

The official emulator is a closed-source Java application shipped as a jar. Three things about
it hurt everyday local development and CI (details and raw logs: [`docs/why.md`](docs/why.md)):

<div align="center">

| | Official emulator v1.22.0 | hidane v0.4.0 |
|---|---|---|
| Runtime | Java 21 or newer ([required since firebase-tools 15.0.0](https://github.com/firebase/firebase-tools/releases/tag/v15.0.0)) | none, single binary |
| Time until the port accepts | 941 ms | 1.56 ms |
| Resident memory, idle | 148 MiB | 7.8 MiB |
| Resident memory after 1,000 documents | 268 MiB | 10.1 MiB |
| 100,000 documents written in commits of 500 | 17.1 s | 1.46 s |
| `firebase emulators:exec` around a CI-style workload (100 documents seeded, 200 tests) | 6.75 s | 1.67 s |

</div>

In our canonical benchmark environment, these are the medians for both emulators on the same
machine: an AWS c7i.xlarge (Intel Xeon Platinum 8488C, 4 vCPUs, 8 GiB), Ubuntu 24.04, Java
21.0.12.1, firebase-tools 15.33.0, and hidane v0.4.0's published binary (commit `8a81062`), measured
on 2026-10-13. The [result](https://github.com/hidane-dev/benchmark-results/blob/main/results/release/2026/10/2026-10-13-release.md)
comes with every sample in its JSON. The methodology is in [`bench/`](bench/README.md), and anyone
can rerun it; other machines give other numbers.

One cost the benchmark does not measure yet (#50) is writing while a listener is attached. In
that case, the official emulator's batch writes slow down as documents accumulate
([firebase-tools#3477](https://github.com/firebase/firebase-tools/issues/3477)), while hidane's
stay flat per batch ([`docs/why.md`](docs/why.md#3-writes-slow-down-while-a-listener-is-attached)).

## What works today

Which clients can use hidane depends on the transport their SDK speaks to an emulator
([`docs/compatibility.md`](docs/compatibility.md)). "Verified" means the client's results were
compared with the official emulator's (`results/`):

| Client | Transport | In v0.4.0 |
|---|---|---|
| firebase-admin (Node) / `@google-cloud/firestore` | gRPC | verified: documents, queries, aggregations, vector search, transactions, listeners |
| firebase-js-sdk in Node | gRPC | verified: writes, transactions, listeners, offline and resume |
| firebase-js-sdk Lite | REST | verified |
| firebase-js-sdk in the browser | WebChannel and REST | verified in Chromium: the SDK's own browser suite passes, 1,183 tests, as on the official emulator ([`tools/js-sdk-browser/`](tools/js-sdk-browser/)); listeners, writes, queries, long polling, mock tokens ([`docs/webchannel.md`](docs/webchannel.md)) |
| Flutter Web (`cloud_firestore`) | WebChannel and REST, through firebase-js-sdk | verified in Chromium: reads, writes, transforms, queries, aggregations, `snapshots()`, batches, transactions |
| `@firebase/rules-unit-testing` | the above | verified under Security Rules, in Node and in Chromium: `assertSucceeds` / `assertFails` for signed-in, signed-out and rules-disabled contexts (gets, lists, listeners, writes, batches, transactions), `clearFirestore()` |
| Go (`cloud.google.com/go/firestore`), Python and Java (`google-cloud-firestore`) | gRPC | verified: documents, queries, aggregations, vector search, transactions, listeners |
| iOS, Android, Flutter (Android, iOS, macOS, Windows), C++ / Unity | gRPC | same transport, not verified yet |
| Security Rules | every transport | enforced: `allow` conditions, `get` / `exists`, `list` decided from the query's filter, mock tokens, denial messages, `:ruleCoverage`, `debug()`; each recorded from the official emulator and replayed ([`docs/parity-exceptions.md`](docs/parity-exceptions.md) lists the differences) |
| `firebase emulators:start` / `emulators:exec` | `hidane exec -- firebase …` | verified with firebase-tools 15.33.0, no Java installed; with the Functions emulator, Firestore-triggered functions, v1 and v2 (`withAuthContext` too), fire on the same writes with the same data as with the official emulator ([`docs/parity-exceptions.md`](docs/parity-exceptions.md) lists the differences) |
| Emulator UI | REST and the emulator's endpoints | verified: browse, create and delete documents, Clear all data, and the request monitor (the Requests tab) |
| Export / import (`--import`, `--export-on-exit`, `emulators:export`) | the official format | verified: hidane and the official emulator read each other's exports, byte for byte the same files ([`docs/export-format.md`](docs/export-format.md)) |

## Parity

hidane treats the official emulator as the oracle. Scripts in [`tools/oracle/`](tools/oracle/)
ask the official emulator how it answers — from value ordering to WebChannel framing, export
files and Security Rules — and the test suite replays every recording against hidane; SDK transcripts
from both emulators are compared in [`results/`](results/). [`docs/parity.md`](docs/parity.md)
counts the recorded answers the tests replay, per area and transport, generated from the
fixtures. No claim of compatibility without a test behind it. Differences that are intentional (performance bugs and
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
| Security Rules | enforced (v0.2) | not documented | not documented |
| Verification | the official emulator's answers recorded and replayed, SDK transcripts compared | own test suite, also runnable against real Cloud Firestore | tested against the official emulator |

As read from their READMEs in October 2026; corrections welcome.

## Migrating

`hidane doctor` checks a Firebase project before it moves to hidane, without running or changing
anything: `firebase.json` and the emulators it starts, the rules, the SDK and firebase-tools
versions, and the CI workflows and scripts that start the emulators, each marked verified, not
verified or incompatible. `hidane migrate` shows the changes as a diff (`hidane exec --` before
`firebase emulators:exec`, `hidane-dev/setup-hidane` where `actions/setup-java` served only the
Firestore emulator) and makes them with `--apply`. [`docs/migrating.md`](docs/migrating.md)
explains both.

## Documentation

- [Why hidane](docs/why.md) — the measurements behind the table above, with sources
- [Compatibility](docs/compatibility.md) — transport per SDK, how each finds the emulator, which RPCs they use
- [Migrating](docs/migrating.md) — `hidane doctor` and `hidane migrate`: checking a project that used the official emulator, and moving it to hidane
- [Parity](docs/parity.md) — the recorded answers the tests replay, per area and transport, and the SDK transcripts
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

Contributions are accepted under a [Contributor License Agreement](docs/cla/README.md), which a
pull request asks for once. Contributors keep the copyright in their contributions; the project
receives the rights to use, distribute, sublicense and relicense them.

## Limitations and non-goals

- Local development and CI only. hidane is not a production database and must not be exposed on
  a network without authentication.
- Security Rules are evaluated once per condition: where the official emulator evaluates twice and
  lists the reasons of both passes in a denial (firebase-tools#6252), hidane lists one.
- Cloud Firestore only. The Realtime Database, Cloud Storage and Auth emulators are out of scope.
- Features the official emulator does not implement either (the Admin API, TTL, point-in-time
  recovery, bundles) stay unimplemented. See `docs/parity-exceptions.md`.

## Acknowledgements

hidane is a clean-room implementation written from the public protobuf definitions
([googleapis](https://github.com/googleapis/googleapis)), the public Firebase and Google Cloud
documentation, and black-box observation of the official emulator. No code was decompiled or
ported from the official jar. [`docs/clean-room/`](docs/clean-room/README.md) lists every source
hidane's behaviour may come from (the SDKs' and firebase-tools' public source and CEL's
specification among them), how the official emulator is observed, and how each behaviour traces
back. Thanks to the authors of skunkteam/rust-firestore-emulator and
YutaUra/firestore-emulator, whose public work helped us map the landscape, and to the model
projects whose approach to parity testing we follow: [dynoxide](https://github.com/nubo-db/dynoxide)
and [zerobrew](https://github.com/zerobrewhq/zerobrew).

## Supporting hidane

hidane is free and open source, and stays that way: everything it needs to work as a Firestore
emulator (the wire protocols, Security Rules, imports and exports, compatibility with
firebase-tools, the SDKs and the Emulator UI, and every parity fix) is in this repository under
`MIT OR Apache-2.0`.

If hidane saves your team time in local development or CI, sponsorship helps pay for the
compatibility work, performance work and long-term maintenance it needs, and companies that rely
on it are welcome as corporate sponsors. Organizations interested in priority support, migration
help or long-term compatibility support can contact the maintainers too; paid support is not
offered yet. Sponsorship supports the project; it does not decide what gets built, which is
discussed in the open. To get in touch, start a
[discussion](https://github.com/hidane-dev/hidane/discussions).

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option ([ADR 0001](docs/adr/0001-license.md)).

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in
the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without
any additional terms or conditions. Separately, its author licenses it to the project under the
[Contributor License Agreement](docs/cla/README.md).

## Trademarks

Firebase, Cloud Firestore and Google Cloud are trademarks of Google LLC. hidane is an independent
open-source project and is not affiliated with, endorsed by, or sponsored by Google.
