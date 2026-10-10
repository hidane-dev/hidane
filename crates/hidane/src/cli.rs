//! Command-line interface compatible with the official emulator.
//!
//! Flag names, help texts and defaults are copied from `cloud-firestore-emulator --help`
//! (v1.22.0), because firebase-tools and existing scripts pass them verbatim. The official
//! emulator parses flags with JCommander, which prints `--help` in a specific layout; [`usage`]
//! reproduces that layout from the clap definitions so the two never drift apart.
//!
//! Boolean flags accept an optional value (`--single_project_mode` or
//! `--single_project_mode true`): firebase-tools passes the two-argument form.

use std::{fmt::Write as _, path::PathBuf};

use clap::{ArgAction, CommandFactory, Parser, ValueEnum, builder::BoolishValueParser};

const ISSUES: &str = "https://github.com/hidane-dev/hidane/issues";

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum DatabaseEdition {
    #[value(name = "STANDARD")]
    Standard,
    #[value(name = "ENTERPRISE")]
    Enterprise,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum DatabaseMode {
    #[value(name = "CLOUD_DATASTORE")]
    CloudDatastore,
    #[value(name = "CLOUD_FIRESTORE_IN_DATASTORE_MODE", alias = "datastore-mode")]
    CloudFirestoreInDatastoreMode,
    #[value(name = "CLOUD_FIRESTORE", alias = "firestore-native")]
    CloudFirestore,
}

/// Display name used in the startup banner (`Database Edition: STANDARD`).
pub fn value_name<T: ValueEnum>(value: &T) -> String {
    value
        .to_possible_value()
        .map(|v| v.get_name().to_owned())
        .unwrap_or_default()
}

#[derive(Debug, Parser)]
#[command(
    name = "hidane",
    disable_help_flag = true,
    disable_version_flag = true,
    // Reported in clap errors only; `--help` is rendered by `usage()`.
    override_usage = "hidane [options] <ignored>"
)]
pub struct Cli {
    #[arg(
        long = "database-edition",
        value_enum,
        ignore_case = true,
        default_value = "STANDARD",
        help = "The 'edition' of the Firestore database. Supported values are `standard` and `enterprise`"
    )]
    pub database_edition: DatabaseEdition,

    #[arg(
        long = "database-mode",
        value_enum,
        ignore_case = true,
        default_value = "CLOUD_FIRESTORE",
        help = "Cloud Firestore database mode supports `firestore-native` and `datastore-mode`"
    )]
    pub database_mode: DatabaseMode,

    #[arg(
        long = "export-name",
        help = "Export name for emulator export (e.g., ../exportName.overall_export_metadata)."
    )]
    pub export_name: Option<String>,

    #[arg(long = "export-on-exit", help = "Directory path for emulator export.")]
    pub export_on_exit: Option<PathBuf>,

    #[arg(
        long = "functions_emulator",
        help = "The HTTP host and port of the Functions Emulator. (e.g. localhost:9002)"
    )]
    pub functions_emulator: Option<String>,

    #[arg(long, action = ArgAction::SetTrue, help = "Print usage and exit")]
    pub help: bool,

    #[arg(
        long,
        default_value = "localhost",
        help = "The address to bind to on the local machine"
    )]
    pub host: String,

    #[arg(
        long = "import-data",
        help = "File path to data being imported into the emulator."
    )]
    pub import_data: Option<PathBuf>,

    #[arg(
        long = "index_file",
        help = "The path to a Firestore emulator index.yaml file."
    )]
    pub index_file: Option<PathBuf>,

    #[arg(
        long,
        num_args = 0..=1,
        default_value = "false",
        default_missing_value = "true",
        value_parser = BoolishValueParser::new(),
        help = "Print open-source dependencies and licenses, then exit"
    )]
    pub licenses: bool,

    #[arg(
        long,
        default_value_t = 8080,
        help = "The port number to listen to on the local machine"
    )]
    pub port: u16,

    #[arg(
        long = "project_id",
        help = "Project ID to be used for single project mode"
    )]
    pub project_id: Option<String>,

    #[arg(
        long = "require_indexes",
        num_args = 0..=1,
        default_value = "false",
        default_missing_value = "true",
        value_parser = BoolishValueParser::new(),
        help = "Whether to require indexes for queries. If set, emulator will throw an error if a query run does not have a matching index in the index file. This flag is only supported when `database-mode` is set to `datastore-mode`."
    )]
    pub require_indexes: bool,

    #[arg(long, help = "Cloud Firestore rules file")]
    pub rules: Option<PathBuf>,

    #[arg(
        long = "seed_from_export",
        help = "Local filesystem path to a Cloud Firestore export (e.g., '/home/foo/firestore_export/2019-10-22T19:41:32_89121.overall_export_metadata')"
    )]
    pub seed_from_export: Option<PathBuf>,

    #[arg(
        long = "single_project_mode",
        num_args = 0..=1,
        default_value = "false",
        default_missing_value = "true",
        value_parser = BoolishValueParser::new(),
        help = "Whether to operate in single project mode. If set, warnings will be logged for project ID not matching the provided project_id flag."
    )]
    pub single_project_mode: bool,

    #[arg(
        long = "single_project_mode_error",
        num_args = 0..=1,
        default_value = "false",
        default_missing_value = "true",
        value_parser = BoolishValueParser::new(),
        help = "Turns project ID mismatches from warnings to errors."
    )]
    pub single_project_mode_error: bool,

    #[arg(
        long,
        num_args = 0..=1,
        default_value = "false",
        default_missing_value = "true",
        value_parser = BoolishValueParser::new(),
        help = "Print version and exit"
    )]
    pub version: bool,

    #[arg(
        long = "webchannel_port",
        help = "The port number to bind for WebChannel traffic"
    )]
    pub webchannel_port: Option<u16>,

    #[arg(long = "websocket_port", help = "The port number for the emulator ui")]
    pub websocket_port: Option<u16>,

    /// Undocumented flag of the official emulator; accepted and ignored.
    #[arg(
        long,
        hide = true,
        num_args = 0..=1,
        default_value = "false",
        default_missing_value = "true",
        value_parser = BoolishValueParser::new()
    )]
    pub testing: bool,

    /// Positional arguments are ignored, as in the official emulator (`<ignored>`).
    #[arg(hide = true)]
    pub ignored: Vec<String>,
}

/// `--help` output in the official emulator's (JCommander) layout: options sorted by name, the
/// description indented below each option, then `Default:` and `Possible Values:` lines.
pub fn usage() -> String {
    const WIDTH: usize = 79;
    const INDENT: &str = "      ";
    let cmd = Cli::command();
    let mut args: Vec<_> = cmd
        .get_arguments()
        .filter(|a| a.get_long().is_some() && !a.is_hide_set())
        .collect();
    args.sort_by_key(|a| a.get_long());

    let mut out = String::from("Usage: hidane [options] <ignored>\n  Options:\n");
    for arg in args {
        let long = arg.get_long().unwrap_or_default();
        let _ = writeln!(out, "    --{long}");
        if let Some(help) = arg.get_help() {
            for line in wrap(&help.to_string(), WIDTH - INDENT.len()) {
                let _ = writeln!(out, "{INDENT}{line}");
            }
        }
        if long != "help"
            && let Some(default) = arg.get_default_values().first()
        {
            let _ = writeln!(out, "{INDENT}Default: {}", default.to_string_lossy());
        }
        let names: Vec<_> = arg
            .get_possible_values()
            .into_iter()
            .filter(|v| !v.is_hide_set())
            .map(|v| v.get_name().to_owned())
            .filter(|n| !matches!(n.as_str(), "true" | "false"))
            .collect();
        if !names.is_empty() {
            let _ = writeln!(out, "{INDENT}Possible Values: [{}]", names.join(", "));
        }
    }
    out
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.len() + 1 + word.len() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// Output of `--licenses`.
pub fn licenses() -> String {
    format!(
        "hidane {}\nLicense: MIT OR Apache-2.0\nSource: https://github.com/hidane-dev/hidane\n\
         Third-party license notices will ship with release binaries ({ISSUES}/56).\n",
        env!("CARGO_PKG_VERSION")
    )
}

impl Cli {
    /// The export to seed from: `--seed_from_export`, which firebase-tools passes, or the
    /// official emulator's other spelling `--import-data`.
    pub fn seed(&self) -> Option<&std::path::Path> {
        self.seed_from_export
            .as_deref()
            .or(self.import_data.as_deref())
    }

    /// Cross-flag checks. Returns warnings to print, or the error that must stop startup.
    ///
    /// The first four checks and their messages are the official emulator's. The rest reject
    /// or flag features hidane does not implement yet, so nobody silently loses data or rules.
    pub fn validate(&self) -> Result<Vec<String>, String> {
        if self.single_project_mode && self.project_id.is_none() {
            return Err("Expected project_id to be set for single_project_mode".into());
        }
        if self.single_project_mode_error && !self.single_project_mode {
            return Err(
                "Expected single_project_mode to be set for single_project_mode_error".into(),
            );
        }
        if self.export_name.is_some() && self.export_on_exit.is_none() {
            return Err("Export path not provided along with export name".into());
        }
        if self.database_mode != DatabaseMode::CloudFirestore {
            return Err(format!(
                "Datastore mode is not supported by hidane yet ({ISSUES}/81)"
            ));
        }
        if self.require_indexes {
            return Err("Firestore Native mode does not support index enforcement".into());
        }

        let mut warnings = Vec::new();
        if self.export_on_exit.is_some() {
            // Observed on v1.22.0 in Firestore Native mode (`tests/fixtures/export_import.json`).
            warnings.push(
                "--export-on-exit exports nothing, as on the official emulator; \
                 `firebase emulators:start --export-on-exit` exports through \
                 POST /emulator/v1/projects/{project}:export instead"
                    .into(),
            );
        }
        if self.rules.is_some() {
            warnings.push(format!(
                "Security Rules are not evaluated yet: every request is allowed ({ISSUES}/9)"
            ));
        }
        if self.functions_emulator.is_some() {
            warnings.push(format!(
                "Events are not delivered to the Functions emulator yet ({ISSUES}/70)"
            ));
        }
        if self.webchannel_port.is_some() {
            warnings.push(format!(
                "WebChannel is not implemented yet; --webchannel_port is ignored ({ISSUES}/10)"
            ));
        }
        if self.index_file.is_some() {
            warnings.push("--index_file is ignored in Firestore Native mode".into());
        }
        if self.database_edition == DatabaseEdition::Enterprise {
            warnings.push(format!(
                "Enterprise edition features (pipelines) are not implemented yet ({ISSUES}/80)"
            ));
        }
        Ok(warnings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("hidane").chain(args.iter().copied()))
    }

    /// The exact argument set firebase-tools 15.33 passes (controller.ts / downloadableEmulators.ts).
    const FIREBASE_TOOLS_ARGS: &[&str] = &[
        "--host",
        "127.0.0.1",
        "--port",
        "8080",
        "--websocket_port",
        "9150",
        "--database-edition",
        "standard",
        "--project_id",
        "demo-hidane",
        "--rules",
        "/abs/firestore.rules",
        "--single_project_mode",
        "true",
        "--functions_emulator",
        "127.0.0.1:5001",
    ];

    #[test]
    fn accepts_the_firebase_tools_argument_set() {
        let cli = parse(FIREBASE_TOOLS_ARGS).unwrap();
        assert_eq!(cli.host, "127.0.0.1");
        assert_eq!(cli.port, 8080);
        assert_eq!(cli.websocket_port, Some(9150));
        assert_eq!(cli.database_edition, DatabaseEdition::Standard);
        assert_eq!(cli.project_id.as_deref(), Some("demo-hidane"));
        assert!(cli.single_project_mode);
        let warnings = cli.validate().unwrap();
        assert!(warnings.iter().any(|w| w.contains("Security Rules")));
        assert!(warnings.iter().any(|w| w.contains("Functions emulator")));
    }

    #[test]
    fn defaults_match_the_official_emulator() {
        let cli = parse(&[]).unwrap();
        assert_eq!(cli.host, "localhost");
        assert_eq!(cli.port, 8080);
        assert_eq!(cli.database_edition, DatabaseEdition::Standard);
        assert_eq!(cli.database_mode, DatabaseMode::CloudFirestore);
        assert!(!cli.single_project_mode);
        assert!(cli.validate().unwrap().is_empty());
    }

    #[test]
    fn boolean_flags_take_an_optional_value() {
        for (args, expected) in [
            (&["--single_project_mode", "--project_id", "p"][..], true),
            (
                &["--single_project_mode", "true", "--project_id", "p"][..],
                true,
            ),
            (&["--single_project_mode", "false"][..], false),
            (
                &["--single_project_mode=TRUE", "--project_id", "p"][..],
                true,
            ),
        ] {
            assert_eq!(
                parse(args).unwrap().single_project_mode,
                expected,
                "{args:?}"
            );
        }
    }

    #[test]
    fn enum_values_accept_both_spellings() {
        let cli = parse(&["--database-edition", "ENTERPRISE"]).unwrap();
        assert_eq!(cli.database_edition, DatabaseEdition::Enterprise);
        let cli = parse(&["--database-mode", "firestore-native"]).unwrap();
        assert_eq!(cli.database_mode, DatabaseMode::CloudFirestore);
        let cli = parse(&["--database-mode", "datastore-mode"]).unwrap();
        assert_eq!(
            cli.database_mode,
            DatabaseMode::CloudFirestoreInDatastoreMode
        );
    }

    #[test]
    fn every_official_flag_is_accepted() {
        let cli = parse(&[
            "positional-is-ignored",
            "--database-edition",
            "standard",
            "--database-mode",
            "firestore-native",
            "--export-name",
            "x",
            "--export-on-exit",
            "/tmp/e",
            "--functions_emulator",
            "h:1",
            "--host",
            "0.0.0.0",
            "--import-data",
            "/tmp/i",
            "--index_file",
            "/tmp/index.yaml",
            "--licenses",
            "false",
            "--port",
            "1",
            "--project_id",
            "p",
            "--require_indexes",
            "false",
            "--rules",
            "/tmp/r",
            "--seed_from_export",
            "/tmp/s",
            "--single_project_mode",
            "true",
            "--single_project_mode_error",
            "true",
            "--version",
            "false",
            "--webchannel_port",
            "2",
            "--websocket_port",
            "3",
            "--testing",
        ]);
        let cli = cli.unwrap();
        assert_eq!(cli.ignored, ["positional-is-ignored"]);
    }

    #[test]
    fn unknown_flags_are_rejected() {
        assert!(parse(&["--no-such-flag"]).is_err());
        assert!(parse(&["--port", "not-a-number"]).is_err());
    }

    #[test]
    fn official_consistency_checks() {
        let err = |args: &[&str]| parse(args).unwrap().validate().unwrap_err();
        assert_eq!(
            err(&["--single_project_mode", "true"]),
            "Expected project_id to be set for single_project_mode"
        );
        assert_eq!(
            err(&["--single_project_mode_error", "true"]),
            "Expected single_project_mode to be set for single_project_mode_error"
        );
        assert_eq!(
            err(&["--export-name", "x"]),
            "Export path not provided along with export name"
        );
        assert_eq!(
            err(&["--require_indexes", "true"]),
            "Firestore Native mode does not support index enforcement"
        );
    }

    #[test]
    fn unimplemented_data_features_refuse_to_start() {
        let err = |args: &[&str]| parse(args).unwrap().validate().unwrap_err();
        assert!(err(&["--database-mode", "datastore-mode"]).contains("/81"));
    }

    #[test]
    fn export_and_import_flags_start() {
        let warnings = |args: &[&str]| parse(args).unwrap().validate().unwrap();
        assert!(warnings(&["--seed_from_export", "/x.overall_export_metadata"]).is_empty());
        assert!(warnings(&["--import-data", "/x.overall_export_metadata"]).is_empty());
        assert!(warnings(&["--export-on-exit", "/e", "--export-name", "n"])[0].contains("nothing"));
    }

    #[test]
    fn the_seed_is_either_flag() {
        let seed = |args: &[&str]| parse(args).unwrap().seed().map(PathBuf::from);
        assert_eq!(seed(&["--import-data", "/a"]), Some(PathBuf::from("/a")));
        assert_eq!(
            seed(&["--import-data", "/a", "--seed_from_export", "/b"]),
            Some(PathBuf::from("/b"))
        );
        assert_eq!(seed(&[]), None);
    }

    #[test]
    fn usage_matches_the_official_layout() {
        let text = usage();
        assert!(text.starts_with("Usage: hidane [options] <ignored>\n  Options:\n"));
        let flags: Vec<_> = text
            .lines()
            .filter_map(|l| l.strip_prefix("    --"))
            .collect();
        assert_eq!(
            flags,
            [
                "database-edition",
                "database-mode",
                "export-name",
                "export-on-exit",
                "functions_emulator",
                "help",
                "host",
                "import-data",
                "index_file",
                "licenses",
                "port",
                "project_id",
                "require_indexes",
                "rules",
                "seed_from_export",
                "single_project_mode",
                "single_project_mode_error",
                "version",
                "webchannel_port",
                "websocket_port",
            ]
        );
        assert!(text.contains("      Default: localhost\n"));
        assert!(text.contains("      Default: 8080\n"));
        assert!(text.contains("      Possible Values: [STANDARD, ENTERPRISE]\n"));
        assert!(text.contains(
            "      Possible Values: [CLOUD_DATASTORE, CLOUD_FIRESTORE_IN_DATASTORE_MODE, CLOUD_FIRESTORE]\n"
        ));
        assert!(!text.contains("--testing"));
        // Descriptions wrap at 79 columns like JCommander; only the `Possible Values` line and
        // unbreakable words (the example export path) may run longer, as in the official output.
        assert!(
            text.lines()
                .filter(|l| !l.trim_start().starts_with("Possible Values"))
                .all(|l| l.len() <= 79 || !l.trim().contains(' '))
        );
    }
}
