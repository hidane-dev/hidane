//! Evaluating a ruleset for one request: what the evaluator takes and what it decides.
//!
//! This module fixes the interface between the evaluator (#60, #62, #65, #66) and the server
//! that calls it (#63, #64, #67, #68). The decision is not implemented yet: every request is
//! allowed, as before.

use indexmap::IndexMap;

use crate::{Ruleset, Version};

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
    /// The segments of a path (a reference holds the whole document name, from `projects`).
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
}

/// A document as `resource`, `request.resource` and `get()` give it.
#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    /// The path from `databases`: `databases`, the database ID, `documents`, then the
    /// document's segments.
    pub path: Vec<String>,
    pub data: Map,
}

/// `request.auth` for a caller with a token: `uid` is the token's `sub`.
#[derive(Debug, Clone, PartialEq)]
pub struct Auth {
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
/// A document looked up again costs nothing, across the writes of a batch too.
#[derive(Debug, Clone)]
pub struct Budget {
    /// Distinct documents looked up so far, by the whole request or batch.
    pub looked_up: Vec<Vec<String>>,
    /// How many of them the current write or read looked up first.
    pub this_part: usize,
    pub part_limit: usize,
    pub total_limit: usize,
}

impl Budget {
    fn with(part_limit: usize, total_limit: usize) -> Self {
        Self {
            looked_up: Vec::new(),
            this_part: 0,
            part_limit,
            total_limit,
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
pub fn authorize(
    ruleset: &Ruleset,
    request: &Request,
    resource: Option<&Document>,
    documents: &dyn Documents,
    budget: &mut Budget,
) -> Decision {
    let _ = (
        ruleset.version == Version::V2,
        request,
        resource,
        documents,
        budget,
    );
    Decision::Allow
}
