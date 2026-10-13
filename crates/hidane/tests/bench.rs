//! `hidane benchmark` (bench/README.md), run as users and the harness run it: its JSON result
//! matches `bench/schema/result.schema.json`, the Markdown report is rendered from that JSON,
//! and nothing in a result points at the machine's user, paths or network.
//!
//! These runs measure hidane only, with tiny parameters. The official emulator joins when
//! `HIDANE_BENCH_OFFICIAL_JAR` names its jar, with the Java 21 or newer on `PATH` or the one
//! `HIDANE_BENCH_JAVA` names (CI has neither, so that test returns early there).

use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
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
    let schema: Value = serde_json::from_str(
        &std::fs::read_to_string(repo("bench/schema/result.schema.json")).unwrap(),
    )
    .unwrap();
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
