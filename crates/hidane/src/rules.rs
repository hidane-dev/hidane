//! Security Rules as the emulator API serves them: `PUT` and `GET
//! /emulator/v1/projects/{p}:securityRules`, the coverage report (`GET …:ruleCoverage` and
//! `…:ruleCoverage.html`), `debug()`, and `--rules`.
//!
//! As the official emulator answers (`tests/fixtures/rules_compile.json`,
//! `tests/fixtures/rules_coverage.json`):
//!
//! - Each database has its ruleset: `projects/{p}:securityRules` is the `(default)` database's,
//!   `projects/{p}/databases/{d}:securityRules` another's. A database without one uses the
//!   `--rules` file, or allows everything (`OPEN_RULES`). `POST /reset` forgets the rulesets
//!   loaded since.
//! - `PUT` compiles the files: `200` with the warnings (`issues[]`) or `{}`, or `400` "Error
//!   compiling rules:" with one line per issue. A ruleset that does not compile leaves the
//!   database's previous one in place.
//! - `GET :securityRules` answers the files as loaded, fingerprints included; `GET
//!   :ruleCoverage` the same with the coverage report (`hidane_rules::coverage`), counted since
//!   the ruleset was loaded; `GET :ruleCoverage.html` a page that shows it, but only for a
//!   request without a query string or a content type. Other methods are `404`, and so is
//!   `:ruleCoverage/html`. An empty project ID is a `500` without a message, but with a database
//!   it is a bad database name, as an empty database ID is.
//! - Each `debug()` value goes to standard output, which firebase-tools writes to
//!   `firestore-debug.log` (`report::debug_text`).
//! - With the request monitor on (`--websocket_port`), each evaluation is recorded
//!   (`monitor`), and a database without rules is checked against the open rules, so that its
//!   requests are recorded too.
//!
//! The body of a `PUT` is read as protobuf-java's JSON parser reads `{rules: {files: [{name,
//! content, fingerprint}]}}`: a number or a boolean as text, `null` as empty, the fingerprint in
//! base64 (either alphabet, padding optional), any other key or shape "Payload isn't valid for
//! request.". The official emulator answers a body without files with a 500 (a crash); hidane
//! answers it as an invalid payload (`docs/parity-exceptions.md`).

pub mod monitor;
mod report;

use std::{
    collections::HashMap,
    fmt::Write as _,
    sync::{Arc, Mutex, OnceLock, RwLock},
};

use axum::{
    body::Body,
    http::{HeaderMap, Method, header},
    response::{IntoResponse, Response},
};
use base64::{
    Engine as _, alphabet,
    engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig},
};
use hidane_rules::{
    Compilation, Issue, Ruleset, Severity, SourceFile, coverage::Coverage, eval::Trace,
};
use serde_json::Value as Json;
use tonic::Status;

use crate::{firestore::names, rest};

/// A file of a ruleset, as it was loaded.
#[derive(Debug, Clone, Default)]
pub(crate) struct File {
    pub name: Option<String>,
    pub content: String,
    pub fingerprint: Vec<u8>,
}

/// What the official emulator answers for a database without rules.
const OPEN_RULES: &str = "rules_version = '2';\nservice cloud.firestore {\n  match \
                          /databases/{database}/documents {\n    match /{document=**} {\n      \
                          allow read, write: if true;\n    }\n  }\n}\n";

/// Where the values of `debug()` go: lines of text, each ending with a line break.
pub type DebugLog = Arc<dyn Fn(&str) + Send + Sync>;

/// A ruleset and the files it was compiled from, as they were loaded.
struct Compiled {
    files: Vec<File>,
    ruleset: Arc<Ruleset>,
}

/// A database's ruleset, with its coverage report.
pub(crate) struct Loaded {
    compiled: Arc<Compiled>,
    coverage: Mutex<Coverage>,
}

impl Loaded {
    fn new(compiled: Arc<Compiled>) -> Self {
        let source = compiled.files.first().map_or("", |f| f.content.as_str());
        let coverage = Mutex::new(Coverage::new(&compiled.ruleset, source));
        Self { compiled, coverage }
    }

    pub(crate) fn ruleset(&self) -> &Ruleset {
        &self.compiled.ruleset
    }

    /// Counts what one decision traced into the coverage report.
    pub(crate) fn record(&self, trace: &Trace) {
        self.coverage
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .record(trace);
    }
}

/// The rulesets: one per database loaded through `:securityRules`, and the `--rules` file for
/// the others (each of which counts its own coverage).
pub struct Rules {
    /// By database name (`projects/{p}/databases/{d}`).
    databases: RwLock<HashMap<String, Arc<Loaded>>>,
    default: RwLock<Option<Arc<Compiled>>>,
    log: RwLock<DebugLog>,
    monitor: RwLock<Option<Arc<monitor::Monitor>>>,
    /// `OPEN_RULES`, for the request monitor.
    open: OnceLock<Arc<Loaded>>,
}

impl Default for Rules {
    fn default() -> Self {
        Self {
            databases: RwLock::default(),
            default: RwLock::default(),
            log: RwLock::new(Arc::new(stdout)),
            monitor: RwLock::default(),
            open: OnceLock::new(),
        }
    }
}

/// The official emulator prints `debug()` values to its standard output.
fn stdout(text: &str) {
    use std::io::Write as _;
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(text.as_bytes()).and_then(|()| out.flush());
}

impl Rules {
    /// The ruleset `database` (`projects/{p}/databases/{d}`) uses, if any.
    pub fn ruleset(&self, database: &str) -> Option<Arc<Ruleset>> {
        let databases = self.databases.read().unwrap_or_else(|e| e.into_inner());
        match databases.get(database) {
            Some(loaded) => Some(Arc::clone(&loaded.compiled.ruleset)),
            None => self
                .default
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .map(|d| Arc::clone(&d.ruleset)),
        }
    }

    /// The ruleset of `database`, if it has one; the `--rules` file's gets a coverage report
    /// of its own in each database.
    pub(crate) fn get(&self, database: &str) -> Option<Arc<Loaded>> {
        if let Some(loaded) = self
            .databases
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(database)
        {
            return Some(Arc::clone(loaded));
        }
        let default = self
            .default
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()?;
        let mut databases = self.databases.write().unwrap_or_else(|e| e.into_inner());
        let loaded = databases
            .entry(database.to_owned())
            .or_insert_with(|| Arc::new(Loaded::new(default)));
        Some(Arc::clone(loaded))
    }

    fn set(&self, database: &str, files: Vec<File>, ruleset: Ruleset) {
        let compiled = Compiled {
            files,
            ruleset: Arc::new(ruleset),
        };
        self.databases
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                database.to_owned(),
                Arc::new(Loaded::new(Arc::new(compiled))),
            );
    }

    /// `--rules`: the ruleset compiled from `file`, for the databases that load none.
    pub fn set_default(&self, file: &std::path::Path, content: String, ruleset: Ruleset) {
        let compiled = Compiled {
            files: vec![File {
                name: Some(file.display().to_string()),
                content,
                fingerprint: Vec::new(),
            }],
            ruleset: Arc::new(ruleset),
        };
        *self.default.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(compiled));
    }

    /// `POST /reset`: every database is back to the `--rules` file, or to no rules.
    pub(crate) fn reset(&self) {
        self.databases
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    /// Records each evaluation for the Emulator UI's request monitor (`--websocket_port`).
    pub fn monitor_requests(&self, monitor: Arc<monitor::Monitor>) {
        *self.monitor.write().unwrap_or_else(|e| e.into_inner()) = Some(monitor);
    }

    /// The ruleset a request on `database` is checked against: the database's, or with the
    /// request monitor on, the open rules.
    pub(crate) fn checked(&self, database: &str) -> Option<Arc<Loaded>> {
        self.get(database).or_else(|| {
            self.monitor
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()?;
            let open = self.open.get_or_init(|| {
                let file = File {
                    name: Some("OPEN_RULES".to_owned()),
                    content: OPEN_RULES.to_owned(),
                    fingerprint: Vec::new(),
                };
                let ruleset = hidane_rules::compile(&[SourceFile {
                    name: file.name.clone(),
                    content: file.content.clone(),
                }])
                .ruleset
                .expect("the open rules compile");
                Arc::new(Loaded::new(Arc::new(Compiled {
                    files: vec![file],
                    ruleset: Arc::new(ruleset),
                })))
            });
            Some(Arc::clone(open))
        })
    }

    /// The records of one request on `database` under `loaded`, with the request monitor on.
    pub(crate) fn session(&self, database: &str, loaded: &Loaded) -> Option<monitor::Session> {
        let monitor = self.monitor.read().unwrap_or_else(|e| e.into_inner());
        Some(monitor.as_ref()?.session(database, &loaded.compiled.files))
    }

    /// Sends the values of `debug()` to `log` instead of standard output.
    pub fn set_debug_log(&self, log: DebugLog) {
        *self.log.write().unwrap_or_else(|e| e.into_inner()) = log;
    }

    /// Writes the values `trace` saw `debug()` receive, as the official emulator prints them.
    pub(crate) fn debug(&self, trace: &Trace) {
        if trace.debug().is_empty() {
            return;
        }
        let log = Arc::clone(&self.log.read().unwrap_or_else(|e| e.into_inner()));
        log(&report::debug_text(trace.debug()));
    }
}

/// What a request under `/emulator/v1/projects/` asks of the rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verb {
    SecurityRules,
    RuleCoverage,
    RuleCoverageHtml,
}

/// `/emulator/v1/projects/{p}[/databases/{d}]:{verb}`: the project, the database if named, and
/// the verb.
pub(crate) fn route(path: &str) -> Option<(&str, Option<&str>, Verb)> {
    let rest = path.strip_prefix("/emulator/v1/projects/")?;
    let (target, verb) = rest.rsplit_once(':')?;
    let verb = match verb {
        "securityRules" => Verb::SecurityRules,
        "ruleCoverage" => Verb::RuleCoverage,
        "ruleCoverage.html" => Verb::RuleCoverageHtml,
        _ => return None,
    };
    match target.split_once('/') {
        None => Some((target, None, verb)),
        Some((project, database)) => {
            let database = database.strip_prefix("databases/")?;
            (!database.contains('/')).then_some((project, Some(database), verb))
        }
    }
}

/// The largest body read (as REST bodies).
const MAX_BODY: usize = 16 * 1024 * 1024;

pub(crate) async fn handle(
    rules: &Rules,
    (project, database, verb): (&str, Option<&str>, Verb),
    method: &Method,
    query: Option<&str>,
    headers: &HeaderMap,
    body: Body,
) -> Response {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok());
    let served = match verb {
        // The official emulator answers 404 to a form-encoded body.
        Verb::SecurityRules if method == Method::PUT => {
            !content_type.is_some_and(|v| v.starts_with("application/x-www-form-urlencoded"))
        }
        Verb::SecurityRules | Verb::RuleCoverage => method == Method::GET,
        Verb::RuleCoverageHtml => {
            method == Method::GET && content_type.is_none() && query.is_none()
        }
    };
    if !served {
        return crate::not_found_response();
    }
    if project.is_empty() && database.is_none() {
        return rest::error(&Status::unknown(""));
    }
    let database = match names::database(&format!(
        "projects/{project}/databases/{}",
        database.unwrap_or("(default)")
    )) {
        Ok(database) => database,
        Err(status) => return rest::error(&status),
    };
    if method == Method::PUT {
        return put(rules, &database, body).await;
    }
    let loaded = rules.get(&database);
    let open = || {
        vec![File {
            name: Some("OPEN_RULES".to_owned()),
            content: OPEN_RULES.to_owned(),
            fingerprint: Vec::new(),
        }]
    };
    let files = loaded
        .as_ref()
        .map_or_else(open, |l| l.compiled.files.clone());
    let coverage = loaded
        .as_ref()
        .map(|l| l.coverage.lock().unwrap_or_else(|e| e.into_inner()));
    let coverage = coverage.as_deref();
    let (content_type, body) = match verb {
        Verb::SecurityRules => ("application/json", report::security_rules(&files)),
        Verb::RuleCoverage => ("application/json", report::coverage(&files, coverage)),
        Verb::RuleCoverageHtml => ("text/html", report::coverage_html(&files, coverage)),
    };
    ([(header::CONTENT_TYPE, content_type)], body).into_response()
}

async fn put(rules: &Rules, database: &str, body: Body) -> Response {
    let Ok(body) = axum::body::to_bytes(body, MAX_BODY).await else {
        return rest::error(&invalid_payload());
    };
    let files = match parse(&body) {
        Ok(files) => files,
        Err(status) => return rest::error(&status),
    };
    let sources: Vec<SourceFile> = files
        .iter()
        .map(|f| SourceFile {
            name: f.name.clone(),
            content: f.content.clone(),
        })
        .collect();
    // Compiling runs on a thread of its own (`hidane_rules::compile`): off the runtime.
    let Ok(compilation) =
        tokio::task::spawn_blocking(move || hidane_rules::compile(&sources)).await
    else {
        return rest::error(&Status::internal("Compilation failed."));
    };
    if compilation.has_errors() {
        return rest::error(&Status::invalid_argument(compilation.error_message()));
    }
    let body = issues_json(&compilation.issues);
    if let Some(ruleset) = compilation.ruleset {
        rules.set(database, files, ruleset);
    }
    ([(header::CONTENT_TYPE, "application/json")], body).into_response()
}

fn invalid_payload() -> Status {
    Status::invalid_argument("Payload isn't valid for request.")
}

/// A fingerprint as protobuf-java reads bytes: base64, standard or URL-safe, padding optional.
fn fingerprint(text: &str) -> Result<Vec<u8>, Status> {
    let config = GeneralPurposeConfig::new()
        .with_decode_padding_mode(DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(true);
    GeneralPurpose::new(&alphabet::STANDARD, config)
        .decode(text)
        .or_else(|_| GeneralPurpose::new(&alphabet::URL_SAFE, config).decode(text))
        .map_err(|_| invalid_payload())
}

/// `{rules: {files: [{name, content, fingerprint}]}}`, as protobuf-java reads it. A request
/// without files is refused here.
fn parse(body: &[u8]) -> Result<Vec<File>, Status> {
    let json: Json = if body.iter().all(u8::is_ascii_whitespace) {
        Json::Null
    } else {
        serde_json::from_slice(body).map_err(|_| invalid_payload())?
    };
    let mut files = Vec::new();
    for (key, value) in object(json)? {
        if key != "rules" {
            return Err(invalid_payload());
        }
        for (key, value) in object(value)? {
            if key != "files" {
                return Err(invalid_payload());
            }
            let list = match value {
                Json::Null => Vec::new(),
                Json::Array(list) => list,
                _ => return Err(invalid_payload()),
            };
            for file in list {
                let mut source = File::default();
                for (key, value) in object(file)? {
                    let text = scalar(value)?;
                    match key.as_str() {
                        "name" => source.name = text,
                        "content" => source.content = text.unwrap_or_default(),
                        "fingerprint" => {
                            source.fingerprint = fingerprint(&text.unwrap_or_default())?;
                        }
                        _ => return Err(invalid_payload()),
                    }
                }
                files.push(source);
            }
        }
    }
    if files.is_empty() {
        return Err(invalid_payload());
    }
    Ok(files)
}

fn object(json: Json) -> Result<serde_json::Map<String, Json>, Status> {
    match json {
        Json::Null => Ok(serde_json::Map::new()),
        Json::Object(fields) => Ok(fields),
        _ => Err(invalid_payload()),
    }
}

/// A string field: `null` is unset, a number or a boolean its text.
fn scalar(json: Json) -> Result<Option<String>, Status> {
    match json {
        Json::Null => Ok(None),
        Json::String(s) => Ok(Some(s)),
        Json::Number(n) => Ok(Some(n.to_string())),
        Json::Bool(b) => Ok(Some(b.to_string())),
        Json::Array(_) | Json::Object(_) => Err(invalid_payload()),
    }
}

/// The success body in protobuf-java's JsonFormat layout: `{}` without warnings.
fn issues_json(issues: &[Issue]) -> String {
    if issues.is_empty() {
        return "{\n}\n".to_owned();
    }
    let printed: Vec<String> = issues
        .iter()
        .map(|issue| {
            let mut fields = Vec::new();
            if let Some(name) = &issue.file_name {
                fields.push(format!("      \"fileName\": {}", json_string(name)));
            }
            if let Some(p) = issue.position {
                fields.push(format!("      \"line\": {}", p.line));
                fields.push(format!("      \"column\": {}", p.column));
                fields.push(format!("      \"currentOffset\": {}", p.current_offset));
                fields.push(format!("      \"endOffset\": {}", p.end_offset));
            }
            let position = if fields.is_empty() {
                "{\n    }".to_owned()
            } else {
                format!("{{\n{}\n    }}", fields.join(",\n"))
            };
            let severity = match issue.severity {
                Severity::Warning => "WARNING",
                Severity::Error => "ERROR",
            };
            format!(
                "{{\n    \"sourcePosition\": {position},\n    \"description\": {},\n    \
                 \"severity\": \"{severity}\"\n  }}",
                json_string(&issue.description)
            )
        })
        .collect();
    format!("{{\n  \"issues\": [{}]\n}}\n", printed.join(", "))
}

/// A JSON string as protobuf-java's JsonFormat writes it.
pub(crate) fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if u32::from(c) < 0x20 || c == '\u{2028}' || c == '\u{2029}' => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `--rules`: compiles the file at startup. Returns the lines to print and, if it compiled,
/// the file's content and the ruleset (for [`Rules::set_default`]).
pub fn load(path: &std::path::Path) -> (Vec<String>, Option<(String, Ruleset)>) {
    let name = path.display().to_string();
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) => return (vec![format!("ERROR: could not read {name}: {err}")], None),
    };
    let compilation: Compilation = hidane_rules::compile(&[SourceFile {
        name: Some(name.clone()),
        content: content.clone(),
    }]);
    let lines = compilation
        .issues
        .iter()
        .map(|issue| {
            let level = match issue.severity {
                Severity::Warning => "WARNING",
                Severity::Error => "ERROR",
            };
            match issue.position {
                Some(p) => format!(
                    "{level}: {name}:{}:{}: {}",
                    p.line, p.column, issue.description
                ),
                None => format!("{level}: {name}: {}", issue.description),
            }
        })
        .collect();
    (lines, compilation.ruleset.map(|ruleset| (content, ruleset)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes() {
        let rules = Some(("p", None, Verb::SecurityRules));
        assert_eq!(route("/emulator/v1/projects/p:securityRules"), rules);
        assert_eq!(
            route("/emulator/v1/projects/:securityRules"),
            Some(("", None, Verb::SecurityRules))
        );
        assert_eq!(
            route("/emulator/v1/projects/p/databases/d:ruleCoverage"),
            Some(("p", Some("d"), Verb::RuleCoverage))
        );
        assert_eq!(
            route("/emulator/v1/projects/p:ruleCoverage.html"),
            Some(("p", None, Verb::RuleCoverageHtml))
        );
        assert_eq!(route("/emulator/v1/projects/p:securityRules/"), None);
        assert_eq!(route("/emulator/v1/projects/p:ruleCoverage/html"), None);
        assert_eq!(route("/emulator/v1/projects/p/x/y:ruleCoverage"), None);
        assert_eq!(
            route("/emulator/v1/projects/p/databases/d/documents:ruleCoverage"),
            None
        );
        assert_eq!(route("/emulator/v1/projects/p:export"), None);
    }

    #[test]
    fn bodies() {
        let files =
            parse(br#"{"rules": {"files": [{"name": 1, "content": "x", "fingerprint": "-_8"}]}}"#)
                .unwrap();
        assert_eq!(files[0].name.as_deref(), Some("1"));
        assert_eq!(files[0].content, "x");
        assert_eq!(files[0].fingerprint, b"\xfb\xff");
        for bad in [
            &b"not json"[..],
            b"[]",
            b"{}",
            b"{\"rules\": null}",
            b"{\"rules\": {\"files\": []}}",
            b"{\"rules\": {\"files\": [{\"x\": 1}]}}",
            b"{\"rules\": {\"files\": {}}}",
            b"{\"rules\": {}, \"x\": 1}",
            b"{\"rules\": {\"files\": [{\"content\": \"x\", \"fingerprint\": \"!!!\"}]}}",
            b"{\"rules\": {\"files\": [{\"content\": \"x\", \"fingerprint\": 5}]}}",
        ] {
            assert!(parse(bad).is_err(), "{}", String::from_utf8_lossy(bad));
        }
    }

    #[test]
    fn json_strings_escape_as_jsonformat_does() {
        assert_eq!(
            json_string("a<b>&='\u{e9}\u{2028}/\u{1}\"\\"),
            "\"a<b>&='\u{e9}\\u2028/\\u0001\\\"\\\\\""
        );
    }
}
