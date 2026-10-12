//! Security Rules on each request (#63–#68), applied the way the official emulator
//! applies them (`tools/oracle/rules_*.py`, fixtures in `tests/fixtures/rules_*.json`).
//!
//! A request is checked when its database has a ruleset (loaded with `:securityRules`, or the
//! `--rules` file) and its caller is not an administrator (`Bearer owner`); otherwise everything
//! is allowed, as before rules were loaded. `request.auth` holds the token's claims, with `uid`
//! from `sub`; nothing in the token is verified.
//!
//! - A read of a document is a `get`, whether the document exists or not; a denied document
//!   denies a whole batchGet.
//! - A query is a `list` of its collection, decided from its filter (`hidane_rules::eval`).
//! - A write is a `create` or an `update` by whether the document exists, unless an `exists`
//!   precondition says which; a delete is a `delete`. The rules come before the precondition:
//!   a create allowed on a document that exists fails with `ALREADY_EXISTS`, a denied one with
//!   `PERMISSION_DENIED`. Every write of a commit is checked after all of them were applied,
//!   so `getAfter()` sees the whole batch; one denied write denies the commit.
//! - A denial is `PERMISSION_DENIED` with the evaluator's message.
//!
//! Each decision is counted in the ruleset's coverage report, and the values of its `debug()`
//! calls are printed (`crate::rules`).

use std::{
    collections::HashMap,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use hidane_core::{path::ResourcePath, store::StoredDocument};
use hidane_proto::google::firestore::v1::{Value, value::ValueType};
use hidane_rules::eval::{self, Auth, Budget, Decision, Method, Request, Trace};
use serde_json::Value as Json;
use tonic::Status;

use super::auth::Caller;
use crate::rules::{Loaded, Rules};

/// The checks of one request: the ruleset and who is asking.
pub(crate) struct Guard {
    loaded: Arc<Loaded>,
    rules: Arc<Rules>,
    auth: Option<Auth>,
    /// `databases/{id}/documents`: the start of every path rules see.
    root: Vec<String>,
}

impl Guard {
    /// The checks for `caller` on `database` (`projects/{p}/databases/{d}`) under `loaded`, or
    /// `None` when nothing is checked: for an administrator.
    pub(crate) fn new(
        loaded: Arc<Loaded>,
        rules: Arc<Rules>,
        caller: &Caller,
        database: &str,
    ) -> Option<Self> {
        let auth = match caller {
            Caller::Admin => return None,
            Caller::Anonymous => None,
            Caller::User(claims) => Some(Auth {
                uid: claims.get("sub").and_then(Json::as_str).map(str::to_owned),
                token: claims
                    .iter()
                    .map(|(name, value)| (name.clone(), json(value)))
                    .collect(),
            }),
        };
        let database_id = database.rsplit('/').next().unwrap_or_default();
        Some(Self {
            loaded,
            rules,
            auth,
            root: vec![
                "databases".to_owned(),
                database_id.to_owned(),
                "documents".to_owned(),
            ],
        })
    }

    /// Decides `request`, counting what it evaluated and printing its `debug()` values.
    fn decide(
        &self,
        request: &Request,
        resource: Option<&eval::Document>,
        documents: &Lookup<'_>,
        budget: &mut Budget,
    ) -> Result<(), String> {
        let mut trace = Trace::with_values();
        let decision = eval::authorize_traced(
            self.loaded.ruleset(),
            request,
            resource,
            documents,
            budget,
            &mut trace,
        );
        self.loaded.record(&trace);
        self.rules.debug(&trace);
        match decision {
            Decision::Allow => Ok(()),
            Decision::Deny(message) => Err(message),
        }
    }

    fn path(&self, path: &ResourcePath) -> Vec<String> {
        self.root
            .iter()
            .cloned()
            .chain(path.segments().map(str::to_owned))
            .collect()
    }

    fn document(&self, doc: &StoredDocument) -> eval::Document {
        eval::Document {
            path: self.path(&doc.path),
            data: fields(&doc.fields()),
        }
    }

    fn request(
        &self,
        method: Method,
        path: &ResourcePath,
        resource: Option<eval::Document>,
        time: eval::Timestamp,
    ) -> Request {
        Request {
            method,
            upsert: false,
            path: self.path(path),
            auth: self.auth.clone(),
            time,
            resource,
            query: None,
        }
    }

    /// A read of the document at `path` (`stored` if it exists), with `lookup` for `get()`.
    pub(crate) fn get(
        &self,
        path: &ResourcePath,
        stored: Option<&StoredDocument>,
        lookup: &dyn Fn(&ResourcePath) -> Option<Arc<StoredDocument>>,
    ) -> Result<(), String> {
        let request = self.request(Method::Get, path, None, now());
        let resource = stored.map(|doc| self.document(doc));
        let documents = Lookup {
            guard: self,
            before: lookup,
            after: lookup,
        };
        self.decide(
            &request,
            resource.as_ref(),
            &documents,
            &mut Budget::single(),
        )
    }

    /// A query of the collection at `collection` (for a collection group, of the collections
    /// below it), decided from the query's filter.
    pub(crate) fn list(
        &self,
        collection: &ResourcePath,
        query: eval::Query,
        lookup: &dyn Fn(&ResourcePath) -> Option<Arc<StoredDocument>>,
    ) -> Result<(), String> {
        let mut request = self.request(Method::List, collection, None, now());
        request.query = Some(query);
        let documents = Lookup {
            guard: self,
            before: lookup,
            after: lookup,
        };
        self.decide(&request, None, &documents, &mut Budget::query())
    }

    /// The writes of one commit, each with the document before and after it.
    pub(crate) fn writes(
        &self,
        writes: &[CheckedWrite],
        time: eval::Timestamp,
        before: &dyn Fn(&ResourcePath) -> Option<Arc<StoredDocument>>,
        after: &dyn Fn(&ResourcePath) -> Option<Arc<StoredDocument>>,
    ) -> Result<(), String> {
        let documents = Lookup {
            guard: self,
            before,
            after,
        };
        let mut budget = if writes.len() > 1 {
            Budget::batch()
        } else {
            Budget::single()
        };
        // Any write may look up a document of the batch for free.
        for write in writes {
            budget.add_written(self.path(&write.path));
        }
        let mut denials = Vec::new();
        for write in writes {
            budget.next_write();
            let new = match write.method {
                Method::Delete => None,
                _ => write.after.as_deref().map(|doc| self.document(doc)),
            };
            let mut request = self.request(write.method, &write.path, new, time);
            request.upsert = write.upsert;
            let resource = write.before.as_deref().map(|doc| self.document(doc));
            if let Err(message) = self.decide(&request, resource.as_ref(), &documents, &mut budget)
            {
                denials.push(message);
            }
        }
        match denials.as_slice() {
            [] => Ok(()),
            [one] => Err(one.clone()),
            many => Err(join(many)),
        }
    }
}

/// A write of a commit, as the rules check it.
pub(crate) struct CheckedWrite {
    pub method: Method,
    /// Whether the document's existence chose the method (see `eval::Request::upsert`).
    pub upsert: bool,
    pub path: ResourcePath,
    /// The document before the commit.
    pub before: Option<Arc<StoredDocument>>,
    /// The document after the write.
    pub after: Option<Arc<StoredDocument>>,
}

/// The documents before the commit, for writes: the version a write of the batch replaced, or
/// for a document the batch did not touch, what the batch reads.
pub(crate) struct Before {
    pub touched: HashMap<ResourcePath, Option<Arc<StoredDocument>>>,
}

impl Before {
    pub(crate) fn get(
        &self,
        path: &ResourcePath,
        read: impl Fn(&ResourcePath) -> Option<Arc<StoredDocument>>,
    ) -> Option<Arc<StoredDocument>> {
        match self.touched.get(path) {
            Some(before) => before.clone(),
            None => read(path),
        }
    }
}

/// `get()` / `getAfter()` for the evaluator, from paths that start at `databases`.
struct Lookup<'a> {
    guard: &'a Guard,
    before: &'a dyn Fn(&ResourcePath) -> Option<Arc<StoredDocument>>,
    after: &'a dyn Fn(&ResourcePath) -> Option<Arc<StoredDocument>>,
}

impl Lookup<'_> {
    /// The document path below `databases/{id}/documents`, if `path` is in this database.
    fn local(&self, path: &[String]) -> Option<ResourcePath> {
        let rest = path.strip_prefix(self.guard.root.as_slice())?;
        if rest.is_empty() || rest.len() % 2 != 0 {
            return None;
        }
        Some(ResourcePath::from_segments(rest.iter().cloned()))
    }
}

impl eval::Documents for Lookup<'_> {
    fn get(&self, path: &[String]) -> Option<eval::Document> {
        let path = self.local(path)?;
        (self.before)(&path).map(|doc| self.guard.document(&doc))
    }

    fn get_after(&self, path: &[String]) -> Option<eval::Document> {
        let path = self.local(path)?;
        (self.after)(&path).map(|doc| self.guard.document(&doc))
    }
}

/// Several denials as one message: the entries of each, in order.
pub(crate) fn join(messages: &[String]) -> String {
    let entries: Vec<&str> = messages
        .iter()
        .map(|m| m.strip_prefix('\n').unwrap_or(m))
        .filter(|m| !m.is_empty())
        .collect();
    format!("\n{}", entries.join(", "))
}

pub(crate) fn denied(message: String) -> Status {
    Status::permission_denied(message)
}

fn now() -> eval::Timestamp {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    eval::Timestamp {
        seconds: i64::try_from(since.as_secs()).unwrap_or(i64::MAX),
        // `request.time` has millisecond precision, as `REQUEST_TIME` does.
        nanos: i32::try_from(since.subsec_millis() * 1_000_000).unwrap_or(0),
    }
}

/// A commit time for `request.time`.
pub(crate) fn timestamp(ts: &prost_types::Timestamp) -> eval::Timestamp {
    eval::Timestamp {
        seconds: ts.seconds,
        nanos: ts.nanos,
    }
}

fn fields(fields: &hidane_core::field_path::Fields) -> eval::Map {
    fields
        .iter()
        .map(|(name, value)| (name.clone(), self::value(value)))
        .collect()
}

/// A Firestore value as rules see it.
pub(crate) fn value(value: &Value) -> eval::Value {
    match &value.value_type {
        None | Some(ValueType::NullValue(_)) => eval::Value::Null,
        Some(ValueType::BooleanValue(b)) => eval::Value::Bool(*b),
        Some(ValueType::IntegerValue(n)) => eval::Value::Int(*n),
        Some(ValueType::DoubleValue(f)) => eval::Value::Float(*f),
        Some(ValueType::TimestampValue(ts)) => eval::Value::Timestamp(timestamp(ts)),
        Some(ValueType::StringValue(s)) => eval::Value::String(s.clone()),
        Some(ValueType::BytesValue(b)) => eval::Value::Bytes(b.to_vec()),
        // A reference is the document's path from `databases`, as `request.path` is.
        Some(ValueType::ReferenceValue(name)) => eval::Value::Path(
            name.split('/')
                .skip_while(|s| *s != "databases")
                .map(str::to_owned)
                .collect(),
        ),
        Some(ValueType::GeoPointValue(point)) => {
            eval::Value::LatLng(point.latitude, point.longitude)
        }
        Some(ValueType::ArrayValue(array)) => {
            eval::Value::List(array.values.iter().map(self::value).collect())
        }
        Some(ValueType::MapValue(map)) => eval::Value::Map(fields(&map.fields)),
        // Pipeline expressions are never stored.
        Some(
            ValueType::FieldReferenceValue(_)
            | ValueType::VariableReferenceValue(_)
            | ValueType::FunctionValue(_)
            | ValueType::PipelineValue(_),
        ) => eval::Value::Null,
    }
}

/// A token claim as rules see it: JSON numbers that are integers are ints (`exp`, `iat`).
fn json(value: &Json) -> eval::Value {
    match value {
        Json::Null => eval::Value::Null,
        Json::Bool(b) => eval::Value::Bool(*b),
        Json::Number(n) => match n.as_i64() {
            Some(i) => eval::Value::Int(i),
            None => eval::Value::Float(n.as_f64().unwrap_or(f64::NAN)),
        },
        Json::String(s) => eval::Value::String(s.clone()),
        Json::Array(items) => eval::Value::List(items.iter().map(json).collect()),
        Json::Object(map) => eval::Value::Map(
            map.iter()
                .map(|(name, value)| (name.clone(), json(value)))
                .collect(),
        ),
    }
}
