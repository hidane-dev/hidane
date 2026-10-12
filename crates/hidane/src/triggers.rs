//! Firestore triggers, as firebase-tools' Functions emulator registers them when functions with
//! Firestore triggers load (`functionsEmulator.js`, `addFirestoreTrigger`); a registration that
//! fails stops the whole emulator suite.
//!
//! - v1 (`functions.firestore.document(…)`): `PUT /emulator/v1/projects/{p}/triggers/{key}` with
//!   `{"eventTrigger": {eventType, resource, service}}`, answered `{}`;
//! - v2 (`onDocumentCreated` and the like): `POST
//!   /emulator/v1/projects/{p}/eventarcTrigger?eventarcTriggerId={key}` with `{eventType,
//!   database, namespace, document: {value, matchType}}`, answered with the trigger as read.
//!
//! As the official emulator answers (`tests/fixtures/function_triggers.json`), with or without
//! `--functions_emulator`:
//!
//! - other methods, other paths and form content types are `404`; the content type is not
//!   otherwise checked, and the query string is ignored but for `eventarcTriggerId` (once, in
//!   either spelling) and `parent` (already in the path);
//! - the body is read as protobuf-java's JSON parser reads it: an empty body is `{}`; a string
//!   field takes a number or a boolean as its text, `null` as empty, and an array of one value
//!   as that value; an enum takes its name or its number; any other key or shape, or a field in
//!   both spellings, is `400` "Payload isn't valid for request.";
//! - then the names, the `Authorization` header (as `firestore::auth` reads it) and the trigger
//!   are checked, in that order. A name or a trigger that fails is a `500` without a message
//!   (the official emulator's log gives the reason): the project must be a project ID (whitespace
//!   around it is ignored), the v1 key must not be blank; v1 triggers must have the service
//!   `firestore.googleapis.com` and a v1 Firestore event type; v2 triggers a v2 Firestore event
//!   type, the namespace `(default)` if any, a database ID if any, and a match type for a
//!   document value. Nothing else is checked: the project and the database need not exist, the
//!   resource of a v1 trigger is not read, and registering a key again replaces its trigger.
//!
//! Registered triggers are kept per project, in the order they were first registered
//! ([`Triggers::registered`]), also across `POST /reset`: the Functions emulator registers each
//! trigger once, when its functions load. (The official emulator forgets them on a reset, and
//! sends no event afterwards: docs/parity-exceptions.md.) Each commit then sends an event to
//! every trigger that matches a document it changed (`events`), one at a time, in the
//! background (`delivery`).

mod delivery;
mod events;

use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
};

use axum::{
    body::Body,
    http::{HeaderMap, Method, header},
    response::{IntoResponse, Response},
};
use percent_encoding::percent_decode_str;
use serde_json::{Map, Value as Json};
use tonic::Status;

use crate::{firestore::auth, rest, rules::json_string};

/// Where the `WARNING`s about events go: one record at a time, its lines joined by line breaks,
/// without the `WARNING: ` that standard error shows before the first.
pub type Log = Arc<dyn Fn(&str) + Send + Sync>;

/// The largest body read (as REST bodies).
const MAX_BODY: usize = 16 * 1024 * 1024;

const SERVICE: &str = "firestore.googleapis.com";

const V1_EVENT_TYPES: [&str; 4] = [
    "providers/cloud.firestore/eventTypes/document.create",
    "providers/cloud.firestore/eventTypes/document.delete",
    "providers/cloud.firestore/eventTypes/document.update",
    "providers/cloud.firestore/eventTypes/document.write",
];

const V2_EVENT_TYPES: [&str; 8] = [
    "google.cloud.firestore.document.v1.created",
    "google.cloud.firestore.document.v1.deleted",
    "google.cloud.firestore.document.v1.updated",
    "google.cloud.firestore.document.v1.written",
    "google.cloud.firestore.document.v1.created.withAuthContext",
    "google.cloud.firestore.document.v1.deleted.withAuthContext",
    "google.cloud.firestore.document.v1.updated.withAuthContext",
    "google.cloud.firestore.document.v1.written.withAuthContext",
];

/// A v1 trigger (`google.firestore.emulator.v1.EventTrigger`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventTrigger {
    pub event_type: String,
    /// `projects/{p}/databases/{d}/documents/{path pattern}`, not checked.
    pub resource: String,
    pub service: String,
}

/// A v2 trigger (`google.firestore.emulator.v1.EventarcTrigger`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventarcTrigger {
    pub event_type: String,
    /// Empty when not given.
    pub database: String,
    pub namespace: String,
    pub document: Option<Document>,
}

/// The documents a v2 trigger watches.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Document {
    pub value: String,
    pub match_type: MatchType,
}

/// `EventarcTrigger.Document.MatchType`: 1 is `EXACT`, 2 `PATH_PATTERN`; other numbers are kept
/// as they came.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MatchType(pub i32);

impl MatchType {
    pub const EXACT: Self = Self(1);
    pub const PATH_PATTERN: Self = Self(2);

    fn name(self) -> Option<&'static str> {
        match self.0 {
            0 => Some("MATCH_TYPE_UNSPECIFIED"),
            1 => Some("EXACT"),
            2 => Some("PATH_PATTERN"),
            _ => None,
        }
    }
}

/// A registered trigger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trigger {
    V1(EventTrigger),
    V2(EventarcTrigger),
}

/// The triggers of each project, v1 and v2 apart, each in the order its key was first
/// registered.
#[derive(Default)]
struct Registry {
    v1: BTreeMap<String, Vec<(String, EventTrigger)>>,
    v2: BTreeMap<String, Vec<(String, EventarcTrigger)>>,
}

/// Puts `trigger` under `key`, in the place of an earlier trigger of the same key if any.
fn put<T>(list: &mut Vec<(String, T)>, key: &str, trigger: T) {
    match list.iter_mut().find(|(k, _)| k == key) {
        Some((_, old)) => *old = trigger,
        None => list.push((key.to_owned(), trigger)),
    }
}

/// The triggers registered, and where their events go.
#[derive(Default)]
pub struct Triggers {
    registry: RwLock<Registry>,
    delivery: delivery::Delivery,
}

impl Triggers {
    /// The triggers of `project`, v1 first, each in the order its key was first registered.
    pub fn registered(&self, project: &str) -> Vec<(String, Trigger)> {
        let registry = self.registry.read().unwrap_or_else(|e| e.into_inner());
        let v1 = registry.v1.get(project).into_iter().flatten();
        let v2 = registry.v2.get(project).into_iter().flatten();
        v1.map(|(key, trigger)| (key.clone(), Trigger::V1(trigger.clone())))
            .chain(v2.map(|(key, trigger)| (key.clone(), Trigger::V2(trigger.clone()))))
            .collect()
    }

    /// `--functions_emulator`: the host and port of the Functions emulator, where events go.
    /// Without one, each event is a `WARNING`, as on the official emulator.
    pub fn set_functions_emulator(&self, host: Option<String>) {
        self.delivery.set_host(host);
    }

    /// Sends the `WARNING`s about events to `log` instead of standard error.
    pub fn set_log(&self, log: Log) {
        self.delivery.set_log(log);
    }

    /// Resolves once every event sent so far has been answered, or has failed.
    pub async fn delivered(&self) {
        self.delivery.idle().await;
    }

    fn register(&self, project: &str, key: &str, trigger: Trigger) {
        let mut registry = self.registry.write().unwrap_or_else(|e| e.into_inner());
        match trigger {
            Trigger::V1(trigger) => put(
                registry.v1.entry(project.to_owned()).or_default(),
                key,
                trigger,
            ),
            Trigger::V2(trigger) => put(
                registry.v2.entry(project.to_owned()).or_default(),
                key,
                trigger,
            ),
        }
    }
}

/// A registration's path.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Route {
    /// `/emulator/v1/projects/{project}/triggers/{key}`.
    V1 { project: String, key: String },
    /// `/emulator/v1/projects/{project}/eventarcTrigger`.
    V2 { project: String },
}

/// The registration a path names. Segments are percent-decoded; one that holds a `/` then
/// names nothing, as on the official emulator.
pub(crate) fn route(path: &str) -> Option<Route> {
    let rest = path.strip_prefix("/emulator/v1/projects/")?;
    let decode = |segment: &str| {
        let decoded = percent_decode_str(segment).decode_utf8_lossy();
        (!decoded.contains('/')).then(|| decoded.into_owned())
    };
    match rest.split('/').collect::<Vec<_>>().as_slice() {
        [project, "triggers", key] => Some(Route::V1 {
            project: decode(project)?,
            key: decode(key)?,
        }),
        [project, "eventarcTrigger"] => Some(Route::V2 {
            project: decode(project)?,
        }),
        _ => None,
    }
}

pub(crate) async fn handle(
    triggers: &Triggers,
    route: Route,
    method: &Method,
    query: Option<&str>,
    headers: &HeaderMap,
    body: Body,
) -> Response {
    let expected = match route {
        Route::V1 { .. } => Method::PUT,
        Route::V2 { .. } => Method::POST,
    };
    let form = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_ascii_lowercase)
        .is_some_and(|v| {
            v.starts_with("application/x-www-form-urlencoded")
                || v.starts_with("multipart/form-data")
        });
    if *method != expected || form {
        return crate::not_found_response();
    }
    let Ok(body) = axum::body::to_bytes(body, MAX_BODY).await else {
        return rest::error(&invalid_payload());
    };
    match route {
        Route::V1 { project, key } => {
            let result = parse_v1(&body).and_then(|trigger| {
                let project = project_id(&project)?;
                if key.trim().is_empty() {
                    return Err(failed());
                }
                auth::from_header(crate::authorization(headers))?;
                check_v1(&trigger)?;
                Ok((project, trigger))
            });
            match result {
                Ok((project, trigger)) => {
                    // The key as given: it names the function to call (#70).
                    triggers.register(project, &key, Trigger::V1(trigger));
                    crate::empty_json()
                }
                Err(status) => rest::error(&status),
            }
        }
        Route::V2 { project } => {
            let result = trigger_id(query.unwrap_or_default()).and_then(|key| {
                let trigger = parse_v2(&body)?;
                let project = project_id(&project)?;
                auth::from_header(crate::authorization(headers))?;
                check_v2(&trigger)?;
                Ok((project, key, trigger))
            });
            match result {
                Ok((project, key, trigger)) => {
                    let printed = print_v2(&trigger);
                    triggers.register(project, &key, Trigger::V2(trigger));
                    ([(header::CONTENT_TYPE, "application/json")], printed).into_response()
                }
                Err(status) => rest::error(&status),
            }
        }
    }
}

fn invalid_payload() -> Status {
    Status::invalid_argument("Payload isn't valid for request.")
}

/// What the official emulator answers for a registration it refuses after reading it: a 500
/// without a message.
fn failed() -> Status {
    Status::unknown("")
}

/// `eventarcTriggerId` (or `eventarc_trigger_id`), at most once; `parent` is in the path
/// already. Other parameters are ignored.
fn trigger_id(query: &str) -> Result<String, Status> {
    let mut id = None;
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let decode = |s: &str| {
            percent_decode_str(&s.replace('+', " "))
                .decode_utf8_lossy()
                .into_owned()
        };
        match decode(key).as_str() {
            "eventarcTriggerId" | "eventarc_trigger_id" if id.is_none() => id = Some(decode(value)),
            "eventarcTriggerId" | "eventarc_trigger_id" | "parent" => {
                return Err(invalid_payload());
            }
            _ => {}
        }
    }
    Ok(id.unwrap_or_default())
}

/// A project ID, around which whitespace is ignored: lower-case letters, digits and hyphens,
/// not starting with a hyphen, after an optional domain and colon (`example.com:my-project`);
/// at most 100 characters.
fn project_id(project: &str) -> Result<&str, Status> {
    let project = project.trim();
    let alnum = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
    let part = |s: &str, extra: &[char]| {
        s.chars().next().is_none_or(alnum) && s.chars().all(|c| alnum(c) || extra.contains(&c))
    };
    let (domain, id) = project.split_once(':').unwrap_or(("", project));
    let valid = !project.is_empty()
        && project.chars().count() <= 100
        && part(domain, &['.', '-'])
        && part(id, &['-']);
    if valid { Ok(project) } else { Err(failed()) }
}

/// Empty (none given), `(default)`, or letters, digits, `.`, `_` and `-`, at most 100.
fn database_id(database: &str) -> bool {
    database.is_empty()
        || database == "(default)"
        || (database.len() <= 100
            && database
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
}

fn check_v1(trigger: &EventTrigger) -> Result<(), Status> {
    if trigger.service != SERVICE || !V1_EVENT_TYPES.contains(&trigger.event_type.as_str()) {
        return Err(failed());
    }
    Ok(())
}

fn check_v2(trigger: &EventarcTrigger) -> Result<(), Status> {
    let matched = trigger.document.as_ref().is_none_or(|document| {
        document.value.is_empty()
            || matches!(
                document.match_type,
                MatchType::EXACT | MatchType::PATH_PATTERN
            )
    });
    if !V2_EVENT_TYPES.contains(&trigger.event_type.as_str())
        || !matches!(trigger.namespace.as_str(), "" | "(default)")
        || !database_id(&trigger.database)
        || !matched
    {
        return Err(failed());
    }
    Ok(())
}

// --- reading, as protobuf-java's JsonFormat reads JSON ------------------------------------------

/// The body as a JSON object: empty is `{}`.
fn object(body: &[u8]) -> Result<Map<String, Json>, Status> {
    if body.is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_slice(body) {
        Ok(Json::Object(fields)) => Ok(fields),
        _ => Err(invalid_payload()),
    }
}

/// The fields of a message, each a JSON name and a field name, read from `fields`: an unknown
/// key, or a field given in both spellings, is an invalid payload. `null` is left out.
fn fields<'a>(
    fields: &'a Map<String, Json>,
    names: &[(&str, &str)],
) -> Result<Vec<(usize, &'a Json)>, Status> {
    let mut seen = vec![false; names.len()];
    let mut out = Vec::new();
    for (key, value) in fields {
        let index = names
            .iter()
            .position(|(json, proto)| key == json || key == proto)
            .ok_or_else(invalid_payload)?;
        if std::mem::replace(&mut seen[index], true) {
            return Err(invalid_payload());
        }
        if !value.is_null() {
            out.push((index, value));
        }
    }
    Ok(out)
}

/// A string field's value: a number or a boolean as its text, an array of one value as that
/// value.
fn string(value: &Json) -> Result<String, Status> {
    match value {
        Json::String(s) => Ok(s.clone()),
        Json::Number(n) => Ok(n.to_string()),
        Json::Bool(b) => Ok(b.to_string()),
        Json::Array(items) if items.len() == 1 => string(&items[0]),
        _ => Err(invalid_payload()),
    }
}

/// An enum field's value: a name, or a 32-bit number (`"2"` and `2.0` too).
fn match_type(value: &Json) -> Result<MatchType, Status> {
    let text = string(value)?;
    if let Some(n) = (0..=2).find(|&n| MatchType(n).name() == Some(text.as_str())) {
        return Ok(MatchType(n));
    }
    if let Ok(n) = text.parse::<i32>() {
        return Ok(MatchType(n));
    }
    match text.parse::<f64>() {
        Ok(f) if f.fract() == 0.0 && f >= f64::from(i32::MIN) && f <= f64::from(i32::MAX) => {
            Ok(MatchType(f as i32))
        }
        _ => Err(invalid_payload()),
    }
}

fn message(value: &Json) -> Result<&Map<String, Json>, Status> {
    value.as_object().ok_or_else(invalid_payload)
}

/// `{"eventTrigger": {eventType, resource, service}}`.
fn parse_v1(body: &[u8]) -> Result<EventTrigger, Status> {
    let body = object(body)?;
    let mut trigger = EventTrigger::default();
    for (_, value) in fields(&body, &[("eventTrigger", "event_trigger")])? {
        for (index, value) in fields(
            message(value)?,
            &[
                ("eventType", "event_type"),
                ("resource", "resource"),
                ("service", "service"),
            ],
        )? {
            let value = string(value)?;
            match index {
                0 => trigger.event_type = value,
                1 => trigger.resource = value,
                _ => trigger.service = value,
            }
        }
    }
    Ok(trigger)
}

/// `{eventType, database, namespace, document: {value, matchType}}`.
fn parse_v2(body: &[u8]) -> Result<EventarcTrigger, Status> {
    let body = object(body)?;
    let mut trigger = EventarcTrigger::default();
    for (index, value) in fields(
        &body,
        &[
            ("eventType", "event_type"),
            ("database", "database"),
            ("namespace", "namespace"),
            ("document", "document"),
        ],
    )? {
        match index {
            0 => trigger.event_type = string(value)?,
            1 => trigger.database = string(value)?,
            2 => trigger.namespace = string(value)?,
            _ => {
                let mut document = Document::default();
                for (index, value) in fields(
                    message(value)?,
                    &[("value", "value"), ("matchType", "match_type")],
                )? {
                    match index {
                        0 => document.value = string(value)?,
                        _ => document.match_type = match_type(value)?,
                    }
                }
                trigger.document = Some(document);
            }
        }
    }
    Ok(trigger)
}

// --- printing, as protobuf-java's JsonFormat prints a message ------------------------------------

/// The trigger as the official emulator answers it: two-space indentation, fields in their
/// order, empty strings and an unset document left out, a newline at the end.
fn print_v2(trigger: &EventarcTrigger) -> String {
    let mut entries = Vec::new();
    for (name, value) in [
        ("eventType", &trigger.event_type),
        ("database", &trigger.database),
        ("namespace", &trigger.namespace),
    ] {
        if !value.is_empty() {
            entries.push(format!("  \"{name}\": {}", json_string(value)));
        }
    }
    if let Some(document) = &trigger.document {
        let mut inner = Vec::new();
        if !document.value.is_empty() {
            inner.push(format!("    \"value\": {}", json_string(&document.value)));
        }
        if document.match_type.0 != 0 {
            let printed = match document.match_type.name() {
                Some(name) => format!("\"{name}\""),
                None => document.match_type.0.to_string(),
            };
            inner.push(format!("    \"matchType\": {printed}"));
        }
        entries.push(if inner.is_empty() {
            "  \"document\": {\n  }".to_owned()
        } else {
            format!("  \"document\": {{\n{}\n  }}", inner.join(",\n"))
        });
    }
    if entries.is_empty() {
        "{\n}\n".to_owned()
    } else {
        format!("{{\n{}\n}}\n", entries.join(",\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes() {
        assert_eq!(
            route("/emulator/v1/projects/p/triggers/k"),
            Some(Route::V1 {
                project: "p".into(),
                key: "k".into()
            })
        );
        assert_eq!(
            route("/emulator/v1/projects/p/triggers/a%20b"),
            Some(Route::V1 {
                project: "p".into(),
                key: "a b".into()
            })
        );
        assert_eq!(
            route("/emulator/v1/projects//eventarcTrigger"),
            Some(Route::V2 {
                project: String::new()
            })
        );
        for path in [
            "/emulator/v1/projects/p/triggers",
            "/emulator/v1/projects/p/triggers/k/",
            "/emulator/v1/projects/p/triggers/a%2Fb",
            "/emulator/v1/projects/a%2Fb/triggers/k",
            "/emulator/v1/projects/p/databases/d/triggers/k",
            "/emulator/v1/projects/p/eventarcTrigger/",
            "/emulator/v1/projects/p:securityRules",
        ] {
            assert_eq!(route(path), None, "{path}");
        }
    }

    #[test]
    fn project_ids() {
        for ok in [
            "demo-p",
            "a",
            "1",
            ":",
            "a-",
            "a:",
            ":a",
            "a.:b",
            "example.com:my-project",
            " a ",
            &"p".repeat(100),
        ] {
            assert!(project_id(ok).is_ok(), "{ok:?}");
        }
        for bad in [
            "",
            " ",
            "-",
            ".",
            "A",
            "a_b",
            "a.b",
            "-a",
            "::",
            ":-",
            "a:b:c",
            ".a:b",
            "a b",
            "é",
            &"p".repeat(101),
        ] {
            assert!(project_id(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn strings_and_enums_are_read_leniently() {
        assert_eq!(string(&serde_json::json!(["x"])).unwrap(), "x");
        assert!(string(&serde_json::json!(["x", "y"])).is_err());
        assert!(string(&serde_json::json!([null])).is_err());
        assert_eq!(match_type(&serde_json::json!("2")).unwrap(), MatchType(2));
        assert_eq!(match_type(&serde_json::json!(1.0)).unwrap(), MatchType(1));
        assert_eq!(match_type(&serde_json::json!(-7)).unwrap(), MatchType(-7));
        assert!(match_type(&serde_json::json!(1.5)).is_err());
        assert!(match_type(&serde_json::json!("exact")).is_err());
    }
}
