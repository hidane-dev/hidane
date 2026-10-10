//! `--seed_from_export`: data every database starts with, as on the official emulator.
//!
//! The export is read at startup, but a database receives its documents on first access, in
//! one commit at that time (so `create_time` and `update_time` are the access time). Every
//! project is seeded, each database with the documents whose key names it: an export of
//! `(default)` seeds every project's `(default)` database and no other. Clearing a database
//! leaves it empty; after `POST /reset`, every database is seeded again on its next access
//! (`tests/fixtures/export_import.json`, "seeding").
//!
//! [`SeededStore`] wraps the store, so every access (RPCs, REST, WebChannel, the admin
//! endpoints) seeds first. A database is seeded before anything else can commit to it, so the
//! seeding commit is its first event in the change feed, and listeners see the documents.

use std::{
    collections::HashSet,
    ops::ControlFlow,
    sync::{Arc, Mutex, PoisonError, RwLock},
};

use hidane_core::{
    export::ExportedDocument,
    path::ResourcePath,
    store::{
        Commit, ListItem, ListOptions, ReadTime, Store, StoreError, StoredDocument, Visit,
        WriteBatch,
    },
};

use super::changes::ChangeFeed;

pub(crate) struct Seeder {
    documents: Vec<ExportedDocument>,
    changes: Arc<ChangeFeed>,
    seeded: RwLock<HashSet<String>>,
    /// One seeding at a time; accesses to the database being seeded wait for it.
    seeding: Mutex<()>,
}

impl Seeder {
    pub(crate) fn new(documents: Vec<ExportedDocument>, changes: Arc<ChangeFeed>) -> Self {
        Self {
            documents,
            changes,
            seeded: RwLock::default(),
            seeding: Mutex::default(),
        }
    }

    fn is_seeded(&self, database: &str) -> bool {
        self.seeded
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(database)
    }

    fn ensure(&self, store: &dyn Store, database: &str) {
        if self.is_seeded(database) {
            return;
        }
        let _one_at_a_time = self.seeding.lock().unwrap_or_else(PoisonError::into_inner);
        if self.is_seeded(database) {
            return;
        }
        let id = database.rsplit('/').next().unwrap_or_default();
        let project = database.split('/').nth(1).unwrap_or_default();
        let documents: Vec<&ExportedDocument> = self
            .documents
            .iter()
            .filter(|doc| doc.database_id == id)
            .collect();
        if !documents.is_empty() {
            let commit = store.commit(database, &mut |batch| {
                for doc in &documents {
                    batch.set(&doc.path, doc.fields_in(project));
                }
                Ok(())
            });
            if let Ok(commit) = commit {
                self.changes.publish(database, commit);
            }
        }
        self.seeded
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(database.to_owned());
    }

    /// `POST /reset`: every database is seeded again on its next access.
    pub(crate) fn forget(&self) {
        self.seeded
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }
}

/// A store whose databases are seeded on first access.
pub(crate) struct SeededStore {
    pub(crate) inner: Arc<dyn Store>,
    pub(crate) seeder: Arc<Seeder>,
}

impl SeededStore {
    fn ensure(&self, database: &str) {
        self.seeder.ensure(&*self.inner, database);
    }
}

impl Store for SeededStore {
    fn latest_read_time(&self, database: &str) -> ReadTime {
        self.ensure(database);
        self.inner.latest_read_time(database)
    }

    fn earliest_read_time(&self, database: &str) -> ReadTime {
        self.ensure(database);
        self.inner.earliest_read_time(database)
    }

    fn get(
        &self,
        database: &str,
        path: &ResourcePath,
        at: ReadTime,
    ) -> Option<Arc<StoredDocument>> {
        self.ensure(database);
        self.inner.get(database, path, at)
    }

    fn list_collection(
        &self,
        database: &str,
        collection: &ResourcePath,
        at: ReadTime,
        options: ListOptions<'_>,
        visit: &mut dyn FnMut(ListItem<'_>) -> ControlFlow<()>,
    ) {
        self.ensure(database);
        self.inner
            .list_collection(database, collection, at, options, visit);
    }

    fn scan_collection_group(
        &self,
        database: &str,
        parent: &ResourcePath,
        collection_id: &str,
        at: ReadTime,
        visit: &mut Visit<'_>,
    ) {
        self.ensure(database);
        self.inner
            .scan_collection_group(database, parent, collection_id, at, visit);
    }

    fn scan_descendants(
        &self,
        database: &str,
        parent: &ResourcePath,
        at: ReadTime,
        visit: &mut Visit<'_>,
    ) {
        self.ensure(database);
        self.inner.scan_descendants(database, parent, at, visit);
    }

    fn list_collection_ids(
        &self,
        database: &str,
        parent: &ResourcePath,
        at: ReadTime,
    ) -> Vec<String> {
        self.ensure(database);
        self.inner.list_collection_ids(database, parent, at)
    }

    fn commit(
        &self,
        database: &str,
        write: &mut dyn FnMut(&mut dyn WriteBatch) -> Result<(), StoreError>,
    ) -> Result<Commit, StoreError> {
        self.ensure(database);
        self.inner.commit(database, write)
    }

    fn clear_database(&self, database: &str) -> ReadTime {
        self.ensure(database);
        self.inner.clear_database(database)
    }

    fn database_names(&self) -> Vec<String> {
        self.inner.database_names()
    }

    fn clear(&self) {
        self.inner.clear();
        self.seeder.forget();
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use hidane_core::store::MemoryStore;
    use hidane_proto::google::firestore::v1::{Value, value::ValueType};

    use super::*;

    fn document(database_id: &str, path: &str, n: i64) -> ExportedDocument {
        ExportedDocument {
            database_id: database_id.to_owned(),
            path: ResourcePath::parse(path).unwrap(),
            fields: BTreeMap::from([(
                "n".to_owned(),
                Value {
                    value_type: Some(ValueType::IntegerValue(n)),
                },
            )]),
        }
    }

    #[test]
    fn databases_are_seeded_once_with_their_documents() {
        let changes = Arc::new(ChangeFeed::default());
        let seeder = Arc::new(Seeder::new(
            vec![document("(default)", "c/a", 1), document("db2", "c/b", 2)],
            Arc::clone(&changes),
        ));
        let store = SeededStore {
            inner: Arc::new(MemoryStore::new()),
            seeder,
        };
        let a = ResourcePath::parse("c/a").unwrap();
        let b = ResourcePath::parse("c/b").unwrap();
        let default = "projects/p/databases/(default)";
        let db2 = "projects/q/databases/db2";

        let seeded = store.get(default, &a, ReadTime::MAX).unwrap();
        assert_eq!(seeded.create_time, seeded.update_time);
        assert!(store.get(default, &b, ReadTime::MAX).is_none());
        assert!(store.get(db2, &b, ReadTime::MAX).is_some());
        // The seeding commit is the database's first event.
        assert_eq!(
            changes.published(default),
            ReadTime::from_timestamp(&seeded.update_time)
        );

        store.clear_database(default);
        assert!(store.get(default, &a, ReadTime::MAX).is_none());
        store.clear();
        assert!(store.get(default, &a, ReadTime::MAX).is_some());
    }
}
