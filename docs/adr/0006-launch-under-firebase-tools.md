# ADR 0006: Launching hidane under firebase-tools

- **Status**: Accepted
- **Date**: 2026-10-10
- **Issue**: #13

## Context

Most projects start the Firestore emulator with `firebase emulators:start` or
`firebase emulators:exec`, not by hand. firebase-tools 15.33.0 (`lib/emulator/downloadableEmulators.js`,
`controller.js`, `commandUtils.js`) does this:

1. Before starting any emulator whose command is `java`, it runs `java -Duser.language=en
   -Dfile.encoding=UTF-8 -version` and refuses to continue unless the major version is 21 or more.
2. It downloads the emulator jar into its cache unless the file exists. When
   `FIRESTORE_EMULATOR_BINARY_PATH` is set, that path is used instead (made executable with
   `chmod 755`) and nothing is downloaded.
3. It runs `java -Dgoogle.cloud_firestore.debug_log_level=FINE -Duser.language=en -jar <path>
   --host … --port … --websocket_port … --project_id … --single_project_mode …` (plus `--rules`,
   `--seed_from_export`, `--functions_emulator` when configured), and stops it with SIGINT.

hidane already accepts those flags and follows the process contract (#11, #12). What is missing is
a way for firebase-tools to start it.

## Options

**A. A `java` shim.** Something named `java`, first on `PATH`, answers the version probe and, for
the Firestore jar, runs hidane with the flags after `-jar <path>`.

- *A1, global*: install the shim on the user's `PATH`. It then sees every `java` invocation on the
  machine and must pass everything else to the real Java.
- *A2, scoped*: `hidane exec -- <command>` runs one command with a temporary directory first on
  its `PATH`, holding `java` (hidane itself) and a placeholder named by
  `FIRESTORE_EMULATOR_BINARY_PATH`.

**B. A firebase-tools change.** When `FIRESTORE_EMULATOR_BINARY_PATH` names something that is not a
`.jar`, run it directly with the flags, and do not require Java for it. A local prototype needed
two changes in `downloadableEmulators.js` (about ten lines: choose the binary in `_getCommand`,
return `false` from `requiresJava`); with it, `FIRESTORE_EMULATOR_BINARY_PATH=$(command -v hidane)
firebase emulators:exec …` ran hidane with no Java installed.

| | A1 global shim | A2 `hidane exec` | B upstream change |
|---|---|---|---|
| Works with released firebase-tools | yes | yes (15.33.0 verified) | only once merged and released |
| Java needed for Firestore | no | no | no |
| Official jar downloaded | yes, unless the user sets the variable | no | no |
| Touches the user's `PATH` | permanently | for one command | no |
| Other JVM emulators (Database, Storage, Pub/Sub) | passed to the real Java | passed to the real Java | unchanged |
| What the user types | nothing new, after a setup step | `hidane exec -- firebase emulators:start` | `FIRESTORE_EMULATOR_BINARY_PATH=… firebase …` |
| Depends on | `PATH` order, which IDEs and version managers change | firebase-tools keeping its command shape | Google accepting it |

## Decision

Ship **A2** now as `hidane exec`, and propose **B** upstream later, as the long-term path.

Invoked as `java`, hidane:

- serves the emulator itself when the jar is its placeholder or an official
  `cloud-firestore-emulator-*.jar`, with the flags that follow it (JVM options before `-jar` are
  dropped);
- answers `-version` with the real Java's output when a Java 21 or newer is on `PATH`, and with a
  Java 21 version line otherwise;
- execs the next `java` on `PATH` for anything else, or exits 127 explaining that only the
  Firestore emulator is served.

The `java` it runs is marked (`HIDANE_JAVA_SHIM_CALLED`): a version manager's shim (mise, asdf,
jenv) with no Java behind it runs the first `java` on `PATH`, hidane's again, and without the
mark the version probe went round in circles. Called back that way, hidane answers the probe
itself and refuses other jars.

`hidane exec` passes Ctrl-C to the command (they share the process group), waits for it, removes
its directory and exits with the command's code. Verified with firebase-tools 15.33.0 and no Java
on `PATH`: `emulators:start` was ready in 2.3 s and stopped cleanly on Ctrl-C, and
`emulators:exec` ran an Admin SDK script against hidane (`crates/hidane/tests/launch.rs` covers the
contract without firebase-tools). With the Emulator UI enabled, UI v1.15.0 browsed, created and
deleted documents and cleared all data through hidane (#38).

A1 is not offered: a permanent `java` on `PATH` surprises every other Java user on the machine.
B would remove the wrapper; proposing it to firebase-tools is a separate, outward-facing step that
waits for a first release.

## Consequences

- The README and `docs/compatibility.md` document `hidane exec -- firebase emulators:start`.
- firebase-tools prints "Env variable override detected. Using firestore emulator at …" and
  "Firestore Emulator UI websocket is running on …", although hidane does not serve that port yet
  (#69).
- Projects that also run the Database, Storage or Pub/Sub emulators still need a Java for those.
- On Windows the shim is a copy of the binary named `java.exe`; it is not verified yet.
- Distribution channels (#77, #78) install the same binary; `hidane exec` needs nothing else.
