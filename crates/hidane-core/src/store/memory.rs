//! In-memory engine: ordered maps of versioned documents, guarded per database.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    ops::{Bound, ControlFlow},
    sync::{Arc, PoisonError, RwLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use hidane_proto::google::firestore::v1::Value;

use super::{
    Change, Commit, ListItem, ListOptions, ReadTime, Store, StoreError, StoredDocument, Visit,
    WriteBatch,
};
use crate::{
    key::{encode_escaped, encode_path, encode_path_prefix, path_subtree_end},
    path::ResourcePath,
};

type Clock = Box<dyn Fn() -> i64 + Send + Sync>;

/// How far back reads may go by default. Firestore serves stale reads up to one hour old.
pub const DEFAULT_RETENTION: Duration = Duration::from_secs(60 * 60);

pub struct MemoryStore {
    databases: RwLock<HashMap<String, Arc<RwLock<Database>>>>,
    clock: Clock,
    retention_micros: i64,
}

#[derive(Default)]
struct Database {
    /// Commit time of the last commit, in microseconds.
    last_commit: i64,
    /// Keyed by `encode_path(document path)`: iteration order is `__name__` order.
    documents: BTreeMap<Vec<u8>, Record>,
    /// `encode_escaped(collection id) ++ encode_path(document path)` for every key in
    /// `documents`, so a collection group is one key range in full-path order.
    groups: BTreeSet<Vec<u8>>,
}

impl Database {
    /// Whether any document at or below `prefix` exists at `time`.
    fn subtree_has_live(&self, prefix: &[u8], time: i64) -> bool {
        self.documents
            .range(prefix.to_vec()..path_subtree_end(prefix))
            .any(|(_, record)| record.at(time).is_some())
    }
}

struct Record {
    path: Arc<ResourcePath>,
    /// Ascending by commit time; `None` marks a deletion.
    versions: Vec<(i64, Option<Arc<StoredDocument>>)>,
}

impl Record {
    fn at(&self, time: i64) -> Option<&Arc<StoredDocument>> {
        let newer = self.versions.partition_point(|(t, _)| *t <= time);
        newer
            .checked_sub(1)
            .and_then(|i| self.versions[i].1.as_ref())
    }

    fn latest(&self) -> Option<&Arc<StoredDocument>> {
        self.versions.last().and_then(|(_, doc)| doc.as_ref())
    }

    /// Drops versions no read can reach any more: everything older than the newest version at
    /// or before `cutoff`.
    fn prune(&mut self, cutoff: i64) {
        let reachable_from = self.versions.partition_point(|(t, _)| *t <= cutoff);
        if reachable_from > 1 {
            self.versions.drain(..reachable_from - 1);
        }
    }
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::with_clock(system_micros)
    }

    /// A store whose "now" comes from `clock` (microseconds since the epoch). Commit times are
    /// still strictly increasing if the clock stands still or goes backwards.
    pub fn with_clock(clock: impl Fn() -> i64 + Send + Sync + 'static) -> Self {
        Self {
            databases: RwLock::default(),
            clock: Box::new(clock),
            retention_micros: micros(DEFAULT_RETENTION),
        }
    }

    /// How long old versions stay readable after they are overwritten.
    #[must_use]
    pub fn with_retention(mut self, retention: Duration) -> Self {
        self.retention_micros = micros(retention);
        self
    }

    fn existing(&self, database: &str) -> Option<Arc<RwLock<Database>>> {
        self.databases
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(database)
            .cloned()
    }

    fn get_or_create(&self, database: &str) -> Arc<RwLock<Database>> {
        if let Some(db) = self.existing(database) {
            return db;
        }
        self.databases
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(database.to_owned())
            .or_default()
            .clone()
    }

    fn read<R>(&self, database: &str, empty: R, f: impl FnOnce(&Database) -> R) -> R {
        match self.existing(database) {
            Some(db) => f(&db.read().unwrap_or_else(PoisonError::into_inner)),
            None => empty,
        }
    }
}

fn system_micros() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_micros()).unwrap_or(i64::MAX))
}

fn micros(d: Duration) -> i64 {
    i64::try_from(d.as_micros()).unwrap_or(i64::MAX)
}

impl Store for MemoryStore {
    fn latest_read_time(&self, database: &str) -> ReadTime {
        let last = self.read(database, 0, |db| db.last_commit);
        ReadTime((self.clock)().max(last))
    }

    fn earliest_read_time(&self, database: &str) -> ReadTime {
        ReadTime(
            self.latest_read_time(database)
                .0
                .saturating_sub(self.retention_micros),
        )
    }

    fn get(
        &self,
        database: &str,
        path: &ResourcePath,
        at: ReadTime,
    ) -> Option<Arc<StoredDocument>> {
        self.read(database, None, |db| {
            db.documents
                .get(&encode_path(path.segments()))
                .and_then(|record| record.at(at.0))
                .cloned()
        })
    }

    fn list_collection(
        &self,
        database: &str,
        collection: &ResourcePath,
        at: ReadTime,
        options: ListOptions<'_>,
        visit: &mut dyn FnMut(ListItem<'_>) -> ControlFlow<()>,
    ) {
        self.read(database, (), |db| {
            let prefix = encode_path_prefix(collection.segments());
            let end = Bound::Excluded(path_subtree_end(&prefix));
            let depth = collection.len() + 1;
            let mut start = match options.after {
                Some(after) => {
                    Bound::Included(path_subtree_end(&encode_path_prefix(after.segments())))
                }
                None => Bound::Included(prefix),
            };
            'scan: loop {
                // Each pass handles one child of the collection, then seeks past its subtree.
                let Some((_, record)) = db.documents.range((start.clone(), end.clone())).next()
                else {
                    return;
                };
                let child = encode_path_prefix(record.path.segments().take(depth));
                let live = (record.path.len() == depth)
                    .then(|| record.at(at.0))
                    .flatten();
                let flow = match live {
                    Some(doc) => visit(ListItem::Document(doc)),
                    None if options.include_missing && db.subtree_has_live(&child, at.0) => {
                        let path = ResourcePath::from_segments(record.path.segments().take(depth));
                        visit(ListItem::Missing(&path))
                    }
                    None => ControlFlow::Continue(()),
                };
                if flow.is_break() {
                    return;
                }
                start = Bound::Included(path_subtree_end(&child));
                continue 'scan;
            }
        });
    }

    fn scan_collection_group(
        &self,
        database: &str,
        parent: &ResourcePath,
        collection_id: &str,
        at: ReadTime,
        visit: &mut Visit<'_>,
    ) {
        self.read(database, (), |db| {
            let mut prefix = encode_escaped(collection_id.as_bytes());
            let group_len = prefix.len();
            prefix.extend(encode_path_prefix(parent.segments()));
            let end = path_subtree_end(&prefix);
            for entry in db.groups.range(prefix..end) {
                let doc = db
                    .documents
                    .get(&entry[group_len..])
                    // The parent itself is not under the parent.
                    .filter(|record| record.path.len() > parent.len())
                    .and_then(|record| record.at(at.0));
                if let Some(doc) = doc
                    && visit(doc).is_break()
                {
                    return;
                }
            }
        });
    }

    fn scan_descendants(
        &self,
        database: &str,
        parent: &ResourcePath,
        at: ReadTime,
        visit: &mut Visit<'_>,
    ) {
        self.read(database, (), |db| {
            let start = Bound::Excluded(encode_path(parent.segments()));
            let end = Bound::Excluded(path_subtree_end(&encode_path_prefix(parent.segments())));
            for (_, record) in db.documents.range((start, end)) {
                if let Some(doc) = record.at(at.0)
                    && visit(doc).is_break()
                {
                    return;
                }
            }
        });
    }

    fn list_collection_ids(
        &self,
        database: &str,
        parent: &ResourcePath,
        at: ReadTime,
    ) -> Vec<String> {
        self.read(database, Vec::new(), |db| {
            let prefix = encode_path_prefix(parent.segments());
            let end = Bound::Excluded(path_subtree_end(&prefix));
            let mut start = Bound::Excluded(encode_path(parent.segments()));
            let mut ids = Vec::new();
            'scan: loop {
                for (_, record) in db.documents.range((start.clone(), end.clone())) {
                    if record.at(at.0).is_none() {
                        continue;
                    }
                    // A live document somewhere under this collection: record the collection
                    // and skip the rest of it.
                    let collection = record.path.segments().take(parent.len() + 1);
                    if let Some(id) = collection.clone().last() {
                        ids.push(id.to_owned());
                    }
                    let collection_prefix = encode_path_prefix(collection);
                    start = Bound::Included(path_subtree_end(&collection_prefix));
                    continue 'scan;
                }
                return ids;
            }
        })
    }

    fn commit(
        &self,
        database: &str,
        write: &mut dyn FnMut(&mut dyn WriteBatch) -> Result<(), StoreError>,
    ) -> Result<Commit, StoreError> {
        let db = self.get_or_create(database);
        let mut db = db.write().unwrap_or_else(PoisonError::into_inner);
        let commit_time = (self.clock)().max(db.last_commit + 1);

        let mut batch = MemoryBatch {
            db: &db,
            commit_time,
            pending: BTreeMap::new(),
        };
        write(&mut batch)?;
        let pending = batch.pending;

        let cutoff = commit_time.saturating_sub(self.retention_micros);
        let mut changes = Vec::with_capacity(pending.len());
        for (key, (path, after)) in pending {
            let before = db.documents.get(&key).and_then(Record::latest).cloned();
            if before.is_none() && after.is_none() {
                continue; // deleting a document that does not exist changes nothing
            }
            if !db.documents.contains_key(&key)
                && let Some(collection_id) = path.collection_id()
            {
                let mut group_key = encode_escaped(collection_id.as_bytes());
                group_key.extend_from_slice(&key);
                db.groups.insert(group_key);
            }
            let record = db.documents.entry(key).or_insert_with(|| Record {
                path: Arc::clone(&path),
                // Most documents never get a second version inside the retention window.
                versions: Vec::with_capacity(1),
            });
            record.versions.push((commit_time, after.clone()));
            record.prune(cutoff);
            changes.push(Change {
                path,
                before,
                after,
            });
        }
        db.last_commit = commit_time;
        Ok(Commit {
            commit_time: ReadTime(commit_time),
            changes,
        })
    }

    fn clear(&self) {
        self.databases
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }
}

/// A pending write: the document's shared path and its new version (`None` deletes it).
type PendingWrite = (Arc<ResourcePath>, Option<Arc<StoredDocument>>);

struct MemoryBatch<'a> {
    db: &'a Database,
    commit_time: i64,
    /// Keyed like `Database::documents`.
    pending: BTreeMap<Vec<u8>, PendingWrite>,
}

impl MemoryBatch<'_> {
    /// The path as already shared by the document's earlier versions, if any.
    fn shared_path(&self, key: &[u8], path: &ResourcePath) -> Arc<ResourcePath> {
        self.pending
            .get(key)
            .map(|(p, _)| Arc::clone(p))
            .or_else(|| self.db.documents.get(key).map(|r| Arc::clone(&r.path)))
            .unwrap_or_else(|| Arc::new(path.clone()))
    }
}

impl WriteBatch for MemoryBatch<'_> {
    fn commit_time(&self) -> ReadTime {
        ReadTime(self.commit_time)
    }

    fn get(&self, path: &ResourcePath) -> Option<Arc<StoredDocument>> {
        let key = encode_path(path.segments());
        match self.pending.get(&key) {
            Some((_, pending)) => pending.clone(),
            None => self
                .db
                .documents
                .get(&key)
                .and_then(Record::latest)
                .cloned(),
        }
    }

    fn set(&mut self, path: &ResourcePath, fields: BTreeMap<String, Value>) {
        let now = ReadTime(self.commit_time).to_timestamp();
        let create_time = self
            .get(path)
            .map_or_else(|| now, |existing| existing.create_time);
        let key = encode_path(path.segments());
        let shared = self.shared_path(&key, path);
        let document = StoredDocument::new(Arc::clone(&shared), fields, create_time, now);
        self.pending.insert(key, (shared, Some(Arc::new(document))));
    }

    fn delete(&mut self, path: &ResourcePath) {
        let key = encode_path(path.segments());
        let shared = self.shared_path(&key, path);
        self.pending.insert(key, (shared, None));
    }
}

#[cfg(test)]
mod tests;
