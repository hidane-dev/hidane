//! Security Rules: `PUT /emulator/v1/projects/{p}:securityRules`, how firebase-tools and
//! rules-unit-testing load a ruleset, and `--rules`.
//!
//! The ruleset is compiled by `hidane_rules` and kept per project; requests are not checked
//! against it yet (#60, #62). The answers are the official emulator's
//! (`tests/fixtures/rules_compile.json`): `200` with the warnings (`issues[]`) or `{}`, or
//! `400` "Error compiling rules:" with one line per issue. A ruleset that does not compile
//! leaves the project's previous one in place.
//!
//! The body is read as protobuf-java's JSON parser reads `{rules: {files: [{name, content,
//! fingerprint}]}}`: a number or a boolean as text, `null` as empty, any other key or shape
//! "Payload isn't valid for request.". The official emulator answers a body without files with
//! a 500 (a crash); hidane answers it as an invalid payload (`docs/parity-exceptions.md`).

use std::{
    collections::HashMap,
    fmt::Write as _,
    sync::{Arc, RwLock},
};

use axum::{
    body::Body,
    http::{HeaderMap, Method, header},
    response::{IntoResponse, Response},
};
use hidane_rules::{Compilation, Issue, Ruleset, Severity, SourceFile};
use serde_json::Value as Json;
use tonic::Status;

use crate::rest;

/// The compiled rulesets: one per project loaded through `:securityRules`, and the `--rules`
/// file for the others.
#[derive(Default)]
pub struct Rules {
    projects: RwLock<HashMap<String, Arc<Ruleset>>>,
    default: RwLock<Option<Arc<Ruleset>>>,
}

impl Rules {
    /// The ruleset that applies to `project`.
    pub fn get(&self, project: &str) -> Option<Arc<Ruleset>> {
        let projects = self.projects.read().unwrap_or_else(|e| e.into_inner());
        projects.get(project).cloned().or_else(|| {
            self.default
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        })
    }

    pub fn set(&self, project: &str, ruleset: Ruleset) {
        self.projects
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(project.to_owned(), Arc::new(ruleset));
    }

    /// The `--rules` file's ruleset, for projects that loaded none.
    pub fn set_default(&self, ruleset: Ruleset) {
        *self.default.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(ruleset));
    }
}

/// `/emulator/v1/projects/{p}:securityRules`: the project.
pub(crate) fn route(path: &str) -> Option<&str> {
    let project = path
        .strip_prefix("/emulator/v1/projects/")?
        .strip_suffix(":securityRules")?;
    (!project.is_empty() && !project.contains('/')).then_some(project)
}

/// The largest body read (as REST bodies).
const MAX_BODY: usize = 16 * 1024 * 1024;

pub(crate) async fn handle(
    rules: &Rules,
    project: &str,
    method: &Method,
    headers: &HeaderMap,
    body: Body,
) -> Response {
    // The official emulator answers 404 to anything but PUT, and to a form-encoded body.
    let form = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/x-www-form-urlencoded"));
    if method != Method::PUT || form {
        return crate::not_found_response();
    }
    let Ok(body) = axum::body::to_bytes(body, MAX_BODY).await else {
        return rest::error(&invalid_payload());
    };
    let files = match parse(&body) {
        Ok(files) => files,
        Err(status) => return rest::error(&status),
    };
    // Compiling runs on a thread of its own (`hidane_rules::compile`): off the runtime.
    let Ok(compilation) = tokio::task::spawn_blocking(move || hidane_rules::compile(&files)).await
    else {
        return rest::error(&Status::internal("Compilation failed."));
    };
    if compilation.has_errors() {
        return rest::error(&Status::invalid_argument(compilation.error_message()));
    }
    let body = issues_json(&compilation.issues);
    if let Some(ruleset) = compilation.ruleset {
        rules.set(project, ruleset);
    }
    ([(header::CONTENT_TYPE, "application/json")], body).into_response()
}

fn invalid_payload() -> Status {
    Status::invalid_argument("Payload isn't valid for request.")
}

/// `{rules: {files: [{name, content, fingerprint}]}}`, as protobuf-java reads it. A request
/// without files is refused here.
fn parse(body: &[u8]) -> Result<Vec<SourceFile>, Status> {
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
                let mut source = SourceFile::default();
                for (key, value) in object(file)? {
                    let text = scalar(value)?;
                    match key.as_str() {
                        "name" => source.name = text,
                        "content" => source.content = text.unwrap_or_default(),
                        "fingerprint" => {}
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
fn json_string(s: &str) -> String {
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

/// `--rules`: compiles the file at startup. Returns the lines to print and the ruleset, if it
/// compiled.
pub fn load(path: &std::path::Path) -> (Vec<String>, Option<Ruleset>) {
    let name = path.display().to_string();
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) => return (vec![format!("ERROR: could not read {name}: {err}")], None),
    };
    let compilation: Compilation = hidane_rules::compile(&[SourceFile {
        name: Some(name.clone()),
        content,
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
    (lines, compilation.ruleset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes() {
        assert_eq!(route("/emulator/v1/projects/p:securityRules"), Some("p"));
        assert_eq!(route("/emulator/v1/projects/:securityRules"), None);
        assert_eq!(
            route("/emulator/v1/projects/p/databases/d:securityRules"),
            None
        );
        assert_eq!(route("/emulator/v1/projects/p:securityRules/"), None);
        assert_eq!(route("/emulator/v1/projects/p:export"), None);
    }

    #[test]
    fn bodies() {
        let files =
            parse(br#"{"rules": {"files": [{"name": 1, "content": "x", "fingerprint": "AA=="}]}}"#)
                .unwrap();
        assert_eq!(files[0].name.as_deref(), Some("1"));
        assert_eq!(files[0].content, "x");
        for bad in [
            &b"not json"[..],
            b"[]",
            b"{}",
            b"{\"rules\": null}",
            b"{\"rules\": {\"files\": []}}",
            b"{\"rules\": {\"files\": [{\"x\": 1}]}}",
            b"{\"rules\": {\"files\": {}}}",
            b"{\"rules\": {}, \"x\": 1}",
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
