//! `hidane benchmark`: hidane and the official emulator measured side by side, on one machine,
//! under the same conditions (bench/README.md).
//!
//! One engine serves the canonical harness (`bench/run.sh`) and anyone's own runs. It starts
//! each target as a fresh process on a free localhost port, drives both with the same gRPC
//! client, and writes a JSON result (`bench/schema/result.schema.json`). The Markdown report is
//! rendered from that JSON (`hidane benchmark render <file.json>`). Nothing is sent anywhere but
//! to the emulators it starts, the jar download when asked for, and on EC2 the instance metadata
//! service for the instance type.

mod client;
mod config;
mod render;
mod savings;
mod scenarios;
mod stats;
mod system;
mod target;

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::ExitCode,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use clap::{Parser, ValueEnum};
use serde_json::{Value, json};

use config::Params;
use scenarios::{Bench, Ctx, Results};
use target::{Kind, Target};

/// Version of the result format (`bench/schema/result.schema.json`).
pub const SCHEMA_VERSION: u64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Which {
    Hidane,
    Official,
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum RunKind {
    Local,
    PullRequest,
    Nightly,
    Release,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Scenario {
    Startup,
    Memory,
    Operations,
    Datasets,
    Ci,
    #[value(name = "ci_firebase")]
    CiFirebase,
    #[value(name = "listen_writes")]
    ListenWrites,
    Command,
    #[value(name = "owner_writes")]
    OwnerWrites,
    #[value(name = "listen_limit_writes")]
    ListenLimitWrites,
    #[value(name = "firebase_startup")]
    FirebaseStartup,
    #[value(name = "large_dataset")]
    LargeDataset,
}

impl Scenario {
    /// The order they run in. A new scenario goes after the existing ones, so that it cannot
    /// change the conditions the earlier ones are measured under.
    const ALL: [Self; 12] = [
        Self::Startup,
        Self::Memory,
        Self::Operations,
        Self::Datasets,
        Self::Ci,
        Self::CiFirebase,
        Self::ListenWrites,
        Self::Command,
        Self::OwnerWrites,
        Self::ListenLimitWrites,
        Self::FirebaseStartup,
        Self::LargeDataset,
    ];

    fn id(self) -> &'static str {
        match self {
            Self::Startup => "startup",
            Self::Memory => "memory",
            Self::Operations => "operations",
            Self::Datasets => "datasets",
            Self::Ci => "ci",
            Self::CiFirebase => "ci_firebase",
            Self::ListenWrites => "listen_writes",
            Self::Command => "command",
            Self::OwnerWrites => "owner_writes",
            Self::ListenLimitWrites => "listen_limit_writes",
            Self::FirebaseStartup => "firebase_startup",
            Self::LargeDataset => "large_dataset",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Format {
    Json,
    Markdown,
}

/// Measures hidane and the official Cloud Firestore emulator on this machine.
///
/// Also: `hidane benchmark render <result.json>` prints the Markdown report of a saved result,
/// `hidane benchmark savings <result.json> --runs-per-month N` turns its difference into CI time
/// saved, and `hidane benchmark workload` runs the synthetic CI tests against
/// FIRESTORE_EMULATOR_HOST.
#[derive(Debug, Parser)]
#[command(
    name = "hidane benchmark",
    disable_version_flag = true,
    after_help = "Methodology, metrics and the canonical harness: bench/README.md in the hidane repository."
)]
struct Args {
    /// Which emulator to measure.
    #[arg(long, value_enum, default_value = "hidane")]
    target: Which,
    /// Measure both, side by side (same as --target both).
    #[arg(long)]
    compare_official: bool,
    /// The hidane binary to measure (default: this one).
    #[arg(long, value_name = "PATH")]
    hidane: Option<PathBuf>,
    /// The git commit the hidane binary was built from, to record (in full; required for a
    /// nightly or release run).
    #[arg(long, value_name = "SHA")]
    hidane_commit: Option<String>,
    /// The hidane binary was built from a working tree with uncommitted changes.
    #[arg(long)]
    hidane_dirty: bool,
    /// The hidane binary's build profile, to record (default: this binary's, when measuring it).
    #[arg(long, value_name = "PROFILE")]
    hidane_profile: Option<String>,
    /// The official emulator's jar.
    #[arg(long, value_name = "PATH")]
    official_jar: Option<PathBuf>,
    /// Download the pinned official jar (v1.22.0) into the cache directory if it is not there,
    /// checking its SHA-256.
    #[arg(long)]
    official_download: bool,
    /// Where --official-download keeps the jar (default: ~/.cache/hidane-bench).
    #[arg(long, value_name = "DIR")]
    cache_dir: Option<PathBuf>,
    /// The Java to run the official emulator with (default: java on PATH; 21 or newer).
    #[arg(long, value_name = "PATH")]
    java: Option<PathBuf>,
    /// firebase-tools' `firebase`, for the ci_firebase and firebase_startup scenarios (skipped
    /// without it).
    #[arg(long, value_name = "PATH")]
    firebase: Option<PathBuf>,
    /// The git commit this binary (the engine) was built from, to record (in full; required for a
    /// nightly or release run).
    #[arg(long, value_name = "SHA")]
    engine_commit: Option<String>,
    /// Scenarios to run (default: all but command; with a command, only command).
    #[arg(long, value_enum, value_delimiter = ',')]
    scenarios: Option<Vec<Scenario>>,
    /// Warm-up runs per target (startup, memory, ci, command, dataset sizes, listen_writes,
    /// owner_writes, listen_limit_writes, firebase_startup and large_dataset; larger sizes,
    /// the writes and large_dataset are capped).
    #[arg(long)]
    warmup: Option<usize>,
    /// Measured runs per target (as --warmup).
    #[arg(long)]
    runs: Option<usize>,
    /// Measured iterations of each operation (operations scenario).
    #[arg(long)]
    iterations: Option<usize>,
    /// Dataset sizes, in documents.
    #[arg(long, value_delimiter = ',')]
    dataset_sizes: Option<Vec<usize>>,
    /// Tests in the synthetic CI workload.
    #[arg(long)]
    ci_tests: Option<usize>,
    /// Parameters file (bench/canonical.json).
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,
    /// A few runs of everything, to check the setup.
    #[arg(long)]
    quick: bool,
    /// What kind of run this is, to record. Nightly and release runs are the canonical
    /// history's: they need --hidane-commit and --engine-commit.
    #[arg(long, value_enum, default_value = "local")]
    kind: RunKind,
    /// The machine's name in result histories, chosen by the operator.
    #[arg(long, value_name = "ID")]
    machine_id: Option<String>,
    /// An operator's environment file: environment_revision and machine fields
    /// (bench/README.md).
    #[arg(long, value_name = "FILE")]
    environment_file: Option<PathBuf>,
    /// Do not ask the EC2 instance metadata service for the instance type.
    #[arg(long)]
    no_imds: bool,
    /// Output format.
    #[arg(long, value_enum, default_value = "json")]
    format: Format,
    /// Write the result here instead of stdout.
    #[arg(long, value_name = "FILE")]
    out: Option<PathBuf>,
    /// Render a saved JSON result as Markdown and exit.
    #[arg(long, value_name = "FILE")]
    render: Option<PathBuf>,
    /// Keep the emulators' output in this directory.
    #[arg(long, value_name = "DIR")]
    keep_logs: Option<PathBuf>,
    /// A command to measure against each target, with FIRESTORE_EMULATOR_HOST set.
    #[arg(last = true, value_name = "COMMAND")]
    command: Vec<OsString>,
}

/// `hidane benchmark …`: `args` are the arguments after `benchmark`.
pub fn main(args: Vec<OsString>) -> ExitCode {
    if args.first().is_some_and(|a| a == "workload") {
        return workload(&args[1..]);
    }
    if args.first().is_some_and(|a| a == "savings") {
        return savings::main(&args[1..]);
    }
    if args.first().is_some_and(|a| a == "render") {
        return match args.get(1) {
            Some(file) if args.len() == 2 => render_file(Path::new(file)),
            _ => {
                eprintln!("usage: hidane benchmark render <result.json>");
                ExitCode::from(2)
            }
        };
    }
    let args =
        match Args::try_parse_from(std::iter::once(OsString::from("hidane benchmark")).chain(args))
        {
            Ok(args) => args,
            Err(err) => {
                let _ = err.print();
                return ExitCode::from(u8::try_from(err.exit_code()).unwrap_or(2));
            }
        };
    if let Some(file) = &args.render {
        return render_file(file);
    }
    match run(&args) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("hidane benchmark: {message}");
            ExitCode::from(2)
        }
    }
}

fn render_file(path: &Path) -> ExitCode {
    let parsed = std::fs::read_to_string(path)
        .map_err(|e| e.to_string())
        .and_then(|text| serde_json::from_str::<Value>(&text).map_err(|e| e.to_string()));
    match parsed {
        Ok(result) => {
            emit(&render::markdown(&result));
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("hidane benchmark render: {}: {e}", path.display());
            ExitCode::from(2)
        }
    }
}

/// `hidane benchmark workload [--tests N] [--seed N] [--report FILE]`: the synthetic CI tests
/// against the emulator at `FIRESTORE_EMULATOR_HOST`, for `firebase emulators:exec` and the
/// command mode. Writes the time it took, in milliseconds, to the report file.
fn workload(args: &[OsString]) -> ExitCode {
    let usage = "usage: hidane benchmark workload [--tests N] [--seed N] [--report FILE]";
    let (mut tests, mut seed, mut report) = (200usize, 100usize, None::<PathBuf>);
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let value = it.next();
        let parsed = match (flag.to_str(), value) {
            (Some("--tests"), Some(v)) => {
                v.to_str().and_then(|v| v.parse().ok()).map(|v| tests = v)
            }
            (Some("--seed"), Some(v)) => v.to_str().and_then(|v| v.parse().ok()).map(|v| seed = v),
            (Some("--report"), Some(v)) => {
                report = Some(PathBuf::from(v));
                Some(())
            }
            _ => None,
        };
        if parsed.is_none() {
            eprintln!("{usage}");
            return ExitCode::from(2);
        }
    }
    let Some(host) = std::env::var("FIRESTORE_EMULATOR_HOST")
        .ok()
        .filter(|h| !h.is_empty())
    else {
        eprintln!("hidane benchmark workload: FIRESTORE_EMULATOR_HOST is not set");
        return ExitCode::from(2);
    };
    let addr = {
        use std::net::ToSocketAddrs as _;
        host.to_socket_addrs()
            .ok()
            .and_then(|mut addrs| addrs.find(std::net::SocketAddr::is_ipv4))
    };
    let Some(addr) = addr else {
        eprintln!("hidane benchmark workload: cannot resolve FIRESTORE_EMULATOR_HOST={host}");
        return ExitCode::from(2);
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("hidane benchmark workload: {e}");
            return ExitCode::FAILURE;
        }
    };
    let start = Instant::now();
    let done = runtime.block_on(async {
        let mut c = client::Client::connect(addr).await?;
        c.seed("fixtures", seed, 500, 10).await?;
        client::ci_tests(&mut c, tests).await
    });
    let elapsed = start.elapsed().as_secs_f64() * 1000.0;
    if let Err(e) = done {
        eprintln!("hidane benchmark workload: {e}");
        return ExitCode::FAILURE;
    }
    if let Some(path) = report
        && let Err(e) = std::fs::write(&path, format!("{elapsed:.3}\n"))
    {
        eprintln!("hidane benchmark workload: {}: {e}", path.display());
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn emit(text: &str) {
    use std::io::Write as _;
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(text.as_bytes()).and_then(|()| out.flush());
}

/// The parameters, and the command-line flags that changed them.
fn params(args: &Args) -> Result<(Params, Option<String>, Vec<&'static str>), String> {
    let mut params = if args.quick {
        Params::quick()
    } else {
        Params::default()
    };
    let mut config_sha256 = None;
    if let Some(path) = &args.config {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("could not read the config {} ({e})", path.display()))?;
        let value: Value = serde_json::from_str(&text)
            .map_err(|e| format!("the config {} is not JSON ({e})", path.display()))?;
        params
            .apply_config(&value, args.quick)
            .map_err(|e| format!("config {}: {e}", path.display()))?;
        config_sha256 = Some(target::sha256_file(path)?);
    }
    let mut overrides = Vec::new();
    if args.warmup.is_some() || args.runs.is_some() {
        let set = |r: &mut config::Runs| {
            if let Some(w) = args.warmup {
                r.warmup = w;
            }
            if let Some(n) = args.runs {
                r.runs = n.max(1);
            }
        };
        set(&mut params.startup);
        set(&mut params.memory.runs);
        set(&mut params.ci.runs);
        set(&mut params.ci_firebase.runs);
        set(&mut params.command);
        set(&mut params.firebase_startup);
        // Capped like the largest dataset sizes, as the official emulator writes 100,000
        // documents in each run.
        let mut owner = params.owner_writes.runs;
        set(&mut owner);
        params.owner_writes.runs = config::Runs {
            warmup: owner.warmup.min(1),
            runs: owner.runs.min(3),
        };
        // Capped like the largest dataset sizes: a run of the official emulator takes minutes.
        let mut listen = params.listen_writes.runs;
        set(&mut listen);
        params.listen_writes.runs = config::Runs {
            warmup: listen.warmup.min(1),
            runs: listen.runs.min(3),
        };
        let mut limited = params.listen_limit_writes.runs;
        set(&mut limited);
        params.listen_limit_writes.runs = config::Runs {
            warmup: limited.warmup.min(1),
            runs: limited.runs.min(3),
        };
        // Capped like the largest dataset sizes: a run of the official emulator takes a minute
        // or more.
        let mut large = params.large_dataset.runs;
        set(&mut large);
        params.large_dataset.runs = config::Runs {
            warmup: large.warmup.min(1),
            runs: large.runs.min(3),
        };
        for size in &mut params.datasets.sizes {
            let mut base = config::Runs {
                warmup: 3,
                runs: 10,
            };
            set(&mut base);
            size.runs = config::size_runs(size.documents, base);
        }
        if args.warmup.is_some() {
            overrides.push("--warmup");
        }
        if args.runs.is_some() {
            overrides.push("--runs");
        }
    }
    if let Some(n) = args.iterations {
        params.operations.iterations = n.max(1);
        params.operations.warmup = params.operations.warmup.min(n);
        overrides.push("--iterations");
    }
    if let Some(sizes) = &args.dataset_sizes {
        let mut base = config::Runs {
            warmup: 3,
            runs: 10,
        };
        if let Some(w) = args.warmup {
            base.warmup = w;
        }
        if let Some(n) = args.runs {
            base.runs = n.max(1);
        }
        params.datasets.sizes = sizes
            .iter()
            .filter(|n| **n > 0)
            .map(|&documents| config::Size {
                documents,
                runs: config::size_runs(documents, base),
            })
            .collect();
        overrides.push("--dataset-sizes");
    }
    if let Some(n) = args.ci_tests {
        params.ci.tests = n;
        params.ci_firebase.tests = n;
        overrides.push("--ci-tests");
    }
    Ok((params, config_sha256, overrides))
}

/// The scenarios to run, in their fixed order, and those left out for a missing tool.
fn scenario_list(args: &Args) -> Result<(Vec<Scenario>, Vec<Value>), String> {
    let list = match &args.scenarios {
        Some(list) => list.clone(),
        None if args.command.is_empty() => Scenario::ALL
            .into_iter()
            .filter(|s| *s != Scenario::Command)
            .collect(),
        None => vec![Scenario::Command],
    };
    if list.contains(&Scenario::Command) && args.command.is_empty() {
        return Err("the command scenario needs a command after `--`".into());
    }
    let mut ordered = Vec::new();
    let mut skipped = Vec::new();
    for s in Scenario::ALL {
        if !list.contains(&s) && !(s == Scenario::Command && !args.command.is_empty()) {
            continue;
        }
        if matches!(s, Scenario::CiFirebase | Scenario::FirebaseStartup) && args.firebase.is_none()
        {
            skipped.push(json!({
                "scenario": s.id(),
                "reason": "no firebase-tools binary was given (--firebase)",
            }));
            continue;
        }
        ordered.push(s);
    }
    Ok((ordered, skipped))
}

fn targets(args: &Args) -> Result<(Vec<Target>, Option<String>), String> {
    let which = if args.compare_official {
        Which::Both
    } else {
        args.target
    };
    let mut targets = Vec::new();
    let mut java_text = None;
    if which != Which::Hidane {
        let jar = match (&args.official_jar, args.official_download) {
            (Some(jar), _) => absolute(jar)?,
            (None, true) => {
                let cache = args
                    .cache_dir
                    .clone()
                    .or_else(target::default_cache_dir)
                    .ok_or("no cache directory; pass --cache-dir")?;
                target::download_official(&cache)?
            }
            (None, false) => {
                return Err(
                    "the official target needs --official-jar <path> or --official-download".into(),
                );
            }
        };
        let java = target::java(args.java.as_deref())?;
        let sha256 = target::sha256_file(&jar)?;
        let verified = sha256 == config::OFFICIAL_SHA256;
        if !verified {
            eprintln!(
                "hidane benchmark: WARNING: the jar's SHA-256 is {sha256}, not the pinned v{}'s; \
                 the result is recorded as not canonical",
                config::OFFICIAL_VERSION
            );
        }
        java_text = Some(match &java.vendor {
            Some(vendor) => format!("{} ({vendor})", java.version),
            None => java.version.clone(),
        });
        targets.push(Target::official(&java, &jar, verified, &sha256)?);
    }
    if which != Which::Official {
        let (path, profile) = match &args.hidane {
            Some(path) => {
                let path = absolute(path)?;
                // The engine measuring itself knows its own profile.
                let own = std::env::current_exe()
                    .and_then(|me| me.canonicalize())
                    .ok()
                    .filter(|me| path.canonicalize().is_ok_and(|p| p == *me))
                    .map(|_| {
                        if cfg!(debug_assertions) {
                            "debug"
                        } else {
                            "release"
                        }
                        .to_owned()
                    });
                (path, args.hidane_profile.clone().or(own))
            }
            None => (
                std::env::current_exe().map_err(|e| format!("could not find this binary ({e})"))?,
                Some(args.hidane_profile.clone().unwrap_or_else(|| {
                    if cfg!(debug_assertions) {
                        "debug"
                    } else {
                        "release"
                    }
                    .to_owned()
                })),
            ),
        };
        targets.push(Target::hidane(
            &path,
            args.hidane_commit.as_deref(),
            args.hidane_dirty,
            profile.as_deref(),
        )?);
    }
    Ok((targets, java_text))
}

/// Paths given on the command line, made absolute: firebase-tools runs in a scratch project.
fn absolute(path: &Path) -> Result<PathBuf, String> {
    std::path::absolute(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn utc_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let secs = i64::try_from(secs).unwrap_or(0);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Days since 1970-01-01 → (year, month, day), proleptic Gregorian (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn own_pids() -> Vec<u32> {
    let mut pids = vec![std::process::id()];
    #[cfg(unix)]
    pids.push(std::os::unix::process::parent_id());
    pids
}

fn snapshot_json(s: &system::Snapshot) -> Value {
    json!({
        "load_average": s.load,
        "available_memory_bytes": s.available_memory_bytes,
        "cpu_temperature_c": s.cpu_temperature_c.map(stats::round),
        "cpu_busy_percent": s.cpu_busy_percent.map(stats::round),
        "cpu_steal_percent": s.cpu_steal_percent.map(stats::round),
        "power_source": s.power_source,
        "known_jobs": s.known_jobs.iter().map(|(name, cpu)| json!({
            "name": name, "cpu_percent": stats::round(*cpu),
        })).collect::<Vec<_>>(),
        "other_busy_processes": s.other_busy_processes,
        "other_busy_cpu_percent": stats::round(s.other_busy_cpu_percent),
    })
}

struct Check {
    json: Value,
    reason: Option<String>,
}

/// A check of `value` against `limit` (`upper`: value must not exceed it). `None` on either side
/// is recorded with `passed: null`: not measurable here, or turned off.
fn check(
    name: &str,
    phase: &str,
    value: Option<f64>,
    limit: Option<f64>,
    upper: bool,
    describe: impl Fn(f64, f64) -> String,
) -> Check {
    let passed = match (value, limit) {
        (Some(v), Some(l)) => Some(if upper { v <= l } else { v >= l }),
        _ => None,
    };
    Check {
        json: json!({
            "name": name,
            "phase": phase,
            "value": value.map(stats::round),
            "limit": limit,
            "passed": passed,
        }),
        reason: match (passed, value, limit) {
            (Some(false), Some(v), Some(l)) => Some(describe(v, l)),
            _ => None,
        },
    }
}

fn stability(
    params: &Params,
    before: &system::Snapshot,
    after: &system::Snapshot,
    run_steal: Option<f64>,
    results: &Results,
    targets: &[Target],
) -> Value {
    let limits = &params.stability;
    let mib = 1024.0 * 1024.0;
    let mut checks = vec![
        check(
            "load_1m",
            "before",
            before.load.map(|l| l[0]),
            limits.max_load_1m,
            true,
            |v, l| format!("load average {v:.2} over the last minute before the run, above {l}"),
        ),
        check(
            "available_memory_mib",
            "before",
            before.available_memory_bytes.map(|b| b as f64 / mib),
            limits.min_available_memory_mib,
            false,
            |v, l| format!("{v:.0} MiB of memory available before the run, under {l}"),
        ),
        check(
            "cpu_steal_percent",
            "before",
            before.cpu_steal_percent,
            limits.max_cpu_steal_percent,
            true,
            |v, l| format!("CPU steal {v:.2} % before the run, above {l} %"),
        ),
        check(
            "cpu_temperature_c",
            "before",
            before.cpu_temperature_c,
            limits.max_cpu_temperature_c,
            true,
            |v, l| format!("CPU at {v:.0} °C before the run, above {l} °C"),
        ),
        check(
            "known_job_cpu_percent",
            "before",
            Some(
                before
                    .known_jobs
                    .iter()
                    .map(|(_, c)| *c)
                    .fold(0.0, f64::max),
            ),
            limits.max_known_job_cpu_percent,
            true,
            |v, l| {
                let names: Vec<&str> = before.known_jobs.iter().map(|(n, _)| n.as_str()).collect();
                format!(
                    "known background jobs running before the run ({}), the busiest at {v:.0} % CPU, above {l} %",
                    names.join(", ")
                )
            },
        ),
        // Recorded, not judged: the last minute before this reading is the end of the run itself,
        // so its load is the benchmark's own (on four vCPUs it is always above the limit).
        check(
            "load_1m",
            "after",
            after.load.map(|l| l[0]),
            None,
            true,
            |v, l| format!("load average {v:.2} over the last minute after the run, above {l}"),
        ),
        check(
            "cpu_temperature_c",
            "after",
            after.cpu_temperature_c,
            limits.max_cpu_temperature_c,
            true,
            |v, l| format!("CPU at {v:.0} °C after the run, above {l} °C"),
        ),
        check(
            "cpu_steal_percent",
            "run",
            run_steal,
            limits.max_cpu_steal_percent,
            true,
            |v, l| format!("CPU steal {v:.2} % over the run, above {l} %"),
        ),
    ];
    let battery = |s: &system::Snapshot, phase: &str| {
        let passed = s
            .power_source
            .map(|p| p != "battery" || limits.allow_battery);
        Check {
            json: json!({"name": "power_source", "phase": phase, "value": s.power_source,
                "limit": if limits.allow_battery { "any" } else { "ac" }, "passed": passed}),
            reason: (passed == Some(false))
                .then(|| format!("running on battery power ({phase} the run)")),
        }
    };
    checks.push(battery(before, "before"));
    checks.push(battery(after, "after"));
    for target in targets {
        let cv = results
            .stats("startup", target.kind, "time_to_port")
            .filter(|s| s.n >= 3)
            .map(|s| s.cv());
        checks.push(check(
            &format!("startup_cv_{}", target.kind.id()),
            "run",
            cv,
            limits.max_startup_cv,
            true,
            |v, l| {
                format!(
                    "{}'s startup times vary too much (coefficient of variation {v:.2}, above {l})",
                    target.kind.id()
                )
            },
        ));
    }
    let reasons: Vec<String> = checks.iter().filter_map(|c| c.reason.clone()).collect();
    let steal_available = cfg!(target_os = "linux") && run_steal.is_some();
    json!({
        "stable": reasons.is_empty(),
        "reasons": reasons,
        "limits": params.stability_json(),
        "checks": checks.into_iter().map(|c| c.json).collect::<Vec<_>>(),
        "before": snapshot_json(before),
        "after": snapshot_json(after),
        "cpu_steal": {
            "available": steal_available,
            "note": (!steal_available).then_some("CPU steal is read from /proc/stat on Linux only"),
            "pre_run_percent": before.cpu_steal_percent.map(stats::round),
            "run_percent": run_steal.map(stats::round),
            "scenarios": results.steal.iter().map(|(s, v)| ((*s).to_owned(), json!(stats::round(*v)))).collect::<serde_json::Map<_, _>>(),
        },
    })
}

fn command_label(command: &[OsString]) -> String {
    // The program's file name and the number of arguments: arguments can hold paths.
    let program = command
        .first()
        .map(|p| {
            Path::new(p)
                .file_name()
                .map_or_else(|| p.to_string_lossy(), |n| n.to_string_lossy())
                .into_owned()
        })
        .unwrap_or_default();
    match command.len().saturating_sub(1) {
        0 => program,
        1 => format!("{program} (1 argument)"),
        n => format!("{program} ({n} arguments)"),
    }
}

/// A full git commit SHA, as a canonical result records it.
fn is_commit(sha: &str) -> bool {
    sha.len() == 40 && sha.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A nightly or release result goes into the canonical history, which traces every number to the
/// commit of the hidane measured and of the engine that measured it (bench/README.md,
/// Provenance). Such a run measures hidane and is given both commits in full, or it does not
/// start.
fn check_provenance(args: &Args) -> Result<(), String> {
    let kind = match args.kind {
        RunKind::Nightly => "nightly",
        RunKind::Release => "release",
        RunKind::Local | RunKind::PullRequest => return Ok(()),
    };
    if !args.compare_official && args.target == Which::Official {
        return Err(format!(
            "a {kind} run measures hidane (and records its commit); --target official measures \
             the official emulator alone"
        ));
    }
    for (flag, what, value) in [
        (
            "--hidane-commit",
            "the hidane it measures",
            &args.hidane_commit,
        ),
        ("--engine-commit", "this engine", &args.engine_commit),
    ] {
        match value.as_deref() {
            Some(sha) if is_commit(sha) => {}
            Some(sha) => {
                return Err(format!(
                    "a {kind} run records full commits: {flag} {sha:?} is not 40 lowercase \
                     hexadecimal digits"
                ));
            }
            None => {
                return Err(format!(
                    "a {kind} run records the commit {what} was built from: pass {flag} <SHA>"
                ));
            }
        }
    }
    Ok(())
}

fn run(args: &Args) -> Result<ExitCode, String> {
    check_provenance(args)?;
    let begun = Instant::now();
    let timestamp = utc_now();
    let (params, config_sha256, overrides) = params(args)?;
    let (scenarios, skipped) = scenario_list(args)?;
    let engine_path =
        std::env::current_exe().map_err(|e| format!("could not find this binary ({e})"))?;
    let engine = json!({
        "version": env!("CARGO_PKG_VERSION"),
        "git_commit": args.engine_commit,
        "build_profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        "binary_sha256": target::sha256_file(&engine_path)?,
    });
    let (environment_revision, operator_machine) = match &args.environment_file {
        Some(path) => {
            let text = std::fs::read_to_string(path).map_err(|e| {
                format!(
                    "could not read the environment file {} ({e})",
                    path.display()
                )
            })?;
            let (revision, machine) = system::environment_file(&text)
                .map_err(|e| format!("environment file {}: {e}", path.display()))?;
            (Some(revision), Some(machine))
        }
        None => (None, None),
    };
    if let Some(dir) = &args.keep_logs {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("could not create {} ({e})", dir.display()))?;
    }
    let (targets, java) = targets(args)?;
    let official_verified = targets
        .iter()
        .find(|t| t.kind == Kind::Official)
        .is_none_or(|t| t.info["jar_verified"] == true);
    let canonical =
        !args.quick && overrides.is_empty() && params == Params::default() && official_verified;

    eprintln!("hidane benchmark: checking the machine before the run");
    let own = own_pids();
    let before = system::snapshot(Duration::from_secs(1), &own);
    let machine = system::machine(
        args.machine_id.as_deref(),
        operator_machine.as_ref(),
        !args.no_imds,
    );
    let official_java = targets
        .iter()
        .find(|t| t.kind == Kind::Official)
        .map(|t| t.program.clone());
    let environment = system::environment(
        java.as_deref(),
        official_java.as_deref(),
        args.firebase.as_deref(),
    );

    // Two workers keep the client's connections driven between requests, so a connection the
    // client is done with closes as it would in a test process, instead of holding up the
    // target's shutdown.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| format!("could not start the runtime ({e})"))?;
    let mut bench = Bench {
        ctx: Ctx {
            rt: runtime,
            targets: targets.clone(),
            params: params.clone(),
            logs: args.keep_logs.clone(),
        },
        results: Results::default(),
    };
    let run_start = system::cpu_times();
    let label = command_label(&args.command);
    for scenario in &scenarios {
        let start = system::cpu_times();
        match scenario {
            Scenario::Startup => bench.startup(),
            Scenario::Memory => bench.memory(),
            Scenario::Operations => bench.operations(),
            Scenario::Datasets => bench.datasets(),
            Scenario::Ci => bench.ci(),
            Scenario::CiFirebase => {
                if let Some(firebase) = &args.firebase {
                    bench.ci_firebase(&absolute(firebase)?, &engine_path);
                }
            }
            Scenario::ListenWrites => bench.listen_writes(),
            Scenario::Command => bench.command(&args.command, &label),
            Scenario::OwnerWrites => bench.owner_writes(),
            Scenario::ListenLimitWrites => bench.listen_limit_writes(),
            Scenario::FirebaseStartup => {
                if let Some(firebase) = &args.firebase {
                    bench.firebase_startup(&absolute(firebase)?);
                }
            }
            Scenario::LargeDataset => bench.large_dataset(),
        }
        if let (Some(a), Some(b)) = (start, system::cpu_times())
            && let Some((_, steal)) = a.shares(&b)
        {
            bench.results.steal.push((scenario.id(), steal));
        }
    }
    let run_steal = match (run_start, system::cpu_times()) {
        (Some(a), Some(b)) => a.shares(&b).map(|(_, steal)| steal),
        _ => None,
    };
    eprintln!("hidane benchmark: checking the machine after the run");
    let after = system::snapshot(Duration::from_secs(1), &own);
    let stability = stability(
        &params,
        &before,
        &after,
        run_steal,
        &bench.results,
        &targets,
    );

    let mut scenario_params = serde_json::Map::new();
    for scenario in &scenarios {
        let mut p = params.scenario_json(scenario.id());
        if *scenario == Scenario::Command {
            p["command"] = json!(label);
        }
        scenario_params.insert(scenario.id().to_owned(), p);
    }
    let mut target_info = serde_json::Map::new();
    for target in &targets {
        target_info.insert(target.kind.id().to_owned(), target.info.clone());
    }
    let result = json!({
        "schema_version": SCHEMA_VERSION,
        "benchmark": {
            "version": params.version,
            "kind": match args.kind {
                RunKind::Local => "local",
                RunKind::PullRequest => "pull-request",
                RunKind::Nightly => "nightly",
                RunKind::Release => "release",
            },
            "canonical": canonical,
            "quick": args.quick,
            "config_sha256": config_sha256,
            "overrides": overrides,
            "engine": engine,
            "scenarios": scenario_params,
            "skipped": skipped,
        },
        "timestamp": timestamp,
        "duration_s": stats::round(begun.elapsed().as_secs_f64()),
        "environment_revision": environment_revision,
        "machine": machine,
        "environment": environment,
        "stability": stability,
        "targets": target_info,
        "results": bench.results.to_json(),
        "errors": bench.results.errors,
    });
    let text = match args.format {
        Format::Json => {
            let mut text = serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?;
            text.push('\n');
            text
        }
        Format::Markdown => render::markdown(&result),
    };
    match &args.out {
        Some(path) => std::fs::write(path, text)
            .map_err(|e| format!("could not write {} ({e})", path.display()))?,
        None => emit(&text),
    }
    let stable = result["stability"]["stable"] == true;
    eprintln!(
        "hidane benchmark: done in {:.0} s; {}",
        begun.elapsed().as_secs_f64(),
        if stable {
            "stable".to_owned()
        } else {
            format!(
                "NOT stable: {}",
                result["stability"]["reasons"]
                    .as_array()
                    .map(|r| r
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("; "))
                    .unwrap_or_default()
            )
        }
    );
    Ok(if failed_runs(&result) > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// The runs a result has no values of: failures that were not run again (a retry that failed
/// too). A failure that was retried successfully costs nothing.
fn failed_runs(result: &Value) -> usize {
    result["errors"].as_array().map_or(0, |errors| {
        errors.iter().filter(|e| e["retried"] != true).count()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #291: only a failure that was not run again makes the exit status 1.
    #[test]
    fn retried_failures_do_not_fail_the_run() {
        let error = |retried: Option<bool>| {
            let mut e = json!({"scenario": "ci", "target": "official", "run": 2, "warmup": false, "message": "x"});
            if let Some(r) = retried {
                e["retried"] = r.into();
            }
            e
        };
        assert_eq!(failed_runs(&json!({"errors": []})), 0);
        assert_eq!(failed_runs(&json!({"errors": [error(Some(true))]})), 0);
        assert_eq!(
            failed_runs(&json!({"errors": [error(Some(true)), error(Some(false))]})),
            1
        );
        // Results from before retries existed count every failure.
        assert_eq!(failed_runs(&json!({"errors": [error(None)]})), 1);
    }

    #[test]
    fn civil_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(18_263), (2020, 1, 2));
        assert_eq!(civil_from_days(20_739), (2026, 10, 13));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        let now = utc_now();
        assert_eq!(now.len(), 20);
        assert!(now.ends_with('Z'));
    }

    #[test]
    fn command_labels_hold_no_paths() {
        let label = command_label(&[
            OsString::from("/home/someone/bin/node"),
            OsString::from("/home/someone/test.mjs"),
        ]);
        assert_eq!(label, "node (1 argument)");
        assert_eq!(command_label(&[OsString::from("npm")]), "npm");
    }

    fn parse(args: &[&str]) -> Args {
        Args::try_parse_from(std::iter::once("hidane benchmark").chain(args.iter().copied()))
            .unwrap()
    }

    #[test]
    fn flags_and_overrides() {
        let args = parse(&[
            "--compare-official",
            "--runs",
            "2",
            "--dataset-sizes",
            "100,100000",
        ]);
        let (p, sha, overrides) = params(&args).unwrap();
        assert_eq!(sha, None);
        assert_eq!(overrides, ["--runs", "--dataset-sizes"]);
        assert_eq!(p.startup.runs, 2);
        assert_eq!(p.datasets.sizes.len(), 2);
        assert_eq!(p.datasets.sizes[1].runs.warmup, 1);
        assert_eq!(p.datasets.sizes[1].runs.runs, 2);
        assert_eq!(p.listen_writes.runs, config::Runs { warmup: 0, runs: 2 });
        let (p, _, _) = params(&parse(&["--warmup", "3", "--runs", "10"])).unwrap();
        assert_eq!(p.listen_writes.runs, config::Runs { warmup: 1, runs: 3 });
        assert_eq!(p.owner_writes.runs, config::Runs { warmup: 1, runs: 3 });
        assert_eq!(
            p.listen_limit_writes.runs,
            config::Runs { warmup: 1, runs: 3 }
        );
        assert_eq!(p.large_dataset.runs, config::Runs { warmup: 1, runs: 3 });
        assert_eq!(
            p.firebase_startup,
            config::Runs {
                warmup: 3,
                runs: 10
            }
        );

        let args = parse(&["--", "node", "test.mjs"]);
        assert_eq!(scenario_list(&args).unwrap().0, [Scenario::Command]);
        assert_eq!(args.command, ["node", "test.mjs"]);
        let args = parse(&["--scenarios", "ci,startup"]);
        assert_eq!(
            scenario_list(&args).unwrap().0,
            [Scenario::Startup, Scenario::Ci]
        );
        assert!(scenario_list(&parse(&["--scenarios", "command"])).is_err());
        // Without --firebase, ci_firebase and firebase_startup are skipped and say why.
        let (list, skipped) = scenario_list(&parse(&[])).unwrap();
        assert_eq!(list.len(), 9);
        assert_eq!(list.last(), Some(&Scenario::LargeDataset));
        assert_eq!(skipped[0]["scenario"], "ci_firebase");
        assert_eq!(skipped[1]["scenario"], "firebase_startup");
        let (list, skipped) = scenario_list(&parse(&["--firebase", "/x/firebase"])).unwrap();
        assert_eq!(list.len(), 11);
        assert!(skipped.is_empty());
        assert_eq!(params(&parse(&[])).unwrap().0, Params::default());
    }

    #[test]
    fn stability_checks_and_reasons() {
        let quiet = system::Snapshot {
            load: Some([0.3, 0.2, 0.1]),
            available_memory_bytes: Some(32 << 30),
            power_source: Some("ac"),
            ..system::Snapshot::default()
        };
        let params = Params::default();
        let results = Results::default();
        let s = stability(&params, &quiet, &quiet, Some(0.4), &results, &[]);
        assert_eq!(s["stable"], true, "{s:#}");

        let busy = system::Snapshot {
            load: Some([12.5, 11.0, 14.3]),
            available_memory_bytes: Some(1 << 30),
            cpu_steal_percent: Some(9.0),
            known_jobs: vec![("rustc".into(), 95.0)],
            ..quiet.clone()
        };
        let s = stability(&params, &busy, &quiet, Some(6.5), &results, &[]);
        assert_eq!(s["stable"], false);
        let reasons: Vec<&str> = s["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r.as_str().unwrap())
            .collect();
        assert_eq!(reasons.len(), 5, "{reasons:#?}");
        assert!(reasons[0].contains("load average 12.50"));
        assert!(reasons[1].contains("1024 MiB"));
        assert!(reasons[2].contains("steal 9.00 %"));
        assert!(reasons[3].contains("rustc"));
        assert!(reasons[4].contains("steal 6.50 % over the run"));
        // The load right after the run is the run's own: recorded, not judged.
        let busy_after = system::Snapshot {
            load: Some([3.2, 1.5, 0.8]),
            ..system::Snapshot::default()
        };
        let quiet_before = system::Snapshot {
            load: Some([0.1, 0.1, 0.1]),
            ..system::Snapshot::default()
        };
        let s = stability(&params, &quiet_before, &busy_after, None, &results, &[]);
        assert_eq!(s["stable"], true, "{s:#}");
        // Not measurable: recorded, not failed.
        let unknown = system::Snapshot::default();
        let s = stability(&params, &unknown, &unknown, None, &results, &[]);
        assert_eq!(s["stable"], true);
        assert!(
            s["checks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["passed"].is_null())
        );
    }
}
