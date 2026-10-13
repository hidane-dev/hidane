# Reporting a bug

A bug is fixed fastest when it can be reproduced. `hidane report` writes what a maintainer needs
for that: hidane's build, the machine's OS, the Java, Node.js and firebase-tools versions, the
official emulator versions firebase-tools has downloaded, the environment variables the emulators
read, and `hidane doctor`'s scan of the project with its `firebase.json`. Secrets and identifiers
are replaced with placeholders first, and nothing is sent anywhere: the report is printed or
written to a file on your machine, and you decide what to share.

```sh
hidane report                      # Markdown on stdout, to paste into an issue
hidane report --output report.md   # to a file; it says what the file holds
hidane report --path ../my-app     # another project directory (default: the current one)
hidane report --format text        # for reading in a terminal
hidane report --format json        # for tools (schema hidane.report/1)
```

Paste the Markdown into the **Bug report** or **Parity gap**
[issue template](https://github.com/hidane-dev/hidane/issues/new/choose), after reading it.

## What it includes

| Section | What | Read from |
|---|---|---|
| hidane | version, commit, build profile (`release`, `debug`), Rust target | the binary itself (the commit is recorded at build time) |
| System | OS and architecture, OS version, whether it runs in a container, which CI service | `std::env::consts`, `sw_vers`, `/etc/os-release`, `ver`; `/.dockerenv`; CI variables such as `GITHUB_ACTIONS` |
| Java | the version and runtime lines of `java -version`, and whether that `java` is `hidane exec`'s | `java -version`, run with nothing on stdin; other lines it prints (such as `Picked up JAVA_TOOL_OPTIONS: …`) are dropped |
| Node.js | `node --version` | |
| firebase-tools | the version on `PATH`, and the one in the project's `node_modules` | their `package.json`; firebase-tools is not run (its update check would use the network) |
| Official emulator | the Firestore emulator versions firebase-tools has cached, and the other emulators it has | the **file names** in `$FIREBASE_EMULATORS_PATH`, else `~/.cache/firebase/emulators` (`cloud-firestore-emulator-v1.22.0.jar`); no file there is opened |
| Environment variables | the variables the emulators and firebase-tools read, by name, with a value only where it is safe (below) | the environment |
| Project | `hidane doctor`'s findings and, in the JSON form, its whole report; each `firebase.json`, sanitized; the aliases of `.firebaserc` | what `hidane doctor` reads ([migrating.md](migrating.md#hidane-doctor)), and `.firebaserc` |
| Official emulator vs hidane | the project's commands that start the Firestore emulator, and whether the official emulator can run here ([below](#the-official-emulator-and-hidane)) | `hidane doctor`'s scan, `java -version` |

Environment variables are shown as follows:

| Variables | Shown as |
|---|---|
| `FIRESTORE_EMULATOR_HOST` and every other `*_EMULATOR_HOST`, `FIREBASE_FIRESTORE_EMULATOR_ADDRESS`, `FIREBASE_EMULATOR_HUB` | `host:port`, the host only when it is a loopback one (`127.0.0.1:8080`, `<host>:8080`) |
| `GCLOUD_PROJECT`, `GOOGLE_CLOUD_PROJECT`, `CLOUDSDK_CORE_PROJECT`, `DATASTORE_PROJECT_ID` | the project ID if it starts with `demo-`, else `<project-id-N>` |
| `FIRESTORE_EMULATOR_BINARY_PATH` | which file it names (`hidane exec`'s placeholder, `cloud-firestore-emulator-v<version>.jar`), never where |
| `*_EMULATOR_VERSION`, `CI`, `GITHUB_ACTIONS`, `EXPERIMENTAL_MODE`, `FIREBASE_CLI_EXPERIMENTS` | the value, when it is a version, a boolean or a list of names |
| `GOOGLE_APPLICATION_CREDENTIALS`, `FIREBASE_TOKEN`, `FIREBASE_CONFIG`, `JAVA_HOME`, `JAVA_TOOL_OPTIONS`, `NODE_OPTIONS`, `HTTPS_PROXY`, and any other variable whose name starts with `FIREBASE_`, `FIRESTORE_`, `GCLOUD_`, `GOOGLE_`, `CLOUDSDK_`, `HIDANE_` or `FUNCTIONS_` | `<set>` (or `<empty>`): never the value |

Other variables are not shown at all.

## What it leaves out

Every text the report takes from the machine or the project goes through the same redaction
before it is written:

| What | Becomes |
|---|---|
| Project IDs: in `.firebaserc`, in the project variables above, after `--project`, `-P` or `--project_id`, in `projects/<id>/…`, in `<id>.firebaseapp.com`, `.web.app`, `.appspot.com`, `.firebaseio.com`, `.firebasestorage.app` and the like; and once known, wherever they appear again | `<project-id-1>`, `<project-id-2>`, … (the same ID always gets the same number) |
| Firebase app IDs (`1:1234567890:web:0a1b2c`) and project numbers | `<app-id>`, `<project-number>` |
| API keys (`AIza…`) and tokens: OAuth (`ya29.`, `1//`), GitHub, GitLab, Slack, Stripe, npm, PyPI, AWS, JWTs (the emulator's mock tokens too), `Bearer …` | `<api-key>`, `<token>` |
| The value of anything whose name looks secret: `FIREBASE_TOKEN=…`, `--token …`, `"apiKey": …`, `password: …`, `client_secret=…`, a secret query parameter (`?key=`, `?access_token=`, `X-Goog-Signature=`), a header such as `Authorization` | `<redacted>` |
| The values of your secret environment variables (by name, as above), wherever they appear | `<redacted>` |
| Private key blocks | `<private-key>` |
| Credentials in URLs (`https://user:password@host`) | `https://<credentials>@host` |
| E-mail addresses | `<email>` |
| The project's directory, your home directory (and anyone else's: `/Users/<name>`, `/home/<name>`, `C:\Users\<name>`), temporary directories | `<project>`, `<home>`, `<temp>` |
| Your user name and the machine's name | `<user>`, `<hostname>` |
| Hosts other than loopback ones in `firebase.json`, workflows and the variables above, wherever they appear | `<host>` |

Kept as they are: project IDs that start with `demo-` (firebase-tools treats them as fake
projects), `.firebaserc` aliases (`default`, `staging`), references to secrets
(`$FIREBASE_TOKEN`, `${{ secrets.FIREBASE_TOKEN }}`), the emulator's `Bearer owner`, loopback
hosts, paths relative to the project, system paths (`/usr/lib/jvm/…`), and generic account and
machine names (`runner`, `root`). The end of the report counts what was replaced, by kind.

The report never holds document data: it reads no export, no `--import` directory, no emulator
data and no `firestore-debug.log`. Of the project it reads what `hidane doctor` reads
(`firebase.json`, the rules, manifests and lockfiles, the Functions' sources for their trigger
names, workflows and scripts) and `.firebaserc`. It runs `java -version`, `node --version`, and
where needed `hostname`, `sw_vers` or `ver`; it makes no network request and sends no telemetry.
The official emulator's jar is only ever named, never opened.

## Before you share it

Read the report. Redaction finds secrets by their name and their shape, which covers the usual
ones but not, say, a password written into a script under an ordinary name. Remove anything you
would rather not post; the placeholders are there so that a report stays useful with values
removed. If the report let something through that it should have replaced, please tell us: as a
bug if it is harmless, through [private vulnerability reporting](../SECURITY.md) if it is not.

## The official emulator and hidane

What the same steps do with the official emulator is often the most useful part of a report: it
tells a hidane bug from a difference between the official emulator and production, or from a
problem in the project. The report prepares that comparison without running it. Its last section
lists the project's commands that start the Firestore emulator through firebase-tools (from its
workflows, npm scripts and shell scripts), which use the official emulator as they are and hidane
after `hidane exec --`, and says whether the official emulator can run on this machine (it needs
Java 21 or newer) and which versions firebase-tools has cached. Please run the steps both ways and
say in the issue what each did: exit codes, errors, results.

The report does not run them itself, on purpose: it would run your tests twice, with whatever
they change; firebase-tools may download the official emulator on the way; and their output can
hold your documents, which a report never includes. You see both outputs, and choose what of
them to share.

## JSON

`hidane report --format json` holds the same as the Markdown, for tools. Its shape is versioned
by `schema` ([versioning.md](versioning.md#json-output)); every string in it has been redacted.

| Field | What |
|---|---|
| `schema` | `hidane.report/1` |
| `notice` | That the report was written locally, with placeholders, and should be read before it is shared |
| `hidane` | `version`, `commit` (or `null`), `profile` (`release`, `debug`), `target` |
| `system` | `os`, `arch`, `os_version`, `container`, `ci` (each `null` when not known or not applicable) |
| `tools` | `java` (`found`, `version`, `runtime`, `major`, `hidane_exec`), `node`, `firebase_tools` (`on_path`, `path_version`, `installed`: `file` and `version` of each) |
| `official_emulator_cache` | `dir`, `listed` (the directory could be listed), `firestore` (versions, oldest first), `others` (`emulator` and `version` of each) |
| `environment` | `name` and `value` of each variable shown |
| `project` | `path` (always `<project>`), `firebase_json` (`file`, `content`, `error` of each), `firebaserc` (`file` and its `projects`: `alias` and `project` of each), `doctor` (a `hidane.doctor/1` report, [migrating.md](migrating.md#json)) |
| `comparison` | `compared` (always `false`), `commands` (`file`, `line`, `command`, `under_hidane` of each), `java_for_official` |
| `redacted` | How many values of each kind were replaced: `project-id`, `app-id`, `email`, `api-key`, `token`, `private-key`, `url-credentials`, `secret`, `project-path`, `home-path`, `temp-path`, `host`, `host-name`, `user-name` |
