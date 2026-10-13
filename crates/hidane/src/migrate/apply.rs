//! Writing a plan to disk: every file is checked to be the one the plan was made from, the
//! files are copied to a backup directory, and each is replaced atomically (a temporary file in
//! the same directory, then a rename). Files with uncommitted changes are refused unless the
//! caller forces it.

use std::{
    io::Write as _,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use super::Plan;
use crate::project::files::Dir;

/// Where backups go, in the project: one directory per run, named by its UTC time.
pub const BACKUP_DIR: &str = ".hidane-backup";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    /// The files written, by path.
    pub written: Vec<String>,
    /// The backup directory, relative to the project.
    pub backup: Option<String>,
    /// Whether the files were checked for uncommitted changes (false outside a git work
    /// tree, or when forced).
    pub checked_git: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// These files have uncommitted changes (`git status` lines).
    Uncommitted(Vec<String>),
    /// These files changed since the plan was made.
    Changed(Vec<String>),
    /// Writing failed; the files listed were written before it did.
    Io {
        message: String,
        written: Vec<String>,
    },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Uncommitted(lines) => write!(
                f,
                "these files have uncommitted changes, so nothing was written (commit or stash them, or pass --force):\n  {}",
                lines.join("\n  ")
            ),
            Error::Changed(files) => write!(
                f,
                "these files changed while the plan was made, so nothing was written; run hidane migrate again:\n  {}",
                files.join("\n  ")
            ),
            Error::Io { message, written } if written.is_empty() => write!(f, "{message}"),
            Error::Io { message, written } => write!(
                f,
                "{message}\nalready written (their backups are in {BACKUP_DIR}/): {}",
                written.join(", ")
            ),
        }
    }
}

/// Writes the plan's files under `root`. Unless `force`, refuses when git reports changes in
/// any of them.
pub fn apply(root: &Path, plan: &Plan, force: bool) -> Result<Applied, Error> {
    let mut applied = Applied {
        written: Vec::new(),
        backup: None,
        checked_git: false,
    };
    if plan.files.is_empty() {
        return Ok(applied);
    }
    let dir = Dir::new(root);
    let mut paths = Vec::new();
    let mut changed = Vec::new();
    for file in &plan.files {
        let Some(path) = dir.path(&file.file) else {
            changed.push(file.file.clone());
            continue;
        };
        match std::fs::read(&path) {
            Ok(bytes) if bytes == file.before.as_bytes() => paths.push(path),
            _ => changed.push(file.file.clone()),
        }
    }
    if !changed.is_empty() {
        return Err(Error::Changed(changed));
    }
    if !force {
        let names: Vec<&str> = plan.files.iter().map(|f| f.file.as_str()).collect();
        if let Some(dirty) = uncommitted(root, &names) {
            applied.checked_git = true;
            if !dirty.is_empty() {
                return Err(Error::Uncommitted(dirty));
            }
        }
    }
    let backup = backup_dir(root);
    let io = |message: String, written: &[String]| Error::Io {
        message,
        written: written.to_vec(),
    };
    for (file, path) in plan.files.iter().zip(&paths) {
        let target = backup.join(&file.file);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| io(format!("could not create {}: {e}", parent.display()), &[]))?;
        }
        std::fs::copy(path, &target)
            .map_err(|e| io(format!("could not back up {}: {e}", file.file), &[]))?;
    }
    applied.backup = backup
        .strip_prefix(root)
        .ok()
        .map(|p| p.to_string_lossy().replace('\\', "/"));
    for (file, path) in plan.files.iter().zip(&paths) {
        write_atomically(path, file.after.as_bytes()).map_err(|e| {
            io(
                format!("could not write {}: {e}", file.file),
                &applied.written,
            )
        })?;
        applied.written.push(file.file.clone());
    }
    Ok(applied)
}

/// `git status` lines for the files, or `None` when git is not there or `root` is not in a
/// work tree.
fn uncommitted(root: &Path, files: &[&str]) -> Option<Vec<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["status", "--porcelain=v1", "--untracked-files=all", "--"])
        .args(files)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect(),
    )
}

/// A new directory for this run's backups: `.hidane-backup/<UTC time>`.
fn backup_dir(root: &Path) -> PathBuf {
    let base = root.join(BACKUP_DIR).join(utc_stamp(SystemTime::now()));
    let mut dir = base.clone();
    let mut n = 2;
    while dir.exists() {
        dir = PathBuf::from(format!("{}-{n}", base.display()));
        n += 1;
    }
    dir
}

/// `YYYYMMDDTHHMMSSZ`.
fn utc_stamp(time: SystemTime) -> String {
    let secs = time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let days = (secs / 86_400) as i64;
    let rest = secs % 86_400;
    // Days to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    )
}

/// Replaces `path` with `content`: written to a temporary file in the same directory, with the
/// file's permissions, then renamed over it, so that the file is never half written.
fn write_atomically(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = dir.join(format!(".{name}.hidane-{}.tmp", std::process::id()));
    let result = (|| {
        let permissions = std::fs::metadata(path)?.permissions();
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(content)?;
        file.sync_all()?;
        drop(file);
        std::fs::set_permissions(&temp, permissions)?;
        std::fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps() {
        assert_eq!(utc_stamp(UNIX_EPOCH), "19700101T000000Z");
        // 2026-10-13T09:30:05Z
        let t = UNIX_EPOCH + std::time::Duration::from_secs(1_791_883_805);
        assert_eq!(utc_stamp(t), "20261013T093005Z");
        // 2024-02-29T23:59:59Z
        let t = UNIX_EPOCH + std::time::Duration::from_secs(1_709_251_199);
        assert_eq!(utc_stamp(t), "20240229T235959Z");
    }
}
