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
//! - The operands of an operator, a call or a list or map literal are all evaluated, the last
//!   first (as `debug()` shows), and the first error in order wins (for `x in y`, `y`'s; for a
//!   method, a null receiver or a method it does not have comes before its arguments' errors).
//! - The arguments of a function (the last first), then its `let`s in order, are evaluated
//!   before its result, but an error in one only matters if the function uses it.
//!
//! Each condition is evaluated once, so a denial lists each statement once; the official
//! emulator evaluates again after an error and lists everything twice
//! (`docs/parity-exceptions.md`).
//!
//! [`authorize_traced`] also reports what the official emulator logs and counts for its
//! coverage report (`tools/oracle/rules_coverage.py`, [`Trace`]): each `debug()` value, and
//! what each expression evaluated to. Which statements it evaluates shows there: those of
//! every `match` covering the path, each `match`'s until one allows.

mod constraint;
mod interp;
mod library;
mod list;
mod time;
mod trace;
mod value;

pub use constraint::Comparator;
pub use trace::{Evaluation, KeyStatus, Observed, Outcome, Segment, Trace, Verdict};
pub(crate) use trace::{NodeKey, Traced, hash_val, observe, shows};
pub use value::java_double;

use std::rc::Rc;

use indexmap::IndexMap;

use crate::{
    Ruleset, ast,
    paths::{Binding, Covering, Part},
};
use interp::{Frame, Interp, Level};
pub(crate) use value::Val;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
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

/// `request.query` for a list, and the filter that decides it.
///
/// `request.query` holds `limit`, `offset` and `orderBy` from here, and what the official
/// emulator derives from the path (`tools/oracle/rules_request_fields.py`): `kind`, the
/// collection ID (null for a query of every collection); `parent`, the document the collection
/// is in (null at the root), named `ancestor` for a collection group; `allDescendants`; and
/// `distinct` and `selectOnlyKeys`, always false, and `groupBy`, always empty, which Firestore
/// queries cannot set.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Query {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    /// The orderings the query gives (not those completing it), field path to `ASC` or
    /// `DESC`, in the order `request.query.orderBy` lists them.
    pub order_by: Vec<(String, String)>,
    pub filter: Option<Filter>,
    /// For a collection group, its collection ID: the list covers the collections with that ID
    /// at any depth below [`Request::path`] (an empty ID: every collection below it).
    pub group: Option<String>,
    /// With an empty [`Query::group`]: only the collections directly below [`Request::path`]
    /// (a query whose `from` has no collection ID and is not `allDescendants`).
    pub children: bool,
    /// `request.fields`: the fields the query selects, as the official emulator lists them
    /// (`None`, null, without a projection).
    pub fields: Option<Vec<String>>,
}

/// A query's filter.
#[derive(Debug, Clone, PartialEq)]
pub enum Filter {
    And(Vec<Filter>),
    Or(Vec<Filter>),
    /// A field, by its segments (`a.b` is `["a", "b"]`), and the value it is compared with
    /// (null for the unary operators; a list for `In`, `NotIn` and `ArrayContainsAny`).
    Field {
        field: Vec<String>,
        op: FilterOp,
        value: Value,
    },
    /// The document's name, and a reference ([`Value::Path`]) or a list of them.
    Name {
        op: FilterOp,
        value: Value,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterOp {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    ArrayContains,
    In,
    ArrayContainsAny,
    NotIn,
    IsNan,
    IsNull,
    IsNotNan,
    IsNotNull,
}

/// One request, as rules see it.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub method: Method,
    /// A create or an update without an existence precondition (a set, a patch, a transform):
    /// whether the document exists chose the method. A denial then also lists the statements of
    /// the other of the two, which cannot allow the write (`tools/oracle/rules_writes.py`,
    /// `rules_denials.py`).
    pub upsert: bool,
    /// The path checked, from `databases` (a document, or for a list its collection).
    pub path: Vec<String>,
    pub auth: Option<Auth>,
    pub time: Timestamp,
    /// The document as it would be after a create or an update; `None` otherwise.
    pub resource: Option<Document>,
    pub query: Option<Query>,
    /// `request.inTransaction`: true for every write, which the official emulator commits in
    /// a transaction of its own, and for a read of documents in a transaction begun before it;
    /// false for a read that begins one, and for every list, in a transaction or not
    /// (`tools/oracle/rules_request_fields.py`).
    pub in_transaction: bool,
    /// For a write, `request.writeFields`: the paths of its transforms, then those of its
    /// update mask (`None`, null, for a write with neither).
    pub write_fields: Option<Vec<String>>,
    /// For a write, `request.transforms`: the paths its field transforms change, in order
    /// (`None`, null, without any).
    pub transforms: Option<Vec<String>>,
    /// A create whose document ID the server chooses (a `CreateDocument` without one). The
    /// official emulator decides it without the ID: [`Request::path`] is the collection's,
    /// the statements are those of the collection's documents, with the document's wildcard
    /// unbound, and `request.resource` holds only `data`. `request.autoId` says so for a
    /// create that is not an upsert; other writes do not have it.
    pub auto_id: bool,
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
/// source order; one whose condition is true allows. A denial lists each one's result:
/// `false for 'get' @ L5` or the error instead of `false`, with the line of the condition, or
/// says `No matching allow statements` when none applies. For an upsert the statements of the
/// other method are listed too, `create`'s first: each is `false`, or its condition's error.
/// Their lookups do not count against `budget`.
///
/// A list is decided from its query's filter, not from the documents (`list`).
pub fn authorize(
    ruleset: &Ruleset,
    request: &Request,
    resource: Option<&Document>,
    documents: &dyn Documents,
    budget: &mut Budget,
) -> Decision {
    authorize_traced(
        ruleset,
        request,
        resource,
        documents,
        budget,
        &mut Trace::new(),
    )
}

/// [`authorize`], reporting into `trace` what the conditions evaluated.
///
/// An upsert evaluates the statements of both methods, whichever applies, as the official
/// emulator does: `create`'s, then `update`'s, as [`authorize`] evaluates those of a method
/// (`tools/oracle/rules_coverage.py`). Only those of the method that applies decide.
pub fn authorize_traced(
    ruleset: &Ruleset,
    request: &Request,
    resource: Option<&Document>,
    documents: &dyn Documents,
    budget: &mut Budget,
    trace: &mut Trace,
) -> Decision {
    let Some(service) = ruleset.service("cloud.firestore") else {
        return unmatched(request, resource.map_or(Val::Null, value::resource), trace);
    };
    if request.method == Method::List {
        return list::authorize(ruleset, service, request, documents, budget, trace);
    }
    let mut parts: Vec<Part<'_>> = request.path.iter().map(|s| Part::Segment(s)).collect();
    if request.auto_id {
        parts.push(Part::UnknownId);
    }
    let coverings = crate::paths::covering_parts(ruleset.version, service, &parts);
    let other = match request.method {
        Method::Create if request.upsert => Some(Method::Update),
        Method::Update if request.upsert => Some(Method::Create),
        _ => None,
    };
    let applying = statements(&coverings, request.method);
    let others = other.map(|other| (other, statements(&coverings, other)));
    if applying.is_empty() && others.as_ref().is_none_or(|(_, s)| s.is_empty()) {
        return unmatched(request, resource.map_or(Val::Null, value::resource), trace);
    }

    // The statements of each method see `request.method` as theirs.
    let context_for = |method| Context {
        ruleset,
        service,
        request: request_value(request, method),
        resource: resource.map_or(Val::Null, value::resource),
        target: &request.path,
        documents,
    };
    let context = context_for(request.method);
    let Some((other, others)) = others else {
        return match context.decide(&applying, request.method, budget, trace) {
            Ok(()) => Decision::Allow,
            Err(entries) => denied(&entries),
        };
    };
    let other_context = context_for(other);
    // `create`'s statements come first: before the decision when `update` applies. The other
    // method's lookups count against a copy of the budget.
    if other == Method::Create {
        other_context.each(&others, other, &mut budget.clone(), trace);
    }
    let decided = context.decide(&applying, request.method, budget, trace);
    let Err(entries) = decided else {
        if other == Method::Update {
            other_context.each(&others, other, &mut budget.clone(), trace);
        }
        return Decision::Allow;
    };
    // A denial lists every statement of the other method; `create`'s, evaluated already, are
    // evaluated again without being traced.
    let listed = match other {
        Method::Create => {
            other_context.listed(&others, other, &mut budget.clone(), &mut Trace::new())
        }
        _ => other_context.listed(&others, other, &mut budget.clone(), trace),
    };
    let entries: Vec<String> = match other {
        Method::Create => listed.into_iter().chain(entries).collect(),
        _ => entries.into_iter().chain(listed).collect(),
    };
    denied(&entries)
}

fn no_match() -> Decision {
    Decision::Deny("No matching allow statements".to_owned())
}

/// The denial of a request no statement applies to, reported as an evaluation of none.
fn unmatched(request: &Request, resource: Val, trace: &mut Trace) -> Decision {
    if trace.evaluations.is_some() {
        trace.evaluating(request.method, &request_value(request, request.method), &resource);
    }
    no_match()
}

/// A denial listing `entries`.
fn denied(entries: &[String]) -> Decision {
    Decision::Deny(format!("\n{}", entries.join(", ")))
}

/// The `allow` statements for `method` of every `match` in `coverings`, in source order.
fn statements<'c, 'a>(
    coverings: &'c [Covering<'a>],
    method: Method,
) -> Vec<(&'c ast::Allow, &'c Covering<'a>)> {
    let method = method.statement();
    let mut statements = Vec::new();
    for covering in coverings {
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
    statements.sort_by_key(|(allow, _)| allow.span.start);
    statements
}

/// What a statement's condition came to.
fn verdict(result: &Result<bool, String>) -> Verdict {
    match result {
        Ok(true) => Verdict::Allow,
        Ok(false) => Verdict::False,
        Err(_) => Verdict::Error,
    }
}

fn entry(text: &str, method: Method, line: u32) -> String {
    format!("{text} for '{}' @ L{line}", method.name())
}

/// What every condition of one request is evaluated with.
struct Context<'a> {
    ruleset: &'a Ruleset,
    service: &'a ast::Service,
    request: Val,
    resource: Val,
    /// The path whose document is free to look up, in the database `get()` may read.
    target: &'a [String],
    documents: &'a dyn Documents,
}

impl Context<'_> {
    /// Evaluates `statements` in order; if none allows, each one's entry. As the official
    /// emulator does, the statements of a `match` stop at the first that allows, but those of
    /// every other `match` are evaluated too (`tools/oracle/rules_coverage.py`).
    fn decide(
        &self,
        statements: &[(&ast::Allow, &Covering<'_>)],
        method: Method,
        budget: &mut Budget,
        trace: &mut Trace,
    ) -> Result<(), Vec<String>> {
        let (entries, allowed) = self.each(statements, method, budget, trace);
        if allowed { Ok(()) } else { Err(entries) }
    }

    /// The statements of an upsert's method that does not apply, for a denial: each one's
    /// entry, `false` or its condition's error.
    fn listed(
        &self,
        statements: &[(&ast::Allow, &Covering<'_>)],
        method: Method,
        budget: &mut Budget,
        trace: &mut Trace,
    ) -> Vec<String> {
        let mut entries = Vec::with_capacity(statements.len());
        let mut marks = Vec::with_capacity(statements.len());
        let at = trace.evaluating(method, &self.request, &self.resource);
        for (allow, covering) in statements {
            marks.push(trace.mark());
            let (result, line) = match &allow.condition {
                Some(condition) => (
                    self.evaluate(covering, condition, budget, trace),
                    condition.span.line,
                ),
                None => (Ok(true), allow.span.line),
            };
            trace.verdict(at, line, verdict(&result));
            let text = result.err();
            entries.push(entry(text.as_deref().unwrap_or("false"), method, line));
        }
        trace.repeat(&marks);
        entries
    }

    /// Evaluates `statements` but those after one that allowed in the same `match`. Returns the
    /// entries of those that did not allow, and whether one did.
    fn each(
        &self,
        statements: &[(&ast::Allow, &Covering<'_>)],
        method: Method,
        budget: &mut Budget,
        trace: &mut Trace,
    ) -> (Vec<String>, bool) {
        let mut entries = Vec::with_capacity(statements.len());
        let mut marks = Vec::with_capacity(statements.len());
        let mut allowed: Vec<*const ast::Match> = Vec::new();
        let at = trace.evaluating(method, &self.request, &self.resource);
        for (allow, covering) in statements {
            let target = std::ptr::from_ref(covering.target());
            if allowed.contains(&target) {
                continue;
            }
            marks.push(trace.mark());
            let Some(condition) = &allow.condition else {
                trace.verdict(at, allow.span.line, Verdict::Allow);
                allowed.push(target);
                continue;
            };
            let result = self.evaluate(covering, condition, budget, trace);
            trace.verdict(at, condition.span.line, verdict(&result));
            let text = match result {
                Ok(true) => {
                    allowed.push(target);
                    continue;
                }
                Ok(false) => "false".to_owned(),
                Err(e) => e,
            };
            entries.push(entry(&text, method, condition.span.line));
        }
        trace.repeat(&marks);
        (entries, !allowed.is_empty())
    }

    /// The condition of a statement at the end of `covering`: true, false or the error's text.
    fn evaluate(
        &self,
        covering: &Covering<'_>,
        condition: &ast::Expr,
        budget: &mut Budget,
        trace: &mut Trace,
    ) -> Result<bool, String> {
        let mut interp = Interp::new(
            levels(self.ruleset, self.service, covering),
            self.request.clone(),
            self.resource.clone(),
            self.target,
            self.documents,
            budget,
            trace,
            &self.ruleset.parens,
        );
        let frame = Frame::condition(covering.chain.len() + 1);
        interp
            .eval(condition, frame.scope())
            .and_then(interp::boolean)
            .map_err(|e| {
                if e.message.is_empty() {
                    // Undecided: the official emulator names the condition's start.
                    format!(
                        "evaluation error at L{}:{}",
                        condition.span.line, condition.span.column
                    )
                } else {
                    e.message
                }
            })
    }
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
            // A wildcard that took a list's unknown document ID is not bound: using it is a
            // `Null value error`, as for any unknown name.
            .filter_map(|(name, binding)| {
                let value = match binding {
                    Binding::Segment(s) => Val::str(s),
                    Binding::Path(p) => Val::path(p.clone()),
                    Binding::Unbound => return None,
                };
                Some((name.as_str(), value))
            })
            .collect();
        levels.push(Level {
            functions: &m.body.functions,
            variables,
        });
    }
    levels
}

/// `request` for the statements of `method`, with the keys the official emulator gives it, in
/// the order its coverage report lists them (`tools/oracle/rules_request_fields.py`):
///
/// - for a `get`: `time`, `auth`, `inTransaction`, `path`, `fields` (null), `method`;
/// - for a `list`: `time`, `auth`, `inTransaction`, `fields`, `query`, `path`, `method`;
/// - for a write: `time`, `auth`, `inTransaction`, `path`, `transforms`, `writeFields`,
///   `readFields` (null), `autoId` (for a create that is not an upsert), `resource` (null for a
///   delete; only `data` when the ID is the server's), `method`.
///
/// `auth` is null without a token, else `uid` and `token`.
fn request_value(request: &Request, method: Method) -> Val {
    let strings = |list: &Option<Vec<String>>| {
        list.as_ref().map_or(Val::Null, |items| {
            Val::list(items.iter().map(|s| Val::str(s)).collect())
        })
    };
    let mut map = IndexMap::new();
    map.insert(
        "time".to_owned(),
        Val::from(&Value::Timestamp(request.time)),
    );
    map.insert("auth".to_owned(), auth_value(request));
    map.insert(
        "inTransaction".to_owned(),
        Val::Bool(request.in_transaction),
    );
    let path = Val::path(request.path.clone());
    match method {
        Method::Get => {
            map.insert("path".to_owned(), path);
            map.insert("fields".to_owned(), Val::Null);
        }
        Method::List => {
            let query = request.query.clone().unwrap_or_default();
            map.insert("fields".to_owned(), strings(&query.fields));
            map.insert("query".to_owned(), query_value(request, &query));
            map.insert("path".to_owned(), path);
        }
        Method::Create | Method::Update | Method::Delete => {
            map.insert("path".to_owned(), path);
            map.insert("transforms".to_owned(), strings(&request.transforms));
            map.insert("writeFields".to_owned(), strings(&request.write_fields));
            map.insert("readFields".to_owned(), Val::Null);
            if method == Method::Create && !request.upsert {
                map.insert("autoId".to_owned(), Val::Bool(request.auto_id));
            }
            let resource = match &request.resource {
                None => Val::Null,
                // The document has no name yet.
                Some(doc) if request.auto_id => match value::resource(doc) {
                    Val::Map(map) => {
                        let data = map.get("data").cloned().unwrap_or(Val::Null);
                        Val::Map(Rc::new(IndexMap::from([("data".to_owned(), data)])))
                    }
                    other => other,
                },
                Some(doc) => value::resource(doc),
            };
            map.insert("resource".to_owned(), resource);
        }
    }
    map.insert("method".to_owned(), Val::str(method.name()));
    Val::Map(Rc::new(map))
}

/// `request.query`, its keys in the order of the official emulator's coverage report.
fn query_value(request: &Request, query: &Query) -> Val {
    let all_descendants = query.group.is_some() && !query.children;
    // The collection ID, and the document holding the collection (or the collections).
    let (kind, parent) = match &query.group {
        None => match request.path.split_last() {
            Some((id, parent)) => (Val::str(id), parent),
            None => (Val::Null, &[][..]),
        },
        Some(id) if id.is_empty() => (Val::Null, request.path.as_slice()),
        Some(id) => (Val::str(id), request.path.as_slice()),
    };
    // `databases/{id}/documents`, the root, is no document.
    let parent = if parent.len() > 3 {
        Val::path(parent.to_vec())
    } else {
        Val::Null
    };
    let order_by = query
        .order_by
        .iter()
        .map(|(field, direction)| (field.clone(), Val::str(direction)))
        .collect();
    let mut q = IndexMap::new();
    q.insert("distinct".to_owned(), Val::Bool(false));
    q.insert("selectOnlyKeys".to_owned(), Val::Bool(false));
    q.insert("groupBy".to_owned(), Val::Map(Rc::default()));
    q.insert("kind".to_owned(), kind);
    let parent_key = if all_descendants {
        "ancestor"
    } else {
        "parent"
    };
    q.insert(parent_key.to_owned(), parent);
    q.insert("orderBy".to_owned(), Val::Map(Rc::new(order_by)));
    q.insert("limit".to_owned(), query.limit.map_or(Val::Null, Val::Int));
    q.insert("offset".to_owned(), Val::Int(query.offset.unwrap_or(0)));
    q.insert("allDescendants".to_owned(), Val::Bool(all_descendants));
    Val::Map(Rc::new(q))
}

/// `request.auth`: null without a token, else `uid` and `token`.
fn auth_value(request: &Request) -> Val {
    match &request.auth {
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
    }
}
