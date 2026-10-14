//! `hidane benchmark savings <result.json>`: what a measured difference means for a CI bill.
//!
//! It reads a result of `hidane benchmark` that measured both emulators, takes the medians of one
//! CI-shaped scenario's `total` (the user's command, `ci_firebase` or `ci`), and multiplies their
//! difference by how often the job runs and, when asked, by what a minute of CI costs. Nothing is
//! measured here and no price is built in: prices change and differ, so the rate is the user's.
//! The output follows the claim policy (bench/README.md): medians, the environment stated, no
//! "N× faster".

use std::{ffi::OsString, fmt::Write as _, io::Read as _, path::PathBuf, process::ExitCode};

use clap::{Parser, ValueEnum};
use serde_json::{Value, json};

use super::{render, stats};

/// Version of the savings JSON (`bench/schema/savings.schema.json`).
pub const SCHEMA_VERSION: u64 = 1;

const METHOD: &str = "https://github.com/hidane-dev/hidane/blob/main/bench/README.md";

/// The scenarios shaped like a CI job, each with a `total` from emulator start to exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Source {
    Command,
    #[value(name = "ci_firebase")]
    CiFirebase,
    Ci,
}

impl Source {
    /// Without `--scenario`, the first one the result measured for both emulators: the user's
    /// own command says the most about their CI, the synthetic workloads less.
    const PREFERENCE: [Self; 3] = [Self::Command, Self::CiFirebase, Self::Ci];

    fn id(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::CiFirebase => "ci_firebase",
            Self::Ci => "ci",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Format {
    Text,
    Json,
    Markdown,
}

/// Turns a measured difference between the two emulators into CI time, and optionally cost,
/// saved per month.
#[derive(Debug, Parser)]
#[command(
    name = "hidane benchmark savings",
    disable_version_flag = true,
    long_about = "Turns a measured difference between the official emulator and hidane into CI \
                  time, and optionally cost, saved per month. It reads a result of `hidane \
                  benchmark` that measured both (from a file, or `-` for stdin), takes the median \
                  `total` of your command (the command scenario), else of `ci_firebase`, else of \
                  `ci`, and multiplies the difference by --runs-per-month. No price is built in: \
                  give your own rate with --cost-per-minute and --currency.\n\n\
                  A fresh measurement of your own test suite, straight into an estimate:\n\n  \
                  hidane benchmark --compare-official --official-download -- npm test \\\n    \
                  | hidane benchmark savings - --runs-per-month 600",
    after_help = "Exit codes: 0 when the estimate was printed, 2 for a usage error or a result \
                  without both emulators measured.\n\nMethodology and claim policy: \
                  bench/README.md in the hidane repository."
)]
struct Args {
    /// A result of `hidane benchmark` with both emulators measured; `-` reads it from stdin.
    #[arg(value_name = "RESULT")]
    result: PathBuf,
    /// How many times the job runs per month.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
    runs_per_month: u64,
    /// What one minute of CI costs you, in --currency (no default: prices change).
    #[arg(long, value_name = "AMOUNT", requires = "currency")]
    cost_per_minute: Option<f64>,
    /// The currency of --cost-per-minute, as a label (USD, EUR, JPY, …).
    #[arg(long, value_name = "LABEL", requires = "cost_per_minute")]
    currency: Option<String>,
    /// The scenario to read (default: command, else ci_firebase, else ci).
    #[arg(long, value_enum)]
    scenario: Option<Source>,
    /// text: for the terminal; json: for tools; markdown: a case-study block to paste.
    #[arg(long, value_enum, default_value = "text")]
    format: Format,
}

/// One emulator's side: the median `total` and how many measured runs it is the median of.
#[derive(Debug, Clone, PartialEq)]
struct Side {
    version: String,
    median_ms: f64,
    runs: u64,
}

#[derive(Debug, Clone, PartialEq)]
struct Rate {
    per_minute: f64,
    currency: String,
}

/// The estimate: the measured medians and the user's figures.
#[derive(Debug, Clone, PartialEq)]
struct Estimate {
    scenario: Source,
    /// The official emulator.
    before: Side,
    /// hidane.
    after: Side,
    runs_per_month: u64,
    rate: Option<Rate>,
}

impl Estimate {
    /// Positive when hidane's median was shorter.
    fn per_run_ms(&self) -> f64 {
        self.before.median_ms - self.after.median_ms
    }

    fn per_month_ms(&self) -> f64 {
        self.per_run_ms() * self.runs_per_month as f64
    }

    fn per_month_minutes(&self) -> f64 {
        self.per_month_ms() / 60_000.0
    }

    fn cost_per_month(&self) -> Option<f64> {
        self.rate
            .as_ref()
            .map(|r| self.per_month_minutes() * r.per_minute)
    }

    fn saves(&self) -> bool {
        self.per_run_ms() > 0.0
    }
}

/// `hidane benchmark savings …`: `args` are the arguments after `savings`.
pub fn main(args: &[OsString]) -> ExitCode {
    let args = match Args::try_parse_from(
        std::iter::once(OsString::from("hidane benchmark savings")).chain(args.iter().cloned()),
    ) {
        Ok(args) => args,
        Err(err) => {
            let _ = err.print();
            return ExitCode::from(u8::try_from(err.exit_code()).unwrap_or(2));
        }
    };
    match run(&args) {
        Ok(text) => {
            super::emit(&text);
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("hidane benchmark savings: {message}");
            ExitCode::from(2)
        }
    }
}

fn run(args: &Args) -> Result<String, String> {
    let rate = match (args.cost_per_minute, &args.currency) {
        (Some(per_minute), Some(currency)) => Some(rate(per_minute, currency)?),
        _ => None,
    };
    let text = if args.result.as_os_str() == "-" {
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .map_err(|e| format!("could not read the result from stdin ({e})"))?;
        text
    } else {
        std::fs::read_to_string(&args.result)
            .map_err(|e| format!("could not read {} ({e})", args.result.display()))?
    };
    let result: Value = serde_json::from_str(&text).map_err(|e| {
        format!("the result is not JSON ({e}); give the JSON `hidane benchmark` writes")
    })?;
    if result["schema_version"] != json!(super::SCHEMA_VERSION) {
        return Err(format!(
            "the result's schema_version is {}, not {}: not a result of this `hidane benchmark`",
            result["schema_version"],
            super::SCHEMA_VERSION
        ));
    }
    let estimate = estimate(&result, args.scenario, args.runs_per_month, rate)?;
    for line in advice(&result, &estimate) {
        eprintln!("hidane benchmark savings: note: {line}");
    }
    Ok(match args.format {
        Format::Text => text_report(&result, &estimate),
        Format::Json => {
            let mut text = serde_json::to_string_pretty(&json_report(&result, &estimate))
                .map_err(|e| e.to_string())?;
            text.push('\n');
            text
        }
        Format::Markdown => case_study(&result, &estimate),
    })
}

/// The user's rate, checked: a finite amount, and a label that reads as one in Markdown.
fn rate(per_minute: f64, currency: &str) -> Result<Rate, String> {
    if !per_minute.is_finite() || per_minute < 0.0 {
        return Err(format!(
            "--cost-per-minute must be a number of at least 0, not {per_minute}"
        ));
    }
    let label = currency.trim();
    if label.is_empty()
        || label.chars().count() > 16
        || label
            .chars()
            .any(|c| c.is_control() || matches!(c, '|' | '`' | '<' | '>' | '[' | ']' | '\\'))
    {
        return Err(format!(
            "--currency must be a short label such as USD (at most 16 characters, no |, `, <, >, [, ] or \\), not {currency:?}"
        ));
    }
    Ok(Rate {
        per_minute,
        currency: label.to_owned(),
    })
}

fn side(result: &Value, scenario: Source, target: &str) -> Option<Side> {
    let total = &result["results"][scenario.id()][target]["total"];
    Some(Side {
        version: result["targets"][target]["version"]
            .as_str()
            .unwrap_or("unknown")
            .to_owned(),
        median_ms: total["stats"]["median"]
            .as_f64()
            .filter(|v| v.is_finite())?,
        runs: total["stats"]["n"].as_u64()?,
    })
}

fn estimate(
    result: &Value,
    scenario: Option<Source>,
    runs_per_month: u64,
    rate: Option<Rate>,
) -> Result<Estimate, String> {
    let both = |s: Source| Some((s, side(result, s, "official")?, side(result, s, "hidane")?));
    let found = match scenario {
        Some(s) => both(s).ok_or_else(|| {
            let missing: Vec<&str> = ["official", "hidane"]
                .into_iter()
                .filter(|t| side(result, s, t).is_none())
                .collect();
            format!(
                "the result has no `{}` measurement of {}; measure both with `hidane benchmark --compare-official …`",
                s.id(),
                missing.join(" or ")
            )
        })?,
        None => Source::PREFERENCE
            .into_iter()
            .find_map(both)
            .ok_or("the result measured no CI-shaped scenario (command, ci_firebase or ci) on both emulators; measure your test command with `hidane benchmark --compare-official --official-download -- <command>`")?,
    };
    let (scenario, before, after) = found;
    Ok(Estimate {
        scenario,
        before,
        after,
        runs_per_month,
        rate,
    })
}

/// A duration for people: milliseconds below a second, seconds with two decimals below a
/// minute, then minutes and seconds, then hours and minutes. A value whose rounding reaches the
/// next unit is written in that unit ("1 min 0 s", not "60.00 s").
fn duration(ms: f64) -> String {
    let ms = ms.abs();
    if ms.round() < 1_000.0 {
        return format!("{ms:.0} ms");
    }
    if (ms / 10.0).round() < 6_000.0 {
        return format!("{:.2} s", ms / 1_000.0);
    }
    let seconds = (ms / 1_000.0).round() as u64;
    if seconds < 3_600 {
        return format!("{} min {} s", seconds / 60, seconds % 60);
    }
    let minutes = (ms / 60_000.0).round() as u64;
    format!("{} h {} min", minutes / 60, minutes % 60)
}

/// An amount of money in the user's currency, to the cent (or its equivalent).
fn money(amount: f64, currency: &str) -> String {
    format!("{amount:.2} {currency}")
}

fn str_of(value: &Value) -> Option<&str> {
    value.as_str().filter(|s| !s.is_empty())
}

/// What ran, in a phrase: "the test command `npm` (1 argument), …".
fn workload(result: &Value, scenario: Source, own: &str) -> String {
    let p = &result["benchmark"]["scenarios"][scenario.id()];
    let n = |key: &str| p[key].as_u64().unwrap_or(0);
    match scenario {
        Source::Command => format!(
            "{own} test command ({}), timed from the emulator's start to its shutdown",
            command_phrase(str_of(&p["command"]).unwrap_or("unknown"))
        ),
        Source::CiFirebase => format!(
            "`firebase emulators:exec --only firestore` around hidane's synthetic CI workload ({} documents seeded, {} tests), not {own} test suite",
            render::thousands(n("seed_documents")),
            render::thousands(n("tests"))
        ),
        Source::Ci => format!(
            "hidane's synthetic CI workload (the emulator started directly, {} documents seeded, {} tests, shut down), not {own} test suite",
            render::thousands(n("seed_documents")),
            render::thousands(n("tests"))
        ),
    }
}

/// The command scenario's label, "npm (1 argument)", as "`npm` with 1 argument".
fn command_phrase(label: &str) -> String {
    match label.split_once(" (") {
        Some((program, rest)) if rest.ends_with(')') => {
            format!("`{program}` with {}", &rest[..rest.len() - 1])
        }
        _ => format!("`{label}`"),
    }
}

/// How the runs were made: "5 measured runs per emulator after 1 warm-up".
fn runs_text(result: &Value, e: &Estimate) -> String {
    let p = &result["benchmark"]["scenarios"][e.scenario.id()];
    let runs = if e.before.runs == e.after.runs {
        format!(
            "{} measured run{} per emulator",
            e.before.runs,
            if e.before.runs == 1 { "" } else { "s" }
        )
    } else {
        format!(
            "{} measured runs of the official emulator and {} of hidane",
            e.before.runs, e.after.runs
        )
    };
    match p["warmup"].as_u64() {
        Some(0) | None => runs,
        Some(1) => format!("{runs} after 1 warm-up"),
        Some(w) => format!("{runs} after {w} warm-ups"),
    }
}

/// The machine and tools in a sentence each: what a reader needs to weigh the numbers.
fn environment(result: &Value) -> (String, String, String) {
    let m = &result["machine"];
    let mut machine = Vec::new();
    if let Some(cpu) = str_of(&m["cpu_model"]) {
        machine.push(cpu.to_owned());
    }
    match (m["cpu_cores"].as_u64(), m["cpu_threads"].as_u64()) {
        (Some(c), Some(t)) if c == t => machine.push(format!("{c} cores")),
        (Some(c), Some(t)) => machine.push(format!("{c} cores, {t} threads")),
        (None, Some(t)) => machine.push(format!("{t} threads")),
        _ => {}
    }
    if let Some(ram) = m["ram_bytes"].as_f64() {
        machine.push(format!("{:.0} GiB RAM", ram / (1u64 << 30) as f64));
    }
    let instance = str_of(&m["instance_type"]).or_else(|| str_of(&m["cloud"]["instance_type"]));
    if let Some(instance) = instance {
        machine.push(format!("instance type {instance}"));
    }
    let mut os = Vec::new();
    for key in ["distribution", "arch"] {
        if let Some(v) = str_of(&m[key]) {
            os.push(v.to_owned());
        }
    }
    if os.is_empty()
        && let Some(v) = str_of(&m["os"])
    {
        os.push(v.to_owned());
    }
    let mut tools = Vec::new();
    let official = &result["targets"]["official"];
    if let Some(java) = str_of(&official["java"]) {
        tools.push(match str_of(&official["java_vendor"]) {
            Some(vendor) => format!("Java {java} ({vendor})"),
            None => format!("Java {java}"),
        });
    }
    let env = &result["environment"];
    for (key, name) in [("node", "Node.js"), ("firebase_tools", "firebase-tools")] {
        if let Some(v) = str_of(&env[key]) {
            tools.push(format!("{name} {v}"));
        }
    }
    let or_unknown = |v: Vec<String>| {
        if v.is_empty() {
            "not recorded".to_owned()
        } else {
            v.join(", ")
        }
    };
    (or_unknown(machine), or_unknown(os), or_unknown(tools))
}

fn date(result: &Value) -> &str {
    str_of(&result["timestamp"])
        .and_then(|t| t.get(..10))
        .unwrap_or("an unknown date")
}

/// Caveats a reader of the numbers needs, in every format.
fn caveats(result: &Value, e: &Estimate) -> Vec<String> {
    let mut out = Vec::new();
    if result["stability"]["stable"] != true {
        let reasons: Vec<&str> = result["stability"]["reasons"]
            .as_array()
            .map(|r| r.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        out.push(if reasons.is_empty() {
            "The run did not pass the stability checks, so its numbers are noisy and not fit to cite.".to_owned()
        } else {
            format!(
                "The run did not pass the stability checks ({}), so its numbers are noisy and not fit to cite.",
                reasons.join("; ")
            )
        });
    }
    if result["benchmark"]["quick"] == true {
        out.push("The run used `--quick` parameters: only a few runs per emulator.".to_owned());
    }
    let fewest = e.before.runs.min(e.after.runs);
    if fewest < 3 {
        out.push(format!(
            "A median of {fewest} run{} is fragile; more runs (`--runs 5`) make it steadier.",
            if fewest == 1 { "" } else { "s" }
        ));
    }
    let failed = result["errors"].as_array().map_or(0, |errors| {
        errors
            .iter()
            .filter(|err| err["scenario"] == e.scenario.id() && err["retried"] != true)
            .count()
    });
    if failed > 0 {
        out.push(format!(
            "{failed} run{} of this workload failed and {} not counted.",
            if failed == 1 { "" } else { "s" },
            if failed == 1 { "is" } else { "are" }
        ));
    }
    if result["targets"]["official"]["jar_verified"] == false {
        out.push(
            "The official emulator's jar was not the pinned one (v1.22.0, as firebase-tools 15.33.0 downloads it).".to_owned(),
        );
    }
    if !e.saves() {
        out.push(format!(
            "hidane's median was {} in this measurement, so it shows no time saved.",
            if e.per_run_ms() == 0.0 {
                "the same as the official emulator's".to_owned()
            } else {
                format!("{} longer", duration(e.per_run_ms()))
            }
        ));
    }
    if e.rate.is_some() {
        out.push(
            "The cost is the saved minutes times the given rate. A CI provider that bills each job in whole minutes can bill a saving of seconds per job differently."
                .to_owned(),
        );
    }
    out
}

/// Advice for the person running the command, not for readers of the numbers: on stderr.
fn advice(result: &Value, e: &Estimate) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(kind @ ("nightly" | "release")) = result["benchmark"]["kind"].as_str() {
        out.push(format!(
            "this is a {kind} result, the kind hidane's canonical benchmark machine makes; a case study should come from your own measurement"
        ));
    }
    if e.scenario != Source::Command {
        out.push(
            "the estimate rests on a synthetic workload; for your own test suite, measure it: hidane benchmark --compare-official --official-download -- <your test command>"
                .to_owned(),
        );
    }
    out
}

fn saved_text(e: &Estimate, ms: f64) -> String {
    if e.saves() {
        duration(ms)
    } else {
        "none".to_owned()
    }
}

fn text_report(result: &Value, e: &Estimate) -> String {
    let (machine, os, tools) = environment(result);
    let mut out = String::from("CI time saved, from a measurement of both emulators\n\n");
    let mut row = |key: &str, value: String| {
        let _ = writeln!(out, "  {key:<17}{value}");
    };
    row("Workload", workload(result, e.scenario, "your"));
    row(
        "Before",
        format!(
            "the official emulator {}: {} per run (median of {})",
            e.before.version,
            duration(e.before.median_ms),
            e.before.runs
        ),
    );
    row(
        "After",
        format!(
            "hidane {}: {} per run (median of {})",
            e.after.version,
            duration(e.after.median_ms),
            e.after.runs
        ),
    );
    row("Saved per run", saved_text(e, e.per_run_ms()));
    row("Runs per month", render::thousands(e.runs_per_month));
    row("Saved per month", saved_text(e, e.per_month_ms()));
    if let (Some(rate), Some(cost)) = (&e.rate, e.cost_per_month()) {
        row(
            "Saved cost",
            if e.saves() {
                format!(
                    "{} per month, at {} {} per minute",
                    money(cost, &rate.currency),
                    rate.per_minute,
                    rate.currency
                )
            } else {
                "none".to_owned()
            },
        );
    }
    let _ = write!(
        out,
        "\nMeasured on {} with `hidane benchmark` (benchmark definitions v{}, {}): {}.\n\
         Machine: {machine}; {os}. Tools: {tools}.\n\
         These are medians of your own measurement on that machine; other machines and workloads \
         give other numbers.\n",
        date(result),
        str_of(&result["benchmark"]["version"]).unwrap_or("?"),
        if result["stability"]["stable"] == true {
            "stable"
        } else {
            "not stable"
        },
        runs_text(result, e)
    );
    let caveats = caveats(result, e);
    if !caveats.is_empty() {
        out.push_str("\nNotes:\n");
        for c in caveats {
            let _ = writeln!(out, "- {c}");
        }
    }
    out
}

fn json_report(result: &Value, e: &Estimate) -> Value {
    let (machine, os, tools) = environment(result);
    let p = &result["benchmark"]["scenarios"][e.scenario.id()];
    json!({
        "schema_version": SCHEMA_VERSION,
        "source": {
            "schema_version": result["schema_version"],
            "benchmark_version": result["benchmark"]["version"],
            "kind": result["benchmark"]["kind"],
            "canonical": result["benchmark"]["canonical"] == true,
            "quick": result["benchmark"]["quick"] == true,
            "timestamp": result["timestamp"],
            "stable": result["stability"]["stable"] == true,
            "scenario": e.scenario.id(),
            "metric": "total",
            "command": p["command"],
            "warmup": p["warmup"],
        },
        "workload": workload(result, e.scenario, "your"),
        "environment": {"machine": machine, "os": os, "tools": tools},
        "before": {
            "target": "official",
            "version": e.before.version,
            "median_ms": stats::round(e.before.median_ms),
            "runs": e.before.runs,
        },
        "after": {
            "target": "hidane",
            "version": e.after.version,
            "median_ms": stats::round(e.after.median_ms),
            "runs": e.after.runs,
        },
        "runs_per_month": e.runs_per_month,
        "saved": {
            "per_run_ms": stats::round(e.per_run_ms()),
            "per_month_ms": stats::round(e.per_month_ms()),
            "per_month_minutes": stats::round(e.per_month_minutes()),
            "per_month_hours": stats::round(e.per_month_minutes() / 60.0),
        },
        "cost": e.rate.as_ref().map(|r| json!({
            "per_minute": r.per_minute,
            "currency": r.currency,
            "saved_per_month": e.cost_per_month().map(stats::round),
        })),
        "notes": caveats(result, e),
    })
}

/// The Markdown block a team can paste into a README, a post or an issue: their numbers, how
/// they were taken, and on what.
fn case_study(result: &Value, e: &Estimate) -> String {
    let (machine, os, tools) = environment(result);
    let mut out = String::from("### CI time with hidane\n\n| | |\n|---|---|\n");
    let mut row = |key: String, value: String| {
        let _ = writeln!(out, "| {key} | {value} |");
    };
    row(
        format!("Before: the official emulator {}", e.before.version),
        format!("{} per run", duration(e.before.median_ms)),
    );
    row(
        format!("After: hidane {}", e.after.version),
        format!("{} per run", duration(e.after.median_ms)),
    );
    row("Runs per month".into(), render::thousands(e.runs_per_month));
    row("Saved per run".into(), saved_text(e, e.per_run_ms()));
    row("Saved per month".into(), saved_text(e, e.per_month_ms()));
    if let (Some(rate), Some(cost)) = (&e.rate, e.cost_per_month()) {
        row(
            "Saved cost per month".into(),
            if e.saves() {
                format!(
                    "{}, at {} {} per minute",
                    money(cost, &rate.currency),
                    rate.per_minute,
                    rate.currency
                )
            } else {
                "none".to_owned()
            },
        );
    }
    let _ = write!(
        out,
        "\nThis is our own measurement, made with `hidane benchmark` (benchmark definitions v{}) \
         on {}: {}, with {}, the emulators taking turns. The figures are medians of those runs. \
         Machine: {machine}; {os}. Tools: {tools}. Other machines and workloads give other \
         numbers. Method: [hidane's benchmark]({METHOD}).\n",
        str_of(&result["benchmark"]["version"]).unwrap_or("?"),
        date(result),
        workload(result, e.scenario, "our"),
        runs_text(result, e)
    );
    let caveats = caveats(result, e);
    if !caveats.is_empty() {
        out.push_str("\nNotes:\n\n");
        for c in caveats {
            let _ = writeln!(out, "- {c}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A result with the `command` scenario measured on both emulators.
    fn result(official: f64, hidane: f64) -> Value {
        let metric = |median: f64| {
            json!({"unit": "ms", "description": "d", "stats": {"n": 5, "median": median,
                "p95": median, "min": median, "max": median, "mean": median, "stddev": 0.0},
                "samples": [median, median, median, median, median]})
        };
        json!({
            "schema_version": 1,
            "benchmark": {"version": "1", "kind": "local", "canonical": false, "quick": false,
                "scenarios": {
                    "command": {"warmup": 1, "runs": 5, "command": "npm (1 argument)"},
                    "ci": {"warmup": 3, "runs": 10, "tests": 200, "seed_documents": 100},
                }},
            "timestamp": "2026-10-13T08:00:00Z",
            "machine": {"os": "linux", "arch": "x86_64", "cpu_model": "Example CPU",
                "cpu_cores": 4, "cpu_threads": 8, "ram_bytes": 17_179_869_184_u64,
                "distribution": "Ubuntu 24.04 LTS"},
            "environment": {"java": "21.0.8", "node": "22.22.3", "firebase_tools": null},
            "stability": {"stable": true, "reasons": []},
            "targets": {
                "official": {"version": "1.22.0", "java": "21.0.8", "java_vendor": "Eclipse Adoptium",
                    "jar_verified": true},
                "hidane": {"version": "0.4.0"},
            },
            "results": {
                "command": {"official": {"total": metric(official)}, "hidane": {"total": metric(hidane)}},
                "ci": {"official": {"total": metric(5_000.0)}, "hidane": {"total": metric(120.0)}},
            },
            "errors": [],
        })
    }

    fn usd() -> Option<Rate> {
        Some(rate(0.008, "USD").unwrap())
    }

    #[test]
    fn the_arithmetic() {
        let e = estimate(&result(6_750.792, 1_672.901), None, 600, usd()).unwrap();
        assert_eq!(e.scenario, Source::Command);
        assert!((e.per_run_ms() - 5_077.891).abs() < 1e-9);
        assert!((e.per_month_ms() - 3_046_734.6).abs() < 1e-6);
        assert!((e.per_month_minutes() - 50.778_91).abs() < 1e-9);
        assert!((e.cost_per_month().unwrap() - 0.406_231_28).abs() < 1e-9);
        let json = json_report(&result(6_750.792, 1_672.901), &e);
        assert_eq!(json["saved"]["per_run_ms"], 5_077.891);
        assert_eq!(json["saved"]["per_month_ms"], 3_046_734.6);
        assert_eq!(json["saved"]["per_month_minutes"], 50.779);
        assert_eq!(json["saved"]["per_month_hours"], 0.846);
        assert_eq!(json["cost"]["saved_per_month"], 0.406);
        assert_eq!(json["cost"]["currency"], "USD");
        assert_eq!(json["before"]["median_ms"], 6_750.792);
        assert_eq!(json["after"]["runs"], 5);
        // No rate, no cost.
        let e = estimate(&result(2_000.0, 1_000.0), None, 3, None).unwrap();
        assert_eq!(e.cost_per_month(), None);
        assert!(json_report(&result(2_000.0, 1_000.0), &e)["cost"].is_null());
    }

    #[test]
    fn durations_switch_units_where_rounding_reaches_the_next() {
        assert_eq!(duration(0.4), "0 ms");
        assert_eq!(duration(740.4), "740 ms");
        assert_eq!(duration(999.4), "999 ms");
        assert_eq!(duration(999.6), "1.00 s");
        assert_eq!(duration(5_077.891), "5.08 s");
        assert_eq!(duration(59_994.0), "59.99 s");
        assert_eq!(duration(59_996.0), "1 min 0 s");
        assert_eq!(duration(61_500.0), "1 min 2 s");
        assert_eq!(duration(3_046_734.6), "50 min 47 s");
        assert_eq!(duration(3_599_400.0), "59 min 59 s");
        assert_eq!(duration(3_599_500.0), "1 h 0 min");
        assert_eq!(duration(48_300_000.0), "13 h 25 min");
        // The sign is the caller's to say.
        assert_eq!(duration(-1_250.0), "1.25 s");
        assert_eq!(money(0.406_231, "USD"), "0.41 USD");
        assert_eq!(money(1_234.5, "JPY"), "1234.50 JPY");
    }

    #[test]
    fn the_scenario_is_the_users_command_first() {
        let r = result(2_000.0, 1_000.0);
        assert_eq!(
            estimate(&r, None, 1, None).unwrap().scenario,
            Source::Command
        );
        let e = estimate(&r, Some(Source::Ci), 1, None).unwrap();
        assert_eq!((e.before.median_ms, e.after.median_ms), (5_000.0, 120.0));
        // Without the command, the synthetic workload, and the advice to measure your own.
        let mut r = result(2_000.0, 1_000.0);
        r["results"].as_object_mut().unwrap().remove("command");
        let e = estimate(&r, None, 1, None).unwrap();
        assert_eq!(e.scenario, Source::Ci);
        assert!(advice(&r, &e)[0].contains("-- <your test command>"));
        assert!(workload(&r, e.scenario, "our").contains("not our test suite"));
        // One emulator only: refused, saying what is missing.
        r["results"]["ci"]
            .as_object_mut()
            .unwrap()
            .remove("official");
        let err = estimate(&r, None, 1, None).unwrap_err();
        assert!(err.contains("--compare-official"), "{err}");
        let err = estimate(&r, Some(Source::Ci), 1, None).unwrap_err();
        assert!(err.contains("no `ci` measurement of official"), "{err}");
    }

    #[test]
    fn rates_are_checked() {
        assert!(rate(-0.1, "USD").is_err());
        assert!(rate(f64::NAN, "USD").is_err());
        assert!(rate(0.0, "USD").is_ok());
        assert!(rate(0.01, "").is_err());
        assert!(rate(0.01, "US|D").is_err());
        assert!(rate(0.01, "a very long currency label").is_err());
        assert_eq!(rate(0.01, " EUR ").unwrap().currency, "EUR");
    }

    #[test]
    fn the_case_study_states_medians_environment_and_whose_measurement() {
        let r = result(6_750.792, 1_672.901);
        let e = estimate(&r, None, 600, usd()).unwrap();
        let md = case_study(&r, &e);
        for line in [
            "### CI time with hidane",
            "| Before: the official emulator 1.22.0 | 6.75 s per run |",
            "| After: hidane 0.4.0 | 1.67 s per run |",
            "| Runs per month | 600 |",
            "| Saved per run | 5.08 s |",
            "| Saved per month | 50 min 47 s |",
            "| Saved cost per month | 0.41 USD, at 0.008 USD per minute |",
            "This is our own measurement",
            "on 2026-10-13: our test command (`npm` with 1 argument), timed from the emulator's start to its shutdown, with 5 measured runs per emulator after 1 warm-up",
            "medians of those runs",
        ] {
            assert!(md.contains(line), "{line}\n---\n{md}");
        }
        assert!(md.contains(
            "Machine: Example CPU, 4 cores, 8 threads, 16 GiB RAM; Ubuntu 24.04 LTS, x86_64."
        ));
        assert!(md.contains("Tools: Java 21.0.8 (Eclipse Adoptium), Node.js 22.22.3."));
        assert!(md.contains("\n- The cost is the saved minutes times the given rate."));
        // The claim policy: no ratio headline.
        assert!(!md.contains('×'));
        assert!(!md.to_lowercase().contains("faster"));
        let text = text_report(&r, &e);
        assert!(text.contains("  Saved per month  50 min 47 s\n"), "{text}");
        assert!(text.contains("  Saved cost       0.41 USD per month, at 0.008 USD per minute\n"));
        assert!(text.contains("medians of your own measurement"));
        assert!(!text.contains('×'));
    }

    #[test]
    fn no_saving_and_noisy_runs_are_said_plainly() {
        let mut r = result(1_000.0, 1_250.0);
        r["stability"] =
            json!({"stable": false, "reasons": ["load average 3.10 before the run, above 2"]});
        r["benchmark"]["quick"] = json!(true);
        r["errors"] = json!([{"scenario": "command", "target": "official", "run": 2, "warmup": false, "message": "x"}]);
        r["targets"]["official"]["jar_verified"] = json!(false);
        let e = estimate(&r, None, 100, usd()).unwrap();
        assert!(!e.saves());
        let json = json_report(&r, &e);
        assert_eq!(json["saved"]["per_run_ms"], -250.0);
        assert_eq!(json["saved"]["per_month_ms"], -25_000.0);
        assert_eq!(json["source"]["stable"], false);
        let md = case_study(&r, &e);
        assert!(md.contains("| Saved per run | none |"));
        assert!(md.contains("| Saved cost per month | none |"));
        for caveat in [
            "- The run did not pass the stability checks (load average 3.10 before the run, above 2), so its numbers are noisy and not fit to cite.",
            "- The run used `--quick` parameters",
            "- 1 run of this workload failed and is not counted.",
            "- The official emulator's jar was not the pinned one",
            "- hidane's median was 250 ms longer in this measurement, so it shows no time saved.",
        ] {
            assert!(md.contains(caveat), "{caveat}\n---\n{md}");
        }
        let few = {
            let mut r = result(2_000.0, 1_000.0);
            r["results"]["command"]["official"]["total"]["stats"]["n"] = json!(2);
            r
        };
        let e = estimate(&few, None, 1, None).unwrap();
        assert!(caveats(&few, &e)[0].contains("A median of 2 runs is fragile"));
        let mut release = result(2_000.0, 1_000.0);
        release["benchmark"]["kind"] = json!("release");
        let e = estimate(&release, None, 1, None).unwrap();
        assert!(advice(&release, &e)[0].contains("your own measurement"));
    }
}
