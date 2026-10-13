# hidane documentation

| Document | What it answers |
|---|---|
| [why.md](why.md) | Why replace the official emulator: Java 21, startup / memory, write slowdown — with measurements and sources |
| [compatibility.md](compatibility.md) | Which SDKs will work with which hidane version, how each finds the emulator, which RPCs they use |
| [migrating.md](migrating.md) | What `hidane doctor` checks in a Firebase project, and what `hidane migrate` changes to move it from the official emulator to hidane |
| [parity.md](parity.md) | How many of the official emulator's recorded answers the tests replay against hidane, per area and transport, and which SDK transcripts are identical (generated) |
| [oracle-corpus.md](oracle-corpus.md) | The Hidane Oracle Corpus: every recording of the official emulator, listed with its provenance in a generated manifest ([oracle-corpus.json](oracle-corpus.json), [schema](oracle-corpus.schema.json)) |
| [parity-exceptions.md](parity-exceptions.md) | Where hidane intentionally differs from the official emulator, and where the official emulator differs from production |
| [releasing.md](releasing.md) | How a release is built, what it contains, and how a maintainer cuts and publishes one |
| [setup-hidane-marketplace.md](setup-hidane-marketplace.md) | How a maintainer lists the `hidane-dev/setup-hidane` Action on GitHub Marketplace, and releases it later |
| [ROADMAP.md](ROADMAP.md) | Tentative scope of v0.1 / v0.2 / v0.3, targets, risks |
| [clean-room/](clean-room/README.md) | Where hidane's behaviour comes from: the allowed sources, how the official emulator is observed, the provenance of its recordings, and what contributors must not use |
| [adr/](adr/) | Draft decision records: license, storage, gRPC stack, WebChannel timing, Rules engine |
| [../results/](../results/) | The official emulator baseline, hidane's measurements and the SDK differentials |

Work is tracked in [issues](https://github.com/hidane-dev/hidane/issues) grouped by
[milestone](https://github.com/hidane-dev/hidane/milestones). Epics carry the `epic` label.
