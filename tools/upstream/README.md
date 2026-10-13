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

- **watch list** runs the tests (`test_watch.py`, `test_canary.py`, offline) and
  `python3 tools/upstream/watch.py check`, which fails when `watch.json` does not name the
  versions the repository says are verified: `docs/sdk-certification.json`'s
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

   New SDK and tool releases are listed with what the canary ran for each. Today that is nothing:
   each is listed as **not run**, with the reason. The jar a new firebase-tools release pins is
   checked as above when it is a new jar, but firebase-tools' CLI (`hidane exec`, exports) is not
   run; `tools/oracle/functions/` (firebase-functions) and `tools/oracle/flutter_web/`
   (`cloud_firestore`) need tools the runners do not set up; the SDK harnesses in
   `tools/oracle/` are run by hand ([When something is new](#when-something-is-new)).

A verdict covers the recordings the canary re-recorded and hidane's tests, nothing more: the
other fixtures are not recorded against the new release, so **same** is not "verified". The
report says how many fixtures were re-recorded.

The canary runs once for each set of new releases: the report job saves a cache entry named by
the set's fingerprint (`upstream-canary-<sha256>`), and a later run with the same set skips the
canary unless it is run by hand with **force**. A run that was inconclusive saves nothing. A pull
request runs the canary against the pinned emulator instead, whose recordings must come out as
committed (verdict **same**).

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
  (the pinned one, `1.22.0` and `9b6498b7…`, should come out **same**).

### Acting on a canary issue

1. Open the run the issue links, and download the `canary-<release>-*` artifacts: each holds the
   recording (`out/<recorder>.json`, and `.again.json` from the second emulator), `result.json`
   and the emulator's output. Compare them with the committed fixtures (`diff -u`).
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
