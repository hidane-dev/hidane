# bench/

A reproducible, side-by-side benchmark of hidane and the official Cloud Firestore emulator
v1.22.0: the same machine, the same client, the same data and workload, the same way of starting
and stopping each process. Anyone can rerun it, and every result says where and how it was taken.

| Path | What |
|---|---|
| `run.sh` | The canonical entry point: builds or takes the binaries, finds the official jar, runs the benchmark with `canonical.json`, writes `<name>.json` and `<name>.md` |
| `canonical.json` | The canonical parameters, versioned (`benchmark_version`). The engine's built-in defaults are the same values; a test keeps them in step |
| `schema/result.schema.json` | The JSON Schema of a result (`schema_version` 1); a test validates the engine's output against it |
| `schema/savings.schema.json` | The JSON Schema of `hidane benchmark savings --format json` ([CI time saved](#ci-time-saved)) |
| `fixtures/example-result.json`, `.md` | A **synthetic** result (made-up machine, made-up numbers) and its rendering, for the tests and as an example of the format. Not a measurement |
| `targets.json`, `check_targets.py` | hidane's targets (docs/ROADMAP.md §1) and the check of a result against them, run on GitHub-hosted runners ([Targets in CI](#targets-in-ci)) |

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
about 15 to 25 minutes for a full run, most of it in the official emulator's runs (its three
runs of `listen_writes` take several minutes alone); `--quick` takes one or two.

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

### CI time saved

`hidane benchmark savings <result.json>` turns a result that measured both emulators into the
time a CI job saves, per run and per month, and, at your own rate, into cost. It measures nothing
itself. It takes the median `total` of the command scenario (your test suite), or else of
`ci_firebase`, or else of `ci` (`--scenario` picks one), and computes:

| Figure | How |
|---|---|
| Saved per run | the official emulator's median minus hidane's |
| Saved per month | saved per run × `--runs-per-month` |
| Saved cost per month | saved minutes per month × `--cost-per-minute`, labelled with `--currency` |

No CI price is built in: prices change and differ between providers and plans, so the rate is
yours to look up and pass. A provider that bills each job in whole minutes can bill a saving of
seconds per job differently; the output says so whenever a rate is given.

```sh
hidane benchmark --compare-official --official-download --out result.json -- npm test
hidane benchmark savings result.json --runs-per-month 600
hidane benchmark savings result.json --runs-per-month 600 \
  --cost-per-minute <your rate> --currency USD --format markdown
# the measurement and the estimate in one go (the result is read from stdin):
hidane benchmark --compare-official --official-download -- npm test \
  | hidane benchmark savings - --runs-per-month 600
```

`--format text` (the default) is for the terminal, `json` for tools
(`schema/savings.schema.json`), and `markdown` prints a case-study block to paste into a README,
a post or an issue. The block keeps to the [claim policy](#claim-policy): it gives both medians
and the number of runs behind them, the date, machine, operating system and tools, says that it
is the team's own measurement and median-based, and carries the caveats of the run: not stable,
`--quick`, fewer than three runs, failed runs, an official jar other than the pinned one, or no
time saved (a negative difference is "none" in the text and the block, and stays negative in the
JSON). It has no ratio and no "N× faster". When the estimate rests on the synthetic `ci` or
`ci_firebase` workload, the block says it is not the team's own test suite.

Durations read as milliseconds below a second, seconds with two decimals below a minute, then
minutes and seconds, then hours and minutes; money has two decimals. The JSON keeps three
decimals, like a result. With the synthetic `fixtures/example-result.json`, its command
scenario's medians changed to 61.5 s for the official emulator and 21.25 s for hidane, 1,200 runs
a month and a placeholder rate of 0.01 per minute, the block reads (a test keeps this copy
current):

```markdown
### CI time with hidane

| | |
|---|---|
| Before: the official emulator 1.22.0 | 1 min 2 s per run |
| After: hidane 0.3.0 | 21.25 s per run |
| Runs per month | 1,200 |
| Saved per run | 40.25 s |
| Saved per month | 13 h 25 min |
| Saved cost per month | 8.05 USD, at 0.01 USD per minute |

This is our own measurement, made with `hidane benchmark` (benchmark definitions v1) on 2026-01-01: our test command (`npm` with 1 argument), timed from the emulator's start to its shutdown, with 3 measured runs per emulator, the emulators taking turns. The figures are medians of those runs. Machine: Example CPU, 2 cores, 4 threads, 8 GiB RAM, instance type c7i.xlarge; Ubuntu 24.04 LTS, x86_64. Tools: Java 21.0.8 (Eclipse Adoptium), Node.js 22.22.3, firebase-tools 15.33.0. Other machines and workloads give other numbers. Method: [hidane's benchmark](https://github.com/hidane-dev/hidane/blob/main/bench/README.md).

Notes:

- The run did not pass the stability checks (synthetic example: these numbers are not measurements), so its numbers are noisy and not fit to cite.
- The cost is the saved minutes times the given rate. A CI provider that bills each job in whole minutes can bill a saving of seconds per job differently.
```

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
  It sends no credential, as a signed-out client SDK does, so Security Rules apply to every
  request on both, except in `owner_writes`. That scenario sends `Authorization: Bearer owner`,
  as the Admin SDKs do to an emulator, which skips the Rules: the official emulator's writes are
  many times as fast with it, and hidane's take about the same time either way. Every other
  scenario, the CI workloads included, is a signed-out app's, so compare `owner_writes` with
  `operations.batch_write` and `datasets.seed_100000` to see what the credential changes. In the
  CI workloads it changes little: in local runs of `ci`, seeding its 100 documents with the owner
  credential took about 0.2 s less on the official emulator, out of about 4.4 s, and the tests
  were no faster with it. An SDK's own work adds to both: through firebase-admin, 100,000
  documents took 2.2 s on the official emulator and 1.1 s on hidane in a single local run.
- **Same data and workload.** Every scenario writes the same documents and runs the same queries
  against both.
- **Pinned versions.** The official jar is the one firebase-tools 15.33.0 downloads, checked
  against its SHA-256 (`9b6498b7…5166c`, `canonical.json`); the result records the hash of the
  jar used and of the hidane binary measured, the Java, and the commit of each binary (required
  in a nightly or release result, [Provenance](#provenance)).

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
| `listen_writes` | Writes with a listener attached ([below](#writes-with-a-listener-attached)): start, open a Listen stream on a whole collection, write 100,000 documents to it in commits of 500, one at a time, until the listener's snapshot holds them all | `writes`, `delivered`, `batch_first_decile`, `batch_last_decile`, `batch_growth` (ms); `rss_after` (bytes) |
| `command` | Your command, with `FIRESTORE_EMULATOR_HOST` set (see above) | `total`, `command`, `ready` (ms) |
| `owner_writes` | Writes with the owner credential (`Authorization: Bearer owner`, as the Admin SDKs send it): start, wait for the port, commit 500 writes to the same documents 120 times (the first 20 discarded), then write 100,000 documents in commits of 500 | `batch_write`, `seed_100000` (ms) |

Runs per target: 3 warm-up and 10 measured for `startup`, `memory`, `ci` and the dataset sizes up to
1,000 documents. Larger sizes are scaled down, because one run of the official emulator takes
seconds there: 1 warm-up and 5 measured runs for 10,000 documents, 1 and 3 for 100,000.
`ci_firebase` and `command` run 1 warm-up and 5 measured runs. `listen_writes` runs no warm-up
and 3 measured runs: one run of the official emulator takes minutes (121 s in Phase 0), the
earlier scenarios have already brought the binary and the jar into the OS's cache, and the JVM's
own warm-up inside a run is part of what the first batches measure; `--warmup` and `--runs`
are capped at 1 and 3 for it. `owner_writes` runs 1 warm-up and 3 measured runs, as the largest
dataset size does, and is capped the same way. `canonical.json` states every value.

#### Writes with a listener attached

firebase-tools [#3477](https://github.com/firebase/firebase-tools/issues/3477) reports batch
writes slowing down as the database grows while a listener is attached; Phase 0 reproduced it on
the official emulator ([`docs/why.md`](../docs/why.md#3-writes-slow-down-while-a-listener-is-attached)).
`listen_writes` measures that case for both emulators. A run:

1. starts the target and waits for the port;
2. opens a Listen stream, on its own HTTP/2 connection, with one target: a query on the whole
   collection `listened` (the SDKs' Listen protocol, with the `google-cloud-resource-prefix`
   header they send), and waits for its first consistent snapshot (empty);
3. writes `listened/doc-0` … `doc-99999` in commits of 500 over a second connection, one commit
   at a time, timing each commit from request to answer, while the stream is read as it arrives;
4. waits for the first consistent snapshot holding every document, reads the RSS and shuts down.

The listener keeps the target's documents as the SDKs do: a `RESET` empties them until the target
is `CURRENT` again (the official emulator answers each write to a watched collection with `RESET`
and the whole result, [`docs/parity-exceptions.md`](../docs/parity-exceptions.md)), and a
`NO_CHANGE` for all targets after that is a consistent snapshot.

| Metric | What |
|---|---|
| `writes` | The first commit sent until the last one answered |
| `delivered` | The first commit sent until the listener's snapshot holds every document. The official emulator can send the last documents before it answers the last commit, so this can be at most `writes` |
| `batch_first_decile`, `batch_last_decile` | The median commit time of the first and of the last tenth of the batches (20 of 200 each) |
| `batch_growth` | How much a commit's time grows per 10,000 documents already stored: the Theil–Sen estimator, the median of the slopes between every two batches of the run, so a few slow batches (a cold first commit, a pause) do not tilt it. Recorded with unit `ms`; it can be negative |
| `rss_after` | RSS once the listener holds every document |

Each metric has one sample per run. The same writes without a listener are the `datasets`
scenario's `seed_100000`. Phase 0 measured this with other clients (the Go client of
williamhaley/firestore-emulator-slow for the writes, a Node.js `onSnapshot` listener in another
process) on the official emulator only, and hidane later with the Admin SDK
(`results/batch-write-degradation-official-v1.22.0.csv`, `results/listen-batch-writes-hidane.csv`);
this scenario takes both emulators side by side with one client.

The same scenario is hidane's regression check for #49 and #30: a test in
`crates/hidane/tests/bench.rs` runs it on hidane alone, with the canonical 100,000 documents, on
every pull request, and fails when the last tenth of the batches takes more than twice the first
tenth's median, or when the growth over the run exceeds one early batch. Ratios, not
milliseconds, so a slow runner or a debug build passes them.

Memory and CPU time are read from `/proc/<pid>/status` and `/proc/<pid>/stat` on Linux and from
`ps` on macOS. Peak RSS is the kernel's high-water mark (`VmHWM`) on Linux and the highest of
the samples (every 50 ms) on macOS. CPU time is user + system; `ps` reports it in 10 ms steps.

### Statistics

Every metric reports the median, the 95th percentile (linear interpolation between the closest
ranks), the minimum, maximum, mean, sample standard deviation and the number of samples, with
every sample. Comparisons use medians.

A run that fails is listed under `errors`, marked `retried`, and run again once, in a fresh
process as every run is, for the same target and in the same place in the order. So a transient
failure, such as one dropped connection among hundreds of runs, costs neither target a sample.
The rule is the same for both emulators, and the report lists every run that was run again with
its error. A run whose retry fails too is listed again, not retried, and leaves no sample; the
run's exit status is then 1 (#291). `check_targets.py` is stricter for hidane's own targets: any
failed attempt of hidane's fails it, retried or not.

### Stability

A benchmark on a busy machine measures the machine. Before and after the run the engine reads
the load average, the available memory, the CPU temperature where it can (Linux thermal zones),
the power source, known build and system jobs using CPU (a fixed list of names such as `rustc`,
`java`, `node`, `dockerd`, `mds_stores`; other processes are counted, not named), and on Linux the
CPU steal time in `/proc/stat`, before the run, over the run and over each scenario. It marks the
result `stable: false`, with the reasons, when:

| Check | Limit (`canonical.json`, `stability`) |
|---|---|
| Load average over the last minute before the run (after the run it is recorded only: that minute is the run's own load) | at most 2.0 |
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

### Provenance

A nightly or release result goes into the canonical history, so it must trace every number to
the binaries behind it: the commit of the hidane measured (`targets.hidane.git_commit`) and of
the engine that measured it (`benchmark.engine.git_commit`), each a full SHA, 40 lowercase
hexadecimal digits. The engine refuses a `--kind nightly` or `--kind release` run that does not
measure hidane, or lacks `--hidane-commit` or `--engine-commit` in that form: it exits 2 before
measuring anything and writes no result. The schema holds such a result to the same rule (an
`if`/`then` on `benchmark.kind`). Local and pull-request runs may leave the commits out, or give
them in any form.

Where the commits come from is the operator's job. The canonical infrastructure takes a nightly
run's from the commit of `main` it builds; for a release run, the measured hidane's commit is the
source commit named by the published binary's build provenance attestation (signed by
`release.yml` at the release tag), checked against the commit the tag names. Its results, and any
correction made to one after publication, are listed with their evidence in
[hidane-dev/benchmark-results](https://github.com/hidane-dev/benchmark-results) (`PROVENANCE.md`).

`run.sh` writes exactly one `<name>.json` and one `<name>.md` into `--out` (name: `--name`, or
`<UTC date>-<kind>`). It exits 0 when the run completed, stable or not, and every run that failed
succeeded when run again; 1 when it completed but some runs failed twice (both files are written,
`errors` lists the runs); 2 when the benchmark could not run (no files).

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

### Versions

`benchmark.version` (`benchmark_version` in `canonical.json`) names the benchmark definitions. It
changes when an existing scenario changes what it does or measures, or one of its canonical
values changes: numbers of different versions are not compared. A new scenario keeps the
version when it runs after the existing ones (scenarios run in a fixed order), because it cannot
change the conditions they are measured under; its history starts with the first result that has
it. `listen_writes` and `owner_writes` were added this way to definitions v1. The run-level readings (`duration_s`,
the stability readings after the run, CPU steal over the run) cover every scenario that ran.

## Targets in CI

The workflow `.github/workflows/bench-targets.yml` checks hidane alone against its targets on a
GitHub-hosted runner (ubuntu-24.04), on every push to `main`, nightly and by hand (#47). It
builds a release binary, runs `hidane benchmark --scenarios startup,memory,operations` with the
canonical runs, and `check_targets.py` compares hidane's medians with `targets.json`:

| Metric | Bound |
|---|---|
| `startup.time_to_port` | below 100 ms |
| `memory.rss_idle` | below 50 MiB |
| `memory.rss_after_workload` (1,000 documents) | below 100 MiB |
| `operations.batch_write` (a commit of 500 writes) | below 25 ms |

The bounds are the targets themselves, not hidane's current values: a shared runner's noise is
far smaller than the distance between the two, so the job fails only for a regression, a
missing metric or a failed run. The job summary shows the check, the rendered result and the
in-memory store's RSS from 1,000 to 1,000,000 documents (the `hidane-core` example `rss`,
reported, not checked); the files are kept as an artifact. These numbers come from a shared
machine and are not cited (claim policy below). Writes with a listener attached have their own
check in `cargo test` (#49).

```sh
python3 -I bench/check_targets.py bench/targets.json result.json   # any result with startup, memory and operations
python3 -I bench/check_targets.py --self-test
```

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
both emulators. `results/startup-memory-hidane.txt` came from that script. `listen_writes`
replaces the scripts behind `results/batch-write-degradation-official-v1.22.0.csv` and
`results/listen-batch-writes-hidane.csv`, which are not in the repository.
