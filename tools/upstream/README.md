# Upstream watcher

hidane is verified against particular releases: the official Cloud Firestore emulator its
recordings come from, firebase-tools, and the SDKs whose results were compared on both emulators
(`crates/hidane/src/project/verified.rs`, the versions `hidane doctor` treats as verified). The
Upstream workflow (`.github/workflows/upstream.yml`) notices when any of them has a new release.

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
watch list, or what it is checked against:

- **watch list** runs the watcher's tests (`test_watch.py`, offline) and
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

It only reads, with `contents: read`; it never pushes, and needs no secret. To run it locally:

```sh
python3 tools/upstream/watch.py check
python3 tools/upstream/watch.py detect                          # prints the summary
python3 -m unittest discover -s tools/upstream -p "test_*.py"   # the tests, offline
```

## When something is new

- **A new official emulator** (stable, latest or prerelease, or unreleased): run the Official
  emulator workflow by hand with its version and SHA-256 (from the summary): each drift job
  records against that jar and shows which answers changed. To move to it, follow
  [Bumping the verified versions](#bumping-the-verified-versions).
- **A new release of a verified SDK or tool**: run the harness that verified it
  ([`tools/oracle/README.md`](../oracle/README.md)) with the new release against both emulators
  and compare the transcripts; if they still match, bump it as below. A prerelease is worth a
  look early, but only a final release is recorded as verified.
- **A new release of a package watched without being verified** (`firebase-admin` in each
  language, whose Firestore client is the SDK listed next to it): check whether it moved to
  another release of that client, then set its `version` in `watch.json` to the release reviewed.

Nothing is verified by the watcher itself: a release counts as verified only when a test or a
transcript in the repository shows its results on both emulators.

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
