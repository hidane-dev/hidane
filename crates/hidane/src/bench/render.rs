//! Markdown from a result's JSON, and from nothing else: the same function renders a run that
//! just finished and a file saved years ago (`hidane benchmark render <file.json>`).
//!
//! The environment comes first, then the numbers. Comparisons are stated as medians, in this
//! benchmark environment, with no "N× faster" headline (bench/README.md, "Claim policy").

use std::fmt::Write as _;

use serde_json::Value;

/// Targets in the order the text names them.
const TARGETS: &[(&str, &str)] = &[("official", "the official emulator"), ("hidane", "hidane")];

const SCENARIOS: &[(&str, &str)] = &[
    ("startup", "Startup"),
    ("memory", "Memory and CPU time"),
    ("operations", "Operations"),
    ("datasets", "Dataset sizes"),
    ("ci", "CI workload"),
    ("ci_firebase", "CI workload through firebase-tools"),
    ("listen_writes", "Writes with a listener attached"),
    ("owner_writes", "Writes with the owner credential"),
    ("command", "Command"),
];

fn str_of(value: &Value) -> Option<&str> {
    value.as_str().filter(|s| !s.is_empty())
}

/// `ms` and `bytes` values, with three significant digits or so.
pub fn format_value(value: f64, unit: &str) -> String {
    match unit {
        "ms" if value >= 10_000.0 => format!("{:.2} s", value / 1000.0),
        "ms" if value >= 100.0 => format!("{value:.0} ms"),
        "ms" if value >= 10.0 => format!("{value:.1} ms"),
        "ms" => format!("{value:.2} ms"),
        "bytes" => {
            let mib = value / (1024.0 * 1024.0);
            if mib >= 10_240.0 {
                format!("{:.2} GiB", mib / 1024.0)
            } else if mib >= 100.0 {
                format!("{mib:.0} MiB")
            } else {
                format!("{mib:.1} MiB")
            }
        }
        _ => format!("{value} {unit}"),
    }
}

pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn short(hash: &Value) -> String {
    str_of(hash).map_or_else(|| "unknown".into(), |h| h.chars().take(12).collect())
}

/// The summary sentence of a metric, when the result has it for at least one target.
fn sentence(result: &Value, scenario: &str, metric: &str, what: &str) -> Option<String> {
    let per_target: Vec<(String, &str)> = TARGETS
        .iter()
        .filter_map(|(id, name)| {
            let m = &result["results"][scenario][id][metric];
            let median = m["stats"]["median"].as_f64()?;
            Some((format_value(median, m["unit"].as_str()?), *name))
        })
        .collect();
    let values = match per_target.as_slice() {
        [] => return None,
        [(v, name)] => format!("{v} for {name}"),
        [(a, first), (b, second)] => format!("{a} for {first} and {b} for {second}"),
        _ => return None,
    };
    Some(format!(
        "In this benchmark environment, median {what} was {values}."
    ))
}

fn summary(result: &Value) -> Vec<String> {
    let scenarios = &result["benchmark"]["scenarios"];
    let documents = scenarios["memory"]["documents"].as_u64().unwrap_or(1_000);
    let tests = scenarios["ci"]["tests"].as_u64().unwrap_or(0);
    let batch = scenarios["operations"]["batch"].as_u64().unwrap_or(500);
    let largest = scenarios["datasets"]["sizes"]
        .as_array()
        .and_then(|sizes| sizes.iter().filter_map(|s| s["documents"].as_u64()).max());
    let seed_batch = scenarios["datasets"]["batch"].as_u64().unwrap_or(500);
    let mut lines = vec![
        sentence(
            result,
            "startup",
            "time_to_port",
            "time from process start until the port accepted connections",
        ),
        sentence(
            result,
            "startup",
            "time_to_first_request",
            "time from process start until the first gRPC request was answered",
        ),
        sentence(result, "memory", "rss_idle", "resident memory when idle"),
        sentence(
            result,
            "memory",
            "rss_after_workload",
            &format!(
                "resident memory after writing {} documents",
                thousands(documents)
            ),
        ),
        sentence(
            result,
            "operations",
            "single_read",
            "latency of a single-document read over gRPC",
        ),
        sentence(
            result,
            "operations",
            "batch_write",
            &format!("latency of a {}-write batch commit", thousands(batch)),
        ),
    ];
    if let Some(n) = largest {
        lines.push(sentence(
            result,
            "datasets",
            &format!("seed_{n}"),
            &format!(
                "time to write {} documents in commits of {seed_batch}",
                thousands(n)
            ),
        ));
    }
    lines.push(sentence(
        result,
        "ci",
        "total",
        &format!(
            "wall-clock time of the synthetic CI workload (start, ready, seed, {} tests, shutdown)",
            thousands(tests)
        ),
    ));
    lines.push(sentence(
        result,
        "ci_firebase",
        "total",
        &format!(
            "wall-clock time of `firebase emulators:exec` around the same workload ({} tests)",
            thousands(scenarios["ci_firebase"]["tests"].as_u64().unwrap_or(0))
        ),
    ));
    let listen = &scenarios["listen_writes"];
    let listened = format!(
        "{} documents in commits of {} with a listener on the collection",
        thousands(listen["documents"].as_u64().unwrap_or(0)),
        listen["batch"].as_u64().unwrap_or(0)
    );
    lines.push(sentence(
        result,
        "listen_writes",
        "writes",
        &format!("time to write {listened}"),
    ));
    lines.push(sentence(
        result,
        "listen_writes",
        "batch_growth",
        &format!(
            "growth of a commit's time per 10,000 documents already stored, writing {listened},"
        ),
    ));
    let owner = &scenarios["owner_writes"];
    let owner_batch = owner["batch"].as_u64().unwrap_or(0);
    lines.push(sentence(
        result,
        "owner_writes",
        "batch_write",
        &format!(
            "latency of a {}-write batch commit with the owner credential, as the Admin SDKs send it,",
            thousands(owner_batch)
        ),
    ));
    if let Some(n) = owner["documents"].as_u64() {
        lines.push(sentence(
            result,
            "owner_writes",
            &format!("seed_{n}"),
            &format!(
                "time to write {} documents in commits of {owner_batch} with the owner credential",
                thousands(n)
            ),
        ));
    }
    lines.push(sentence(
        result,
        "command",
        "total",
        "wall-clock time of the command from emulator start to shutdown",
    ));
    lines.into_iter().flatten().collect()
}

fn environment(result: &Value, out: &mut String) {
    let machine = &result["machine"];
    let env = &result["environment"];
    let bench = &result["benchmark"];
    let stability = &result["stability"];
    let row = |out: &mut String, key: &str, value: String| {
        let _ = writeln!(out, "| {key} | {value} |");
    };
    out.push_str("## Environment\n\n| | |\n|---|---|\n");
    row(
        out,
        "Date",
        str_of(&result["timestamp"]).unwrap_or("unknown").to_owned(),
    );
    let mut run = format!(
        "{}, benchmark definitions v{}",
        str_of(&bench["kind"]).unwrap_or("unknown"),
        str_of(&bench["version"]).unwrap_or("?")
    );
    if bench["canonical"] == true {
        run.push_str(", canonical parameters");
    } else if bench["quick"] == true {
        run.push_str(", quick parameters (not canonical)");
    } else {
        run.push_str(", non-canonical parameters");
    }
    if let Some(revision) = result["environment_revision"].as_i64() {
        let _ = write!(run, ", environment revision {revision}");
    }
    row(out, "Run", run);

    let mut hw = Vec::new();
    if let Some(id) = str_of(&machine["id"]) {
        hw.push(format!("`{id}`"));
    }
    if let Some(cpu) = str_of(&machine["cpu_model"]) {
        hw.push(cpu.to_owned());
    }
    match (
        machine["cpu_cores"].as_u64(),
        machine["cpu_threads"].as_u64(),
    ) {
        (Some(c), Some(t)) => hw.push(format!("{c} cores, {t} threads")),
        (None, Some(t)) => hw.push(format!("{t} threads")),
        _ => {}
    }
    if let Some(ram) = machine["ram_bytes"].as_f64() {
        hw.push(format!("{:.0} GiB RAM", ram / (1u64 << 30) as f64));
    }
    row(out, "Machine", hw.join(", "));

    let mut cloud = Vec::new();
    let detected = &machine["cloud"];
    match (
        str_of(&detected["instance_type"]),
        str_of(&machine["instance_type"]),
    ) {
        (Some(d), Some(g)) if d == g => cloud.push(format!("instance type {d}")),
        (Some(d), Some(g)) => cloud.push(format!("instance type {g} as given, {d} detected")),
        (Some(d), None) => cloud.push(format!("instance type {d}")),
        (None, Some(g)) => cloud.push(format!("instance type {g}")),
        (None, None) => {}
    }
    if let Some(zone) = str_of(&detected["availability_zone"]) {
        cloud.push(format!("zone {zone}"));
    }
    if let Some(v) = str_of(&machine["os_image"]) {
        cloud.push(format!("image {v}"));
    }
    if let Some(v) = str_of(&machine["ami_id"]) {
        cloud.push(format!("AMI {v}"));
    }
    let ebs = &machine["ebs"];
    if ebs.is_object() {
        let mut disk = format!("EBS {}", str_of(&ebs["type"]).unwrap_or("volume"));
        if let Some(size) = ebs["size_gib"].as_u64() {
            let _ = write!(disk, " {size} GiB");
        }
        cloud.push(disk);
    }
    if !cloud.is_empty() {
        row(out, "Cloud", cloud.join(", "));
    }

    let mut os = Vec::new();
    for key in ["distribution", "kernel", "arch"] {
        if let Some(v) = str_of(&machine[key]) {
            os.push(if key == "kernel" {
                format!("kernel {v}")
            } else {
                v.to_owned()
            });
        }
    }
    if let Some(fs) = str_of(&machine["filesystem"]) {
        os.push(format!("{fs} filesystem"));
    }
    if let Some(v) = str_of(&machine["virtualization"]) {
        os.push(format!("virtualization: {v}"));
    }
    row(out, "OS", os.join(", "));
    let freq = &machine["cpu_frequency"];
    let freq_text = match (str_of(&freq["driver"]), str_of(&freq["governor"])) {
        (None, None) => str_of(&freq["note"]).unwrap_or("not recorded").to_owned(),
        (driver, governor) => format!(
            "driver {}, governor {}{}",
            driver.unwrap_or("unknown"),
            governor.unwrap_or("unknown"),
            match freq["no_turbo"].as_bool() {
                Some(true) => ", turbo off",
                Some(false) => ", turbo on",
                None => "",
            }
        ),
    };
    row(out, "CPU frequency", freq_text);

    let hidane = &result["targets"]["hidane"];
    if hidane.is_object() {
        let mut text = str_of(&hidane["version"]).unwrap_or("unknown").to_owned();
        if let Some(commit) = str_of(&hidane["git_commit"]) {
            let _ = write!(text, ", commit `{}`", &commit[..commit.len().min(12)]);
            if hidane["git_dirty"] == true {
                text.push_str(" with uncommitted changes");
            }
        }
        if let Some(profile) = str_of(&hidane["build_profile"]) {
            let _ = write!(text, ", {profile} build");
        }
        let _ = write!(
            text,
            ", binary SHA-256 `{}…`",
            short(&hidane["binary_sha256"])
        );
        row(out, "hidane", text);
    }
    let engine = &bench["engine"];
    if engine.is_object() {
        let mut text = format!(
            "`hidane benchmark` {}",
            str_of(&engine["version"]).unwrap_or("unknown")
        );
        if let Some(commit) = str_of(&engine["git_commit"]) {
            let _ = write!(text, ", commit `{}`", &commit[..commit.len().min(12)]);
        }
        if let Some(profile) = str_of(&engine["build_profile"]) {
            let _ = write!(text, ", {profile} build");
        }
        row(out, "Engine", text);
    }
    let official = &result["targets"]["official"];
    if official.is_object() {
        let mut text = format!(
            "{}, jar SHA-256 `{}…`{}",
            str_of(&official["version"]).unwrap_or("unknown"),
            short(&official["jar_sha256"]),
            if official["jar_verified"] == true {
                " (the pinned jar)"
            } else {
                " (not the pinned jar)"
            }
        );
        if let Some(java) = str_of(&official["java"]) {
            let _ = write!(text, ", Java {java}");
            if let Some(vendor) = str_of(&official["java_vendor"]) {
                let _ = write!(text, " ({vendor})");
            }
        }
        if let Some(options) = official["java_options"].as_array() {
            let options: Vec<&str> = options.iter().filter_map(Value::as_str).collect();
            if !options.is_empty() {
                let _ = write!(text, ", `{}`", options.join(" "));
            }
        }
        row(out, "Official emulator", text);
    }
    let mut tools = Vec::new();
    for (key, name) in [
        ("node", "Node.js"),
        ("firebase_tools", "firebase-tools"),
        ("docker", "Docker"),
        ("rustc", "rustc"),
    ] {
        if let Some(v) = str_of(&env[key]) {
            tools.push(format!("{name} {v}"));
        }
    }
    if !tools.is_empty() {
        row(out, "Tools", tools.join(", "));
    }

    let mut verdict = if stability["stable"] == true {
        "stable: every check passed".to_owned()
    } else {
        let reasons: Vec<&str> = stability["reasons"]
            .as_array()
            .map(|r| r.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        format!("**not stable**: {}", reasons.join("; "))
    };
    let before = &stability["before"];
    if let Some(load) = before["load_average"].as_array() {
        let load: Vec<String> = load
            .iter()
            .filter_map(Value::as_f64)
            .map(|l| format!("{l:.2}"))
            .collect();
        let _ = write!(
            verdict,
            ". Load average before the run {}",
            load.join(" / ")
        );
    }
    if let Some(after) = stability["after"]["load_average"][0].as_f64() {
        let _ = write!(verdict, ", {after:.2} after");
    }
    if let Some(mem) = before["available_memory_bytes"].as_f64() {
        let _ = write!(
            verdict,
            "; {:.1} GiB memory available",
            mem / (1u64 << 30) as f64
        );
    }
    if let Some(steal) = stability["cpu_steal"]["run_percent"].as_f64() {
        let _ = write!(verdict, "; CPU steal {steal:.2} % over the run");
    }
    if let Some(t) = before["cpu_temperature_c"].as_f64() {
        let _ = write!(verdict, "; CPU {t:.0} °C");
    }
    row(out, "Stability", verdict);
    out.push('\n');
}

fn scenario_line(scenario: &str, p: &Value) -> String {
    let n = |key: &str| p[key].as_u64().unwrap_or(0);
    match scenario {
        "startup" => format!(
            "A fresh process per run: {} warm-up and {} measured runs per target, targets alternating.",
            n("warmup"),
            n("runs")
        ),
        "memory" => format!(
            "A fresh process per run ({} warm-up, {} measured per target): {} ms idle once the port accepts, then {} documents written in commits of {}.",
            n("warmup"),
            n("runs"),
            n("idle_ms"),
            thousands(n("documents")),
            n("batch")
        ),
        "operations" => format!(
            "One process per target, {} documents seeded; each operation runs {} times, the first {} discarded as warm-up.",
            n("seed_documents"),
            n("warmup") + n("iterations"),
            n("warmup")
        ),
        "datasets" => {
            let sizes: Vec<String> = p["sizes"]
                .as_array()
                .map(|s| {
                    s.iter()
                        .map(|s| {
                            format!(
                                "{} documents: {} warm-up + {} runs",
                                thousands(s["documents"].as_u64().unwrap_or(0)),
                                s["warmup"].as_u64().unwrap_or(0),
                                s["runs"].as_u64().unwrap_or(0)
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            format!(
                "A fresh process per run, documents written in commits of {}; {}. Larger sizes run fewer times.",
                n("batch"),
                sizes.join("; ")
            )
        }
        "ci" => format!(
            "A fresh process per run ({} warm-up, {} measured per target): start, wait for the port, seed {} documents, {} tests, POST /shutdown.",
            n("warmup"),
            n("runs"),
            n("seed_documents"),
            thousands(n("tests"))
        ),
        "ci_firebase" => format!(
            "`firebase emulators:exec --only firestore` per run ({} warm-up, {} measured per target), around `hidane benchmark workload` (seed {} documents, {} tests). firebase-tools starts the official emulator from the same jar and Java; hidane runs it through `hidane exec`.",
            n("warmup"),
            n("runs"),
            n("seed_documents"),
            thousands(n("tests"))
        ),
        "listen_writes" => format!(
            "A fresh process per run ({} warm-up, {} measured per target): a Listen stream on the whole collection over its own connection, read as it arrives, then {} documents written in commits of {}, one at a time, over another; the run ends when the listener's snapshot holds every document. Without a listener, the same writes are `seed_{}` under Dataset sizes when that size is measured.",
            n("warmup"),
            n("runs"),
            thousands(n("documents")),
            n("batch"),
            n("documents")
        ),
        "owner_writes" => format!(
            "A fresh process per run ({} warm-up, {} measured per target), its requests sent with `authorization: Bearer owner`, as the Admin SDKs send them to an emulator, so Security Rules do not apply; the other scenarios send no credential, as a signed-out app. Each run commits {} writes to the same documents {} times, the first {} discarded as warm-up, then writes {} documents in commits of {}.",
            n("warmup"),
            n("runs"),
            n("batch"),
            n("commit_warmup") + n("commits"),
            n("commit_warmup"),
            thousands(n("documents")),
            n("batch")
        ),
        "command" => format!(
            "A fresh process per run ({} warm-up, {} measured per target); the command runs with FIRESTORE_EMULATOR_HOST pointing at it.",
            n("warmup"),
            n("runs")
        ),
        _ => String::new(),
    }
}

fn scenario_table(result: &Value, scenario: &str, out: &mut String) {
    let data = &result["results"][scenario];
    // Metrics in the order they appear, across targets.
    let mut metrics: Vec<&str> = Vec::new();
    for (target, _) in TARGETS {
        if let Some(map) = data[target].as_object() {
            for key in map.keys() {
                if !metrics.contains(&key.as_str()) {
                    metrics.push(key);
                }
            }
        }
    }
    if metrics.is_empty() {
        return;
    }
    out.push_str("| Metric | Target | Median | p95 | Min | Max | Mean | Std dev | n |\n");
    out.push_str("|---|---|---:|---:|---:|---:|---:|---:|---:|\n");
    for metric in &metrics {
        for (target, _) in TARGETS {
            let m = &data[target][metric];
            let (Some(unit), Some(stats)) = (m["unit"].as_str(), m["stats"].as_object()) else {
                continue;
            };
            let f = |key: &str| {
                stats
                    .get(key)
                    .and_then(Value::as_f64)
                    .map_or_else(|| "–".into(), |v| format_value(v, unit))
            };
            let _ = writeln!(
                out,
                "| `{metric}` | {} | {} | {} | {} | {} | {} | {} | {} |",
                if *target == "official" {
                    "official"
                } else {
                    "hidane"
                },
                f("median"),
                f("p95"),
                f("min"),
                f("max"),
                f("mean"),
                f("stddev"),
                stats.get("n").and_then(Value::as_u64).unwrap_or(0)
            );
        }
    }
    out.push('\n');
    let mut seen = Vec::new();
    for metric in &metrics {
        for (target, _) in TARGETS {
            if let Some(description) = data[target][metric]["description"].as_str()
                && !seen.contains(metric)
            {
                seen.push(*metric);
                let _ = writeln!(out, "- `{metric}`: {description}");
            }
        }
    }
    out.push('\n');
}

/// The whole Markdown report.
pub fn markdown(result: &Value) -> String {
    let mut out = String::new();
    let has = |id: &str| result["targets"][id].is_object();
    let title = match (has("official"), has("hidane")) {
        (true, true) => "Benchmark: the official Firestore emulator and hidane, side by side",
        (true, false) => "Benchmark: the official Firestore emulator",
        _ => "Benchmark: hidane",
    };
    let _ = writeln!(out, "# {title}\n");
    if result["stability"]["stable"] != true {
        out.push_str(
            "> **This run is not stable.** The stability checks failed (see Stability below), so \
             its numbers are noisy and must not be cited.\n\n",
        );
    }
    if result["benchmark"]["canonical"] != true {
        out.push_str(
            "> These parameters are not the canonical ones (bench/canonical.json), so the \
             numbers are not comparable with canonical results.\n\n",
        );
    }
    environment(result, &mut out);

    let lines = summary(result);
    if !lines.is_empty() {
        out.push_str("## Summary\n\n");
        for line in lines {
            let _ = writeln!(out, "- {line}");
        }
        out.push_str(
            "\nMedians of the measured runs; every statistic and sample is in the JSON result.\n\n",
        );
    }

    for (scenario, heading) in SCENARIOS {
        if !result["results"][scenario].is_object() {
            continue;
        }
        let _ = writeln!(out, "## {heading}\n");
        let line = scenario_line(scenario, &result["benchmark"]["scenarios"][scenario]);
        if !line.is_empty() {
            let _ = writeln!(out, "{line}\n");
        }
        scenario_table(result, scenario, &mut out);
    }

    if let Some(skipped) = result["benchmark"]["skipped"].as_array()
        && !skipped.is_empty()
    {
        out.push_str("## Skipped\n\n");
        for s in skipped {
            let _ = writeln!(
                out,
                "- {}: {}",
                s["scenario"].as_str().unwrap_or("?"),
                s["reason"].as_str().unwrap_or("")
            );
        }
        out.push('\n');
    }

    if let Some(errors) = result["errors"].as_array()
        && !errors.is_empty()
    {
        out.push_str("## Errors\n\n");
        for e in errors {
            let _ = writeln!(
                out,
                "- {} / {} run {}{}: {}",
                e["scenario"].as_str().unwrap_or("?"),
                e["target"].as_str().unwrap_or("?"),
                e["run"].as_u64().unwrap_or(0),
                if e["warmup"] == true {
                    " (warm-up)"
                } else {
                    ""
                },
                e["message"].as_str().unwrap_or("")
            );
        }
        out.push('\n');
    }

    let _ = writeln!(
        out,
        "Rendered by `hidane benchmark render` from the JSON result (schema version {}). \
         Methodology and claim policy: `bench/README.md`.",
        result["schema_version"].as_u64().unwrap_or(0)
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn values_are_formatted_by_unit() {
        assert_eq!(format_value(0.4234, "ms"), "0.42 ms");
        assert_eq!(format_value(5.53, "ms"), "5.53 ms");
        assert_eq!(format_value(42.25, "ms"), "42.2 ms");
        assert_eq!(format_value(740.5, "ms"), "740 ms");
        assert_eq!(format_value(12_345.0, "ms"), "12.35 s");
        assert_eq!(format_value(7.4 * 1024.0 * 1024.0, "bytes"), "7.4 MiB");
        assert_eq!(format_value(456.0 * 1024.0 * 1024.0, "bytes"), "456 MiB");
        assert_eq!(thousands(100_000), "100,000");
        assert_eq!(thousands(500), "500");
    }

    fn result() -> Value {
        let metric = |median: f64| {
            json!({"unit": "ms", "description": "d", "stats": {"n": 2, "median": median,
                "p95": median, "min": median, "max": median, "mean": median, "stddev": 0.0},
                "samples": [median, median]})
        };
        json!({
            "schema_version": 1,
            "benchmark": {"version": "1", "kind": "local", "canonical": false, "quick": true,
                "scenarios": {"startup": {"warmup": 0, "runs": 2}}},
            "timestamp": "2026-10-13T00:00:00Z",
            "environment_revision": null,
            "machine": {"id": "m1", "cpu_model": "CPU", "cpu_cores": 4, "cpu_threads": 8},
            "environment": {},
            "stability": {"stable": false, "reasons": ["load average 9.00 before the run, above 2"]},
            "targets": {"official": {"version": "1.22.0"}, "hidane": {"version": "0.3.0"}},
            "results": {"startup": {
                "official": {"time_to_port": metric(740.5)},
                "hidane": {"time_to_port": metric(5.5)},
            }},
        })
    }

    #[test]
    fn environment_comes_before_the_numbers_and_medians_are_stated_plainly() {
        let md = markdown(&result());
        let env = md.find("## Environment").unwrap();
        let summary = md.find("## Summary").unwrap();
        assert!(env < summary);
        assert!(md.contains(
            "In this benchmark environment, median time from process start until the port \
             accepted connections was 740 ms for the official emulator and 5.50 ms for hidane."
        ));
        assert!(md.contains("**This run is not stable.**"));
        assert!(md.contains("load average 9.00 before the run, above 2"));
        assert!(md.contains("| `time_to_port` | official | 740 ms |"));
        assert!(!md.contains("faster"));
        assert!(!md.contains('×'));
    }

    #[test]
    fn one_target_and_missing_fields() {
        let mut r = result();
        r["targets"].as_object_mut().unwrap().remove("official");
        r["results"]["startup"]
            .as_object_mut()
            .unwrap()
            .remove("official");
        r["stability"] = json!({"stable": true, "reasons": []});
        let md = markdown(&r);
        assert!(md.contains("# Benchmark: hidane"));
        assert!(md.contains("was 5.50 ms for hidane."));
        assert!(!md.contains("not stable"));
        // A result without most fields still renders.
        assert!(markdown(&json!({})).contains("## Environment"));
    }
}
