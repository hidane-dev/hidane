# Migrating from the official emulator

Most projects move their tests from the official Firestore emulator to hidane with two changes:
firebase-tools runs under `hidane exec` ([ADR 0006](adr/0006-launch-under-firebase-tools.md)), and
CI stops installing Java where only the Firestore emulator needed it. `hidane doctor` checks a
project first: what it uses, whether hidane has verified it, and what still needs Java.

## `hidane doctor`

```sh
hidane doctor                    # the project in the current directory
hidane doctor --path ../my-app   # another directory
hidane doctor --format json      # the findings and the facts behind them, for tools
```

It only reads files: it runs nothing, changes nothing and makes no network request.

| It reads | Where | For |
|---|---|---|
| `firebase.json` (comments allowed, as firebase-tools allows them) | the project, up to four directories deep | the Firestore emulator's host, port and `websocketPort`; the other emulators `firebase emulators:start` starts (decided as firebase-tools 15 decides it) and which of them run on Java; `firestore.edition`; several databases; `emulators.singleProjectMode`; the Emulator UI |
| Security Rules | the files `firebase.json` names | compiled with hidane's compiler, which gives the official emulator's errors and warnings |
| Functions | the `functions` source directories | Firestore triggers: `onDocumentCreated` and the other `onDocument…` functions, `functions.firestore.document(…)`, Python's `on_document_…` |
| SDKs and tools | `package.json` with `package-lock.json`, `npm-shrinkwrap.json`, `yarn.lock`, `pnpm-lock.yaml` or `bun.lock`; `requirements*.txt`, `pyproject.toml` with `poetry.lock`, `uv.lock` or `pdm.lock`; `go.mod`; `pom.xml`, `build.gradle(.kts)`; `pubspec.yaml` with `pubspec.lock` | the versions of `firebase`, `firebase-admin` (and the `@google-cloud/firestore` it resolves to), `@google-cloud/firestore`, `@firebase/rules-unit-testing`, `firebase-tools`, `google-cloud-firestore` (Python, Java), `cloud.google.com/go/firestore`, `firebase-firestore` (Android), `cloud_firestore` (Flutter, with its platform directories) |
| CI | `.github/workflows/*.yml` | the steps that start the emulators, directly or through npm scripts and the shell scripts they run; `actions/setup-java` and what else in the job needs Java; `FIRESTORE_EMULATOR_HOST`; caches of `~/.cache/firebase/emulators` |
| Scripts | npm scripts, and shell scripts that workflows and npm scripts run | `firebase emulators:exec` and `emulators:start`, run directly, through `npx`, `pnpm exec`, `yarn` or `bunx`, after environment assignments, in `&&` chains |

### Findings

Each finding has a status:

| Mark | Status | Meaning |
|---|---|---|
| ✓ | `compatible` | Verified with hidane. The finding names its source: a document or a result file in this repository |
| ~ | `likely-compatible` | The same major version as a version verified with hidane, but not that version itself; the finding names the verification. Not an incompatibility |
| ✓ | `ok` | A check that passed and claims nothing about verification (`firebase.json found`) |
| · or ! | `info` | Information; `!` (severity `warning`) when it asks for something, such as keeping Java for another emulator |
| ? | `unknown` | Nothing hidane has verified covers it: manual verification required |
| ✗ | `incompatible` | A known gap, or something that will not work under hidane as it is |

A package's version is judged against the version hidane was verified with:

- `compatible` when the version the project uses is known (resolved by a lockfile, or pinned
  exactly, such as `"firebase-tools": "15.33.0"` or `google-cloud-firestore==2.34.1`) and has the
  verified version's major and minor versions; only the patch version may differ;
- `likely-compatible` when only the major version is the same, or when a range pins that major
  version but the version it resolves to is not known (`^13.0.0` without a lockfile);
- `unknown` otherwise.

The verified versions:

| Package | Verified with | Source |
|---|---|---|
| `firebase` (JS SDK) | 13.0.0, in Node (gRPC), Lite (REST) and Chromium (WebChannel) | [compatibility.md](compatibility.md), `results/sdk-web-writes-*.json`, `results/sdk-rest-*.json`, `results/browser-webchannel-*.json` |
| `@google-cloud/firestore` (the Firestore client of `firebase-admin`) | 9.3.1 | `results/sdk-documents-*.json`, `results/sdk-queries-*.json`, `results/sdk-listen-*.json` |
| `@firebase/rules-unit-testing` | 6.0.0 | `results/sdk-rules-unit-testing-*.json`, `results/browser-rules-*.json` |
| `firebase-tools` | 15.33.0, through `hidane exec` | [ADR 0006](adr/0006-launch-under-firebase-tools.md), `results/export-import-*.json` |
| `firebase-functions` (Firestore triggers under the Functions emulator) | 6.6.0, v1 and v2 triggers | `results/functions-triggers-*.json` |
| `google-cloud-firestore` (Python) | 2.34.1 | `results/sdk-python-*.json` |
| `cloud.google.com/go/firestore` | 1.26.0 | `results/sdk-go-*.json` |
| `com.google.cloud:google-cloud-firestore` (Java) | 3.49.0 | `results/sdk-java-*.json` |
| `cloud_firestore` (Flutter) | 6.10.0, on the web | `results/flutter-web-*.json` |

`firebase-admin` is judged by the `@google-cloud/firestore` its lockfile resolves to; without a
lockfile it is not verified. Firestore triggers are judged by the `firebase-functions` of their
functions; triggers written in Python are not verified. The mobile SDKs and Flutter on Android,
iOS, macOS and Windows are not verified yet ([compatibility.md](compatibility.md)). A version
range that does not pin a major version (`>=2.20`, `*`) is `unknown`. The Emulator UI's version
is the one firebase-tools pins (v1.15.0 for firebase-tools 15.33.0), so it is judged as the
project's firebase-tools is.

Known gaps are `incompatible`:

- The enterprise edition (`firestore.edition: "enterprise"`): pipelines are not implemented yet
  ([#80](https://github.com/hidane-dev/hidane/issues/80)).
- Datastore mode, through `gcloud emulators firestore start --database-mode=datastore-mode`
  ([#81](https://github.com/hidane-dev/hidane/issues/81)). A Datastore client library in the
  dependencies is `unknown`: it may talk to the separate Datastore emulator, which hidane does
  not replace.
- A rules file that does not compile: hidane would start without it. hidane's compiler makes the
  official emulator's checks (`crates/hidane/tests/fixtures/rules_compile.json`), so the official
  emulator most likely rejects the file too; if it does not, please report it.

What still needs Java is information, not a gap: the Realtime Database, Storage and Pub/Sub
emulators run on Java, and under `hidane exec` firebase-tools runs them with the Java on `PATH`.
A CI job keeps `actions/setup-java` when one of them starts, when it runs another Java command
(`./gradlew`, `mvn`, `java`), or when `setup-java` caches a Java build.

`hidane doctor` exits with 0 when no finding is incompatible, 1 when one is, and 2 for a usage
error or a path that is not a directory. `likely-compatible` and `unknown` findings do not change
the exit code.

### JSON

`--format json` prints the report as one object: `schema` (`hidane.doctor/1`, which changes when
the shape does), `hidane` (the version that wrote it), `summary` (the count of each status: `compatible`, `likely_compatible`, `ok`, `info`, `unknown`,
`incompatible`),
`findings` (each with `code`, `group`, `status`, `severity`, `title`, and when they apply
`details`, `source`, `file` and `line`; paths are relative to the project) and `facts` (what the
scan read: `firebase`, `rules`, `functions`, `dependencies`, `emulator_commands`, `workflows`,
`scripts`, and `unread`, the places that start the emulators in a way the scan could not read).
The engine behind it is the `hidane::project` module, which reads a directory or files held in
memory.

## Changing a project

### GitHub Actions

When a job installs Java only for the Firestore emulator, install hidane instead with the
[`hidane-dev/setup-hidane`](https://github.com/hidane-dev/setup-hidane) action (`@v1` follows the
latest 1.x release; its `version` input picks a hidane version), and run firebase-tools under
`hidane exec`:

```yaml
# Before
      - name: Set up Java
        uses: actions/setup-java@v4
        with:
          distribution: temurin
          java-version: "21"
      - run: npm ci
      - run: npx firebase emulators:exec --only firestore "npm test"
```

```yaml
# After
      - name: Set up hidane
        uses: hidane-dev/setup-hidane@v1
      - run: npm ci
      - run: hidane exec -- npx firebase emulators:exec --only firestore "npm test"
```

When another emulator or a build in the job needs Java, keep `actions/setup-java` and add
`hidane-dev/setup-hidane@v1` after it. `FIRESTORE_EMULATOR_HOST` stays as it is: under
`hidane exec`, hidane listens on the host and port firebase-tools gives it, which come from
`firebase.json`.

### npm scripts

```json
"test:emulator": "firebase emulators:exec --only firestore 'npm test'"
```

```json
"test:emulator": "hidane exec -- firebase emulators:exec --only firestore 'npm test'"
```

Scripts that run `hidane` need it installed: `npm install --save-dev hidane` puts it where npm
scripts find it, or install it any other way ([README](../README.md#install)).
