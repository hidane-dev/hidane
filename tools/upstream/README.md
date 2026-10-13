# Upstream watcher and compatibility canary

hidane is verified against particular releases: the official Cloud Firestore emulator its
recordings come from, firebase-tools, and the SDKs whose results were compared on both emulators
(`crates/hidane/src/project/verified.rs`, the versions `hidane doctor` treats as verified). The
Upstream workflow (`.github/workflows/upstream.yml`) notices when any of them has a new release,
checks hidane against a new official emulator, and opens an issue when that release answers
differently.

## What it watches

[`watch.json`](watch.json) lists each package with the release hidane was verified against
(`verified: true`, the version in `verified.rs`) or, for a package watched without being verified,
the release last reviewed (`verified: false`):

| Ecosystem | Packages | Read from |
|---|---|---|
| npm | `firebase-tools`, `firebase`, `firebase-admin`, `@google-cloud/firestore`, `firebase-functions`, `@firebase/rules-unit-testing` | `https://registry.npmjs.org/-/package/<name>/dist-tags` |
| PyPI | `google-cloud-firestore`, `firebase-admin` | `https://pypi.org/pypi/<name>/json` |
| Go | `cloud.google.com/go/firestore` | `https://proxy.golang.org/<module>/@v/list` and `@latest` |
| Maven | `com.google.cloud:google-cloud-firestore`, `com.google.firebase:firebase-admin` | `https://repo1.maven.org/maven2/<group>/<artifact>/maven-metadata.xml` |
| pub | `cloud_firestore` | `https://pub.dev/api/packages/<name>` |

and the official emulator: the version and SHA-256 of the jar the fixtures were recorded from
(`.github/actions/official-emulator/pinned.sh`) and the Emulator UI verified with it. For each
firebase-tools release it sees, and for firebase-tools' default branch, the watcher reads
`src/emulator/downloadableEmulatorInfo.json` at the commit npm says the release was published
from (`gitHead` in `https://registry.npmjs.org/firebase-tools/<version>`, the release's tag when
there is none): the Firestore emulator's `version`, `expectedChecksumSHA256` and `remoteUrl`, and
the Emulator UI's version.

Only these public metadata endpoints are read, never a page meant for people. Every string taken
from them (versions, checksums, commits, URLs) is checked against a strict pattern before it is
used or written; one that does not match is an error, not a value. A jar is only ever named for
download when firebase-tools downloads it from
`https://storage.googleapis.com/firebase-preview-drop/emulator/cloud-firestore-emulator-v<version>.jar`,
the place `.github/actions/official-emulator` fetches it from; any other URL is reported, not
used.

## Channels

Each package is seen on three channels:

- **stable**: its newest final release, without a prerelease part;
- **latest**: the release its registry hands out by default (npm's `latest` dist-tag, PyPI's
  `info.version`, Go's `@latest`, Maven's `<latest>`, pub.dev's `latest`), shown apart from
  stable only when it is another release;
- **prerelease**: its newest prerelease (`rc`, `beta`, `alpha`, `dev`, `next`, `preview`) newer
  than the stable release. On npm these come from the dist-tags in `npm_prerelease_tags` (`next`,
  `beta`, `rc`, `preview`), not from `canary` or the feature tags some packages publish; on the
  other registries, from the list of versions (yanked and retracted releases left out).

A release on any channel that is newer than the watched `version` is **new**. The official
emulator is new when a firebase-tools release on any channel pins another jar (version or
SHA-256) than `pinned.sh`, or firebase-tools' default branch does (**unreleased**: the jar its
next release will download).

## The workflow

It runs daily, by hand (`workflow_dispatch`), and on pull requests that change the watcher, the
canary or what they run; CI runs their tests and `watch.py check` on every pull request:

- **watch list** runs the tests (`test_watch.py`, `test_canary.py`, `test_sdk_canary.py`,
  offline) and `python3 tools/upstream/watch.py check`, which fails when `watch.json` does not
  name the versions the repository says are verified: `docs/sdk-certification.json`'s
  `verified_packages` (generated from the recorded results; `verified.rs` is generated from it),
  or `verified.rs` itself where that file is not there yet; `pinned.sh`, and
  `docs/compatibility.json`'s `official_emulator` where it is there; and the Emulator UI in
  `verified.rs`.
- **detect new releases** runs `python3 tools/upstream/watch.py detect`: the job summary has a
  table of every package's channels with what is new, and which emulator each firebase-tools
  channel pins; the report (`upstream.json`) is uploaded. It fails when a registry did not answer
  or answered something the watcher refuses, after writing the rest.
- **The compatibility canary** (below) runs when something is new, once for each set of new
  releases.
- **open or update issues** files what the canary confirmed.

To run the watcher and the tests locally:

```sh
python3 tools/upstream/watch.py check
python3 tools/upstream/watch.py detect                          # prints the summary
python3 -m unittest discover -s tools/upstream -p "test_*.py"   # the tests, offline
```

## The compatibility canary

For each new official emulator jar, whichever channel pins it:

1. **record** fetches the jar from Google's URL, checks its SHA-256 (both from firebase-tools'
   `downloadableEmulatorInfo.json`, checked by the watcher), and re-records the recordings of the
   Official emulator workflow's drift job against it, each on a freshly started emulator
   (`.github/workflows/official-recordings.yml`, shared with that job): `value_order`, `queries`,
   `aggregations`, `rest`, `limits`, `rules_paths`. Each recording is compared with the committed
   fixture byte for byte; one that differs is recorded again on another fresh emulator, and the
   difference is **confirmed** only when both recordings agree (**unstable** when they do not).
2. **hidane against v…** puts each confirmed recording in its fixture's place and runs
   `cargo test --workspace` (`python3 tools/upstream/canary.py replay`). A test that fails is run
   again with the committed fixtures; it is a **regression** only if it passes there.
3. **compatibility report** (`canary.py report`) writes the job summary and the
   `compatibility-report` artifact (`compatibility-report.json`): for each new emulator, every
   recording's result, hidane's tests, how many of the fixtures the canary re-recorded, and a
   verdict:
   - **same**: every recording is byte for byte the committed one, and hidane's tests pass;
   - **different**: a recording differs, confirmed, or hidane's tests regress with the new
     recordings: a finding, which gets an issue;
   - **inconclusive**: something could not be checked (a recorder or the build failed, a
     difference was not confirmed, or hidane's tests fail with the committed fixtures too). The
     job fails, and the next run tries again.

   New SDK and tool releases are listed with what the canary ran for each (below).

For each new **stable** release of a verified SDK that has a harness in `tools/oracle/`, the
**build hidane** job builds hidane once, and a job per release runs
`python3 tools/upstream/sdk_canary.py run`. A prerelease (and a `latest` that is not the stable
release) is listed in the report as not run, and runs only when asked for by hand: the workflow's
`sdk` input, with that release (see [Running new SDK code](#running-new-sdk-code)).

| Package | Harness | Units |
|---|---|---|
| `go:cloud.google.com/go/firestore` | `tools/oracle/sdk_go/` (`go get` the release, then build) | the sections `documents`, `queries`, `transactions`, `listen` |
| `pypi:google-cloud-firestore` | `tools/oracle/sdk_python/` (a venv from `requirements.txt`, then the release; wheels only) | the same sections |
| `maven:com.google.cloud:google-cloud-firestore` | `tools/oracle/sdk_java/` (a copy of `pom.xml` naming the release) | the same sections |
| `npm:@google-cloud/firestore`, `npm:firebase`, `npm:@firebase/rules-unit-testing` | the `tools/oracle/sdk_*.mjs` scripts that import the package (`npm install --ignore-scripts`; the other two packages at their verified versions) | each script, but `sdk_export_import.mjs` (it runs under firebase-tools) |

Each unit runs once on a freshly started official emulator (the pinned jar, started and stopped by
the action's own scripts) and once on a freshly started hidane, and the two transcripts are
compared: **identical**; **documented** when every difference is one the verified release shows
too, in its committed pair in `results/` (`results/README.md` and `docs/parity-exceptions.md`
explain those) or, when that does not explain it, in a pair of the verified release run the same
way on the same runner (some steps depend on timing: on a runner, `sdk_transactions.mjs` counts 4
attempts against the official emulator where the committed transcript has 3, with the verified
release too). Otherwise the unit runs again on two fresh emulators, and is **different** when the
same unexplained differences come back (a finding, which gets an issue), **unstable** when they do
not. A harness that fails, or prints something that is not JSON, is an **error**. The report also says when the SDK's own transcript on the official emulator changed from
the committed one: its transcripts need recording again before moving to it. The release's verdict
is **same** (every unit identical or documented), **different** or **inconclusive**, as for an
emulator.

Not run, with the reason in the report: firebase-tools' CLI (`hidane exec`, exports; the jar a
new firebase-tools release pins is checked as above when it is a new jar), firebase-functions
(`tools/oracle/functions/` needs firebase-tools and both emulators), `cloud_firestore`
(`tools/oracle/flutter_web/` needs Flutter and a browser), and the packages watched without being
verified (`firebase-admin`). Run those by hand ([When something is new](#when-something-is-new)).

A verdict covers what the canary ran, nothing more: the other fixtures are not recorded against a
new emulator, and a harness covers the steps it has, so **same** is not "verified". The report
says how many fixtures were re-recorded and lists every unit.

The canary runs once for each set of new releases: the report job saves a cache entry named by
the set's fingerprint (`upstream-canary-<sha256>`), and a later run with the same set skips the
canary unless it is run by hand with **force**. A run that was inconclusive saves nothing. A pull
request runs the canary against the pinned emulator and the verified release of the Go, Python,
Java and `firebase` harnesses instead, whose results must come out as committed (verdict **same**
for each). Not `@google-cloud/firestore`: run as the verified release itself, it has nothing but
the committed pair to compare with, and its transactions script counts attempts that depend on
timing.

### Running new SDK code

The SDK jobs install and run an SDK release published hours or minutes earlier, with whatever it
depends on. They get as little as possible: no credential (`persist-credentials: false`, no
secret), a read-only `GITHUB_TOKEN`, no cache restored or saved by a toolchain,
`npm install --ignore-scripts`, wheels only for pip, and hidane as a binary built by another job.

What remains: code running in a job can reach what the runner gives that job, among it the
token the runner uses for the job's artifacts and for the Actions cache, which on the schedule is
`main`'s cache. A malicious release could therefore upload artifacts to the run or write cache
entries that later runs on `main` restore (Rust builds, the jar's entry, whose SHA-256 is checked
again when it is restored). It cannot push, open pull requests or issues, or read a secret.

So the schedule runs the SDK harnesses for **stable** releases only: the ones users install by
default, published through each project's release process. A prerelease or an unreleased build
runs only when a maintainer asks for it by hand (Run workflow, `sdk`: `ecosystem:name@version`),
after looking at what it is. Releases watched without being verified, and packages without a
harness, never run.

## Issues

**open or update issues** opens one issue per finding, labelled `upstream` (created by the
workflow if missing) and `area:parity`. Its body starts with a marker,
`<!-- hidane-upstream-canary key="emulator:<version>:<sha256 prefix>" -->`; a later run that
confirms the same release again updates the open issue carrying that marker instead of opening
another, and leaves a closed one closed. It runs only on `main`, on the schedule, or by hand with
**dry-run** off. Otherwise **issues (dry run)** runs instead, with read-only access, and its
summary says which issues it would open or update.

The issue job is the only one that may write anything (`issues: write`); every other job reads the
repository only (`contents: read`). No job pushes, and none needs a secret beyond the workflow's
own `GITHUB_TOKEN`.

By hand (Actions → Upstream → Run workflow):

- **dry-run** (on by default): only say which issues would be opened or updated;
- **force**: run the canary even if it already ran for these releases;
- **emulator-version** and **emulator-sha256**: also run the canary against this jar, new or not
  (the pinned one, `1.22.0` and `9b6498b7…`, should come out **same**);
- **sdk**: also run the canary for this release of a watched package, `ecosystem:name@version`
  (for example `pypi:google-cloud-firestore@2.34.1`, the verified one, which should come out
  **same**); the only way a prerelease's harness runs.

### Acting on a canary issue

1. Open the run the issue links, and download its artifacts. For an emulator, each
   `canary-<release>-<recorder>` holds the recording (`out/<recorder>.json`, and `.again.json` from
   the second emulator), `result.json` and the emulator's output: compare them with the committed
   fixtures (`diff -u`). For an SDK, `sdk-<release>` holds `sdk.json` (the differing paths of each
   unit), the transcripts of both emulators (`transcripts/`) and the logs: compare them with the
   committed pair in `results/`.
2. Decide what the difference is: a fix or a new behaviour of the official emulator that hidane
   should follow, or something hidane should not follow (a bug), to be recorded in
   `docs/parity-exceptions.md`. The canary does not decide: hidane follows the official emulator
   ([`docs/clean-room/OBSERVATION_POLICY.md`](../../docs/clean-room/OBSERVATION_POLICY.md#how-a-difference-is-decided)).
3. When the release is stable and hidane should move to it, follow
   [Bumping the verified versions](#bumping-the-verified-versions), and close the issue from that
   pull request. For a prerelease or an unreleased pin, wait for firebase-tools to ship it, or
   prepare the change on a branch; the issue stays open meanwhile, and the canary keeps it up to
   date.
4. A difference that is not real (a recorder that depends on something else that changed) is a
   bug in the recorder: fix it in `tools/oracle/`, and close the issue.

## When something is new

- **A new official emulator** (stable, latest or prerelease, or unreleased): the canary has
  already re-recorded the drift recordings against it; its report says what changed. To see a
  release by hand, run the Official emulator workflow with its version and SHA-256 (from the
  summary). To move to it, follow [Bumping the verified versions](#bumping-the-verified-versions).
- **A new release of a verified SDK or tool**: run the harness that verified it
  ([`tools/oracle/README.md`](../oracle/README.md)) with the new release against both emulators
  and compare the transcripts; if they still match, bump it as below. A prerelease is worth a
  look early, but only a final release is recorded as verified.
- **A new release of a package watched without being verified** (`firebase-admin` in each
  language, whose Firestore client is the SDK listed next to it): check whether it moved to
  another release of that client, then set its `version` in `watch.json` to the release reviewed.

Nothing is verified by the watcher or the canary themselves: a release counts as verified only
when a test or a transcript in the repository shows its results on both emulators.

## Bumping the verified versions

- **An SDK or tool**: record its transcripts again with the new release
  ([`tools/oracle/README.md`](../oracle/README.md)) and commit them in `results/`; update the
  harness's pin (`tools/oracle/sdk_go/go.mod`, `sdk_python/requirements.txt`,
  `sdk_java/pom.xml`, or the `npm i` line), regenerate what is generated from the recorded
  results (`docs/sdk-certification.json` and the `VERIFIED` entries of
  `crates/hidane/src/project/verified.rs`, with `tools/parity/`), update `docs/compatibility.md`
  where it names the version, and its `version` in `watch.json`.
- **The official emulator**: re-record every fixture and official transcript against the new jar
  ([`docs/oracle-corpus.md`](../../docs/oracle-corpus.md#versions) says how), change hidane where
  the answers changed, and set the new version and SHA-256 in
  `.github/actions/official-emulator/pinned.sh`, `watch.json`'s `emulator`, and the firebase-tools
  release that pins it in `verified.rs` and `watch.json`. When the Emulator UI changes with it,
  check it against hidane and update `EMULATOR_UI_VERSION` in `verified.rs` and `ui` in
  `watch.json`.
- **A package to watch**: add it to `watch.json` (and, once verified, to `verified.rs`). A new
  ecosystem needs a reader in `watch.py`, with tests.

Then run `python3 tools/upstream/watch.py check`; the pull request's Upstream run does too.
