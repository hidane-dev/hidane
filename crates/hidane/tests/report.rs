//! `hidane report` on a project made for each test in a temporary directory, with an
//! environment that names a user, a machine, projects, hosts and secrets: none of them may
//! appear in any format of the report. The project is written at run time rather than kept as a
//! fixture, so that no secret scanner takes its fake secrets for leaks.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

const BIN: &str = env!("CARGO_BIN_EXE_hidane");

/// A fake secret: `prefix` and `n` more characters.
fn fake(prefix: &str, n: usize) -> String {
    let tail: String = "Qx7k2Lm9Zp4Rt8Vw3Ny6".chars().cycle().take(n).collect();
    format!("{prefix}{tail}")
}

/// A scratch directory with a home, a project in it, firebase-tools' emulator cache and an
/// empty `PATH` directory; removed when dropped.
struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("hidane-report-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let scratch = Self { dir };
        let token = fake("1//0g", 40);
        scratch.write(
            "home/alice/work/acme-app/firebase.json",
            r#"{
              // set up by alice@example.com
              "firestore": {"rules": "firestore.rules"},
              "hosting": {"site": "acme-site-4821", "public": "public"},
              "emulators": {"firestore": {"host": "10.20.30.40", "port": 8081}, "ui": {"enabled": false}}
            }"#,
        );
        scratch.write(
            "home/alice/work/acme-app/.firebaserc",
            r#"{"projects": {"default": "acme-prod-4821"}}"#,
        );
        scratch.write(
            "home/alice/work/acme-app/package.json",
            r#"{"devDependencies": {"firebase-tools": "15.33.0"}}"#,
        );
        scratch.write(
            "home/alice/work/acme-app/.github/workflows/ci.yml",
            &format!(
                "on: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - run: firebase emulators:exec --project acme-prod-4821 --token {token} \"npm test\"\n"
            ),
        );
        scratch.write(
            "home/alice/work/acme-app/firestore.rules",
            "rules_version = '2';\nservice cloud.firestore {\n  match /databases/{database}/documents {\n    match /{document=**} {\n      allow read, write: if false;\n    }\n  }\n}\n",
        );
        // Empty files with the names firebase-tools gives the emulators it downloads.
        for name in [
            "cloud-firestore-emulator-v1.22.0.jar",
            "cloud-firestore-emulator-v1.19.7.jar",
            "ui-v1.15.0.zip",
        ] {
            scratch.write(&format!("home/alice/.cache/firebase/emulators/{name}"), "");
        }
        std::fs::create_dir_all(scratch.path("bin")).unwrap();
        scratch
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.dir.join(relative)
    }

    fn write(&self, relative: &str, text: &str) {
        let path = self.path(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn project(&self) -> PathBuf {
        self.path("home/alice/work/acme-app")
    }

    /// `hidane report` with only the environment given here.
    fn report(&self, args: &[&str]) -> (i32, String, String) {
        let output = Command::new(BIN)
            .arg("report")
            .args(args)
            .env_clear()
            .env("HOME", self.path("home/alice"))
            .env("USER", "alice")
            .env("PATH", self.path("bin"))
            .env("HOSTNAME", "alices-laptop")
            .env("GCLOUD_PROJECT", "acme-prod-4821")
            .env("FIREBASE_TOKEN", fake("1//0g", 40))
            .env("FIRESTORE_EMULATOR_HOST", "10.20.30.40:8081")
            .env("GITHUB_ACTIONS", "true")
            .current_dir(self.project())
            .output()
            .unwrap();
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8(output.stdout).unwrap(),
            String::from_utf8(output.stderr).unwrap(),
        )
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Asserts that nothing of the user, the machine, the projects, the hosts, the secrets or the
/// scratch directory is in `out`.
fn assert_clean(out: &str) {
    let lower = out.to_ascii_lowercase();
    for canary in [
        "alice",
        "acme-prod-4821",
        "acme-site-4821",
        "10.20.30.40",
        "example.com",
        "hidane-report-test",
        &fake("1//0g", 40).to_ascii_lowercase(),
    ] {
        assert!(!lower.contains(canary), "{canary} leaked into:\n{out}");
    }
}

#[cfg(unix)]
#[test]
fn every_format_is_redacted() {
    let scratch = Scratch::new("formats");
    for format in ["markdown", "text", "json"] {
        let (code, out, err) = scratch.report(&["--format", format]);
        assert_eq!(code, 0, "{err}");
        assert_clean(&out);
        assert!(err.contains("nothing was sent anywhere"), "{err}");
        assert!(out.contains("<project-id-1>"), "{format}:\n{out}");
        assert!(out.contains("<host>:8081"), "{format}:\n{out}");
    }
    let (_, md, _) = scratch.report(&[]);
    for expected in [
        "### hidane report",
        "| Java | not found on PATH |",
        "| firebase-tools | not on PATH |",
        "| CI | GitHub Actions |",
        "| Cached official emulator | v1.19.7, v1.22.0, in `<home>/.cache/firebase/emulators` |",
        "| Other cached emulators | ui 1.15.0 |",
        "| `FIREBASE_TOKEN` | `<set>` |",
        "| `GCLOUD_PROJECT` | `<project-id-1>` |",
        "hidane doctor 0.4.0 — <project>",
        "--project <project-id-1> --token <redacted>",
        "\"site\": \"<project-id-2>\"",
        "`.firebaserc`: `default` → `<project-id-1>`",
    ] {
        let expected = expected.replace("0.4.0", env!("CARGO_PKG_VERSION"));
        assert!(md.contains(&expected), "missing {expected}:\n{md}");
    }
    let (_, json, _) = scratch.report(&["--format", "json"]);
    let report: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(report["schema"], "hidane.report/1");
    assert_eq!(report["hidane"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(report["project"]["path"], "<project>");
    assert_eq!(report["project"]["doctor"]["schema"], "hidane.doctor/1");
    assert_eq!(
        report["official_emulator_cache"]["firestore"],
        serde_json::json!(["1.19.7", "1.22.0"])
    );
    assert!(report["redacted"]["project-id"].as_u64().unwrap() > 0);
}

#[cfg(unix)]
#[test]
fn output_writes_a_file_and_says_what_it_holds() {
    let scratch = Scratch::new("output");
    let file = scratch.path("out/report.md");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    let (code, out, err) = scratch.report(&["--output", file.to_str().unwrap()]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out, "");
    assert!(err.contains("wrote "), "{err}");
    assert!(err.contains("1 firebase.json, sanitized"), "{err}");
    assert!(
        err.contains("Nothing was sent anywhere: read the file before you attach it to an issue."),
        "{err}"
    );
    let written = std::fs::read_to_string(&file).unwrap();
    assert_clean(&written);
    assert!(written.starts_with("### hidane report\n"), "{written}");

    let missing = scratch.path("no/such/dir/report.md");
    let (code, _, err) = scratch.report(&["-o", missing.to_str().unwrap()]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("could not write"), "{err}");
}

#[test]
fn usage() {
    let run = |args: &[&str]| {
        let output = Command::new(BIN).arg("report").args(args).output().unwrap();
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8(output.stdout).unwrap(),
        )
    };
    let (code, out) = run(&["--help"]);
    assert_eq!(code, 0);
    assert!(out.contains("Usage: hidane report [OPTIONS]"), "{out}");
    assert!(out.contains("makes no network"), "{out}");
    assert_eq!(run(&["--path", "/nonexistent/hidane-report"]).0, 2);
    assert_eq!(run(&["--format", "xml"]).0, 2);
    let (code, _) = run(&[
        "--path",
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("Cargo.toml")
            .to_str()
            .unwrap(),
    ]);
    assert_eq!(code, 2);
}
