//! The storage boundary (ADR 0002).
//!
//! Everything above this module (gRPC, REST, Listen, transactions) talks to [`Store`]; the
//! engine behind it can change without touching them. [`MemoryStore`] is the engine for now.
//!
//! The model:
//!
//! - Documents are **versioned by commit time** (microseconds, unique and increasing per
//!   database). A read names a [`ReadTime`] and sees the newest version at or before it, which
//!   gives consistent snapshots for queries, transactions and Listen.
//! - A commit runs a closure against a [`WriteBatch`]: the commit time is fixed before the
//!   closure runs (server timestamps need it) and reads inside the batch see the batch's own
//!   writes. The closure's error aborts the commit with nothing applied.
//! - A commit returns every changed document with its old and new version, so change
//!   notification costs O(changed documents), never O(stored documents).

mod memory;

use std::{fmt, ops::ControlFlow, sync::Arc};

use hidane_proto::google::firestore::v1::{ArrayValue, MapValue, Value, value::ValueType};
use hidane_proto::ordered::Fields;
use prost::Message as _;
use prost_types::Timestamp;

pub use memory::MemoryStore;

use crate::path::ResourcePath;

/// A point in a database's history, in microseconds since the Unix epoch. Commit times are
/// read times; reading at a commit time sees that commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReadTime(pub i64);

impl ReadTime {
    pub const MAX: Self = Self(i64::MAX);

    /// Truncates to microseconds, as Firestore does.
    pub fn from_timestamp(ts: &Timestamp) -> Self {
        Self(ts.seconds * 1_000_000 + i64::from(ts.nanos) / 1_000)
    }

    pub fn to_timestamp(self) -> Timestamp {
        #[allow(clippy::cast_possible_truncation)]
        Timestamp {
            seconds: self.0.div_euclid(1_000_000),
            nanos: (self.0.rem_euclid(1_000_000) * 1_000) as i32,
        }
    }
}

/// A stored document version.
///
/// Fields are kept protobuf-encoded: a document then costs about its wire size instead of the
/// decoded tree (a two-field document is ~30 bytes encoded, over 600 decoded, because every
/// map node reserves room for eleven entries). Readers decode with [`StoredDocument::fields`].
#[derive(Debug, Clone, PartialEq)]
pub struct StoredDocument {
    /// Shared by every version of the same document.
    pub path: Arc<ResourcePath>,
    encoded_fields: Box<[u8]>,
    pub create_time: Timestamp,
    pub update_time: Timestamp,
}

impl StoredDocument {
    pub fn new(
        path: Arc<ResourcePath>,
        fields: Fields,
        create_time: Timestamp,
        update_time: Timestamp,
    ) -> Self {
        Self {
            path,
            encoded_fields: MapValue { fields }.encode_to_vec().into_boxed_slice(),
            create_time,
            update_time,
        }
    }

    /// The document's fields, decoded.
    pub fn fields(&self) -> Fields {
        MapValue::decode(&*self.encoded_fields)
            .expect("fields were encoded by StoredDocument::new")
            .fields
    }

    /// Size of the stored field encoding, in bytes.
    pub fn encoded_len(&self) -> usize {
        self.encoded_fields.len()
    }

    /// Whether the document holds exactly `fields`, bit for bit: unlike `==` on the decoded
    /// fields, a NaN equals itself and `-0.0` differs from `0.0`, as for the official emulator,
    /// to which writing either over itself changes nothing (`tools/oracle/negative_zero.py`).
    /// The order of the fields does not matter.
    pub fn has_fields(&self, fields: &Fields) -> bool {
        let canonical = |fields: &Fields| {
            MapValue {
                fields: canonical(fields),
            }
            .encode_to_vec()
        };
        fields.len() == self.fields().len() && canonical(fields) == canonical(&self.fields())
    }
}

/// Whether `a` and `b` are the same value, bit for bit, as [`StoredDocument::has_fields`]
/// compares documents: a NaN equals itself, `-0.0` differs from `0.0`, and the order of a map's
/// fields does not matter.
pub fn identical(a: &Value, b: &Value) -> bool {
    canonical_value(a).encode_to_vec() == canonical_value(b).encode_to_vec()
}

/// `fields` with every map, nested ones included, in UTF-8 key order.
fn canonical(fields: &Fields) -> Fields {
    fields
        .sorted()
        .into_iter()
        .map(|(key, value)| (key.clone(), canonical_value(value)))
        .collect()
}

fn canonical_value(value: &Value) -> Value {
    let value_type = match &value.value_type {
        Some(ValueType::MapValue(map)) => ValueType::MapValue(MapValue {
            fields: canonical(&map.fields),
        }),
        Some(ValueType::ArrayValue(array)) => ValueType::ArrayValue(ArrayValue {
            values: array.values.iter().map(canonical_value).collect(),
        }),
        _ => return value.clone(),
    };
    Value {
        value_type: Some(value_type),
    }
}

/// One document touched by a commit.
#[derive(Debug, Clone)]
pub struct Change {
    pub path: Arc<ResourcePath>,
    pub before: Option<Arc<StoredDocument>>,
    pub after: Option<Arc<StoredDocument>>,
}

/// The result of a successful commit.
#[derive(Debug, Clone)]
pub struct Commit {
    pub commit_time: ReadTime,
    pub changes: Vec<Change>,
}

/// Why a commit was refused. Maps one to one onto gRPC status codes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    InvalidArgument(String),
    NotFound(String),
    AlreadyExists(String),
    FailedPrecondition(String),
    Aborted(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidArgument(m)
            | Self::NotFound(m)
            | Self::AlreadyExists(m)
            | Self::FailedPrecondition(m)
            | Self::Aborted(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for StoreError {}

/// The writes of one commit, with read-your-writes visibility.
pub trait WriteBatch {
    /// The time this commit will be stamped with (and the documents' `update_time`).
    fn commit_time(&self) -> ReadTime;

    /// The document as it will be if the commit succeeds now: the batch's own writes first,
    /// then the latest committed version.
    fn get(&self, path: &ResourcePath) -> Option<Arc<StoredDocument>>;

    /// Replaces the document's fields. Keeps `create_time` if the document exists.
    fn set(&mut self, path: &ResourcePath, fields: Fields);

    /// Deletes the document. Deleting a missing document is not an error.
    fn delete(&mut self, path: &ResourcePath);

    /// Drops the batch's writes to the document, which keeps its latest committed version: a
    /// commit whose writes leave a document as it was changes nothing.
    fn revert(&mut self, path: &ResourcePath);
}

/// Called for each document of a scan, in `__name__` order; return `Break` to stop early.
pub type Visit<'a> = dyn FnMut(&Arc<StoredDocument>) -> ControlFlow<()> + 'a;

/// An entry of a collection listing.
#[derive(Debug)]
pub enum ListItem<'a> {
    Document(&'a Arc<StoredDocument>),
    /// No document exists at this path, but documents exist below it (in subcollections).
    Missing(&'a ResourcePath),
}

/// Options of [`Store::list_collection`].
#[derive(Debug, Default, Clone, Copy)]
pub struct ListOptions<'a> {
    /// Resume after this document path (exclusive), for paging.
    pub after: Option<&'a ResourcePath>,
    /// Also report missing documents that have descendants (`ListDocuments.show_missing`).
    pub include_missing: bool,
}

/// The storage engine. `database` is the full database name,
/// `projects/{project}/databases/{database}`; databases are created on first write.
pub trait Store: Send + Sync {
    /// A read time that sees every commit made so far.
    fn latest_read_time(&self, database: &str) -> ReadTime;

    /// The oldest read time still served; older versions may have been dropped.
    fn earliest_read_time(&self, database: &str) -> ReadTime;

    fn get(&self, database: &str, path: &ResourcePath, at: ReadTime)
    -> Option<Arc<StoredDocument>>;

    /// The documents directly in `collection` (not in its subcollections).
    fn scan_collection(
        &self,
        database: &str,
        collection: &ResourcePath,
        at: ReadTime,
        visit: &mut Visit<'_>,
    ) {
        self.list_collection(
            database,
            collection,
            at,
            ListOptions::default(),
            &mut |item| match item {
                ListItem::Document(doc) => visit(doc),
                ListItem::Missing(_) => ControlFlow::Continue(()),
            },
        );
    }

    /// Like [`Store::scan_collection`], with paging and missing documents.
    fn list_collection(
        &self,
        database: &str,
        collection: &ResourcePath,
        at: ReadTime,
        options: ListOptions<'_>,
        visit: &mut dyn FnMut(ListItem<'_>) -> ControlFlow<()>,
    );

    /// Every document under `parent` (a document path, or the root) whose collection ID is
    /// `collection_id`, at any depth, ordered by full path.
    fn scan_collection_group(
        &self,
        database: &str,
        parent: &ResourcePath,
        collection_id: &str,
        at: ReadTime,
        visit: &mut Visit<'_>,
    );

    /// Every document below `parent` (a document path, or the root), at any depth, ordered by
    /// path. `parent` itself is not included.
    fn scan_descendants(
        &self,
        database: &str,
        parent: &ResourcePath,
        at: ReadTime,
        visit: &mut Visit<'_>,
    );

    /// IDs of the collections directly under `parent` that contain at least one document at
    /// any depth, in UTF-8 order.
    fn list_collection_ids(
        &self,
        database: &str,
        parent: &ResourcePath,
        at: ReadTime,
    ) -> Vec<String>;

    /// Runs `write` as one atomic commit.
    fn commit(
        &self,
        database: &str,
        write: &mut dyn FnMut(&mut dyn WriteBatch) -> Result<(), StoreError>,
    ) -> Result<Commit, StoreError>;

    /// Drops every document of `database` and every older version, at a time after its last
    /// commit, which it returns: later commits are later still, so listeners can order the
    /// clear among them.
    fn clear_database(&self, database: &str) -> ReadTime;

    /// The databases that hold or held documents.
    fn database_names(&self) -> Vec<String>;

    /// Drops every database.
    fn clear(&self);
}
