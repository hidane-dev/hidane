//! Which triggers a commit's changes reach, and what each of them is sent, as the official
//! emulator does (`tests/fixtures/function_events.json`, from `tools/oracle/function_events.py`).
//!
//! A commit that creates, updates or deletes documents sends, for each of them in the order
//! the commit's writes name them, an event to every trigger of the project that watches it:
//! first to the v1 triggers, document by document, then to the v2 triggers. Within a document,
//! triggers come in the order of the official emulator's map of them, a Java `HashMap` sized
//! for the project's triggers of that version and keyed by their names
//! ([`presized_hash_order`]).
//!
//! - A v1 trigger watches the documents of the `(default)` database whose name its resource
//!   matches; `document.write` fires on every change, the others on theirs. The JSON body holds
//!   the documents before (`oldValue`) and after (`value`), an empty `updateMask`, and a
//!   context whose `eventId` all the v1 triggers of a change share.
//! - A v2 trigger watches the documents of its database (`(default)` when it names none) that
//!   its document matches, exactly or as a path pattern (`{id}` and `*` match a segment, `**`
//!   and `{id=**}` any number of them); `written` fires on every change. The body is a
//!   `DocumentEventData` (`value`, `old_value` and an empty `update_mask`), encoded in protobuf
//!   then base64; the CloudEvent attributes travel in `ce-*` headers ([`super::delivery`]).
//!
//! Unlike the official emulator, a write that changes nothing and the delete of a document that
//! does not exist send no v2 event (the official emulator sends `updated`, and `created`, for
//! them; production sends none, nor does the official emulator to v1 triggers). Clearing a
//! database and deleting a path send deletes; `POST /reset`, imports and seeds send nothing.

use std::{
    hash::{BuildHasher, Hasher},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use hidane_core::{
    hash_order::presized_hash_order,
    store::{Change, ReadTime, StoredDocument},
};
use hidane_proto::google::firestore::v1::{Document, DocumentMask};
use prost::Message as _;

use super::{
    EventTrigger, EventarcTrigger, MatchType, Triggers,
    delivery::{Event, Payload},
};
use crate::{firestore::names::Name, rest};

/// `google.events.cloud.firestore.v1.DocumentEventData` (googleapis/google-cloudevents,
/// Apache-2.0). Its `Document`, `Value` and `DocumentMask` are wire-compatible with
/// `google.firestore.v1`'s.
#[derive(Clone, PartialEq, prost::Message)]
pub struct DocumentEventData {
    #[prost(message, optional, tag = "1")]
    pub value: Option<Document>,
    #[prost(message, optional, tag = "2")]
    pub old_value: Option<Document>,
    #[prost(message, optional, tag = "3")]
    pub update_mask: Option<DocumentMask>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Created,
    Updated,
    Deleted,
}

impl Kind {
    fn of(change: &Change) -> Option<Self> {
        match (&change.before, &change.after) {
            (None, Some(_)) => Some(Self::Created),
            (Some(_), Some(_)) => Some(Self::Updated),
            (Some(_), None) => Some(Self::Deleted),
            (None, None) => None,
        }
    }

    /// The last word of the v1 event type that fires on this change only.
    fn v1(self) -> &'static str {
        match self {
            Self::Created => "create",
            Self::Updated => "update",
            Self::Deleted => "delete",
        }
    }

    /// The last word of the v2 event type that fires on this change only.
    fn v2(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Updated => "updated",
            Self::Deleted => "deleted",
        }
    }
}

impl Triggers {
    /// Sends the events of a commit to `database` at `commit_time`: `changes`, in the order the
    /// commit's writes name their documents. Called with the database's commit lock held, so
    /// events leave in the order of the commits.
    pub(crate) fn committed(&self, database: &str, commit_time: ReadTime, changes: &[&Change]) {
        let Some((project, database_id)) = split(database) else {
            return;
        };
        let events = {
            let registry = self.registry.read().unwrap_or_else(|e| e.into_inner());
            let v1 = registry
                .v1
                .get(project)
                .filter(|_| database_id == "(default)");
            let v2 = registry.v2.get(project);
            if v1.is_none() && v2.is_none() {
                return;
            }
            let mut events = Vec::new();
            if let Some(v1) = v1 {
                v1_events(project, database, commit_time, v1, changes, &mut events);
            }
            if let Some(v2) = v2 {
                v2_events(project, database, database_id, v2, changes, &mut events);
            }
            events
        };
        self.delivery.send(events);
    }

    /// Whether a commit to `database` may send events, to skip the work when it cannot.
    pub(crate) fn watches(&self, database: &str) -> bool {
        let Some((project, _)) = split(database) else {
            return false;
        };
        let registry = self.registry.read().unwrap_or_else(|e| e.into_inner());
        registry.v1.contains_key(project) || registry.v2.contains_key(project)
    }
}

/// `projects/{p}/databases/{d}` → `(p, d)`.
fn split(database: &str) -> Option<(&str, &str)> {
    let rest = database.strip_prefix("projects/")?;
    let (project, id) = rest.split_once("/databases/")?;
    Some((project, id))
}

/// The positions of `triggers` in the order the official emulator delivers to them.
fn delivery_order<T>(triggers: &[(String, T)], name: impl Fn(&str) -> String) -> Vec<usize> {
    let names: Vec<String> = triggers.iter().map(|(key, _)| name(key)).collect();
    presized_hash_order(&names)
}

fn v1_events(
    project: &str,
    database: &str,
    commit_time: ReadTime,
    triggers: &[(String, EventTrigger)],
    changes: &[&Change],
    events: &mut Vec<Event>,
) {
    let order = delivery_order(triggers, |key| format!("projects/{project}/triggers/{key}"));
    for change in changes {
        let Some(kind) = Kind::of(change) else {
            continue;
        };
        let name = Name::document_name(database, &change.path);
        let mut shared = None;
        for &i in &order {
            let (key, trigger) = &triggers[i];
            let fires = match trigger.event_type.rsplit_once('.') {
                Some((_, "write")) => true,
                Some((_, last)) => last == kind.v1(),
                None => false,
            };
            if !fires || !resource_matches(&trigger.resource, &name) {
                continue;
            }
            let (event_id, data) = shared.get_or_insert_with(|| {
                let data = v1_data(database, change);
                (uuid(), data)
            });
            // The context's strings are not escaped for HTML, unlike the documents'.
            let body = format!(
                "{{\"data\":{data},\"context\":{{\"eventId\":{},\"timestamp\":{},\"eventType\":{},\
                 \"resource\":{{\"name\":{},\"service\":\"firestore.googleapis.com\"}}}}}}",
                rest::json_string(event_id),
                rest::json_string(&time(commit_time)),
                rest::json_string(&trigger.event_type),
                rest::json_string(&name),
            );
            events.push(Event {
                project: project.to_owned(),
                key: key.clone(),
                payload: Payload::V1(body),
            });
        }
    }
}

fn v2_events(
    project: &str,
    database: &str,
    database_id: &str,
    triggers: &[(String, EventarcTrigger)],
    changes: &[&Change],
    events: &mut Vec<Event>,
) {
    let order = delivery_order(triggers, |key| {
        format!("projects/{project}/eventarcTriggers/{key}")
    });
    for change in changes {
        let Some(kind) = Kind::of(change) else {
            continue;
        };
        let path = change.path.to_string();
        let mut data: Option<Arc<str>> = None;
        for &i in &order {
            let (key, trigger) = &triggers[i];
            let base = trigger
                .event_type
                .strip_suffix(".withAuthContext")
                .unwrap_or(&trigger.event_type);
            let fires = match base.rsplit_once('.') {
                Some((_, "written")) => true,
                Some((_, last)) => last == kind.v2(),
                None => false,
            };
            let watched = if trigger.database.is_empty() {
                "(default)"
            } else {
                &trigger.database
            };
            if !fires || watched != database_id || !document_matches(trigger, &path) {
                continue;
            }
            let data = data.get_or_insert_with(|| v2_data(database, change).into());
            events.push(Event {
                project: project.to_owned(),
                key: key.clone(),
                payload: Payload::V2 {
                    event_type: trigger.event_type.clone(),
                    with_auth: base.len() != trigger.event_type.len(),
                    database: database_id.to_owned(),
                    document: path.clone(),
                    data: Arc::clone(data),
                },
            });
        }
    }
}

fn document(database: &str, doc: &StoredDocument) -> Document {
    Document {
        name: Name::document_name(database, &doc.path),
        fields: doc.fields(),
        create_time: Some(doc.create_time),
        update_time: Some(doc.update_time),
    }
}

/// `{"value": …, "oldValue": …, "updateMask": {}}`, written as the official emulator writes JSON
/// over WebChannel: compact, fields in their order, strings escaped for HTML.
fn v1_data(database: &str, change: &Change) -> String {
    let print = |doc: &StoredDocument| {
        rest::print_top(
            &rest::descriptor("Document"),
            &document(database, doc).encode_to_vec(),
            rest::Layout::WebChannel,
        )
    };
    let mut entries = Vec::with_capacity(3);
    if let Some(after) = &change.after {
        entries.push(format!("\"value\":{}", print(after)));
    }
    if let Some(before) = &change.before {
        entries.push(format!("\"oldValue\":{}", print(before)));
    }
    entries.push("\"updateMask\":{}".to_owned());
    format!("{{{}}}", entries.join(","))
}

/// The `DocumentEventData` of `change`, encoded, in base64.
fn v2_data(database: &str, change: &Change) -> String {
    let data = DocumentEventData {
        value: change.after.as_deref().map(|doc| document(database, doc)),
        old_value: change.before.as_deref().map(|doc| document(database, doc)),
        update_mask: Some(DocumentMask::default()),
    };
    STANDARD.encode(data.encode_to_vec())
}

fn time(at: ReadTime) -> String {
    let ts = at.to_timestamp();
    rest::timestamp(ts.seconds, ts.nanos)
}

/// Whether a v1 trigger's resource (`projects/{p}/databases/{d}/documents/{pattern}`) matches
/// the document `name`.
fn resource_matches(resource: &str, name: &str) -> bool {
    let pattern: Vec<&str> = resource.split('/').collect();
    let path: Vec<&str> = name.split('/').collect();
    segments_match(&pattern, &path)
}

/// Whether a v2 trigger's document matches the document at `path`: exactly, or as a pattern.
/// A leading `/` is ignored.
fn document_matches(trigger: &EventarcTrigger, path: &str) -> bool {
    let Some(document) = &trigger.document else {
        return false;
    };
    let value = document.value.strip_prefix('/').unwrap_or(&document.value);
    if value.is_empty() {
        return false;
    }
    match document.match_type {
        MatchType::EXACT => value == path,
        MatchType::PATH_PATTERN => {
            let pattern: Vec<&str> = value.split('/').collect();
            let path: Vec<&str> = path.split('/').collect();
            segments_match(&pattern, &path)
        }
        _ => false,
    }
}

/// `{name}`, `{name=*}` and `*` match one segment, `{name=**}` and `**` any number of them,
/// anything else itself.
fn segments_match(pattern: &[&str], path: &[&str]) -> bool {
    let Some((&segment, rest)) = pattern.split_first() else {
        return path.is_empty();
    };
    let captured = segment
        .strip_prefix('{')
        .and_then(|s| s.strip_suffix('}'))
        .map(|inner| inner.split_once('=').map_or("*", |(_, what)| what));
    match captured.unwrap_or(segment) {
        "**" => (0..=path.len()).any(|skip| segments_match(rest, &path[skip..])),
        "*" => !path.is_empty() && segments_match(rest, &path[1..]),
        _ if captured.is_some() => !path.is_empty() && segments_match(rest, &path[1..]),
        _ => path
            .split_first()
            .is_some_and(|(&first, tail)| first == segment && segments_match(rest, tail)),
    }
}

/// A random (version 4) UUID, as the official emulator gives each event.
pub(super) fn uuid() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let random = std::collections::hash_map::RandomState::new();
    let mut bytes = [0u8; 16];
    for half in bytes.chunks_mut(8) {
        let mut hasher = random.build_hasher();
        hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
        half.copy_from_slice(&hasher.finish().to_le_bytes());
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(pattern: &str, path: &str) -> bool {
        segments_match(
            &pattern.split('/').collect::<Vec<_>>(),
            &path.split('/').collect::<Vec<_>>(),
        )
    }

    #[test]
    fn patterns() {
        // As the official emulator matched them (`tests/fixtures/function_events.json`).
        assert!(matches("items/{id}", "items/a"));
        assert!(matches("items/{id}", "items/{id}"));
        assert!(!matches("items/{id}", "items/a/sub/x"));
        assert!(!matches("items/{id}", "other/a"));
        assert!(matches("{coll}/{id}", "other/a"));
        assert!(matches("items/{id}/sub/{sid}", "items/a/sub/x"));
        assert!(matches("items/*", "items/b"));
        assert!(!matches("items/*", "items/a/sub/x"));
        for path in ["items/a", "items/a/sub/x", "items/a/sub/x/deeper/y"] {
            assert!(matches("items/**", path), "{path}");
            assert!(matches("items/{id=**}", path), "{path}");
        }
        assert!(!matches("items/{id=**}", "other/a"));
        assert!(matches("items/a", "items/a"));
        assert!(!matches("items/a", "items/b"));
    }

    #[test]
    fn uuids_are_version_4() {
        let (a, b) = (uuid(), uuid());
        assert!(a != b);
        let bytes = a.as_bytes();
        assert!(bytes.len() == 36 && bytes[14] == b'4');
        assert!(matches!(bytes[19], b'8' | b'9' | b'a' | b'b'));
    }
}
