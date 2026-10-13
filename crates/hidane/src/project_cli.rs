//! `hidane doctor`: the command line of [`hidane::project`]. The engine does the work; this
//! parses the flags, prints the report and chooses the exit code.

use std::{ffi::OsString, path::PathBuf, process::ExitCode};

use clap::{Parser, ValueEnum};
use hidane::project::{self, files::Dir};

/// The subcommands this module runs, after `hidane`.
pub const SUBCOMMANDS: [&str; 1] = ["doctor"];

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

/// Runs `hidane <subcommand> …`; `args` starts with the subcommand.
pub fn run(args: Vec<OsString>) -> ExitCode {
    match args.first().and_then(|a| a.to_str()) {
        Some("doctor") => doctor(args),
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

/// Writes to stdout and ends quietly when the reader has gone (`hidane doctor | head`).
fn emit(text: &str) {
    use std::io::Write as _;
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(text.as_bytes()).and_then(|()| out.flush());
}
