//! `hidane benchmark` (bench/README.md), run as users and the harness run it: its JSON result
//! matches `bench/schema/result.schema.json`, the Markdown report is rendered from that JSON,
//! and nothing in a result points at the machine's user, paths or network.
//!
//! These runs measure hidane only, with tiny parameters. The official emulator joins when
//! `HIDANE_BENCH_OFFICIAL_JAR` names its jar, with the Java 21 or newer on `PATH` or the one
//! `HIDANE_BENCH_JAVA` names (CI has neither, so that test returns early there).

use std::{
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_hidane");
const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

fn repo(path: &str) -> PathBuf {
    Path::new(ROOT).join(path)
}

/// A scratch directory for one test.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("hidane-bench-test-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Small parameters, so a full run of every scenario takes seconds.
const TINY: &str = r#"{
  "scenarios": {
    "startup": {"warmup": 1, "runs": 3},
    "memory": {"warmup": 0, "runs": 2, "idle_ms": 100, "documents": 200, "batch": 100},
    "operations": {"warmup": 1, "iterations": 3, "batch": 50, "seed_documents": 20},
    "datasets": {"batch": 100, "query_warmup": 1, "query_runs": 2,
                 "sizes": [{"documents": 100, "warmup": 0, "runs": 2}]},
    "ci": {"warmup": 0, "runs": 2, "tests": 3, "seed_documents": 10},
    "listen_writes": {"warmup": 0, "runs": 2, "documents": 2000, "batch": 500},
    "command": {"warmup": 0, "runs": 2}
  }
}"#;

fn benchmark(args: &[&str], dir: &Path) -> Output {
    Command::new(BIN)
        .arg("benchmark")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run hidane benchmark")
}

fn validate(result: &Value) {
    validate_with("bench/schema/result.schema.json", result);
}

fn validate_with(schema: &str, result: &Value) {
    let schema: Value =
        serde_json::from_str(&std::fs::read_to_string(repo(schema)).unwrap()).unwrap();
    let validator = jsonschema::validator_for(&schema).expect("the schema compiles");
    let errors: Vec<String> = validator
        .iter_errors(result)
        .map(|e| format!("{} at {}", e, e.instance_path()))
        .collect();
    assert!(
        errors.is_empty(),
        "schema violations:\n{}",
        errors.join("\n")
    );
}

fn strings(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(s) => out.push(s.clone()),
        Value::Array(items) => items.iter().for_each(|v| strings(v, out)),
        Value::Object(map) => {
            for (k, v) in map {
                out.push(k.clone());
                strings(v, out);
            }
        }
        _ => {}
    }
}

/// No absolute path, home directory, user name or hostname anywhere in a result.
fn assert_private(result: &Value, dir: &Path) {
    let mut all = Vec::new();
    strings(result, &mut all);
    let mut forbidden: Vec<String> = vec![dir.to_string_lossy().into_owned()];
    for var in ["HOME", "USER", "LOGNAME"] {
        if let Ok(v) = std::env::var(var)
            && v.len() >= 3
        {
            forbidden.push(v);
        }
    }
    // Long enough not to match words of the result by chance.
    if let Ok(out) = Command::new("hostname").output() {
        let host = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        if host.len() >= 6 {
            forbidden.push(host);
        }
    }
    for s in &all {
        assert!(!s.starts_with('/'), "absolute path in the result: {s}");
        for f in &forbidden {
            assert!(!s.contains(f.as_str()), "{s:?} contains {f:?}");
        }
    }
}

#[test]
fn a_hidane_run_matches_the_schema_and_renders_from_its_json() {
    let dir = scratch("run");
    std::fs::write(dir.join("tiny.json"), TINY).unwrap();
    std::fs::write(
        dir.join("environment.json"),
        r#"{"environment_revision": 7, "machine": {"instance_type": "c7i.xlarge",
            "ebs": {"type": "gp3", "size_gib": 30}, "os_image": "ubuntu-24.04"}}"#,
    )
    .unwrap();
    let out = benchmark(
        &[
            "--config",
            "tiny.json",
            "--kind",
            "pull-request",
            "--machine-id",
            "ci-test",
            "--environment-file",
            "environment.json",
            "--hidane-commit",
            "0123456789abcdef0123456789abcdef01234567",
            "--engine-commit",
            "0123456789abcdef0123456789abcdef01234567",
            "--no-imds",
            "--out",
            "result.json",
        ],
        &dir,
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(out.stdout.is_empty(), "--out keeps stdout empty");
    let text = std::fs::read_to_string(dir.join("result.json")).unwrap();
    let result: Value = serde_json::from_str(&text).unwrap();
    validate(&result);
    assert_private(&result, &dir);

    assert_eq!(result["benchmark"]["kind"], "pull-request");
    assert_eq!(result["benchmark"]["canonical"], false);
    assert_eq!(result["benchmark"]["skipped"][0]["scenario"], "ci_firebase");
    assert_eq!(result["environment_revision"], 7);
    assert_eq!(result["machine"]["id"], "ci-test");
    assert_eq!(result["machine"]["instance_type"], "c7i.xlarge");
    assert_eq!(result["machine"]["ebs"]["size_gib"], 30);
    assert!(result["targets"]["official"].is_null());
    assert_eq!(
        result["targets"]["hidane"]["git_commit"],
        "0123456789abcdef0123456789abcdef01234567"
    );
    assert_eq!(result["errors"], serde_json::json!([]));
    let r = &result["results"];
    let n = |scenario: &str, metric: &str| r[scenario]["hidane"][metric]["stats"]["n"].as_u64();
    assert_eq!(n("startup", "time_to_port"), Some(3));
    assert_eq!(n("startup", "time_to_first_request"), Some(3));
    assert_eq!(n("memory", "rss_idle"), Some(2));
    assert_eq!(n("memory", "rss_peak"), Some(2));
    assert_eq!(n("operations", "transaction"), Some(3));
    assert_eq!(n("datasets", "seed_100"), Some(2));
    assert_eq!(n("datasets", "query_100"), Some(4));
    assert_eq!(n("ci", "total"), Some(2));
    for metric in [
        "writes",
        "delivered",
        "batch_first_decile",
        "batch_last_decile",
        "batch_growth",
        "rss_after",
    ] {
        assert_eq!(n("listen_writes", metric), Some(2), "{metric}");
    }
    let time_to_port = r["startup"]["hidane"]["time_to_port"]["stats"]["median"]
        .as_f64()
        .unwrap();
    let first = r["startup"]["hidane"]["time_to_first_request"]["stats"]["median"]
        .as_f64()
        .unwrap();
    assert!(
        time_to_port > 0.0 && first >= time_to_port,
        "{time_to_port} {first}"
    );
    assert!(
        r["memory"]["hidane"]["rss_idle"]["stats"]["min"]
            .as_f64()
            .unwrap()
            > 1e6
    );
    let checks = result["stability"]["checks"].as_array().unwrap();
    assert!(checks.iter().any(|c| c["name"] == "load_1m"));

    let rendered = Command::new(BIN)
        .args(["benchmark", "render"])
        .arg(dir.join("result.json"))
        .output()
        .unwrap();
    assert!(rendered.status.success());
    let md = String::from_utf8(rendered.stdout).unwrap();
    let environment = md.find("## Environment").unwrap();
    let summary = md.find("## Summary").unwrap();
    assert!(environment < summary, "the environment comes first");
    assert!(md.contains("| Run | pull-request, benchmark definitions v1"));
    assert!(md.contains("environment revision 7"));
    assert!(md.contains(
        "In this benchmark environment, median time from process start until the port \
         accepted connections was "
    ));
    assert!(md.contains(" for hidane."));
    assert!(md.contains("## Writes with a listener attached"));
    assert!(md.contains(
        "median time to write 2,000 documents in commits of 500 with a listener on the \
         collection was "
    ));
    assert!(md.contains("## Skipped"));
    // `--format markdown` is the same rendering of the same JSON.
    let direct = benchmark(&["--render", "result.json"], &dir);
    assert_eq!(String::from_utf8(direct.stdout).unwrap(), md);
    let _ = std::fs::remove_dir_all(dir);
}

/// The command mode, with the built-in workload as the command.
#[test]
fn the_command_runs_against_the_target() {
    let dir = scratch("command");
    std::fs::write(dir.join("tiny.json"), TINY).unwrap();
    let out = benchmark(
        &[
            "--config",
            "tiny.json",
            "--no-imds",
            "--format",
            "json",
            "--",
            BIN,
            "benchmark",
            "workload",
            "--tests",
            "2",
            "--seed",
            "3",
        ],
        &dir,
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    let result: Value = serde_json::from_slice(&out.stdout).unwrap();
    validate(&result);
    assert_private(&result, &dir);
    assert_eq!(
        result["benchmark"]["scenarios"]["command"]["command"],
        "hidane (6 arguments)"
    );
    let r = &result["results"]["command"]["hidane"];
    assert_eq!(r["total"]["stats"]["n"], 2);
    assert!(r["command"]["stats"]["median"].as_f64().unwrap() > 0.0);
    // Only the command scenario runs with a command.
    assert!(result["results"]["startup"].is_null());

    // A failing command is recorded as an error, and the exit code says so.
    let out = benchmark(
        &[
            "--config",
            "tiny.json",
            "--no-imds",
            "--",
            BIN,
            "benchmark",
            "workload",
            "--tests",
            "many",
        ],
        &dir,
    );
    assert_eq!(out.status.code(), Some(1));
    let result: Value = serde_json::from_slice(&out.stdout).unwrap();
    validate(&result);
    assert_eq!(result["errors"].as_array().unwrap().len(), 2);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_example_result_validates_and_renders_as_committed() {
    let path = repo("bench/fixtures/example-result.json");
    let result: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    validate(&result);
    let rendered = Command::new(BIN)
        .args(["benchmark", "render"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(rendered.status.success());
    let expected = std::fs::read_to_string(repo("bench/fixtures/example-result.md")).unwrap();
    assert!(
        String::from_utf8(rendered.stdout).unwrap() == expected,
        "bench/fixtures/example-result.md is stale; regenerate it with \
         `cargo run -p hidane -- benchmark render bench/fixtures/example-result.json > bench/fixtures/example-result.md`"
    );
}

/// `hidane benchmark savings`: a result's difference as CI time saved, read from a file or from
/// stdin, as JSON matching `bench/schema/savings.schema.json` and as the Markdown case study
/// that bench/README.md shows.
#[test]
fn savings_turn_a_result_into_ci_time() {
    let dir = scratch("savings");
    // The synthetic example result, with medians that differ for the command scenario.
    let mut result: Value = serde_json::from_str(
        &std::fs::read_to_string(repo("bench/fixtures/example-result.json")).unwrap(),
    )
    .unwrap();
    let command = &mut result["results"]["command"];
    command["official"]["total"]["stats"]["median"] = 61_500.0.into();
    command["hidane"]["total"]["stats"]["median"] = 21_250.0.into();
    result["benchmark"]["scenarios"]["command"]["command"] = "npm (1 argument)".into();
    let text = result.to_string();
    std::fs::write(dir.join("result.json"), &text).unwrap();
    let rate = ["--cost-per-minute", "0.01", "--currency", "USD"];

    let mut args = vec!["savings", "result.json", "--runs-per-month", "1200"];
    args.extend(rate);
    args.extend(["--format", "json"]);
    let out = benchmark(&args, &dir);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let savings: Value = serde_json::from_slice(&out.stdout).unwrap();
    validate_with("bench/schema/savings.schema.json", &savings);
    assert_private(&savings, &dir);
    assert_eq!(savings["source"]["scenario"], "command");
    assert_eq!(savings["saved"]["per_run_ms"], 40_250.0);
    assert_eq!(savings["saved"]["per_month_ms"], 48_300_000.0);
    assert_eq!(savings["saved"]["per_month_minutes"], 805.0);
    assert_eq!(savings["saved"]["per_month_hours"], 13.417);
    assert_eq!(savings["cost"]["saved_per_month"], 8.05);
    // The example result is a nightly one: the note says a case study needs your own.
    assert!(String::from_utf8_lossy(&out.stderr).contains("your own measurement"));

    let mut child = Command::new(BIN)
        .args(["benchmark", "savings", "-", "--runs-per-month", "1200"])
        .args(rate)
        .args(["--format", "markdown"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let md = String::from_utf8(out.stdout).unwrap();
    assert!(md.starts_with("### CI time with hidane\n"));
    assert!(!md.contains('×'));
    let readme = std::fs::read_to_string(repo("bench/README.md")).unwrap();
    assert!(
        readme.contains(&format!("```markdown\n{md}```")),
        "bench/README.md's case-study example is stale; it should read:\n{md}"
    );

    // A rate needs its currency; a result needs both emulators.
    let out = benchmark(
        &[
            "savings",
            "result.json",
            "--runs-per-month",
            "3",
            "--cost-per-minute",
            "1",
        ],
        &dir,
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--currency"));
    for scenario in ["command", "ci", "ci_firebase"] {
        result["results"][scenario]
            .as_object_mut()
            .unwrap()
            .remove("official");
    }
    std::fs::write(dir.join("hidane-only.json"), result.to_string()).unwrap();
    let out = benchmark(
        &["savings", "hidane-only.json", "--runs-per-month", "3"],
        &dir,
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--compare-official"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn operator_files_and_flags_are_checked() {
    let dir = scratch("refuse");
    std::fs::write(
        dir.join("environment.json"),
        r#"{"environment_revision": 1, "machine": {"hostname": "build-01"}}"#,
    )
    .unwrap();
    let out = benchmark(&["--environment-file", "environment.json"], &dir);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown key \"hostname\""));

    let out = benchmark(&["--target", "official"], &dir);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--official-jar"));

    std::fs::write(
        dir.join("bad.json"),
        r#"{"scenarios": {"startup": {"run": 3}}}"#,
    )
    .unwrap();
    let out = benchmark(&["--config", "bad.json"], &dir);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown key \"run\""));
    let _ = std::fs::remove_dir_all(dir);
}

/// #49 and #30: batch writes with a listener on the whole collection do not slow down as
/// documents accumulate (firebase-tools#3477, docs/why.md §3). The listen_writes scenario writes
/// 100,000 documents in commits of 500 while a Listen stream on the collection takes every
/// change. The checks compare the run with itself, not with milliseconds, so a slow runner or a
/// debug build passes them: the median commit time of the last tenth of the batches stays within
/// twice the first tenth's, and the growth over the whole run (Theil–Sen, per 10,000 documents
/// stored, times 10) stays under one early batch's time. hidane's batches are flat (a ratio near 1, a
/// growth near 0); a commit whose cost grows with the documents stored, as the official
/// emulator's does (about +168 ms per 10,000 documents in Phase 0), fails them.
#[test]
fn writes_with_a_listener_attached_stay_flat() {
    let dir = scratch("listen");
    std::fs::write(
        dir.join("listen.json"),
        r#"{"scenarios": {"listen_writes": {"warmup": 0, "runs": 1, "documents": 100000, "batch": 500}}}"#,
    )
    .unwrap();
    let out = benchmark(
        &[
            "--config",
            "listen.json",
            "--scenarios",
            "listen_writes",
            "--no-imds",
            "--out",
            "result.json",
        ],
        &dir,
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let result: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("result.json")).unwrap()).unwrap();
    validate(&result);
    let r = &result["results"]["listen_writes"]["hidane"];
    let median = |metric: &str| r[metric]["stats"]["median"].as_f64().unwrap();
    let (first, last, growth) = (
        median("batch_first_decile"),
        median("batch_last_decile"),
        median("batch_growth"),
    );
    assert!(
        last <= 2.0 * first,
        "the last tenth of the batches took {last} ms (median), the first {first} ms"
    );
    assert!(
        growth * 10.0 < first,
        "batches grew by {growth} ms per 10,000 documents stored; the first took {first} ms"
    );
    assert!(median("delivered") > 0.0);
    let _ = std::fs::remove_dir_all(dir);
}

/// Both targets, when the official emulator is available (`HIDANE_BENCH_OFFICIAL_JAR`).
#[test]
fn side_by_side_with_the_official_emulator() {
    let Some(jar) = std::env::var_os("HIDANE_BENCH_OFFICIAL_JAR") else {
        eprintln!("HIDANE_BENCH_OFFICIAL_JAR is not set; skipping the official emulator");
        return;
    };
    let dir = scratch("official");
    std::fs::write(dir.join("tiny.json"), TINY).unwrap();
    let jar = std::path::absolute(PathBuf::from(jar)).unwrap();
    let mut args = vec![
        "--config",
        "tiny.json",
        "--compare-official",
        "--official-jar",
        jar.to_str().unwrap(),
        "--scenarios",
        "startup,operations,ci",
        "--no-imds",
        "--out",
        "result.json",
    ];
    // HIDANE_BENCH_JAVA: a Java 21+ other than the one on PATH.
    let java = std::env::var("HIDANE_BENCH_JAVA").ok();
    if let Some(java) = &java {
        args.extend(["--java", java]);
    }
    let out = benchmark(&args, &dir);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let result: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("result.json")).unwrap()).unwrap();
    validate(&result);
    assert_private(&result, &dir);
    assert_eq!(result["targets"]["official"]["version"], "1.22.0");
    assert_eq!(result["targets"]["official"]["jar_verified"], true);
    for target in ["official", "hidane"] {
        assert_eq!(
            result["results"]["startup"][target]["time_to_port"]["stats"]["n"], 3,
            "{target}"
        );
        assert_eq!(
            result["results"]["ci"][target]["total"]["stats"]["n"], 2,
            "{target}"
        );
    }
    let _ = std::fs::remove_dir_all(dir);
}
