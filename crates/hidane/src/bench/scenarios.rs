//! The scenarios. Each run starts a fresh process (cold) unless the scenario says otherwise;
//! targets alternate run by run (official, hidane, official, …), so slow drifts in the machine's
//! state reach both alike. Warm-up runs are measured the same way and discarded.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde_json::{Map, Value, json};

use super::{
    client::{self, Client, Credential, Listener},
    config::{Ci, Params, Runs},
    render, stats,
    system::{self, Sampler},
    target::{self, Kind, Target},
};

/// One metric of one scenario.
#[derive(Debug, Clone)]
pub struct Metric {
    pub scenario: &'static str,
    pub id: String,
    pub unit: &'static str,
    pub description: String,
    pub parameters: Option<Value>,
}

fn metric(scenario: &'static str, id: &str, unit: &'static str, description: &str) -> Metric {
    Metric {
        scenario,
        id: id.to_owned(),
        unit,
        description: description.to_owned(),
        parameters: None,
    }
}

struct Series {
    metric: Metric,
    target: Kind,
    samples: Vec<f64>,
}

/// Everything measured, in the order it was first measured.
#[derive(Default)]
pub struct Results {
    series: Vec<Series>,
    pub errors: Vec<Value>,
    /// CPU steal over each scenario (Linux), for the stability checks.
    pub steal: Vec<(&'static str, f64)>,
}

impl Results {
    pub fn add(&mut self, metric: &Metric, target: Kind, value: f64) {
        let index = self.series.iter().position(|s| {
            s.target == target && s.metric.scenario == metric.scenario && s.metric.id == metric.id
        });
        match index {
            Some(i) => self.series[i].samples.push(value),
            None => self.series.push(Series {
                metric: metric.clone(),
                target,
                samples: vec![value],
            }),
        }
    }

    pub fn stats(&self, scenario: &str, target: Kind, id: &str) -> Option<stats::Stats> {
        self.series
            .iter()
            .find(|s| s.target == target && s.metric.scenario == scenario && s.metric.id == id)
            .and_then(|s| stats::summarize(&s.samples))
    }

    /// `results`: scenario → target → metric → {unit, description, parameters, stats, samples}.
    pub fn to_json(&self) -> Value {
        let mut out = Map::new();
        for series in &self.series {
            let Some(stats) = stats::summarize(&series.samples) else {
                continue;
            };
            let mut entry = Map::new();
            entry.insert("unit".into(), json!(series.metric.unit));
            entry.insert("description".into(), json!(series.metric.description));
            if let Some(parameters) = &series.metric.parameters {
                entry.insert("parameters".into(), parameters.clone());
            }
            entry.insert("stats".into(), stats.to_json());
            entry.insert(
                "samples".into(),
                json!(
                    series
                        .samples
                        .iter()
                        .map(|v| stats::round(*v))
                        .collect::<Vec<_>>()
                ),
            );
            let scenario = out
                .entry(series.metric.scenario)
                .or_insert_with(|| json!({}));
            let target = scenario
                .as_object_mut()
                .map(|s| s.entry(series.target.id()).or_insert_with(|| json!({})))
                .and_then(Value::as_object_mut);
            if let Some(target) = target {
                target.insert(series.metric.id.clone(), Value::Object(entry));
            }
        }
        Value::Object(out)
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// How long the listener may take, after the last commit, to hold every document.
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(300);

/// The fixed parts of a benchmark run.
pub struct Ctx {
    pub rt: tokio::runtime::Runtime,
    pub targets: Vec<Target>,
    pub params: Params,
    pub logs: Option<PathBuf>,
}

pub struct Bench {
    pub ctx: Ctx,
    pub results: Results,
}

impl Bench {
    /// Runs `f` for every warm-up and measured run, alternating targets, and records the values
    /// it returns (indexes into `metrics`) for the measured runs. A failed run is recorded as an
    /// error, marked `retried`, and run again once, in a fresh process like every run, so that a
    /// transient failure does not cost the target a sample (#291). A retry that fails too is
    /// recorded as an error that was not retried, and the others go on.
    fn each_run<F>(
        &mut self,
        scenario: &'static str,
        label: &str,
        runs: Runs,
        metrics: &[Metric],
        mut f: F,
    ) where
        F: FnMut(&Ctx, &Target, &str) -> Result<Vec<(usize, f64)>, String>,
    {
        let total = runs.warmup + runs.runs;
        let targets = self.ctx.targets.clone();
        for i in 0..total {
            let measured = i >= runs.warmup;
            for target in &targets {
                let name = format!(
                    "{label}-{}-{}{}",
                    target.kind.id(),
                    i + 1,
                    if measured { "" } else { "-warmup" }
                );
                eprintln!(
                    "hidane benchmark: {label}: {} run {}/{total}{}",
                    target.kind.id(),
                    i + 1,
                    if measured { "" } else { " (warm-up)" }
                );
                let error = |message: &str, retried: bool| {
                    json!({
                        "scenario": scenario,
                        "target": target.kind.id(),
                        "run": i + 1,
                        "warmup": !measured,
                        "message": message,
                        "retried": retried,
                    })
                };
                let mut outcome = f(&self.ctx, target, &name);
                if let Err(message) = &outcome {
                    eprintln!(
                        "hidane benchmark: {label}: {}: {message}; running it again",
                        target.kind.id()
                    );
                    self.results.errors.push(error(message, true));
                    outcome = f(&self.ctx, target, &format!("{name}-retry"));
                }
                match outcome {
                    Ok(values) if measured => {
                        for (index, value) in values {
                            self.results.add(&metrics[index], target.kind, value);
                        }
                    }
                    Ok(_) => {}
                    Err(message) => {
                        eprintln!("hidane benchmark: {label}: {}: {message}", target.kind.id());
                        self.results.errors.push(error(&message, false));
                    }
                }
            }
        }
    }

    /// Process start → port accepts → first gRPC answer → `POST /shutdown` → exit.
    pub fn startup(&mut self) {
        let metrics = [
            metric(
                "startup",
                "time_to_port",
                "ms",
                "Process start until a TCP connection to the port is accepted",
            ),
            metric(
                "startup",
                "time_to_first_request",
                "ms",
                "Process start until a gRPC GetDocument of a missing document is answered (NOT_FOUND)",
            ),
            metric(
                "startup",
                "time_to_exit",
                "ms",
                "POST /shutdown until the process has exited",
            ),
        ];
        let runs = self.ctx.params.startup;
        self.each_run("startup", "startup", runs, &metrics, |ctx, target, name| {
            let mut running = target.spawn(ctx.logs.as_deref(), name)?;
            let port = running.wait_port()?;
            let first = ctx
                .rt
                .block_on(client::first_request(running.addr(), running.started))?;
            let exit = running.shutdown()?;
            Ok(vec![(0, ms(port)), (1, ms(first)), (2, ms(exit))])
        });
    }

    /// Memory and CPU time: when the port accepts, after the idle period, during and after
    /// writing the Phase 0 baseline's documents.
    pub fn memory(&mut self) {
        let p = self.ctx.params.memory.clone();
        let idle = format!("{} ms", p.idle_ms);
        let work = format!(
            "writing {} documents in commits of {} over gRPC",
            p.documents, p.batch
        );
        let metrics = [
            metric(
                "memory",
                "rss_at_ready",
                "bytes",
                "RSS when the port first accepts",
            ),
            metric(
                "memory",
                "rss_idle",
                "bytes",
                &format!("RSS after the port is ready and {idle} idle"),
            ),
            metric(
                "memory",
                "rss_during_workload",
                "bytes",
                &format!(
                    "Highest RSS sampled while {work} (sampled every {} ms)",
                    Sampler::interval().as_millis()
                ),
            ),
            metric(
                "memory",
                "rss_after_workload",
                "bytes",
                &format!("RSS right after {work}"),
            ),
            metric(
                "memory",
                "rss_peak",
                "bytes",
                &format!(
                    "Peak RSS of the process up to the end of the workload ({})",
                    system::peak_rss_method()
                ),
            ),
            metric(
                "memory",
                "cpu_time_to_ready",
                "ms",
                "CPU time (user + system) from process start until the port accepts",
            ),
            metric(
                "memory",
                "cpu_time_idle",
                "ms",
                &format!("CPU time (user + system) during the {idle} idle period"),
            ),
            metric(
                "memory",
                "cpu_time_workload",
                "ms",
                &format!("CPU time (user + system) while {work}"),
            ),
            metric(
                "memory",
                "workload_time",
                "ms",
                &format!("Wall-clock time of {work}"),
            ),
        ];
        self.each_run("memory", "memory", p.runs, &metrics, |ctx, target, name| {
            let mut running = target.spawn(ctx.logs.as_deref(), name)?;
            running.wait_port()?;
            let pid = running.pid();
            let read = || system::sample(pid).ok_or("could not read the process's memory");
            let ready = read()?;
            let sampler = Sampler::start(pid);
            thread::sleep(Duration::from_millis(p.idle_ms));
            let idle = read()?;
            let addr = running.addr();
            let start = Instant::now();
            ctx.rt.block_on(async {
                let mut client = Client::connect(addr).await?;
                client.seed("users", p.documents, p.batch, 100).await
            })?;
            let end = Instant::now();
            let after = read()?;
            let during = sampler
                .max_between(start, end)
                .unwrap_or(0)
                .max(after.rss_bytes);
            let peak = system::peak_rss(pid)
                .or_else(|| sampler.max())
                .unwrap_or(0)
                .max(during)
                .max(idle.rss_bytes)
                .max(ready.rss_bytes);
            sampler.finish();
            running.shutdown()?;
            Ok(vec![
                (0, ready.rss_bytes as f64),
                (1, idle.rss_bytes as f64),
                (2, during as f64),
                (3, after.rss_bytes as f64),
                (4, peak as f64),
                (5, ready.cpu_ms),
                (6, idle.cpu_ms - ready.cpu_ms),
                (7, after.cpu_ms - idle.cpu_ms),
                (8, ms(end - start)),
            ])
        });
    }

    /// Latency of single operations, in one process per target (warm: each operation is
    /// repeated, the first iterations discarded).
    pub fn operations(&mut self) {
        let p = self.ctx.params.operations.clone();
        let matching = p.seed_documents.div_ceil(10);
        let metrics = [
            metric(
                "operations",
                "single_read",
                "ms",
                "GetDocument of an existing document",
            ),
            metric(
                "operations",
                "single_write",
                "ms",
                "Commit with one write (a new document)",
            ),
            metric(
                "operations",
                "delete",
                "ms",
                "Commit with one delete of an existing document",
            ),
            metric(
                "operations",
                "query",
                "ms",
                &format!(
                    "RunQuery with an equality filter matching {matching} of {} documents, read to the end",
                    p.seed_documents
                ),
            ),
            metric(
                "operations",
                "batch_write",
                "ms",
                &format!(
                    "Commit with {} writes (the same {} documents each time)",
                    p.batch, p.batch
                ),
            ),
            metric(
                "operations",
                "transaction",
                "ms",
                "Read-modify-write transaction: BeginTransaction, GetDocument in it, Commit",
            ),
        ];
        let once = Runs { warmup: 0, runs: 1 };
        let total = p.warmup + p.iterations;
        self.each_run(
            "operations",
            "operations",
            once,
            &metrics,
            |ctx, target, name| {
                let mut running = target.spawn(ctx.logs.as_deref(), name)?;
                running.wait_port()?;
                let addr = running.addr();
                let values = ctx.rt.block_on(async {
                    let mut c = Client::connect(addr).await?;
                    c.seed("ops", p.seed_documents, 500, 10).await?;
                    c.seed("ops_delete", total, 500, 10).await?;
                    let mut values = Vec::new();
                    for op in 0..metrics.len() {
                        for i in 0..total {
                            let start = Instant::now();
                            match op {
                                0 => {
                                    c.get(&format!("ops/doc-{}", i % p.seed_documents.max(1)))
                                        .await?;
                                }
                                1 => {
                                    let doc = c.document(&format!("ops_write/doc-{i}"), i, 10);
                                    c.commit(vec![Client::update(doc)]).await?;
                                }
                                2 => {
                                    let write = c.delete(&format!("ops_delete/doc-{i}"));
                                    c.commit(vec![write]).await?;
                                }
                                3 => {
                                    c.query("ops", 3).await?;
                                }
                                4 => {
                                    let writes = (0..p.batch)
                                        .map(|n| {
                                            Client::update(c.document(
                                                &format!("ops_batch/doc-{n}"),
                                                n + i,
                                                10,
                                            ))
                                        })
                                        .collect();
                                    c.commit(writes).await?;
                                }
                                _ => c.increment("ops_tx/counter").await?,
                            }
                            if i >= p.warmup {
                                values.push((op, ms(start.elapsed())));
                            }
                        }
                    }
                    Ok::<_, String>(values)
                })?;
                running.shutdown()?;
                Ok(values)
            },
        );
    }

    /// Seeding N documents, a query over them and the memory they take, per size.
    pub fn datasets(&mut self) {
        let p = self.ctx.params.datasets.clone();
        for size in &p.sizes {
            let n = size.documents;
            let parameters = json!({ "documents": n });
            let with = |mut m: Metric| {
                m.parameters = Some(parameters.clone());
                m
            };
            let metrics = [
                with(metric(
                    "datasets",
                    &format!("seed_{n}"),
                    "ms",
                    &format!("Writing {n} documents in commits of {} over gRPC", p.batch),
                )),
                with(metric(
                    "datasets",
                    &format!("query_{n}"),
                    "ms",
                    &format!(
                        "RunQuery with an equality filter matching {} of the {n} documents, read to the end ({} per process after {} warm-up)",
                        n.div_ceil(100),
                        p.query_runs,
                        p.query_warmup
                    ),
                )),
                with(metric(
                    "datasets",
                    &format!("rss_after_seed_{n}"),
                    "bytes",
                    &format!("RSS right after writing {n} documents"),
                )),
            ];
            let label = format!("datasets {n}");
            self.each_run(
                "datasets",
                &label,
                size.runs,
                &metrics,
                |ctx, target, name| {
                    let mut running = target.spawn(ctx.logs.as_deref(), name)?;
                    running.wait_port()?;
                    let (addr, pid) = (running.addr(), running.pid());
                    let values = ctx.rt.block_on(async {
                        let mut c = Client::connect(addr).await?;
                        let start = Instant::now();
                        c.seed("users", n, p.batch, 100).await?;
                        let mut values = vec![(0, ms(start.elapsed()))];
                        let rss =
                            system::sample(pid).ok_or("could not read the process's memory")?;
                        values.push((2, rss.rss_bytes as f64));
                        for q in 0..p.query_warmup + p.query_runs {
                            let start = Instant::now();
                            c.query("users", 7).await?;
                            if q >= p.query_warmup {
                                values.push((1, ms(start.elapsed())));
                            }
                        }
                        Ok::<_, String>(values)
                    })?;
                    running.shutdown()?;
                    Ok(values)
                },
            );
        }
    }

    /// A CI job in miniature: start, wait until ready, seed, run small tests that each clean
    /// up through the reset endpoint, shut down.
    pub fn ci(&mut self) {
        let p = self.ctx.params.ci.clone();
        let metrics = [
            metric(
                "ci",
                "total",
                "ms",
                &format!(
                    "Process start until exit: ready, seed {} documents, {} tests, shutdown",
                    p.seed_documents, p.tests
                ),
            ),
            metric("ci", "ready", "ms", "Process start until the port accepts"),
            metric(
                "ci",
                "seed",
                "ms",
                &format!("Connecting and writing {} documents", p.seed_documents),
            ),
            metric(
                "ci",
                "tests",
                "ms",
                &format!(
                    "{} tests, each: one commit of 3 writes, 2 reads, 1 query, then a reset (DELETE …/documents)",
                    p.tests
                ),
            ),
            metric(
                "ci",
                "shutdown",
                "ms",
                "POST /shutdown until the process has exited",
            ),
        ];
        self.each_run("ci", "ci", p.runs, &metrics, |ctx, target, name| {
            let mut running = target.spawn(ctx.logs.as_deref(), name)?;
            let ready = running.wait_port()?;
            let addr = running.addr();
            let (seed, tests) = ctx.rt.block_on(async {
                let start = Instant::now();
                let mut c = Client::connect(addr).await?;
                c.seed("fixtures", p.seed_documents, 500, 10).await?;
                let seed = start.elapsed();
                let start = Instant::now();
                client::ci_tests(&mut c, p.tests).await?;
                Ok::<_, String>((seed, start.elapsed()))
            })?;
            let started = running.started;
            let shutdown = running.shutdown()?;
            let total = started.elapsed();
            Ok(vec![
                (0, ms(total)),
                (1, ms(ready)),
                (2, ms(seed)),
                (3, ms(tests)),
                (4, ms(shutdown)),
            ])
        });
    }

    /// The CI workload as projects run it: `firebase emulators:exec --only firestore` around
    /// the synthetic tests (`hidane benchmark workload`). firebase-tools starts the official
    /// emulator from the verified jar (`FIRESTORE_EMULATOR_BINARY_PATH`) with the same Java, and
    /// hidane through `hidane exec`. It runs in a scratch project with its own ports, a fresh
    /// configuration directory (no login, no usage reporting) and no update check.
    pub fn ci_firebase(&mut self, firebase: &Path, engine: &Path) {
        let p = self.ctx.params.ci_firebase.clone();
        let metrics = [
            metric(
                "ci_firebase",
                "total",
                "ms",
                &format!(
                    "`firebase emulators:exec --only firestore` from start to exit: the emulator starts, {} documents are seeded, {} tests run, the emulator stops",
                    p.seed_documents, p.tests
                ),
            ),
            metric(
                "ci_firebase",
                "workload",
                "ms",
                "The seeding and tests inside, as the workload times them",
            ),
        ];
        self.each_run(
            "ci_firebase",
            "ci_firebase",
            p.runs,
            &metrics,
            |ctx, target, name| {
                let dir = std::env::temp_dir()
                    .join(format!("hidane-bench-{}-{name}", std::process::id()));
                let result = firebase_run(ctx, target, firebase, engine, &dir, name, &p);
                let _ = std::fs::remove_dir_all(&dir);
                let (total, workload) = result?;
                Ok(vec![(0, ms(total)), (1, workload)])
            },
        );
    }

    /// Writes with a listener attached (firebase-tools#3477, docs/why.md §3): a Listen stream
    /// on the whole collection, on its own connection and read as it arrives, while the documents
    /// are written in commits over another connection, one commit at a time. The run ends when
    /// the listener's snapshot holds every document. Besides the totals, the batches' trend: the
    /// first and last tenths' medians and the growth per 10,000 documents stored.
    pub fn listen_writes(&mut self) {
        let p = self.ctx.params.listen_writes.clone();
        let work = format!(
            "{} documents written in commits of {} with a listener on the whole collection",
            render::thousands(p.documents as u64),
            p.batch
        );
        let metrics = listen_metrics(
            "listen_writes",
            &work,
            "the listener's snapshot holds every document",
        );
        self.each_run(
            "listen_writes",
            "listen_writes",
            p.runs,
            &metrics,
            |ctx, target, name| {
                listen_run(ctx, target, name, "listened", p.documents, p.batch, None)
            },
        );
    }

    /// As `listen_writes`, with a listener on the collection's first `limit` documents by name,
    /// as an SDK's `query(collection, limit(50))` (#250). The documents' IDs count down, so each
    /// commit's documents come first and replace the listener's previous ones, as in a feed of
    /// the newest items. The run ends when the listener's snapshot holds the last document
    /// written.
    pub fn listen_limit_writes(&mut self) {
        let p = self.ctx.params.listen_limit_writes.clone();
        let work = format!(
            "{} documents written in commits of {} with a `limit({})` listener on the collection",
            render::thousands(p.documents as u64),
            p.batch,
            p.limit
        );
        let metrics = listen_metrics(
            "listen_limit_writes",
            &work,
            "the listener's snapshot holds the last document written",
        );
        self.each_run(
            "listen_limit_writes",
            "listen_limit_writes",
            p.runs,
            &metrics,
            |ctx, target, name| {
                listen_run(
                    ctx,
                    target,
                    name,
                    "limited",
                    p.documents,
                    p.batch,
                    Some(p.limit),
                )
            },
        );
    }

    /// Writes with the owner credential the Admin SDKs send to an emulator, which skips Security
    /// Rules; the other scenarios send none, as a signed-out app. A fresh process per run:
    /// `batch_write` as under Operations (the same documents each time, the first commits
    /// discarded), then `seed_<n>` as under Dataset sizes.
    pub fn owner_writes(&mut self) {
        let p = self.ctx.params.owner_writes.clone();
        let n = p.documents;
        let metrics = [
            metric(
                "owner_writes",
                "batch_write",
                "ms",
                &format!(
                    "Commit with {} writes (the same {} documents each time), with the owner credential",
                    p.batch, p.batch
                ),
            ),
            Metric {
                parameters: Some(json!({ "documents": n })),
                ..metric(
                    "owner_writes",
                    &format!("seed_{n}"),
                    "ms",
                    &format!(
                        "Writing {n} documents in commits of {} over gRPC, with the owner credential",
                        p.batch
                    ),
                )
            },
        ];
        self.each_run(
            "owner_writes",
            "owner_writes",
            p.runs,
            &metrics,
            |ctx, target, name| {
                let mut running = target.spawn(ctx.logs.as_deref(), name)?;
                running.wait_port()?;
                let addr = running.addr();
                let values = ctx.rt.block_on(async {
                    let mut c = Client::connect_as(addr, Credential::Owner).await?;
                    let mut values = Vec::new();
                    for i in 0..p.commit_warmup + p.commits {
                        let start = Instant::now();
                        let writes = (0..p.batch)
                            .map(|k| {
                                Client::update(c.document(
                                    &format!("owner_batch/doc-{k}"),
                                    k + i,
                                    10,
                                ))
                            })
                            .collect();
                        c.commit(writes).await?;
                        if i >= p.commit_warmup {
                            values.push((0, ms(start.elapsed())));
                        }
                    }
                    let start = Instant::now();
                    c.seed("owner_users", n, p.batch, 100).await?;
                    values.push((1, ms(start.elapsed())));
                    Ok::<_, String>(values)
                })?;
                running.shutdown()?;
                Ok(values)
            },
        );
    }

    /// The user's command, run against each target in turn with `FIRESTORE_EMULATOR_HOST`
    /// pointing at it. Its output goes to stderr, so stdout stays the result.
    pub fn command(&mut self, command: &[OsString], label: &str) {
        let Some((program, args)) = command.split_first() else {
            return;
        };
        let parameters = json!({ "command": label });
        let with = |mut m: Metric| {
            m.parameters = Some(parameters.clone());
            m
        };
        let metrics = [
            with(metric(
                "command",
                "total",
                "ms",
                "Process start until exit: ready, the command, shutdown",
            )),
            with(metric(
                "command",
                "command",
                "ms",
                "Wall-clock time of the command, with FIRESTORE_EMULATOR_HOST set to the target",
            )),
            with(metric(
                "command",
                "ready",
                "ms",
                "Process start until the port accepts",
            )),
        ];
        let runs = self.ctx.params.command;
        self.each_run("command", "command", runs, &metrics, |ctx, target, name| {
            let mut running = target.spawn(ctx.logs.as_deref(), name)?;
            let ready = running.wait_port()?;
            let start = Instant::now();
            let status = Command::new(program)
                .args(args)
                .env(
                    "FIRESTORE_EMULATOR_HOST",
                    format!("127.0.0.1:{}", running.port),
                )
                .env("HIDANE_BENCH_TARGET", target.kind.id())
                .stdin(Stdio::null())
                .stdout(std::io::stderr())
                .status()
                .map_err(|e| format!("could not run the command ({e})"))?;
            let command = start.elapsed();
            let started = running.started;
            running.shutdown()?;
            let total = started.elapsed();
            if !status.success() {
                return Err(format!("the command failed ({status})"));
            }
            Ok(vec![(0, ms(total)), (1, ms(command)), (2, ms(ready))])
        });
    }
}

/// Single-quoted for the shell `firebase emulators:exec` runs its script with.
fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', r"'\''"))
}

/// The metrics of `listen_writes` and `listen_limit_writes`: `work` is what a run does, and
/// `delivered` when it ends.
fn listen_metrics(scenario: &'static str, work: &str, delivered: &str) -> [Metric; 6] {
    [
        metric(
            scenario,
            "writes",
            "ms",
            &format!("{work}: the first commit sent until the last one answered"),
        ),
        metric(
            scenario,
            "delivered",
            "ms",
            &format!("{work}: the first commit sent until {delivered}"),
        ),
        metric(
            scenario,
            "batch_first_decile",
            "ms",
            "Median commit time of the first tenth of the batches",
        ),
        metric(
            scenario,
            "batch_last_decile",
            "ms",
            "Median commit time of the last tenth of the batches",
        ),
        metric(
            scenario,
            "batch_growth",
            "ms",
            "How much a batch's commit time grows per 10,000 documents already stored, in ms (Theil–Sen: the median slope between every two batches of a run)",
        ),
        metric(
            scenario,
            "rss_after",
            "bytes",
            &format!("RSS once {delivered}"),
        ),
    ]
}

/// One run of `listen_writes` (`limit` `None`: a listener on the whole collection, documents
/// `doc-<i>`) or `listen_limit_writes` (a `limit(n)` listener, documents named so that the
/// newest come first by name).
fn listen_run(
    ctx: &Ctx,
    target: &Target,
    name: &str,
    collection: &str,
    documents: usize,
    batch: usize,
    limit: Option<usize>,
) -> Result<Vec<(usize, f64)>, String> {
    // Zero-padded IDs counting down to 0, the last document written.
    let width = (documents - 1).to_string().len();
    let path = |i: usize| match limit {
        None => format!("{collection}/doc-{i}"),
        Some(_) => format!("{collection}/doc-{:0width$}", documents - 1 - i),
    };
    let mut running = target.spawn(ctx.logs.as_deref(), name)?;
    running.wait_port()?;
    let (addr, pid) = (running.addr(), running.pid());
    let values = ctx.rt.block_on(async {
        let mut listener = match limit {
            None => Listener::start(addr, collection).await?,
            Some(n) => {
                let n = i32::try_from(n).map_err(|_| "limit too large")?;
                Listener::start_limited(addr, collection, n, &path(documents - 1)).await?
            }
        };
        listener.wait_for(0, Duration::from_secs(60)).await?;
        let mut c = Client::connect(addr).await?;
        let mut batches = Vec::new();
        let start = Instant::now();
        let mut stored = 0;
        while stored < documents {
            let end = (stored + batch).min(documents);
            let writes = (stored..end)
                .map(|i| Client::update(c.document(&path(i), i, 100)))
                .collect();
            let commit = Instant::now();
            c.commit(writes).await?;
            batches.push((stored as f64, ms(commit.elapsed())));
            stored = end;
        }
        let writes = start.elapsed();
        let snapshot = match limit {
            None => listener.wait_for(documents, DELIVERY_TIMEOUT).await?,
            Some(n) => {
                let snapshot = listener.wait_for_awaited(DELIVERY_TIMEOUT).await?;
                if snapshot.documents != n {
                    return Err(format!(
                        "the limit({n}) listener's snapshot holds {} documents",
                        snapshot.documents
                    ));
                }
                snapshot
            }
        };
        let rss = system::sample(pid).ok_or("could not read the process's memory")?;
        drop(listener);
        // The official emulator can send the last documents before it answers the last commit:
        // then `delivered` is at most `writes`.
        let mut values = vec![
            (0, ms(writes)),
            (1, ms(snapshot.at.saturating_duration_since(start))),
        ];
        if let Some(trend) = stats::trend(&batches) {
            values.extend([
                (2, trend.first_decile),
                (3, trend.last_decile),
                (4, trend.growth_per_10k),
            ]);
        }
        values.push((5, rss.rss_bytes as f64));
        Ok::<_, String>(values)
    })?;
    running.shutdown()?;
    Ok(values)
}

/// One `firebase emulators:exec` run in the scratch project `dir`: its wall-clock time and the
/// workload's own time in milliseconds.
fn firebase_run(
    ctx: &Ctx,
    target: &Target,
    firebase: &Path,
    engine: &Path,
    dir: &Path,
    name: &str,
    p: &Ci,
) -> Result<(Duration, f64), String> {
    let io = |e: std::io::Error| format!("could not prepare the firebase project ({e})");
    std::fs::create_dir_all(dir.join("config")).map_err(io)?;
    let ports = [
        target::free_port()?,
        target::free_port()?,
        target::free_port()?,
        target::free_port()?,
    ];
    let config = json!({
        "firestore": {},
        "emulators": {
            "singleProjectMode": true,
            "firestore": {"host": "127.0.0.1", "port": ports[0], "websocketPort": ports[1]},
            "hub": {"host": "127.0.0.1", "port": ports[2]},
            "logging": {"host": "127.0.0.1", "port": ports[3]},
            "ui": {"enabled": false},
        },
    });
    std::fs::write(dir.join("firebase.json"), config.to_string()).map_err(io)?;
    let report = dir.join("workload-ms");
    let script = format!(
        "{} benchmark workload --tests {} --seed {} --report {}",
        shell_quote(engine),
        p.tests,
        p.seed_documents,
        shell_quote(&report)
    );
    let mut command = match target.kind {
        Kind::Official => {
            let mut command = Command::new(firebase);
            let jar = target.jar.as_ref().ok_or("no jar")?;
            command.env("FIRESTORE_EMULATOR_BINARY_PATH", jar);
            command
        }
        Kind::Hidane => {
            let mut command = Command::new(&target.program);
            command.arg("exec").arg("--").arg(firebase);
            command
        }
    };
    // firebase-tools checks and runs the `java` on PATH: for both targets, the Java the
    // official emulator runs with directly (`hidane exec` passes its version check through).
    let java = ctx
        .targets
        .iter()
        .find(|t| t.kind == Kind::Official)
        .and_then(|t| t.program.parent());
    if let Some(bin) = java {
        let mut paths = vec![bin.to_path_buf()];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        if let Ok(path) = std::env::join_paths(paths) {
            command.env("PATH", path);
        }
    }
    command
        .args([
            "emulators:exec",
            "--only",
            "firestore",
            "--project",
            client::PROJECT,
            &script,
        ])
        .current_dir(dir)
        .env("NO_UPDATE_NOTIFIER", "1")
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .stdin(Stdio::null());
    match &ctx.logs {
        Some(logs) => {
            let file = std::fs::File::create(logs.join(format!("{name}.log"))).map_err(io)?;
            command.stdout(file.try_clone().map_err(io)?).stderr(file);
        }
        None => {
            command.stdout(Stdio::null()).stderr(Stdio::null());
        }
    }
    let start = Instant::now();
    let status = command
        .status()
        .map_err(|e| format!("could not run firebase-tools ({e})"))?;
    let total = start.elapsed();
    if !status.success() {
        return Err(format!(
            "firebase emulators:exec failed ({status}) (rerun with --keep-logs DIR to keep its output)"
        ));
    }
    let workload = std::fs::read_to_string(&report)
        .ok()
        .and_then(|t| t.trim().parse::<f64>().ok())
        .ok_or("the workload did not report its time")?;
    Ok((total, workload))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quoting() {
        assert_eq!(shell_quote(Path::new("/a b/hidane")), "'/a b/hidane'");
        assert_eq!(shell_quote(Path::new("/it's")), r"'/it'\''s'");
    }

    /// #291: a failed run is run again once, under its name with `-retry`; the failure stays in
    /// `errors`, marked `retried`, and the retry's values count. A retry that fails too is
    /// recorded as not retried, and that run has no values.
    #[test]
    fn a_failed_run_is_run_again_once() {
        let ctx = Ctx {
            rt: tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap(),
            targets: vec![Target::fake(Kind::Official), Target::fake(Kind::Hidane)],
            params: Params::default(),
            logs: None,
        };
        let mut bench = Bench {
            ctx,
            results: Results::default(),
        };
        let m = metric("startup", "time_to_port", "ms", "d");
        let mut names = Vec::new();
        bench.each_run(
            "startup",
            "startup",
            Runs { warmup: 0, runs: 3 },
            std::slice::from_ref(&m),
            |_, target, name| {
                names.push(name.to_owned());
                match (target.kind, name) {
                    (Kind::Official, "startup-official-2") => Err("transport error".to_owned()),
                    (Kind::Hidane, n) if n.starts_with("startup-hidane-3") => {
                        Err("refused".to_owned())
                    }
                    _ => Ok(vec![(0, 1.0)]),
                }
            },
        );
        assert_eq!(
            names,
            [
                "startup-official-1",
                "startup-hidane-1",
                "startup-official-2",
                "startup-official-2-retry",
                "startup-hidane-2",
                "startup-official-3",
                "startup-hidane-3",
                "startup-hidane-3-retry",
            ]
        );
        let errors: Vec<(&str, u64, bool)> = bench
            .results
            .errors
            .iter()
            .map(|e| {
                (
                    e["target"].as_str().unwrap(),
                    e["run"].as_u64().unwrap(),
                    e["retried"].as_bool().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            errors,
            [
                ("official", 2, true),
                ("hidane", 3, true),
                ("hidane", 3, false)
            ]
        );
        let n = |kind| {
            bench
                .results
                .stats("startup", kind, "time_to_port")
                .unwrap()
                .n
        };
        assert_eq!(n(Kind::Official), 3);
        assert_eq!(n(Kind::Hidane), 2);
    }

    #[test]
    fn results_keep_scenario_target_metric_order() {
        let m = metric("startup", "time_to_port", "ms", "d");
        let mut results = Results::default();
        results.add(&m, Kind::Official, 2.0);
        results.add(&m, Kind::Hidane, 1.0);
        results.add(&m, Kind::Official, 4.0);
        let json = results.to_json();
        let targets: Vec<&String> = json["startup"].as_object().unwrap().keys().collect();
        assert_eq!(targets, ["official", "hidane"]);
        assert_eq!(
            json["startup"]["official"]["time_to_port"]["samples"],
            json!([2.0, 4.0])
        );
        assert_eq!(
            json["startup"]["official"]["time_to_port"]["stats"]["median"],
            3.0
        );
        assert_eq!(
            results
                .stats("startup", Kind::Hidane, "time_to_port")
                .unwrap()
                .n,
            1
        );
    }
}
