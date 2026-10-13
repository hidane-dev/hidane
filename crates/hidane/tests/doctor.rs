//! `hidane doctor` on the projects in `tests/fixtures/projects/`: its JSON report on each
//! matches the golden file next to the project (`<project>.doctor.json`), and its exit code says
//! whether a finding is incompatible. `HIDANE_BLESS=1 cargo test -p hidane --test doctor`
//! rewrites the golden files after an intended change.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

const BIN: &str = env!("CARGO_BIN_EXE_hidane");

/// The projects and the exit code `hidane doctor` gives for each.
const PROJECTS: [(&str, i32); 7] = [
    ("already-migrated", 0),
    ("node-setup-java", 0),
    ("node-storage-auth", 0),
    ("npm-scripts", 0),
    ("polyglot", 1),
    ("rules-broken", 1),
    ("unusual-workflow", 0),
];

fn projects() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/projects")
}

fn doctor(args: &[&str]) -> (i32, String, String) {
    let output = Command::new(BIN).arg("doctor").args(args).output().unwrap();
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

/// The report without the hidane version, which changes with every release.
fn normalized(json: &str) -> String {
    let version = format!("\"hidane\": \"{}\"", env!("CARGO_PKG_VERSION"));
    json.replacen(&version, "\"hidane\": \"<version>\"", 1)
}

#[test]
fn json_reports_match_the_golden_files() {
    let bless = std::env::var_os("HIDANE_BLESS").is_some();
    let mut mismatches = Vec::new();
    for (name, expected_code) in PROJECTS {
        let dir = projects().join(name);
        let (code, out, err) = doctor(&["--path", dir.to_str().unwrap(), "--format", "json"]);
        assert_eq!(code, expected_code, "{name}: {err}");
        let report = normalized(&out);
        let golden = projects().join(format!("{name}.doctor.json"));
        if bless {
            std::fs::write(&golden, &report).unwrap();
            continue;
        }
        let expected = std::fs::read_to_string(&golden).unwrap_or_default();
        if report != expected {
            mismatches.push(format!("{name}:\n{report}"));
        }
    }
    assert!(
        mismatches.is_empty(),
        "reports differ from their golden files (HIDANE_BLESS=1 rewrites them):\n{}",
        mismatches.join("\n")
    );
}

#[test]
fn every_project_has_a_golden_file() {
    let mut names: Vec<String> = std::fs::read_dir(projects())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.file_name().into_string().unwrap())
        .collect();
    names.sort();
    let listed: Vec<&str> = PROJECTS.iter().map(|(name, _)| *name).collect();
    assert_eq!(names, listed);
}

#[test]
fn the_text_report() {
    let dir = projects().join("rules-broken");
    let (code, out, _) = doctor(&["--path", dir.to_str().unwrap()]);
    assert_eq!(code, 1);
    assert!(out.contains("\nSecurity Rules\n"), "{out}");
    assert!(
        out.contains("  ✗ firestore.rules does not compile: hidane would start without these rules\n      error: L5:19 Missing 'if' before conditional expression.\n"),
        "{out}"
    );
    assert!(out.contains("Exit code 1"), "{out}");

    let dir = projects().join("node-storage-auth");
    let (code, out, _) = doctor(&["--path", dir.to_str().unwrap(), "--format", "text"]);
    assert_eq!(code, 0);
    assert!(out.contains("  ✓ firebase.json found\n"), "{out}");
    assert!(
        out.contains("  ? Firebase JS SDK (firebase) ^12.4.0 (package.json): not verified with hidane — manual verification required\n"),
        "{out}"
    );
    assert!(
        out.contains("keep actions/setup-java: the storage emulator still needs Java"),
        "{out}"
    );
}

#[test]
fn usage() {
    let (code, out, _) = doctor(&["--help"]);
    assert_eq!(code, 0);
    assert!(out.contains("Usage: hidane doctor [OPTIONS]"), "{out}");
    assert!(
        out.contains("Exit codes: 0 when no finding is incompatible"),
        "{out}"
    );
    let (code, _, err) = doctor(&["--path", "/nonexistent/hidane-doctor"]);
    assert_eq!(code, 2, "{err}");
    let (code, _, err) = doctor(&["--format", "xml"]);
    assert_eq!(code, 2, "{err}");
}
