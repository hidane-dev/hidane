//! `hidane doctor` and `hidane migrate`: the command line of [`hidane::project`] and
//! [`hidane::migrate`]. The engines do the work; this parses the flags, prints the report or the
//! plan, applies it when asked, and chooses the exit code.

use std::{ffi::OsString, path::PathBuf, process::ExitCode};

use clap::{Parser, ValueEnum};
use hidane::{
    migrate,
    project::{self, files::Dir},
};

/// The subcommands this module runs, after `hidane`.
pub const SUBCOMMANDS: [&str; 2] = ["doctor", "migrate"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Format {
    Text,
    Json,
}

/// Checks whether a Firebase project works with hidane, without changing or running anything.
#[derive(Debug, Parser)]
#[command(
    name = "doctor",
    bin_name = "hidane doctor",
    version,
    long_about = "Checks whether a Firebase project works with hidane, without changing or \
                  running anything and without the network: firebase.json and the emulators it \
                  configures, the Firestore rules (compiled with hidane's compiler), the Firestore \
                  SDK and firebase-tools versions in package.json and lockfiles (and in Python, Go, \
                  Java and Dart manifests), Firestore-triggered functions, and the GitHub Actions \
                  workflows, npm scripts and shell scripts that start the emulators.\n\n\
                  Each finding is compatible (✓, verified with hidane, with the source), not \
                  verified (?, check it by hand) or incompatible (✗, a known gap); · marks \
                  information.",
    after_help = "Exit codes: 0 when no finding is incompatible, 1 when one is, 2 for a usage \
                  error or a path that is not a directory.\n\nSee docs/migrating.md: \
                  https://github.com/hidane-dev/hidane/blob/main/docs/migrating.md"
)]
struct Doctor {
    /// The project's root directory.
    #[arg(long, value_name = "DIR", default_value = ".")]
    path: PathBuf,
    /// text: grouped findings; json: the findings and the facts behind them.
    #[arg(long, value_enum, default_value_t = Format::Text)]
    format: Format,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum PlanFormat {
    Text,
    Json,
    Patch,
}

/// Moves a Firebase project from the official emulator to hidane: shows the changes, and with
/// --apply makes them.
#[derive(Debug, Parser)]
#[command(
    name = "migrate",
    bin_name = "hidane migrate",
    version,
    long_about = "Moves a Firebase project from the official emulator to hidane. Without \
                  --apply it changes nothing: it prints what it detected, the changes as a unified \
                  diff, and what it leaves to you.\n\n\
                  It changes only patterns it recognises: `firebase emulators:exec` and \
                  `emulators:start` in GitHub Actions `run` steps, npm scripts and the shell \
                  scripts they run gain the prefix `hidane exec --`; in a job that starts the \
                  emulators, actions/setup-java becomes hidane-dev/setup-hidane@v1 when only the \
                  Firestore emulator needed Java, and hidane-dev/setup-hidane@v1 is added \
                  otherwise. Anything else becomes a note. Running it again changes nothing.\n\n\
                  With --apply it first checks that git reports no uncommitted change in the \
                  files it would change (--force skips that), copies each of them to \
                  .hidane-backup/<UTC time>/ in the project, then writes each atomically: a \
                  temporary file in the same directory, renamed over the file.",
    after_help = "Exit codes: 0 when the plan was printed or applied (or there was nothing to \
                  change), 1 when --apply refused (uncommitted changes, files changed meanwhile) \
                  or failed, 2 for a usage error or a path that is not a directory.\n\nSee \
                  docs/migrating.md: \
                  https://github.com/hidane-dev/hidane/blob/main/docs/migrating.md"
)]
struct Migrate {
    /// The project's root directory.
    #[arg(long, value_name = "DIR", default_value = ".")]
    path: PathBuf,
    /// Make the changes (backups in .hidane-backup/<UTC time>/).
    #[arg(long)]
    apply: bool,
    /// With --apply: write even files with uncommitted changes.
    #[arg(long, requires = "apply")]
    force: bool,
    /// text: detected items, changes, notes and the diff; json: the plan, for tools; patch:
    /// only the diff, for `git apply`.
    #[arg(long, value_enum, default_value_t = PlanFormat::Text)]
    format: PlanFormat,
}

/// Runs `hidane <subcommand> …`; `args` starts with the subcommand.
pub fn run(args: Vec<OsString>) -> ExitCode {
    match args.first().and_then(|a| a.to_str()) {
        Some("doctor") => doctor(args),
        Some("migrate") => migrate(args),
        _ => ExitCode::from(2),
    }
}

fn parse<T: Parser>(args: Vec<OsString>) -> Result<T, ExitCode> {
    T::try_parse_from(args).map_err(|err| {
        let _ = err.print();
        ExitCode::from(u8::try_from(err.exit_code()).unwrap_or(2))
    })
}

fn doctor(args: Vec<OsString>) -> ExitCode {
    let cli: Doctor = match parse(args) {
        Ok(cli) => cli,
        Err(code) => return code,
    };
    if !cli.path.is_dir() {
        eprintln!("hidane doctor: {} is not a directory", cli.path.display());
        return ExitCode::from(2);
    }
    let report = project::scan(&Dir::new(&cli.path));
    match cli.format {
        Format::Text => emit(&project::report::text(
            &report,
            &cli.path.display().to_string(),
        )),
        Format::Json => match serde_json::to_string_pretty(&report) {
            Ok(json) => emit(&format!("{json}\n")),
            Err(err) => {
                eprintln!("hidane doctor: {err}");
                return ExitCode::FAILURE;
            }
        },
    }
    if report.has_incompatibility() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn migrate(args: Vec<OsString>) -> ExitCode {
    let cli: Migrate = match parse(args) {
        Ok(cli) => cli,
        Err(code) => return code,
    };
    if !cli.path.is_dir() {
        eprintln!("hidane migrate: {} is not a directory", cli.path.display());
        return ExitCode::from(2);
    }
    let tree = Dir::new(&cli.path);
    let plan = migrate::plan(&tree);
    let shown = cli.path.display().to_string();
    let applied = if cli.apply && !plan.is_empty() {
        Some(migrate::apply::apply(&cli.path, &plan, cli.force))
    } else {
        None
    };
    match cli.format {
        PlanFormat::Text => {
            emit(&migrate::text(&plan, &shown));
            if !plan.is_empty() && !cli.apply {
                let path = if shown == "." {
                    String::new()
                } else {
                    format!(" --path {}", quote(&shown))
                };
                emit(&format!(
                    "\nDry run: nothing was changed. To apply: hidane migrate{path} --apply\n"
                ));
            }
        }
        PlanFormat::Patch => emit(&plan.patch),
        PlanFormat::Json => {
            let mut value = match serde_json::to_value(&plan) {
                Ok(value) => value,
                Err(err) => {
                    eprintln!("hidane migrate: {err}");
                    return ExitCode::FAILURE;
                }
            };
            if let (Some(Ok(applied)), Some(object)) = (&applied, value.as_object_mut()) {
                object.insert(
                    "applied".into(),
                    serde_json::json!({
                        "written": applied.written,
                        "backup": applied.backup,
                        "checked_git": applied.checked_git,
                    }),
                );
            }
            match serde_json::to_string_pretty(&value) {
                Ok(json) => emit(&format!("{json}\n")),
                Err(err) => {
                    eprintln!("hidane migrate: {err}");
                    return ExitCode::FAILURE;
                }
            }
        }
    }
    match applied {
        None => ExitCode::SUCCESS,
        Some(Err(err)) => {
            eprintln!("hidane migrate: {err}");
            ExitCode::from(1)
        }
        Some(Ok(applied)) => {
            if let Some(backup) = &applied.backup {
                eprintln!(
                    "Backed up {} to {backup}/",
                    if applied.written.len() == 1 {
                        "1 file".to_owned()
                    } else {
                        format!("{} files", applied.written.len())
                    }
                );
            }
            for file in &applied.written {
                eprintln!("Wrote {file}");
            }
            if !applied.checked_git && !cli.force {
                eprintln!(
                    "Not in a git work tree: the files were not checked for uncommitted changes."
                );
            }
            // A second run must find nothing to change.
            let again = migrate::plan(&tree);
            if !again.is_empty() {
                let files: Vec<&str> = again.files.iter().map(|f| f.file.as_str()).collect();
                eprintln!(
                    "WARNING: a second run would still change {}; please report it \
                     (https://github.com/hidane-dev/hidane/issues).",
                    files.join(", ")
                );
            }
            ExitCode::SUCCESS
        }
    }
}

/// A path as a shell word.
fn quote(path: &str) -> String {
    if path
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-".contains(c))
    {
        path.to_owned()
    } else {
        format!("'{}'", path.replace('\'', r"'\''"))
    }
}

/// Writes to stdout and ends quietly when the reader has gone (`hidane doctor | head`).
fn emit(text: &str) {
    use std::io::Write as _;
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(text.as_bytes()).and_then(|()| out.flush());
}
