//! `hidane report`'s engine: the diagnostic bundle a maintainer needs to reproduce a bug
//! report, written on the user's machine and sent nowhere.
//!
//! [`machine::read`] gathers an [`Input`]: the environment, what `java -version` and
//! `node --version` say, the firebase-tools on `PATH`, the file names in firebase-tools'
//! emulator cache. [`build`] turns it and the project's files into a [`Report`]: hidane's build,
//! the system, those tools, the official emulator versions firebase-tools has cached, the
//! relevant environment variables (values only where they are safe), `hidane doctor`'s scan of
//! the project ([`project::scan`]), and its `firebase.json` and `.firebaserc`. Every text taken
//! from the machine or the project goes through a [`redact::Redactor`] first; the report counts
//! what was replaced. The report serialises to JSON, whose `schema` field versions its shape,
//! and [`render`] writes it as Markdown or text.

pub mod machine;
pub mod redact;
pub mod render;

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use crate::project::{self, files::Tree, firebase, json};
use redact::{Kind, Redactor, secret_key};

/// The version of the report's JSON shape.
pub const SCHEMA: &str = "hidane.report/1";

/// What every report says first.
pub const NOTICE: &str = "Written by `hidane report` on this machine; nothing was sent \
    anywhere. Secrets, project IDs, e-mail addresses, host and user names, and paths in the home \
    directory and the project were replaced with placeholders such as <project-id-1> and \
    <home> (counted at the end). Read it before you share it.";

/// What the report reads from the machine, before redaction. [`machine::read`] fills it;
/// tests build their own.
#[derive(Debug, Clone, Default)]
pub struct Input {
    /// The project's root, in each form it may be printed in.
    pub roots: Vec<String>,
    /// The home directory, likewise.
    pub homes: Vec<String>,
    /// The temporary directory, likewise.
    pub temps: Vec<String>,
    /// The user's names (`USER`, `USERNAME`, the home directory's name).
    pub users: Vec<String>,
    /// The machine's names.
    pub host_names: Vec<String>,
    /// The environment, names and values. Only [`environment`]'s rules decide what of it is
    /// shown.
    pub env: Vec<(String, String)>,
    pub os_version: Option<String>,
    pub container: Option<String>,
    /// What `java -version` printed; `None` when no `java` could be run.
    pub java: Option<String>,
    pub node: Option<String>,
    pub firebase_on_path: bool,
    /// The version of the firebase-tools on `PATH`.
    pub firebase_tools: Option<String>,
    /// Where firebase-tools caches the emulators it downloads.
    pub cache_dir: Option<String>,
    /// The names in that directory; `None` when it could not be listed.
    pub cache_entries: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub schema: &'static str,
    pub notice: &'static str,
    pub hidane: Build,
    pub system: System,
    pub tools: Tools,
    /// The official emulators firebase-tools has downloaded, from the cache's file names.
    pub official_emulator_cache: Cache,
    pub environment: Vec<Variable>,
    pub project: Project,
    pub comparison: Comparison,
    /// How many values of each kind were replaced with a placeholder.
    pub redacted: BTreeMap<Kind, usize>,
}

/// What it takes to run the same steps on the official emulator and on hidane. The report
/// runs neither: it names the commands in the project that start the Firestore emulator, and
/// whether the official one can run here. Comparing is the user's to do, and to share.
#[derive(Debug, Clone, Serialize)]
pub struct Comparison {
    /// Always false: nothing was run.
    pub compared: bool,
    /// The project's commands that start the Firestore emulator through firebase-tools: as
    /// written they use the official emulator, after `hidane exec --` hidane.
    pub commands: Vec<ProjectCommand>,
    /// A Java of version 21 or newer, which the official emulator needs, was found.
    pub java_for_official: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectCommand {
    pub file: String,
    pub line: usize,
    pub command: String,
    /// It already runs under `hidane exec`.
    pub under_hidane: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Build {
    pub version: &'static str,
    /// The commit it was built from, when the build knew it.
    pub commit: Option<&'static str>,
    /// `release` or `debug`.
    pub profile: &'static str,
    /// The Rust target triple.
    pub target: &'static str,
}

impl Build {
    /// This binary's.
    pub fn this() -> Self {
        let commit = env!("HIDANE_BUILD_COMMIT");
        let target = env!("HIDANE_BUILD_TARGET");
        Self {
            version: env!("CARGO_PKG_VERSION"),
            commit: (!commit.is_empty()).then_some(commit),
            profile: if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            },
            target: if target.is_empty() { "unknown" } else { target },
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct System {
    /// `linux`, `macos`, `windows`, …
    pub os: &'static str,
    pub arch: &'static str,
    /// `macOS 15.6`, `Ubuntu 24.04.1 LTS`, `Windows 10.0.26100.4652`.
    pub os_version: Option<String>,
    /// `docker`, `podman` or `kubernetes`, when hidane runs in one.
    pub container: Option<String>,
    /// The CI service, from its environment variables.
    pub ci: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Tools {
    pub java: Java,
    /// `node --version`.
    pub node: Option<String>,
    pub firebase_tools: FirebaseTools,
}

#[derive(Debug, Clone, Serialize)]
pub struct Java {
    /// A `java` on `PATH` reported a version.
    pub found: bool,
    /// The line of `java -version` with the version (`openjdk version "21.0.5" 2024-10-15`).
    pub version: Option<String>,
    /// The runtime's line (`OpenJDK Runtime Environment Temurin-21.0.5+11 (build …)`).
    pub runtime: Option<String>,
    pub major: Option<u32>,
    /// The `java` on `PATH` is `hidane exec`'s, which answers for the real one (or for none).
    pub hidane_exec: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct FirebaseTools {
    pub on_path: bool,
    /// The version of the one on `PATH`, from its `package.json`.
    pub path_version: Option<String>,
    /// The versions installed in the project's `node_modules`.
    pub installed: Vec<Installed>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Installed {
    pub file: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Cache {
    /// The cache directory (`$FIREBASE_EMULATORS_PATH`, else `~/.cache/firebase/emulators`).
    pub dir: Option<String>,
    /// The directory could be listed.
    pub listed: bool,
    /// The Firestore emulator versions (`cloud-firestore-emulator-v<version>.jar`).
    pub firestore: Vec<String>,
    /// The other emulators in it.
    pub others: Vec<Cached>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Cached {
    pub emulator: &'static str,
    pub version: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Variable {
    pub name: String,
    /// The value where it is safe, else what it holds (`<set>`, `<project-id-1>`).
    pub value: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Project {
    /// Always `<project>`: the root's own path is not reported.
    pub path: &'static str,
    pub firebase_json: Vec<FirebaseJson>,
    pub firebaserc: Vec<Firebaserc>,
    /// `hidane doctor --format json` on the project, redacted.
    pub doctor: Value,
    /// `hidane doctor`'s text report, redacted.
    #[serde(skip)]
    pub doctor_text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct FirebaseJson {
    pub file: String,
    /// The file, without its comments, every value redacted.
    pub content: Option<Value>,
    /// Why it could not be read.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Firebaserc {
    pub file: String,
    /// Each alias and the project it names.
    pub projects: Vec<Alias>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Alias {
    pub alias: String,
    pub project: String,
}

/// Builds the report of the project in `tree`, with what was read from the machine.
pub fn build(input: &Input, tree: &dyn Tree) -> Report {
    let scan = project::scan(tree);
    let mut r = Redactor::new();
    seed(&mut r, input, tree, &scan);
    // The first pass finds every project ID and host, wherever it is; the second, knowing
    // them all, replaces them everywhere, and counts.
    let _ = assemble(input, tree, &scan, &mut r);
    r.reset_counts();
    let mut report = assemble(input, tree, &scan, &mut r);
    report.redacted = r.counts().clone();
    report
}

/// Tells the redactor what it must never let through.
fn seed(r: &mut Redactor, input: &Input, tree: &dyn Tree, scan: &project::Report) {
    for path in &input.roots {
        r.path(path, Kind::ProjectPath);
    }
    for path in &input.homes {
        r.path(path, Kind::HomePath);
    }
    for path in &input.temps {
        r.path(path, Kind::TempPath);
    }
    for user in &input.users {
        r.name(user, Kind::UserName);
    }
    for host in &input.host_names {
        r.name(host, Kind::HostName);
    }
    for (name, value) in &input.env {
        if secret_key(name) {
            r.secret(value);
        }
    }
    for rc in firebaserc_files(tree, scan) {
        for id in rc.projects.iter().map(|(_, id)| id).chain(&rc.others) {
            r.project(id);
        }
        for (alias, _) in &rc.projects {
            r.alias(alias);
        }
    }
    for f in &scan.facts.firebase {
        if let Some(fs) = &f.firestore {
            r.host(&fs.host);
        }
        for e in &f.emulators {
            if let Some(host) = &e.host {
                r.host(host);
            }
        }
    }
    for job in scan.facts.workflows.iter().flat_map(|w| &w.jobs) {
        for host in &job.emulator_host {
            r.host_port(host);
        }
    }
}

fn assemble(input: &Input, tree: &dyn Tree, scan: &project::Report, r: &mut Redactor) -> Report {
    let java = java(input, r);
    let comparison = Comparison {
        compared: false,
        commands: scan
            .facts
            .emulator_commands
            .iter()
            .filter(|c| c.starts_firestore())
            .map(|c| ProjectCommand {
                file: r.text(&c.file),
                line: c.line,
                command: r.text(&c.command),
                under_hidane: c.under_hidane,
            })
            .collect(),
        // Under `hidane exec`, its java answers for a real one of 21 or newer, and otherwise
        // with a line of its own.
        java_for_official: java.major.is_some_and(|m| m >= 21)
            && !java
                .version
                .as_deref()
                .is_some_and(|v| v.contains("(hidane")),
    };
    Report {
        schema: SCHEMA,
        notice: NOTICE,
        hidane: Build::this(),
        system: System {
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            os_version: input.os_version.as_deref().map(|v| r.text(v)),
            container: input.container.clone(),
            ci: ci(&input.env),
        },
        tools: Tools {
            java,
            node: input.node.as_deref().map(|v| r.text(v)),
            firebase_tools: FirebaseTools {
                on_path: input.firebase_on_path,
                path_version: input.firebase_tools.as_deref().map(|v| r.text(v)),
                installed: installed(tree, scan, r),
            },
        },
        official_emulator_cache: cache(input, r),
        environment: environment(&input.env, r),
        project: Project {
            path: "<project>",
            firebase_json: firebase_json(tree, scan, r),
            firebaserc: firebaserc_files(tree, scan)
                .into_iter()
                .map(|rc| Firebaserc {
                    file: r.text(&rc.file),
                    projects: rc
                        .projects
                        .iter()
                        .map(|(alias, id)| Alias {
                            alias: r.text(alias),
                            project: r.project(id),
                        })
                        .collect(),
                })
                .collect(),
            doctor: serde_json::to_value(scan).map_or(Value::Null, |v| r.json("", &v)),
            doctor_text: project::report::text(scan, "<project>")
                .lines()
                .map(|line| r.text(line) + "\n")
                .collect(),
        },
        comparison,
        redacted: BTreeMap::new(),
    }
}

/// The CI service the environment names.
fn ci(env: &[(String, String)]) -> Option<&'static str> {
    let set = |name: &str| env.iter().any(|(n, v)| n == name && !v.is_empty());
    [
        ("GITHUB_ACTIONS", "GitHub Actions"),
        ("GITLAB_CI", "GitLab CI"),
        ("CIRCLECI", "CircleCI"),
        ("BUILDKITE", "Buildkite"),
        ("TF_BUILD", "Azure Pipelines"),
        ("BITBUCKET_BUILD_NUMBER", "Bitbucket Pipelines"),
        ("JENKINS_URL", "Jenkins"),
        ("TRAVIS", "Travis CI"),
        ("CODEBUILD_BUILD_ID", "AWS CodeBuild"),
        ("CI", "a CI service"),
    ]
    .into_iter()
    .find(|(name, _)| set(name))
    .map(|(_, ci)| ci)
}

fn java(input: &Input, r: &mut Redactor) -> Java {
    let text = input.java.as_deref().unwrap_or_default();
    // Only these lines: others can hold anything, such as `Picked up JAVA_TOOL_OPTIONS: …`.
    let line = |needle: &str| {
        text.lines()
            .find(|l| l.contains(needle))
            .and_then(machine::clean)
    };
    let version = line("version \"").map(|l| r.text(&l));
    let major = version.as_deref().and_then(java_major);
    Java {
        found: version.is_some(),
        version,
        runtime: line("Runtime Environment").map(|l| r.text(&l)),
        major,
        hidane_exec: input
            .env
            .iter()
            .any(|(n, v)| n == "HIDANE_SHIM_DIR" && !v.is_empty()),
    }
}

/// The major version in `java -version` output (`version "21.0.1"`, `version "1.8.0"`).
pub fn java_major(output: &str) -> Option<u32> {
    let rest = &output[output.find("version \"")? + 9..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    match digits.parse().ok()? {
        1 => rest
            .get(2..)?
            .chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse()
            .ok(),
        major => Some(major),
    }
}

/// The firebase-tools installed in the `node_modules` next to each `package.json` that
/// depends on it.
fn installed(tree: &dyn Tree, scan: &project::Report, r: &mut Redactor) -> Vec<Installed> {
    let mut dirs: Vec<&str> = scan
        .facts
        .dependencies
        .iter()
        .filter(|d| d.ecosystem == "npm" && d.name == "firebase-tools")
        .map(|d| project::files::parent(&d.file))
        .collect();
    dirs.dedup();
    dirs.into_iter()
        .filter_map(|dir| {
            let file = project::files::join(dir, "node_modules/firebase-tools/package.json")?;
            let doc = json::parse(&tree.read(&file)?).ok()?;
            let version = doc.get("version")?.as_str()?;
            Some(Installed {
                file: r.text(&file),
                version: r.text(version),
            })
        })
        .collect()
}

/// The emulators firebase-tools 15 caches, by the start and end of their file names
/// (`downloadableEmulatorInfo.json`).
const CACHED: [(&str, &str, &[&str]); 6] = [
    ("firestore", "cloud-firestore-emulator-v", &[".jar"]),
    ("database", "firebase-database-emulator-v", &[".jar"]),
    ("storage", "cloud-storage-rules-runtime-v", &[".jar"]),
    ("ui", "ui-v", &[".zip", ""]),
    ("pubsub", "pubsub-emulator-", &[".zip", ""]),
    ("dataconnect", "dataconnect-emulator-", &[".exe", ""]),
];

fn cache(input: &Input, r: &mut Redactor) -> Cache {
    let mut firestore = Vec::new();
    let mut others = Vec::new();
    for name in input.cache_entries.iter().flatten() {
        for (emulator, prefix, ends) in CACHED {
            let Some(rest) = name.strip_prefix(prefix) else {
                continue;
            };
            let Some(version) = ends.iter().find_map(|end| rest.strip_suffix(end)) else {
                continue;
            };
            let valid = !version.is_empty()
                && version.len() <= 40
                && (version.starts_with(|c: char| c.is_ascii_digit()) || version == "SNAPSHOT")
                && version
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'));
            if !valid {
                continue;
            }
            if emulator == "firestore" {
                firestore.push(version.to_owned());
            } else {
                others.push(Cached {
                    emulator,
                    version: version.to_owned(),
                });
            }
            break;
        }
    }
    firestore.sort_by_key(|v| version_key(v));
    firestore.dedup();
    others.sort();
    others.dedup();
    // A directory of the user's choosing is named by its variable: it may be anywhere.
    let chosen = input
        .env
        .iter()
        .any(|(n, v)| n == "FIREBASE_EMULATORS_PATH" && !v.is_empty());
    Cache {
        dir: if chosen {
            Some("$FIREBASE_EMULATORS_PATH".to_owned())
        } else {
            input.cache_dir.as_deref().map(|d| r.text(d))
        },
        listed: input.cache_entries.is_some(),
        firestore,
        others,
    }
}

/// `1.9.0` before `1.22.0`, and `SNAPSHOT` after both.
fn version_key(version: &str) -> Vec<(u64, String)> {
    version
        .split('.')
        .map(|p| {
            let digits: String = p.chars().take_while(char::is_ascii_digit).collect();
            (
                digits.parse().unwrap_or(u64::MAX),
                p[digits.len()..].to_owned(),
            )
        })
        .collect()
}

/// How a variable's value is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shown {
    /// `host:port` (or a URL), the host only when it is a loopback one.
    HostPort,
    /// A project ID, as the redactor decides.
    Project,
    /// Only that it is set.
    Presence,
    /// `true`, `false`, `1`, `0` and the like; anything else as set.
    Flag,
    /// A version.
    Version,
    /// A list of names (`FIREBASE_CLI_EXPERIMENTS`).
    Names,
    /// `FIRESTORE_EMULATOR_BINARY_PATH`: what its file is, not where.
    Binary,
}

/// The variables a report looks at, and how each is shown. Others whose names start with
/// [`PREFIXES`] or end with `_EMULATOR_HOST` are shown as set; the rest are not shown.
const VARIABLES: [(&str, Shown); 25] = [
    ("FIRESTORE_EMULATOR_BINARY_PATH", Shown::Binary),
    ("FIREBASE_FIRESTORE_EMULATOR_ADDRESS", Shown::HostPort),
    ("FIREBASE_EMULATOR_HUB", Shown::HostPort),
    ("GCLOUD_PROJECT", Shown::Project),
    ("GOOGLE_CLOUD_PROJECT", Shown::Project),
    ("CLOUDSDK_CORE_PROJECT", Shown::Project),
    ("DATASTORE_PROJECT_ID", Shown::Project),
    ("GOOGLE_APPLICATION_CREDENTIALS", Shown::Presence),
    ("FIREBASE_TOKEN", Shown::Presence),
    ("FIREBASE_CONFIG", Shown::Presence),
    ("FIREBASE_EMULATORS_PATH", Shown::Presence),
    ("JAVA_HOME", Shown::Presence),
    ("JAVA_TOOL_OPTIONS", Shown::Presence),
    ("_JAVA_OPTIONS", Shown::Presence),
    ("JDK_JAVA_OPTIONS", Shown::Presence),
    ("NODE_OPTIONS", Shown::Presence),
    ("HTTPS_PROXY", Shown::Presence),
    ("HTTP_PROXY", Shown::Presence),
    ("CI", Shown::Flag),
    ("GITHUB_ACTIONS", Shown::Flag),
    ("EXPERIMENTAL_MODE", Shown::Flag),
    ("HIDANE_JAVA_SHIM_CALLED", Shown::Flag),
    ("FIREBASE_CLI_EXPERIMENTS", Shown::Names),
    ("FIREBASE_CLI_PREVIEWS", Shown::Names),
    ("NO_UPDATE_NOTIFIER", Shown::Flag),
];

/// Prefixes of the names of variables shown as set.
const PREFIXES: [&str; 7] = [
    "FIREBASE_",
    "FIRESTORE_",
    "GCLOUD_",
    "GOOGLE_",
    "CLOUDSDK_",
    "HIDANE_",
    "FUNCTIONS_",
];

/// The variables that matter to the emulators, each shown as [`VARIABLES`] says.
pub fn environment(env: &[(String, String)], r: &mut Redactor) -> Vec<Variable> {
    let mut out: Vec<Variable> = env
        .iter()
        .filter_map(|(name, value)| {
            let shown = VARIABLES
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(name))
                .map(|(_, s)| *s)
                .or_else(|| {
                    if name.ends_with("_EMULATOR_HOST") {
                        Some(Shown::HostPort)
                    } else if name.ends_with("_EMULATOR_VERSION") {
                        Some(Shown::Version)
                    } else if PREFIXES.iter().any(|p| name.starts_with(p)) {
                        Some(Shown::Presence)
                    } else {
                        None
                    }
                })?;
            Some(Variable {
                name: r.text(name),
                value: show(shown, value, r),
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn show(shown: Shown, value: &str, r: &mut Redactor) -> String {
    if value.is_empty() {
        return "<empty>".to_owned();
    }
    let set = || "<set>".to_owned();
    match shown {
        Shown::HostPort => r.host_port(value),
        Shown::Project => r.project(value),
        Shown::Presence => set(),
        Shown::Flag => {
            let flag = matches!(
                value.to_ascii_lowercase().as_str(),
                "true" | "false" | "1" | "0" | "yes" | "no" | "on" | "off"
            );
            if flag { value.to_owned() } else { set() }
        }
        Shown::Version => {
            let ok = value.len() <= 40
                && value
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'));
            if ok { value.to_owned() } else { set() }
        }
        Shown::Names => {
            let ok = value.len() <= 200
                && value
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, ',' | '_' | '-' | '.' | ' '));
            if ok { r.text(value) } else { set() }
        }
        Shown::Binary => {
            let name = value.rsplit(['/', '\\']).next().unwrap_or(value);
            if name == "hidane-firestore.jar" {
                "<hidane exec's placeholder>".to_owned()
            } else if name.starts_with("cloud-firestore-emulator-v")
                && name.ends_with(".jar")
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
            {
                format!("<path>/{name}")
            } else {
                set()
            }
        }
    }
}

/// Each `firebase.json` the scan found, read again for its content.
fn firebase_json(tree: &dyn Tree, scan: &project::Report, r: &mut Redactor) -> Vec<FirebaseJson> {
    let mut files: Vec<&str> = scan
        .findings
        .iter()
        .filter(|f| f.code.starts_with("firebase-json."))
        .filter_map(|f| f.file.as_deref())
        .collect();
    files.sort_unstable();
    files.dedup();
    files
        .into_iter()
        .filter_map(|file| {
            let config = firebase::read(tree, file)?;
            Some(match config.doc {
                Ok(node) => FirebaseJson {
                    file: r.text(file),
                    content: Some(r.json("", &value_of(&node))),
                    error: None,
                },
                Err(err) => FirebaseJson {
                    file: r.text(file),
                    content: None,
                    error: Some(r.text(&err.to_string())),
                },
            })
        })
        .collect()
}

/// A parsed JSON value as serde_json's, comments gone.
fn value_of(node: &json::Node) -> Value {
    match &node.kind {
        json::Kind::Null => Value::Null,
        json::Kind::Bool(b) => Value::Bool(*b),
        json::Kind::Number(n) => {
            if n.fract() == 0.0 && n.abs() < 9.0e15 {
                Value::from(*n as i64)
            } else {
                serde_json::Number::from_f64(*n).map_or(Value::Null, Value::Number)
            }
        }
        json::Kind::Str(s) => Value::String(s.text.clone()),
        json::Kind::Array(items) => Value::Array(items.iter().map(value_of).collect()),
        json::Kind::Object(entries) => Value::Object(
            entries
                .iter()
                .map(|(k, v)| (k.text.clone(), value_of(v)))
                .collect(),
        ),
    }
}

/// A `.firebaserc`, as read.
struct Rc {
    file: String,
    /// Each alias and the project it names.
    projects: Vec<(String, String)>,
    /// The other IDs it names: the projects and the sites in `targets`.
    others: Vec<String>,
}

/// The `.firebaserc` files at the root and next to each `firebase.json`.
fn firebaserc_files(tree: &dyn Tree, scan: &project::Report) -> Vec<Rc> {
    let mut dirs: Vec<&str> = vec![""];
    dirs.extend(
        scan.facts
            .firebase
            .iter()
            .map(|f| project::files::parent(&f.file)),
    );
    dirs.sort_unstable();
    dirs.dedup();
    dirs.into_iter()
        .filter_map(|dir| {
            let file = project::files::join(dir, ".firebaserc")?;
            let doc = json::parse_with_comments(&tree.read(&file)?).ok()?;
            let projects = doc
                .get("projects")
                .map(json::Node::entries)
                .unwrap_or_default()
                .iter()
                .filter_map(|(alias, id)| Some((alias.text.clone(), id.as_str()?.to_owned())))
                .collect();
            // "targets": {"<project>": {"hosting": {"<target>": ["<site>"]}}}
            let mut others = Vec::new();
            for (project, types) in doc
                .get("targets")
                .map(json::Node::entries)
                .unwrap_or_default()
            {
                others.push(project.text.clone());
                for (_, targets) in types.entries() {
                    for (_, resources) in targets.entries() {
                        others.extend(
                            resources
                                .as_array()
                                .unwrap_or_default()
                                .iter()
                                .filter_map(|r| r.as_str().map(str::to_owned)),
                        );
                    }
                }
            }
            Some(Rc {
                file,
                projects,
                others,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::files::Memory;

    /// A fake secret, put together at run time so that no secret scanner takes this file for
    /// a leak.
    fn fake(prefix: &str, n: usize) -> String {
        let tail: String = "Qx7k2Lm9Zp4Rt8Vw3Ny6".chars().cycle().take(n).collect();
        format!("{prefix}{tail}")
    }

    /// A project and a machine that leak everything they can.
    fn leaky() -> (Input, Memory, Vec<String>) {
        let token = fake("1//0g", 40);
        let api_key = fake("AIza", 35);
        let other_secret = fake("s3cr3t-", 20);
        let firebase_json = format!(
            r#"{{
              // owner: alice@example.com
              "firestore": {{"rules": "firestore.rules"}},
              "hosting": {{
                "site": "acme-site-4821",
                "public": "/Users/alice/work/acme-app/public",
                "headers": [{{"source": "**", "headers": [{{"key": "Authorization", "value": "Basic Zm9vOmJhcg=="}}]}}]
              }},
              "functions": {{"source": "functions", "predeploy": ["FIREBASE_TOKEN={token} npm run build"]}},
              "emulators": {{
                "firestore": {{"host": "10.20.30.40", "port": 8081}},
                "auth": {{"host": "build-box.corp.example", "port": 9099}}
              }},
              "remoteconfig": {{"apiKey": "{api_key}"}}
            }}"#
        );
        let workflow = format!(
            "on: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n    env:\n      FIRESTORE_EMULATOR_HOST: 10.20.30.40:8081\n    steps:\n      - run: npx firebase emulators:exec --only firestore --project acme-prod-4821 --token {token} \"npm test\"\n"
        );
        let tree: Memory = [
            ("firebase.json", firebase_json),
            (
                ".firebaserc",
                r#"{"projects": {"default": "acme-prod-4821", "staging": "acme-staging-4821"},
                    "targets": {"acme-prod-4821": {"hosting": {"main": ["acme-site-4821"]}}}}"#
                    .to_owned(),
            ),
            (
                "package.json",
                r#"{"devDependencies": {"firebase-tools": "15.33.0"},
                    "scripts": {"test": "firebase emulators:exec --project acme-prod-4821 'npm run t'"}}"#
                    .to_owned(),
            ),
            (
                "node_modules/firebase-tools/package.json",
                r#"{"name": "firebase-tools", "version": "15.33.0"}"#.to_owned(),
            ),
            (".github/workflows/ci.yml", workflow),
            (
                "firestore.rules",
                "rules_version = '2';\nservice cloud.firestore {\n  match /databases/{database}/documents {\n    match /{document=**} {\n      allow read, write: if false;\n    }\n  }\n}\n"
                    .to_owned(),
            ),
        ]
        .into_iter()
        .collect();
        let env: Vec<(String, String)> = [
            ("GCLOUD_PROJECT", "acme-prod-4821"),
            ("FIREBASE_TOKEN", token.as_str()),
            (
                "GOOGLE_APPLICATION_CREDENTIALS",
                "/Users/alice/keys/sa.json",
            ),
            ("FIRESTORE_EMULATOR_HOST", "10.20.30.40:8081"),
            ("STORAGE_EMULATOR_HOST", "http://10.20.30.41:9199"),
            ("FIREBASE_AUTH_EMULATOR_HOST", "127.0.0.1:9099"),
            ("MY_API_SECRET", other_secret.as_str()),
            ("FIREBASE_CONFIG", r#"{"projectId":"acme-prod-4821"}"#),
            (
                "FIRESTORE_EMULATOR_BINARY_PATH",
                "/Users/alice/tmp/hidane-firestore.jar",
            ),
            ("FIREBASE_CLI_EXPERIMENTS", "webframeworks"),
            ("FIRESTORE_EMULATOR_VERSION", "1.22.0"),
            ("FIREBASE_SOMETHING_NEW", "alice@example.com"),
            ("CI", "true"),
            ("GITHUB_ACTIONS", "true"),
            ("PATH", "/Users/alice/bin:/usr/bin"),
            ("HOSTNAME", "alices-laptop.local"),
        ]
        .into_iter()
        .map(|(n, v)| (n.to_owned(), v.to_owned()))
        .collect();
        let input = Input {
            roots: vec!["/Users/alice/work/acme-app".into()],
            homes: vec!["/Users/alice".into()],
            temps: vec!["/var/folders/ab/xyz123/T".into()],
            users: vec!["alice".into()],
            host_names: vec!["alices-laptop.local".into()],
            env,
            os_version: Some("macOS 15.6".into()),
            container: None,
            java: Some(
                "Picked up JAVA_TOOL_OPTIONS: -Djavax.net.ssl.trustStorePassword=hunter2 -Duser.home=/Users/alice\n\
                 openjdk version \"21.0.5\" 2024-10-15 LTS\n\
                 OpenJDK Runtime Environment Temurin-21.0.5+11 (build 21.0.5+11-LTS)\n\
                 OpenJDK 64-Bit Server VM Temurin-21.0.5+11 (build 21.0.5+11-LTS, mixed mode)\n"
                    .into(),
            ),
            node: Some("v22.11.0".into()),
            firebase_on_path: true,
            firebase_tools: Some("15.33.0".into()),
            cache_dir: Some("/Users/alice/.cache/firebase/emulators".into()),
            cache_entries: Some(
                [
                    "cloud-firestore-emulator-v1.22.0.jar",
                    "cloud-firestore-emulator-v1.9.0.jar",
                    "cloud-firestore-emulator-v1.22.0.jar.download",
                    "cloud-firestore-emulator-v../../x.jar",
                    "ui-v1.15.0.zip",
                    "ui-v1.15.0",
                    "pubsub-emulator-0.8.36",
                    "notes-for-alice@example.com.txt",
                ]
                .map(String::from)
                .to_vec(),
            ),
        };
        let canaries = vec![
            "acme-prod-4821".to_owned(),
            "acme-staging-4821".to_owned(),
            "acme-site-4821".to_owned(),
            "alice".to_owned(),
            "10.20.30.40".to_owned(),
            "10.20.30.41".to_owned(),
            "build-box".to_owned(),
            "corp.example".to_owned(),
            "hunter2".to_owned(),
            "Zm9vOmJhcg".to_owned(),
            "example.com".to_owned(),
            "/Users".to_owned(),
            "xyz123".to_owned(),
            token,
            api_key,
            other_secret,
        ];
        (input, tree, canaries)
    }

    #[test]
    fn nothing_identifying_gets_through() {
        let (input, tree, canaries) = leaky();
        let report = build(&input, &tree);
        let outputs = [
            serde_json::to_string_pretty(&report).unwrap(),
            render::markdown(&report),
            render::text(&report),
        ];
        for out in &outputs {
            let lower = out.to_ascii_lowercase();
            for canary in &canaries {
                assert!(
                    !lower.contains(&canary.to_ascii_lowercase()),
                    "{canary} leaked into:\n{out}"
                );
            }
        }
        let md = &outputs[1];
        for expected in [
            "| `FIRESTORE_EMULATOR_HOST` | `<host>:8081` |",
            "| `GCLOUD_PROJECT` | `<project-id-1>` |",
            "| `FIREBASE_TOKEN` | `<set>` |",
            "| `FIREBASE_AUTH_EMULATOR_HOST` | `127.0.0.1:9099` |",
            "| `STORAGE_EMULATOR_HOST` | `http://<host>:9199` |",
            "| `FIRESTORE_EMULATOR_BINARY_PATH` | `<hidane exec's placeholder>` |",
            "| `FIRESTORE_EMULATOR_VERSION` | `1.22.0` |",
            "| `FIREBASE_CLI_EXPERIMENTS` | `webframeworks` |",
            "| `FIREBASE_SOMETHING_NEW` | `<set>` |",
            "| `CI` | `true` |",
            "`default` → `<project-id-1>`, `staging` → `<project-id-2>`",
            "\"site\": \"<project-id-3>\"",
            "\"public\": \"<project>/public\"",
            "\"key\": \"Authorization\",\n            \"value\": \"<redacted>\"",
            "\"FIREBASE_TOKEN=<redacted> npm run build\"",
            "\"apiKey\": \"<redacted>\"",
            "\"host\": \"<host>\"",
            "--project <project-id-1> --token <redacted>",
            "| Java | `openjdk version \"21.0.5\" 2024-10-15 LTS`, OpenJDK Runtime Environment Temurin-21.0.5+11 (build 21.0.5+11-LTS) |",
            "| firebase-tools | 15.33.0 on PATH; 15.33.0 in `node_modules/firebase-tools/package.json` |",
            "| Cached official emulator | v1.9.0, v1.22.0, in `<home>/.cache/firebase/emulators` |",
            "| Other cached emulators | pubsub 0.8.36, ui 1.15.0 |",
            "| CI | GitHub Actions |",
            "- `.github/workflows/ci.yml:8`: `npx firebase emulators:exec --only firestore --project <project-id-1> --token <redacted> \"npm test\"`",
            "- `package.json:2`: `firebase emulators:exec --project <project-id-1> 'npm run t'`",
            "The official emulator needs Java 21 or newer: found here. firebase-tools has cached v1.9.0, v1.22.0.",
        ] {
            assert!(md.contains(expected), "missing {expected}:\n{md}");
        }
        // The environment shows only what the emulators read.
        assert!(
            !md.contains("`PATH`") && !md.contains("MY_API_SECRET") && !md.contains("HOSTNAME")
        );
        assert!(report.redacted[&Kind::ProjectId] > 0);
        assert!(report.redacted[&Kind::Host] > 0);
        assert!(report.redacted[&Kind::Secret] > 0);
        assert!(report.redacted[&Kind::ProjectPath] > 0);
        assert!(report.redacted[&Kind::HomePath] > 0);
        assert_eq!(report.tools.java.major, Some(21));
        assert_eq!(
            report.official_emulator_cache.firestore,
            ["1.9.0", "1.22.0"]
        );
    }

    #[test]
    fn reports_without_a_project_or_tools() {
        let report = build(&Input::default(), &Memory::new());
        let md = render::markdown(&report);
        assert!(md.contains("| Java | not found on PATH |"), "{md}");
        assert!(md.contains("| firebase-tools | not on PATH |"), "{md}");
        assert!(
            md.contains("None of the variables the emulators read is set."),
            "{md}"
        );
        assert!(md.contains("No firebase.json found"), "{md}");
        assert!(
            md.contains("No command in the project starts the Firestore emulator"),
            "{md}"
        );
        assert!(
            md.contains("not found here, so only hidane runs on this machine. firebase-tools downloads its jar the first time it starts it."),
            "{md}"
        );
        assert!(md.contains("its JSON form included): nothing."), "{md}");
        let text = render::text(&report);
        assert!(text.contains("its JSON form included): nothing."), "{text}");
        assert_eq!(serde_json::to_value(&report).unwrap()["schema"], SCHEMA);
    }

    #[test]
    fn cached_emulators_from_names() {
        let input = Input {
            cache_dir: Some("/srv/acme-ci/emulators".into()),
            cache_entries: Some(
                [
                    "cloud-firestore-emulator-v1.22.0.jar",
                    "cloud-firestore-emulator-v1.19.10.jar",
                    "cloud-firestore-emulator-v1.19.9.jar",
                    "cloud-firestore-emulator-vSNAPSHOT.jar",
                    "cloud-firestore-emulator-v1.22.0.jar.tmp",
                    "cloud-firestore-emulator-v1.2 3.jar",
                    "firebase-database-emulator-v4.11.2.jar",
                    "cloud-storage-rules-runtime-v1.1.3.jar",
                    "dataconnect-emulator-3.4.22",
                    "ui-vSNAPSHOT.zip",
                    "my-notes.txt",
                ]
                .map(String::from)
                .to_vec(),
            ),
            env: vec![(
                "FIREBASE_EMULATORS_PATH".into(),
                "/srv/acme-ci/emulators".into(),
            )],
            ..Input::default()
        };
        let cache = cache(&input, &mut Redactor::new());
        assert_eq!(cache.dir.as_deref(), Some("$FIREBASE_EMULATORS_PATH"));
        assert_eq!(cache.firestore, ["1.19.9", "1.19.10", "1.22.0", "SNAPSHOT"]);
        let others: Vec<String> = cache
            .others
            .iter()
            .map(|o| format!("{} {}", o.emulator, o.version))
            .collect();
        assert_eq!(
            others,
            [
                "database 4.11.2",
                "dataconnect 3.4.22",
                "storage 1.1.3",
                "ui SNAPSHOT"
            ]
        );
        let none = cache_of(None);
        assert!(!none.listed && none.firestore.is_empty());
    }

    fn cache_of(entries: Option<Vec<String>>) -> Cache {
        cache(
            &Input {
                cache_entries: entries,
                ..Input::default()
            },
            &mut Redactor::new(),
        )
    }

    #[test]
    fn java_versions() {
        assert_eq!(
            java_major("openjdk version \"24.0.2\" 2025-07-15"),
            Some(24)
        );
        assert_eq!(java_major("java version \"1.8.0_381\""), Some(8));
        assert_eq!(java_major("openjdk version \"21\" (hidane)"), Some(21));
        assert_eq!(java_major("java version \"1\""), None);
        assert_eq!(java_major("nothing"), None);
    }

    #[test]
    fn ci_services() {
        let env = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
            pairs
                .iter()
                .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
                .collect()
        };
        assert_eq!(
            ci(&env(&[("CI", "true"), ("GITLAB_CI", "true")])),
            Some("GitLab CI")
        );
        assert_eq!(ci(&env(&[("CI", "1")])), Some("a CI service"));
        assert_eq!(ci(&env(&[("CI", "")])), None);
        assert_eq!(ci(&[]), None);
    }
}
