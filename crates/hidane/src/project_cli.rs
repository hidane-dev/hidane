//! `hidane doctor`, `hidane migrate` and `hidane report`: the command line of
//! [`hidane::project`], [`hidane::migrate`] and [`hidane::diagnostic`]. The engines do the work;
//! this parses the flags, prints the report or the plan, applies it when asked, and chooses the
//! exit code.

use std::{ffi::OsString, path::PathBuf, process::ExitCode};

use clap::{Parser, ValueEnum};
use hidane::{
    diagnostic, migrate,
    project::{self, files::Dir},
};

/// The subcommands this module runs, after `hidane`.
pub const SUBCOMMANDS: [&str; 3] = ["doctor", "migrate", "report"];

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ReportFormat {
    Markdown,
    Text,
    Json,
}

/// Writes what a bug report needs about hidane, this machine and a Firebase project, with
/// secrets and identifiers redacted. It sends nothing anywhere.
#[derive(Debug, Parser)]
#[command(
    name = "report",
    bin_name = "hidane report",
    version,
    long_about = "Writes what a maintainer needs to reproduce a bug report: hidane's version, \
                  commit, build profile and target; the OS and architecture; Java, Node.js and \
                  firebase-tools; the official emulator versions firebase-tools has cached (from \
                  file names only); the environment variables the emulators read (values only \
                  where they are safe); and `hidane doctor`'s scan of the project with its \
                  firebase.json, sanitized.\n\n\
                  Project IDs (other than demo-* ones), API keys, tokens, passwords, e-mail \
                  addresses, credentials in URLs, host and user names, and paths in the home \
                  directory and the project become placeholders such as <project-id-1> and \
                  <home>; the report counts them at its end. No document data is read. It runs \
                  `java -version` and `node --version`, reads files, and makes no network \
                  request. It prints the report, or writes it to --output: nothing leaves this \
                  machine unless you share it, so read it first.",
    after_help = "Exit codes: 0 when the report was written, 1 when --output could not be \
                  written, 2 for a usage error or a path that is not a directory.\n\nSee \
                  docs/bug-reports.md: \
                  https://github.com/hidane-dev/hidane/blob/main/docs/bug-reports.md"
)]
struct Report {
    /// The project's root directory.
    #[arg(long, value_name = "DIR", default_value = ".")]
    path: PathBuf,
    /// markdown: for a GitHub issue; text: for a terminal; json: for tools.
    #[arg(long, value_enum, default_value_t = ReportFormat::Markdown)]
    format: ReportFormat,
    /// Write the report to this file instead of stdout.
    #[arg(long, short, value_name = "FILE")]
    output: Option<PathBuf>,
}

/// Runs `hidane <subcommand> …`; `args` starts with the subcommand.
pub fn run(args: Vec<OsString>) -> ExitCode {
    match args.first().and_then(|a| a.to_str()) {
        Some("doctor") => doctor(args),
        Some("migrate") => migrate(args),
        Some("report") => report(args),
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

fn report(args: Vec<OsString>) -> ExitCode {
    let cli: Report = match parse(args) {
        Ok(cli) => cli,
        Err(code) => return code,
    };
    if !cli.path.is_dir() {
        eprintln!("hidane report: {} is not a directory", cli.path.display());
        return ExitCode::from(2);
    }
    let input = diagnostic::machine::read(&cli.path);
    let report = diagnostic::build(&input, &Dir::new(&cli.path));
    let text = match cli.format {
        ReportFormat::Markdown => diagnostic::render::markdown(&report),
        ReportFormat::Text => diagnostic::render::text(&report),
        ReportFormat::Json => match serde_json::to_string_pretty(&report) {
            Ok(json) => format!("{json}\n"),
            Err(err) => {
                eprintln!("hidane report: {err}");
                return ExitCode::FAILURE;
            }
        },
    };
    let Some(output) = &cli.output else {
        emit(&text);
        eprintln!(
            "hidane report: nothing was sent anywhere. Read the report before you share it; \
             what was redacted is counted at its end."
        );
        return ExitCode::SUCCESS;
    };
    if let Err(err) = std::fs::write(output, &text) {
        eprintln!("hidane report: could not write {}: {err}", output.display());
        return ExitCode::from(1);
    }
    let p = &report.project;
    let mut contents = vec![
        "hidane's version and build".to_owned(),
        "the OS and architecture".to_owned(),
        "the Java, Node.js and firebase-tools versions".to_owned(),
        "the official emulator versions firebase-tools has cached".to_owned(),
        format!(
            "{} environment variable{} (values only where safe)",
            report.environment.len(),
            if report.environment.len() == 1 {
                ""
            } else {
                "s"
            }
        ),
        "hidane doctor's findings for the project".to_owned(),
    ];
    if !p.firebase_json.is_empty() {
        contents.push(format!(
            "{} firebase.json, sanitized",
            p.firebase_json.len()
        ));
    }
    if !p.firebaserc.is_empty() {
        contents.push(".firebaserc's aliases, with the project IDs redacted".to_owned());
    }
    let redacted: usize = report.redacted.values().sum();
    eprintln!(
        "hidane report: wrote {} ({} bytes): {}. {redacted} value{} redacted. Nothing was sent \
         anywhere: read the file before you attach it to an issue.",
        output.display(),
        text.len(),
        contents.join("; "),
        if redacted == 1 { " was" } else { "s were" }
    );
    ExitCode::SUCCESS
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
