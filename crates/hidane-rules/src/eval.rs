//! Evaluating a ruleset for one request: what the evaluator takes and what it decides.
//!
//! [`authorize`] finds the `allow` statements that apply to the request and evaluates their
//! conditions. What the official emulator does, as measured with
//! `tools/oracle/rules_conditions.py` (`results/rules-conditions-official-v1.22.0.json`) and
//! `tools/oracle/rules_eval.py` (`tests/fixtures/rules_eval.json`):
//!
//! - Types are checked when a condition runs. Each operator, function and method takes the
//!   types its error lists (`Unsupported operation error. Received: list + list. Expected:
//!   int + int, …`); a method a type does not have is a `Function not found error`.
//! - Errors are absorbed the CEL way: `error || true` and `true || error` are true, `error &&
//!   false` and `false && error` are false, anything else with an error is the error. An error
//!   that reaches the top of a condition denies, like `false`.
//! - `==` between values of different types is false; an int equals a float of the same value,
//!   but not inside lists and maps. Sets and map diffs compare numbers by value at any depth.
//! - Strings are Java strings: sizes, indexes and slices count UTF-16 code units, `lower()` and
//!   `upper()` change ASCII letters only, and regular expressions are RE2.
//! - The arguments of a function are evaluated before the call, but an error in one only
//!   matters if the function uses it; a `let` is evaluated when first used.
//!
//! Each condition is evaluated once, so a denial lists each statement once; the official
//! emulator evaluates again after an error and lists everything twice
//! (`docs/parity-exceptions.md`).

mod interp;
mod library;
mod time;
mod value;

use std::rc::Rc;

use indexmap::IndexMap;

use crate::{
    Ruleset, ast,
    paths::{Binding, Covering},
};
use interp::{Frame, Interp, Level};
use value::Val;

/// A value as rules see it. Firestore values convert to these (`timestamp`, `bytes`, `latlng`,
/// a reference as a `path`, maps and lists); the others only come from rules.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Bytes(Vec<u8>),
    Timestamp(Timestamp),
    /// Seconds and nanoseconds.
    Duration(i64, i32),
    /// Latitude and longitude, in degrees.
    LatLng(f64, f64),
    /// The segments of a path. A reference holds its document's path from `databases`, like
    /// [`Request::path`] and [`Document::path`].
    Path(Vec<String>),
    List(Vec<Value>),
    Map(Map),
}

/// A map's entries, in the order the document or the literal holds them. Equality ignores the
/// order; where rules list keys in another order, the evaluator sorts.
pub type Map = IndexMap<String, Value>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp {
    pub seconds: i64,
    pub nanos: i32,
}

/// The method a request is checked against. `read` covers `get` and `list`, `write` covers
/// `create`, `update` and `delete`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    List,
    Create,
    Update,
    Delete,
}

impl Method {
    /// The name rules see in `request.method` and denials print.
    pub fn name(self) -> &'static str {
        match self {
            Self::Get => "get",
            Self::List => "list",
            Self::Create => "create",
            Self::Update => "update",
            Self::Delete => "delete",
        }
    }

    fn statement(self) -> ast::Method {
        match self {
            Self::Get => ast::Method::Get,
            Self::List => ast::Method::List,
            Self::Create => ast::Method::Create,
            Self::Update => ast::Method::Update,
            Self::Delete => ast::Method::Delete,
        }
    }
}

/// A document as `resource`, `request.resource` and `get()` give it.
#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    /// The path from `databases`: `databases`, the database ID, `documents`, then the
    /// document's segments.
    pub path: Vec<String>,
    pub data: Map,
}

/// `request.auth` for a caller with a token.
#[derive(Debug, Clone, PartialEq)]
pub struct Auth {
    /// The token's `sub`; without one, `request.auth.uid` is null.
    pub uid: Option<String>,
    /// Every claim of the token.
    pub token: Map,
}

/// `request.query` for a list.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Query {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    /// Field path to `ASC` or `DESC`.
    pub order_by: Vec<(String, String)>,
}

/// One request, as rules see it.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub method: Method,
    /// The path checked, from `databases` (a document, or for a list its collection).
    pub path: Vec<String>,
    pub auth: Option<Auth>,
    pub time: Timestamp,
    /// The document as it would be after a create or an update; `None` otherwise.
    pub resource: Option<Document>,
    pub query: Option<Query>,
}

/// The documents `get()` / `exists()` read (as they were before the request's writes) and
/// `getAfter()` / `existsAfter()` read (as they would be after all of them).
pub trait Documents {
    fn get(&self, path: &[String]) -> Option<Document>;
    fn get_after(&self, path: &[String]) -> Option<Document>;
}

/// The lookups a request may still make (`tools/oracle/rules_lookups.py`): a single read or
/// write may look up 10 distinct documents, a query 20, and a batch 10 per write and 20 in all.
/// A document looked up again costs nothing, across the writes of a batch too, and so does the
/// document a request reads or writes and every document its batch writes
/// (`tools/oracle/rules_eval.py`).
#[derive(Debug, Clone)]
pub struct Budget {
    /// Distinct documents looked up so far, by the whole request or batch.
    pub looked_up: Vec<Vec<String>>,
    /// How many of them the current write or read looked up first.
    pub this_part: usize,
    pub part_limit: usize,
    pub total_limit: usize,
    /// The documents the batch writes, from `databases` (see [`Budget::add_written`]).
    pub written: Vec<Vec<String>>,
}

impl Budget {
    fn with(part_limit: usize, total_limit: usize) -> Self {
        Self {
            looked_up: Vec::new(),
            this_part: 0,
            part_limit,
            total_limit,
            written: Vec::new(),
        }
    }

    /// Records a document the batch writes (or deletes): any write of the batch may look it up
    /// for free. The document of the request being decided is free without this.
    pub fn add_written(&mut self, path: Vec<String>) {
        if !self.written.contains(&path) {
            self.written.push(path);
        }
    }

    /// A single read or write.
    pub fn single() -> Self {
        Self::with(10, 10)
    }

    /// A query.
    pub fn query() -> Self {
        Self::with(20, 20)
    }

    /// A batch of writes; call [`Budget::next_write`] before each one.
    pub fn batch() -> Self {
        Self::with(10, 20)
    }

    pub fn next_write(&mut self) {
        self.this_part = 0;
    }
}

/// What the rules decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    /// `PERMISSION_DENIED`, with the official emulator's message.
    Deny(String),
}

/// Decides one request against `ruleset` (the `cloud.firestore` service). `resource` is the
/// stored document (`None` if it does not exist, or for a list). The rules' version decides
/// how recursive wildcards match.
///
/// The `allow` statements of every `match` that covers the path and names the method apply, in
/// source order; the first whose condition is true allows. A denial lists each one's result:
/// `false for 'get' @ L5` or the error instead of `false`, with the line of the condition, or
/// says `No matching allow statements` when none applies.
///
/// A list (a query) is allowed for now: deciding it needs the query's constraints to imply the
/// conditions (#66).
pub fn authorize(
    ruleset: &Ruleset,
    request: &Request,
    resource: Option<&Document>,
    documents: &dyn Documents,
    budget: &mut Budget,
) -> Decision {
    if request.method == Method::List {
        return Decision::Allow;
    }
    let no_match = || Decision::Deny("No matching allow statements".to_owned());
    let Some(service) = ruleset.service("cloud.firestore") else {
        return no_match();
    };
    let method = request.method.statement();
    let path: Vec<&str> = request.path.iter().map(String::as_str).collect();
    let coverings = crate::paths::covering(ruleset.version, service, &path);
    let mut statements: Vec<(&ast::Allow, &Covering<'_>)> = Vec::new();
    for covering in &coverings {
        for allow in &covering.target().body.allows {
            if allow
                .methods
                .iter()
                .any(|m| m.method.is_some_and(|m| m.covers(method)))
            {
                statements.push((allow, covering));
            }
        }
    }
    if statements.is_empty() {
        return no_match();
    }
    statements.sort_by_key(|(allow, _)| allow.span.start);

    let request_value = request_value(request);
    let resource_value = resource.map_or(Val::Null, value::resource);
    let mut entries = Vec::with_capacity(statements.len());
    for (allow, covering) in statements {
        let Some(condition) = &allow.condition else {
            return Decision::Allow;
        };
        let mut interp = Interp::new(
            levels(ruleset, service, covering),
            request_value.clone(),
            resource_value.clone(),
            &request.path,
            documents,
            budget,
        );
        let frame = Frame::condition(covering.chain.len() + 1);
        let text = match interp
            .eval(condition, frame.scope())
            .and_then(interp::boolean)
        {
            Ok(true) => return Decision::Allow,
            Ok(false) => "false".to_owned(),
            Err(e) => e.0,
        };
        entries.push(format!(
            "{text} for '{}' @ L{}",
            request.method.name(),
            condition.span.line
        ));
    }
    Decision::Deny(format!("\n{}", entries.join(", ")))
}

/// The names a condition at the end of `covering` sees: the functions of the file, of the
/// service and of each `match`, and the variables each `match` binds.
fn levels<'a>(
    ruleset: &'a Ruleset,
    service: &'a ast::Service,
    covering: &'a Covering<'a>,
) -> Vec<Level<'a>> {
    let mut levels = vec![
        Level {
            functions: &ruleset.functions,
            variables: Vec::new(),
        },
        Level {
            functions: &service.body.functions,
            variables: Vec::new(),
        },
    ];
    let mut bindings = covering.bindings.iter();
    for m in &covering.chain {
        let count = m
            .path
            .iter()
            .filter(|s| s.kind.variable().is_some())
            .count();
        let variables = bindings
            .by_ref()
            .take(count)
            .map(|(name, binding)| {
                let value = match binding {
                    Binding::Segment(s) => Val::str(s),
                    Binding::Path(p) => Val::path(p.clone()),
                };
                (name.as_str(), value)
            })
            .collect();
        levels.push(Level {
            functions: &m.body.functions,
            variables,
        });
    }
    levels
}

/// `request`: `auth` (null without a token, else `uid` and `token`), `method`, `path` and
/// `time`; `resource` for a write (null for a delete); `query` for a list.
fn request_value(request: &Request) -> Val {
    let mut map = IndexMap::new();
    let auth = match &request.auth {
        None => Val::Null,
        Some(auth) => {
            let mut a = IndexMap::new();
            a.insert(
                "uid".to_owned(),
                auth.uid.as_deref().map_or(Val::Null, Val::str),
            );
            a.insert(
                "token".to_owned(),
                Val::from(&Value::Map(auth.token.clone())),
            );
            Val::Map(Rc::new(a))
        }
    };
    map.insert("auth".to_owned(), auth);
    map.insert("method".to_owned(), Val::str(request.method.name()));
    map.insert("path".to_owned(), Val::path(request.path.clone()));
    map.insert(
        "time".to_owned(),
        Val::from(&Value::Timestamp(request.time)),
    );
    if matches!(
        request.method,
        Method::Create | Method::Update | Method::Delete
    ) {
        let resource = request.resource.as_ref().map_or(Val::Null, value::resource);
        map.insert("resource".to_owned(), resource);
    }
    if let Some(query) = &request.query {
        let mut q = IndexMap::new();
        q.insert("limit".to_owned(), query.limit.map_or(Val::Null, Val::Int));
        q.insert("offset".to_owned(), Val::Int(query.offset.unwrap_or(0)));
        let order_by = query
            .order_by
            .iter()
            .map(|(field, direction)| (field.clone(), Val::str(direction)))
            .collect();
        q.insert("orderBy".to_owned(), Val::Map(Rc::new(order_by)));
        map.insert("query".to_owned(), Val::Map(Rc::new(q)));
    }
    Val::Map(Rc::new(map))
}
