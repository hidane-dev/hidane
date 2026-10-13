# bench/

A reproducible, side-by-side benchmark of hidane and the official Cloud Firestore emulator
v1.22.0: the same machine, the same client, the same data and workload, the same way of starting
and stopping each process. Anyone can rerun it, and every result says where and how it was taken.

| Path | What |
|---|---|
| `run.sh` | The canonical entry point: builds or takes the binaries, finds the official jar, runs the benchmark with `canonical.json`, writes `<name>.json` and `<name>.md` |
| `canonical.json` | The canonical parameters, versioned (`benchmark_version`). The engine's built-in defaults are the same values; a test keeps them in step |
| `schema/result.schema.json` | The JSON Schema of a result (`schema_version` 1); a test validates the engine's output against it |
| `fixtures/example-result.json`, `.md` | A **synthetic** result (made-up machine, made-up numbers) and its rendering, for the tests and as an example of the format. Not a measurement |

The measuring is done by one engine, the `hidane benchmark` subcommand of the hidane binary
(`crates/hidane/src/bench/`). `run.sh` is a thin wrapper around it, so a user's own run and the
canonical one share every line of measurement code.

## Run it yourself

You need a Linux or macOS machine, Java 21 or newer (for the official emulator), and either a Rust
toolchain (to build hidane) or a hidane binary.

```sh
bench/run.sh                       # builds hidane, downloads the pinned jar, runs everything
bench/run.sh --quick               # a few runs of everything: checks the setup in a minute or two
bench/run.sh --firebase "$(command -v firebase)"   # also the CI workload through firebase-tools
```

The result lands in `bench/out/` (ignored by git) as `<UTC date>-<kind>.json` and `.md`. Expect
about 10 to 15 minutes for a full run, most of it in the official emulator's runs; `--quick`
takes one or two.

The engine on its own, with any hidane binary:

```sh
hidane benchmark --compare-official --official-download --format markdown
hidane benchmark --compare-official --official-jar path/to/cloud-firestore-emulator-v1.22.0.jar \
  --scenarios startup,memory --out result.json
hidane benchmark render result.json > result.md      # Markdown from a saved result
hidane benchmark --help
```

### Your own test suite (command mode)

`hidane benchmark -- <command…>` starts each target in turn, runs the command with
`FIRESTORE_EMULATOR_HOST` pointing at it (and `HIDANE_BENCH_TARGET` set to `official` or
`hidane`), and measures the command's wall-clock time and the whole start-to-shutdown time. The
command's output goes to stderr; it must exit 0. For example:

```sh
hidane benchmark --compare-official --official-download -- npm test
hidane benchmark --compare-official --official-download -- hidane benchmark workload --tests 200
```

The second runs the built-in synthetic workload as the command. A Node.js test suite measured
this way should pin its toolchain, for example Node.js 22.22.3 and firebase-tools 15.33.0 (the
versions hidane's parity checks use), so that two runs differ only in the emulator.

## Methodology

### Fairness rules

- **Same machine, same moment.** Both targets run on one machine in one benchmark run, under the
  same OS, CPU, RAM and filesystem. Runs alternate between the targets (official, hidane,
  official, …) so that slow drifts in the machine's state reach both alike.
- **Same process model.** Each target is started directly as a process, the way firebase-tools
  starts the official emulator, on a free port the OS assigns, with the same flags where both
  have them: `--host 127.0.0.1 --port <port>`. The official emulator runs as
  `java -Duser.language=en -Duser.country=US -jar cloud-firestore-emulator-v1.22.0.jar …` with
  the JVM's default settings (an English locale is needed for some of its messages; nothing is
  tuned). The `java` on `PATH` is resolved once to `<java.home>/bin/java`, so a version
  manager's shim (mise, asdf, SDKMAN!) does not add its own start-up to the emulator's.
  Output goes to `/dev/null` for both (`--keep-logs DIR` keeps it).
  Both are stopped with `POST /shutdown`.
- **Same client.** One client, built into the engine, talks gRPC to both (the generated
  `google.firestore.v1` client over one HTTP/2 connection), sending the same requests in the same
  order. The documents have the Phase 0 baseline's shape (`{mykey, myid}`, plus a `bucket` field
  for filters).
- **Same data and workload.** Every scenario writes the same documents and runs the same queries
  against both.
- **Pinned versions.** The official jar is the one firebase-tools 15.33.0 downloads, checked
  against its SHA-256 (`9b6498b7…5166c`, `canonical.json`); the result records the hash of the
  jar used and of the hidane binary measured, the Java, and the commit of each binary.

### Cold and warm

Every run starts a fresh process, so startup and the first requests are measured cold, as a CI
job meets them. Warm-up runs are measured the same way and discarded: they let the OS cache the
binary and the jar. The operations scenario is the warm one: one process per target, each
operation repeated, the first iterations discarded. The JVM keeps compiling after that, so the
official emulator's latencies describe it after a short warm-up, which is what a test suite's
first hundred requests see, not a JVM after minutes of load.

### Scenarios and metrics

| Scenario | What a run does | Metrics (unit) |
|---|---|---|
| `startup` | Start, poll the port until a TCP connect succeeds, send a gRPC `GetDocument` of a missing document until it is answered (`NOT_FOUND`), `POST /shutdown`, wait for the exit | `time_to_port`, `time_to_first_request`, `time_to_exit` (ms) |
| `memory` | Start, wait for the port, stay idle 1 s, write 1,000 documents in two commits of 500 (the Phase 0 baseline's workload), sampling RSS in the background | `rss_at_ready`, `rss_idle`, `rss_during_workload`, `rss_after_workload`, `rss_peak` (bytes); `cpu_time_to_ready`, `cpu_time_idle`, `cpu_time_workload`, `workload_time` (ms) |
| `operations` | One process per target; 100 documents seeded; each operation 20 times as warm-up, then 100 times measured | `single_read`, `single_write`, `delete`, `query` (equality filter, 10 matches), `batch_write` (500 writes in one commit), `transaction` (read-modify-write) (ms) |
| `datasets` | For 100, 1,000, 10,000 and 100,000 documents: start, write them in commits of 500, read RSS, run a query matching 1 % of them (3 warm-up, 10 measured per process) | `seed_<n>`, `query_<n>` (ms), `rss_after_seed_<n>` (bytes) |
| `ci` | A CI job in miniature: start, wait for the port, seed 100 documents, 200 tests (each: a commit of 3 writes, 2 reads, a query, then `DELETE /emulator/v1/projects/…/documents`, as `clearFirestore()` does), shut down | `total`, `ready`, `seed`, `tests`, `shutdown` (ms) |
| `ci_firebase` | The same workload as projects run it: `firebase emulators:exec --only firestore` around `hidane benchmark workload`. firebase-tools starts the official emulator from the same jar and Java, and hidane through `hidane exec`; it runs in a scratch project with its own ports and configuration directory (no login, no usage reporting, no update check). Needs `--firebase` | `total`, `workload` (ms) |
| `command` | Your command, with `FIRESTORE_EMULATOR_HOST` set (see above) | `total`, `command`, `ready` (ms) |

Runs per target: 3 warm-up and 10 measured for `startup`, `memory`, `ci` and the dataset sizes up to
1,000 documents. Larger sizes are scaled down, because one run of the official emulator takes
seconds there: 1 warm-up and 5 measured runs for 10,000 documents, 1 and 3 for 100,000.
`ci_firebase` and `command` run 1 warm-up and 5 measured runs. `canonical.json` states every
value.

Memory and CPU time are read from `/proc/<pid>/status` and `/proc/<pid>/stat` on Linux and from
`ps` on macOS. Peak RSS is the kernel's high-water mark (`VmHWM`) on Linux and the highest of
the samples (every 50 ms) on macOS. CPU time is user + system; `ps` reports it in 10 ms steps.

### Statistics

Every metric reports the median, the 95th percentile (linear interpolation between the closest
ranks), the minimum, maximum, mean, sample standard deviation and the number of samples, with
every sample. Comparisons use medians. A failed run is listed under `errors` and leaves no sample.

### Stability

A benchmark on a busy machine measures the machine. Before and after the run the engine reads
the load average, the available memory, the CPU temperature where it can (Linux thermal zones),
the power source, known build and system jobs using CPU (a fixed list of names such as `rustc`,
`java`, `node`, `dockerd`, `mds_stores`; other processes are counted, not named), and on Linux the
CPU steal time in `/proc/stat`, before the run, over the run and over each scenario. It marks the
result `stable: false`, with the reasons, when:

| Check | Limit (`canonical.json`, `stability`) |
|---|---|
| Load average over the last minute, before and after the run | at most 2.0 |
| Available memory before the run | at least 4,096 MiB |
| CPU steal before the run and over the run (Linux) | at most 5 % |
| CPU temperature before and after the run (where readable) | at most 85 °C |
| Known background jobs before the run | none above 10 % CPU |
| Power source | not on battery |
| Coefficient of variation of each target's `time_to_port` (3 or more runs) | at most 0.25 |

A check that cannot be read on the machine is recorded with `passed: null` and does not fail.
Unstable results are kept, never published as claims.

### Privacy and the network

A result records no hostname, IP address, user name, home or other absolute path, or
environment variable; the command mode records the program's file name and its number of
arguments, not the arguments. The engine sends nothing anywhere except to the emulators it
started on 127.0.0.1, the jar download when `--official-download` asks for it (with `curl`, from
the URL firebase-tools uses), and on EC2 the instance metadata service (below). There is no
telemetry.

## Running on a dedicated machine

The infrastructure that runs the canonical benchmark on a dedicated machine (AWS EC2 c7i.xlarge,
Ubuntu 24.04, gp3 EBS, started for a run) calls `run.sh` with binaries it built or downloaded:

```sh
bench/run.sh --kind nightly --machine-id ec2-c7i-xlarge \
  --engine ./hidane-main --engine-commit "$MAIN_SHA" \
  --hidane ./hidane-main --hidane-commit "$MAIN_SHA" \
  --java "$JAVA_HOME/bin/java" --firebase "$(command -v firebase)" \
  --environment-file environment.json --out results/
```

For a release run, `--hidane` is the release's published binary and `--engine` the binary built
from `main`. Without `--official-jar`, the pinned jar is downloaded into `--cache-dir` (default
`~/.cache/hidane-bench`) and checked against its SHA-256. Without `--engine`, `run.sh` builds
hidane from the checkout and reads the commit from git; without `--hidane`, it measures the
engine binary.

`run.sh` writes exactly one `<name>.json` and one `<name>.md` into `--out` (name: `--name`, or
`<UTC date>-<kind>`). It exits 0 when the run completed, stable or not; 1 when it completed but
some runs failed (both files are written, `errors` lists the runs); 2 when the benchmark could
not run (no files).

### The environment file

What the machine cannot see from inside comes from the operator, as a JSON file
(`--environment-file`). It is recorded as given, merged into `machine`, and its revision becomes
the top-level `environment_revision` (`null` without a file). Only these keys are accepted, so
nothing else (an address, a hostname) can slip into a published result:

```json
{
  "environment_revision": 3,
  "machine": {
    "instance_type": "c7i.xlarge",
    "ami_id": "ami-0123456789abcdef0",
    "ebs": { "type": "gp3", "size_gib": 30, "iops": 3000, "throughput_mibps": 125 },
    "os_image": "ubuntu-24.04-amd64-server-20260901"
  }
}
```

Bump `environment_revision` whenever the machine image, instance type, disk or system setup
changes; a history marks the change.

On Linux, when the DMI vendor or the hypervisor says EC2, the engine asks the instance metadata
service (IMDSv2, 300 ms timeouts) for the instance type and availability zone, and records only
those two, under `machine.cloud`. No instance or account ID, AMI owner, address, hostname or
credentials are read. `--no-imds` turns it off. Elsewhere nothing is sent. The CPU frequency
driver and governor are recorded when Linux exposes them; on EC2 they usually are not, and
`machine.cpu_frequency.note` says so.

## Results and history

The proposed layout for published results is one file pair per run:

```
results/YYYY/MM/YYYY-MM-DD-<kind>.json
results/YYYY/MM/YYYY-MM-DD-<kind>.md
```

A history plots `results.<scenario>.<target>.<metric>.stats.median` (with p95 as a band) over
`timestamp`, per `machine.id`, for canonical results (`benchmark.canonical`) of one
`benchmark.version`, and marks where `targets.hidane.version`, `targets.official.version` or
`environment_revision` changes. `schema_version` changes when a field changes meaning.

## Claim policy

- Numbers come from canonical, stable runs on a stated machine, with the environment block
  printed above them. Unstable or non-canonical results are not cited.
- Comparisons use medians of the measured runs and say where they were taken: "In this
  benchmark environment, median startup-to-ready time was X for the official emulator and Y for
  hidane." No single-run or best-run numbers, and no "N× faster" headlines.
- A result is published with its JSON, so every sample behind a statement can be checked, and
  anyone can rerun it with `bench/run.sh`.

This harness replaces `tools/bench/startup_memory.py`: `startup` and `memory` measure the same
things (time until the port accepts, RSS idle and after the baseline's 1,000 documents), for
both emulators. `results/startup-memory-hidane.txt` came from that script.
