//! `hidane migrate` on the projects in `tests/fixtures/projects/`: its patch for each matches
//! the golden file next to the project (`<project>.migrate.patch`, empty when nothing changes),
//! `--apply` writes it with backups and refuses files with uncommitted changes, and a second
//! run changes nothing. `HIDANE_BLESS=1 cargo test -p hidane --test migrate` rewrites the golden
//! files after an intended change.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

const BIN: &str = env!("CARGO_BIN_EXE_hidane");

const PROJECTS: [&str; 7] = [
    "already-migrated",
    "node-setup-java",
    "node-storage-auth",
    "npm-scripts",
    "polyglot",
    "rules-broken",
    "unusual-workflow",
];

fn projects() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/projects")
}

fn migrate(args: &[&str]) -> (i32, String, String) {
    let output = Command::new(BIN)
        .arg("migrate")
        .args(args)
        .output()
        .unwrap();
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// A copy of a fixture project in a temporary directory.
fn copy(name: &str, label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "hidane-migrate-{}-{name}-{label}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    copy_dir(&projects().join(name), &dir);
    dir
}

fn git(dir: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=hidane",
            "-c",
            "user.email=hidane@example.com",
        ])
        .args(args)
        .output()
        .is_ok_and(|o| o.status.success())
}

fn has_git() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Every file under `dir`, relative, with its content, outside `skip`.
fn files(dir: &Path, skip: &[&str]) -> Vec<(String, Vec<u8>)> {
    fn walk(root: &Path, dir: &Path, skip: &[&str], out: &mut Vec<(String, Vec<u8>)>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().into_string().unwrap();
            if skip.contains(&name.as_str()) {
                continue;
            }
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                walk(root, &path, skip, out);
            } else {
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((rel, std::fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, skip, &mut out);
    out.sort();
    out
}

#[test]
fn patches_match_the_golden_files() {
    let bless = std::env::var_os("HIDANE_BLESS").is_some();
    let mut mismatches = Vec::new();
    for name in PROJECTS {
        let dir = projects().join(name);
        let (code, patch, err) = migrate(&["--path", dir.to_str().unwrap(), "--format", "patch"]);
        assert_eq!(code, 0, "{name}: {err}");
        let golden = projects().join(format!("{name}.migrate.patch"));
        if bless {
            std::fs::write(&golden, &patch).unwrap();
            continue;
        }
        let expected = std::fs::read_to_string(&golden).unwrap_or_default();
        if patch != expected {
            mismatches.push(format!("{name}:\n{patch}"));
        }
    }
    assert!(
        mismatches.is_empty(),
        "patches differ from their golden files (HIDANE_BLESS=1 rewrites them):\n{}",
        mismatches.join("\n")
    );
}

#[test]
fn nothing_to_change_where_already_migrated_or_unread() {
    for name in ["already-migrated", "unusual-workflow"] {
        let dir = projects().join(name);
        let (code, out, _) = migrate(&["--path", dir.to_str().unwrap()]);
        assert_eq!(code, 0);
        assert!(out.contains("\nNothing to change.\n"), "{name}: {out}");
        assert!(!out.contains("diff --git"), "{name}: {out}");
    }
    let dir = projects().join("unusual-workflow");
    let (_, out, _) = migrate(&["--path", dir.to_str().unwrap()]);
    assert_eq!(out.matches("manual change suggested").count(), 5, "{out}");
}

#[test]
fn the_dry_run_changes_nothing_and_says_how_to_apply() {
    let dir = copy("npm-scripts", "dry");
    let before = files(&dir, &[]);
    let (code, out, _) = migrate(&["--path", dir.to_str().unwrap()]);
    assert_eq!(code, 0);
    assert!(out.contains("\nSuggested changes (3 files)\n"), "{out}");
    assert!(
        out.contains("Dry run: nothing was changed. To apply: hidane migrate --path "),
        "{out}"
    );
    assert_eq!(files(&dir, &[]), before);
    let (code, json, _) = migrate(&["--path", dir.to_str().unwrap(), "--format", "json"]);
    assert_eq!(code, 0);
    let plan: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(plan["schema"], "hidane.migrate/1");
    assert_eq!(plan["files"].as_array().unwrap().len(), 3);
    assert_eq!(plan["files"][1]["file"], "package.json");
    assert_eq!(plan["files"][1]["changes"][0]["code"], "command.prefix");
    // The git blob id of the file the plan was made from.
    let original = std::fs::read_to_string(dir.join("package.json")).unwrap();
    assert_eq!(
        plan["files"][1]["blob"],
        hidane::migrate::blob_id(&original)
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn apply_writes_with_backups_and_a_second_run_changes_nothing() {
    for name in PROJECTS {
        let dir = copy(name, "apply");
        let pristine = files(&dir, &[]);
        #[cfg(unix)]
        let script = dir.join("scripts/e2e.sh");
        #[cfg(unix)]
        if script.exists() {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let (code, out, err) = migrate(&["--path", dir.to_str().unwrap(), "--apply"]);
        assert_eq!(code, 0, "{name}: {out}{err}");
        let golden =
            std::fs::read_to_string(projects().join(format!("{name}.migrate.patch"))).unwrap();
        if golden.is_empty() {
            assert!(!dir.join(".hidane-backup").exists(), "{name}");
            assert_eq!(files(&dir, &[]), pristine, "{name}");
            continue;
        }
        assert!(!err.contains("WARNING"), "{name}: {err}");
        // Backups of exactly the files written, as they were.
        let backups = std::fs::read_dir(dir.join(".hidane-backup"))
            .unwrap()
            .count();
        assert_eq!(backups, 1, "{name}");
        let backup_dir = std::fs::read_dir(dir.join(".hidane-backup"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        for (file, content) in files(&backup_dir, &[]) {
            let original = pristine.iter().find(|(f, _)| *f == file).unwrap();
            assert_eq!(&original.1, &content, "{name}: backup of {file}");
            assert!(err.contains(&format!("Wrote {file}\n")), "{name}: {err}");
        }
        // No temporary file is left.
        assert!(
            files(&dir, &[".hidane-backup"])
                .iter()
                .all(|(f, _)| !f.contains(".hidane-")),
            "{name}"
        );
        #[cfg(unix)]
        if script.exists() {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&script).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o755, "{name}: the script stays executable");
        }
        // The same as the golden patch applied by git.
        if has_git() {
            let other = copy(name, "git-apply");
            let patch = other.join("migrate.patch");
            std::fs::write(&patch, &golden).unwrap();
            assert!(
                git(&other, &["apply", "migrate.patch"]),
                "{name}: git apply"
            );
            std::fs::remove_file(&patch).unwrap();
            assert_eq!(
                files(&other, &[]),
                files(&dir, &[".hidane-backup"]),
                "{name}"
            );
            let _ = std::fs::remove_dir_all(other);
        }
        // Idempotent.
        let (code, patch, _) = migrate(&["--path", dir.to_str().unwrap(), "--format", "patch"]);
        assert_eq!((code, patch.as_str()), (0, ""), "{name}");
        let (code, out, _) = migrate(&["--path", dir.to_str().unwrap(), "--apply"]);
        assert_eq!(code, 0);
        assert!(out.contains("Nothing to change."), "{name}: {out}");
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn apply_refuses_uncommitted_changes_unless_forced() {
    if !has_git() {
        return;
    }
    let dir = copy("node-setup-java", "dirty");
    assert!(git(&dir, &["init", "-q"]));
    assert!(git(&dir, &["add", "-A"]));
    assert!(git(&dir, &["commit", "-q", "-m", "fixture"]));
    let workflow = dir.join(".github/workflows/test.yml");
    let edited = format!(
        "{}# a local change\n",
        std::fs::read_to_string(&workflow).unwrap()
    );
    std::fs::write(&workflow, &edited).unwrap();

    let (code, _, err) = migrate(&["--path", dir.to_str().unwrap(), "--apply"]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("uncommitted changes"), "{err}");
    assert!(err.contains(".github/workflows/test.yml"), "{err}");
    assert_eq!(std::fs::read_to_string(&workflow).unwrap(), edited);
    assert!(!dir.join(".hidane-backup").exists());

    let (code, _, err) = migrate(&["--path", dir.to_str().unwrap(), "--apply", "--force"]);
    assert_eq!(code, 0, "{err}");
    let written = std::fs::read_to_string(&workflow).unwrap();
    assert!(
        written.contains("uses: hidane-dev/setup-hidane@v1\n"),
        "{written}"
    );
    assert!(written.ends_with("# a local change\n"), "{written}");

    // Committed, a clean tree is not refused, and there is nothing left to change.
    assert!(git(&dir, &["add", "-A"]));
    assert!(git(&dir, &["commit", "-q", "-m", "migrated"]));
    let (code, out, _) = migrate(&["--path", dir.to_str().unwrap(), "--apply"]);
    assert_eq!(code, 0);
    assert!(out.contains("Nothing to change."), "{out}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn usage() {
    let (code, out, _) = migrate(&["--help"]);
    assert_eq!(code, 0);
    assert!(out.contains("Usage: hidane migrate [OPTIONS]"), "{out}");
    assert!(out.contains(".hidane-backup/<UTC time>/"), "{out}");
    assert!(
        out.contains("Exit codes: 0 when the plan was printed or applied"),
        "{out}"
    );
    let (code, _, _) = migrate(&["--force"]);
    assert_eq!(code, 2, "--force needs --apply");
    let (code, _, _) = migrate(&["--path", "/nonexistent/hidane-migrate"]);
    assert_eq!(code, 2);
}
