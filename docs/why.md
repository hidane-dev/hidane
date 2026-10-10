# Why hidane

The official Cloud Firestore emulator is a closed-source Java application shipped as a jar and
started by `firebase-tools`. hidane exists because three properties of that setup hurt everyday
local development and CI. Every number below comes from Phase 0 measurements of the official
emulator v1.22.0; the numbers are in [`results/official-v1.22.0-baseline.md`](../results/official-v1.22.0-baseline.md).

> Measurement caveat: the Phase 0 numbers were taken on an M1 Max **while other heavy jobs were
> running** (load average 12–58). They are indicative, not final; a quiet-runner re-measurement
> is tracked in [#50](https://github.com/hidane-dev/hidane/issues/50).

## 1. You need Java 21 to run a local database

- firebase-tools 15.0.0 (2025-12-10) removed support for Java older than 21:
  "[BREAKING] Removed support for running emulators with Java versions prior to 21."
  ([release notes](https://github.com/firebase/firebase-tools/releases/tag/v15.0.0),
  [`MIN_SUPPORTED_JAVA_MAJOR_VERSION = 21`](https://github.com/firebase/firebase-tools/blob/c21c22df54cccc968a067cb6e0cdd23ff3f27c3c/src/emulator/commandUtils.ts#L593-L595),
  deprecation warning introduced in [#9254](https://github.com/firebase/firebase-tools/pull/9254)).
- The jar itself is compiled to class-file version 65 (= Java 21), so even running it directly
  needs a JRE 21+ (`results/official-v1.22.0-baseline.md`).
- The official install guide still says "Java JDK version 11 or higher"
  ([install_and_configure](https://firebase.google.com/docs/emulator-suite/install_and_configure),
  as of 2026-10-06), which is one reason people hit the requirement by surprise.
- `firebase-tools` only looks at the `java` on `PATH` (`JAVA_HOME` is ignored), so a machine with
  several JDKs can fail even when a Java 21 is installed.

Related reports: [#3871](https://github.com/firebase/firebase-tools/issues/3871),
[#2521](https://github.com/firebase/firebase-tools/issues/2521),
[#9623](https://github.com/firebase/firebase-tools/issues/9623).

hidane is a single Rust binary. No JVM, no `PATH` juggling.

## 2. Startup and memory

| Measurement (official emulator v1.22.0) | Value | Evidence |
|---|---|---|
| Time until the port accepts, `java -jar` directly (mean of 10) | 740 ms (first start 1.1 s) | `results/official-v1.22.0-baseline.md` |
| Same, through `firebase emulators:start --only firestore` (mean of 5) | 2.5 s | `results/official-v1.22.0-baseline.md` |
| RSS after 10 s idle | 95 MiB | `results/official-v1.22.0-baseline.md` |
| RSS after writing 1,000 documents | 456 MiB | same |
| RSS after writing 500,000 documents | 2.18 GiB | `results/official-v1.22.0-baseline.md` |
| JVM default heap limit | 25 % of physical RAM (16 GiB on the test machine) | `results/official-v1.22.0-baseline.md` |

hidane targets under 100 ms to first accept and under 50 MiB idle. These are targets, not
measurements; they will be published in the same table once v0.1 exists.

## 3. Writes slow down while a listener is attached

firebase-tools [#3477](https://github.com/firebase/firebase-tools/issues/3477) (and the older
[#2277](https://github.com/firebase/firebase-tools/issues/2277),
[#1624](https://github.com/firebase/firebase-tools/issues/1624)) report batch writes getting slower
as the database grows. Phase 0 reproduced the condition:

| Run (100,000 documents, 500 per batch) | Total time | Per-batch trend | Evidence |
|---|---|---|---|
| No listener | 11.0 s (≈100k docs/s), 500k docs in 42.6 s | flat, 5–15 ms per batch | `results/official-v1.22.0-baseline.md`, `results/batch-write-degradation-official-v1.22.0.csv` |
| `onSnapshot` on the whole collection | 121 s (11×) | grows ≈ +168 ms per 10k documents | `results/official-v1.22.0-baseline.md`, `results/batch-write-degradation-official-v1.22.0.csv` |
| `onSnapshot` with `limit(50)` | 65.5 s (6×) | grows ≈ +70 ms per 10k documents | `results/official-v1.22.0-baseline.md`, `results/batch-write-degradation-official-v1.22.0.csv` |

The slowdown is proportional to the number of documents already stored, not to the size of the
write. The Emulator UI keeps listeners open, which matches the "only slow when the UI is open"
comments in the issue. hidane is designed so that a commit costs only what it changes
(see [ROADMAP](ROADMAP.md) and ADR 0002).

## What hidane is not

- Not a production database. It is a local-development and CI emulator and is not meant to be
  exposed on a network without authentication (see [`SECURITY.md`](../SECURITY.md)).
- Not a reimplementation of the Realtime Database, Cloud Storage or Auth emulators.
- Not derived from the official jar. hidane is a clean-room implementation built from the public
  `google.firestore.v1` protos, the public documentation and black-box observation of the
  official emulator. No decompilation.

hidane is an independent open-source project and is not affiliated with or endorsed by Google LLC.
Firebase and Cloud Firestore are trademarks of Google LLC.
