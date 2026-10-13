# Versioning and compatibility

What a hidane version number promises: which surfaces are stable, what patch, minor and major
releases may change, how each release records what it was verified against, and how a stable
surface is deprecated. Where releases are published, and what a long-term-support line would
take, is in [release-channels.md](release-channels.md).

Nothing here promises more than the tests check. Each stable surface below names the tests
that hold it in place; a surface without one is listed as documented but not under test.

## One version for everything

A release has one version, `<major>.<minor>.<patch>`, shared by the binary, the crates
(`hidane`, `hidane-core`, `hidane-proto`, `hidane-rules`), the npm packages (`hidane` and
`@hidane-dev/<platform>`), the pub.dev launcher, the container image and the Homebrew formula.
The tag `v<version>` builds all of them from one commit ([releasing.md](releasing.md)). A
published version is not rebuilt or replaced; a fix is a new version.

## Before 1.0

hidane is at 0.x, and follows SemVer's rules for 0.x as Cargo reads them:

- A **patch** release (`0.4.0` → `0.4.1`) fixes bugs and compatibility gaps. It does not change
  a stable surface in a way that breaks its users.
- A **minor** release (`0.4` → `0.5`) adds behaviour, and may also change a stable surface,
  after deprecating it where that is practical ([Deprecation](#deprecation)). Every such change is
  listed under **Breaking changes** in the release notes, with what to do about it.

Only the latest release is supported: fixes go into the next release, and nothing is backported
to an older minor line ([SECURITY.md](../SECURITY.md) says the same for security fixes). To
upgrade on your own schedule, pin an exact version ([release-channels.md](release-channels.md#pinning-a-version)).

No date is set for 1.0.

## From 1.0

| Release | May contain | Never contains |
|---|---|---|
| Patch (`1.2.3` → `1.2.4`) | Bug fixes, compatibility fixes, security fixes | A new feature, a deprecation, an incompatible change to a stable surface |
| Minor (`1.2` → `1.3`) | New Firestore behaviour, newly verified clients or firebase-tools and official emulator versions, new commands, flags and JSON fields, deprecations | The removal or incompatible change of a stable surface |
| Major (`1` → `2`) | The removal or incompatible change of a stable surface: a flag, an exit code, a JSON schema version, a default port, an asset name, an environment variable, after its deprecation | |

## Compatibility fixes are not breaking changes

For the Firestore API, hidane's contract is the official emulator's behaviour. A change that
makes hidane answer as the official emulator does (a status code, an error message, an order,
a Security Rules decision, an export file) is a compatibility fix, even when a test written
against hidane's earlier answer starts failing. Small ones ship in patch releases; ones that
change what many tests see go into a minor release and are called out in its notes, as when
v0.2.0 started enforcing Security Rules.

The places where hidane differs from the official emulator on purpose are listed in
[parity-exceptions.md](parity-exceptions.md). Removing one of them is announced in the release
notes as a change.

## Stable surfaces

| Surface | What stays | Checked by |
|---|---|---|
| Emulator flags | Every flag of the official emulator's `--help` (v1.22.0), with its spelling, its value forms (`--single_project_mode` and `--single_project_mode true`) and its default: `--host localhost`, `--port 8080`. Unknown flags are rejected | `crates/hidane/src/cli.rs` (tests), `crates/hidane/tests/process.rs` (the argument set firebase-tools passes) |
| Startup and shutdown | The banner on stdout, with `API endpoint: http://<host>:<port>`, `export FIRESTORE_EMULATOR_HOST=<host>:<port>` and `Dev App Server is now running.`; `address already in use` on stderr when the port is taken; `POST /shutdown` | `crates/hidane/tests/process.rs` |
| Exit codes | The codes in [the table below](#exit-codes) | `process.rs`, `launch.rs`, `doctor.rs`, `migrate.rs` and `bench.rs` in `crates/hidane/tests/` |
| Ports | gRPC, REST and WebChannel on the one port `--port`; the Emulator UI's request feed on `--websocket_port`, when given; the container image listens on `0.0.0.0:8080` | `crates/hidane/tests/single_port.rs`, `websocket_requests.rs`; the container job of `.github/workflows/install.yml` |
| The Firestore API and the emulator's endpoints | What the official emulator answers over gRPC, REST and WebChannel, and on `/emulator/v1/…`, except the [parity exceptions](parity-exceptions.md) | The recorded answers in the [oracle corpus](oracle-corpus.md), replayed by the test suite and counted in [parity.md](parity.md) |
| `hidane exec -- <command>` | Runs the command with hidane standing in for `java` and the official jar ([ADR 0006](adr/0006-launch-under-firebase-tools.md)); returns the command's exit code | `crates/hidane/tests/launch.rs`; the firebase-tools versions are in [compatibility.md](compatibility.md) |
| `doctor`, `migrate` and `benchmark` command lines | The subcommand names and their flags: `--path`, `--format`, `--apply`, `--force` (and `hidane benchmark`'s, in [bench/README.md](../bench/README.md)) | `doctor.rs`, `migrate.rs`, `bench.rs` in `crates/hidane/tests/` |
| JSON output | The shapes in [the section below](#json-output), versioned by their schema field | Golden reports, golden patches and a JSON Schema, below |
| Export format | The official emulator's export files: hidane reads the official emulator's exports, and the official emulator reads hidane's ([export-format.md](export-format.md)) | `crates/hidane/tests/export_import.rs`, against exports recorded from the official emulator |
| Release assets | Archive names (`hidane-<version>-<target>.tar.gz`, `.zip` on Windows), their layout, `sha256sums.txt`; the package names on each registry; the image tags `<version>`, `<major>.<minor>` and `latest` ([releasing.md](releasing.md#assets)). `install.sh`, setup-hidane, cargo-binstall and mise find the archives by these names | `.github/workflows/install.yml`, which installs every release through every channel; setup-hidane's own CI |

`hidane doctor`, `hidane migrate` and `hidane benchmark` are first released after v0.4.0; their
schemas start at version 1 there.

### Exit codes

| Command | 0 | 1 | 2 | Other |
|---|---|---|---|---|
| `hidane` (the emulator) | Stopped by `POST /shutdown`; `--help`, `--version`, `--licenses` | Could not start (the port is taken, the export to import cannot be read) or the server failed | A usage error: an unknown flag, a bad value, contradicting flags, a mode hidane refuses to start in | 130 after SIGINT (Ctrl-C), 143 after SIGTERM, as the JVM exits; firebase-tools treats anything but 0 and 130 as a failure |
| `hidane exec -- <command>` | The command's own exit code, or 128 + the number of the signal that ended it; 1 when hidane cannot run the command | | | |
| `hidane doctor` | No finding is incompatible | A finding is incompatible | A usage error, or `--path` is not a directory | |
| `hidane migrate` | The plan was printed or applied, or there was nothing to change | `--apply` refused (uncommitted changes, files changed meanwhile) or failed | A usage error, or `--path` is not a directory | |
| `hidane benchmark` | The run completed, stable or not | The run completed, but some runs failed (`errors` in the result) | The benchmark could not run (no result is written), or a usage error | |

### JSON output

| Output | Version | Shape | Checked by |
|---|---|---|---|
| `hidane doctor --format json` | `"schema": "hidane.doctor/1"` | [migrating.md](migrating.md#json) | The golden reports `crates/hidane/tests/fixtures/projects/*.doctor.json` (`tests/doctor.rs`) |
| `hidane migrate --format json` | `"schema": "hidane.migrate/1"` | [migrating.md](migrating.md#json-1) | `crates/hidane/tests/migrate.rs` |
| `hidane migrate --format patch` | A unified diff that `git apply` reads | [migrating.md](migrating.md) | The golden patches in `crates/hidane/tests/fixtures/projects/` (`tests/migrate.rs`) |
| `hidane benchmark` | `"schema_version": 1`; the canonical parameters are versioned apart, by `benchmark.version` | [bench/schema/result.schema.json](../bench/schema/result.schema.json) | `crates/hidane/tests/bench.rs` validates the output against the schema |

Within one schema version:

- Fields can be added. A consumer ignores the fields it does not know.
- New finding codes (`code`) and groups (`group`) can appear in any release. A consumer that
  does not know a code reads the finding's `status`.
- No field is removed, renamed, given another type or another meaning, and the values of
  `status` and `severity` stay the same set.

Any other change is a new schema version (`hidane.doctor/2`), made in a minor release before 1.0
and a major release after, and named in the release notes. The text formats (`--format text`,
the Markdown of `hidane benchmark render`) are for people and can change in any release.

## Not covered

- **The Rust APIs** of the `hidane`, `hidane-core`, `hidane-proto` and `hidane-rules` crates. They
  are published so that `cargo install hidane` works, pin each other with `=`, and can change in
  any release, as their READMEs say.
- **Messages for people**: warnings and errors on stderr (except `address already in use`), the
  text formats above, `--help` texts beyond the flags themselves.
- **Internal environment variables**: `HIDANE_SHIM_DIR` and `HIDANE_JAVA_SHIM_CALLED` (set by
  `hidane exec` for its own `java` shim), `HIDANE_BENCH_TARGET`, `HIDANE_BLESS`.
- **Performance figures.** They are measured, not promised ([bench/README.md](../bench/README.md)).
- **Behaviour no test checks.** Where hidane does something the tests do not record, a change to
  it is a bug fix, not a breaking change.

### Documented, not under test

These are documented and changed only with a note in the release notes, but no test checks them
yet:

- `install.sh`'s `HIDANE_VERSION`, `HIDANE_INSTALL_DIR` and `HIDANE_RELEASES_URL`, and the pub.dev
  launcher's `HIDANE_CACHE_DIR` and `HIDANE_RELEASES_URL` ([packaging/pub](../packaging/pub/README.md)).
- `EXPERIMENTAL_MODE=true`, which prints the official emulator's experimental-mode line and changes
  nothing else.

## What each release was verified against

The documents at a release's tag are its record: `https://github.com/hidane-dev/hidane/blob/v<version>/docs/…`
shows what that release was tested with, and a tag does not move.

- **The official emulator.** Every recording the tests replay was made against one pinned
  `cloud-firestore-emulator` version, today v1.22.0, the one firebase-tools 15.33.0 downloads. The
  [oracle corpus](oracle-corpus.md) lists each recording with the version it came from, and the
  weekly *Official emulator* workflow fails when firebase-tools' latest release pins another
  version.
- **firebase-tools and the SDKs.** [compatibility.md](compatibility.md) lists, per client, whether
  it is verified and with which version; [migrating.md](migrating.md#findings) lists the
  versions `hidane doctor` treats as verified. Each comes from a transcript in `results/` compared
  with the official emulator's.
- **Parity.** [parity.md](parity.md) counts the recorded answers the tests replay, per area and
  transport, at that commit.
- **The release notes** name the official emulator and firebase-tools versions the release was
  verified with, and list breaking changes and deprecations under their own headings.

A newer official emulator or firebase-tools release counts as verified only once the recordings
have been checked against it. Until then, `hidane doctor` reports such a version as
`likely-compatible` (same major version) or `unknown`, never as `compatible`.

## Deprecation

A stable surface is removed or changed incompatibly only after it has been deprecated:

1. The release that deprecates it says so in its notes under **Deprecated**, with the replacement,
   and in the documents that describe it. Where the surface is used at run time (a flag, an
   environment variable), hidane prints a warning when it is used.
2. It keeps working for at least one minor release before 1.0, and until the next major release
   from 1.0.
3. The release that removes it lists it under **Breaking changes**.

Three cases go faster:

- **Before 1.0**, a minor release can change a stable surface without a deprecation period when
  keeping the old form alongside the new one is not practical. The notes say why.

- **Following the official emulator.** When the official emulator or firebase-tools renames or
  drops a flag, an endpoint or a behaviour, hidane follows it in a minor release, and keeps
  accepting the old form for as long as a firebase-tools version it supports still uses it.
- **Security.** A fix for a vulnerability can change a stable surface in any release, when keeping
  the surface would keep the vulnerability. The notes say what changed and why.
