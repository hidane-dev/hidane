//! Clearing data (`POST /reset`, `DELETE /emulator/v1/…/documents[/{path}]`), with the official
//! emulator's scope: a reset drops every database, the database endpoint drops one, and a path
//! drops the document or collection there and everything below it.
//!
//! Unlike the official emulator, which tells attached listeners nothing on a reset, every clear
//! reaches them: a cleared database as a `Cleared` event, a deleted path as an ordinary commit.
//!
//! The Functions emulator's triggers hear of a cleared database and of a deleted path, as
//! deletes of each document, but not of a reset, as on the official emulator
//! (`tests/fixtures/function_events.json`).

use std::{ops::ControlFlow, sync::Arc};

use hidane_core::{path::ResourcePath, store::Change};
use hidane_proto::google::firestore::v1::{Write, write::Operation};
use tonic::Status;

use super::{FirestoreService, auth, names, names::Name};

impl FirestoreService {
    /// Drops every document of `database` and tells its listeners and triggers.
    pub(crate) async fn clear_database(&self, database: &str) {
        self.clear(database, true).await;
    }

    /// Drops every document of `database` and tells its listeners, and with `events` its
    /// triggers.
    async fn clear(&self, database: &str, events: bool) {
        // No target, so no lock to wait for; the database's lock orders the clear among
        // commits for the change feed.
        let _ = self
            .transactions
            .commit(database, None, &[], || {
                let triggers = self
                    .triggers
                    .as_ref()
                    .filter(|triggers| events && triggers.watches(database));
                let mut gone = Vec::new();
                if triggers.is_some() {
                    let at = self.store.latest_read_time(database);
                    self.store
                        .scan_descendants(database, &ResourcePath::root(), at, &mut |doc| {
                            gone.push(Change {
                                path: Arc::clone(&doc.path),
                                before: Some(Arc::clone(doc)),
                                after: None,
                            });
                            ControlFlow::Continue(())
                        });
                }
                let at = self.store.clear_database(database);
                if let Some(triggers) = triggers {
                    triggers.committed(database, at, &gone.iter().collect::<Vec<_>>());
                }
                self.changes.publish_cleared(database, at);
                Ok::<(), Status>(())
            })
            .await;
    }

    /// `POST /reset`: every database, and every open transaction. With `--seed_from_export`,
    /// every database is seeded again on its next access.
    pub(crate) async fn reset(&self) {
        for database in self.store.database_names() {
            self.clear(&database, false).await;
        }
        if let Some(seeder) = &self.seeder {
            seeder.forget();
        }
        self.transactions.clear();
    }

    /// `:bulkDelete` (checked by the caller): in one commit, the documents of `database` in a
    /// collection whose ID is one of `collection_ids` (every collection without one), in a
    /// namespace of `namespace_ids` (every namespace without one): Firestore has the namespace
    /// `""` alone, so another namespace holds nothing (`tests/fixtures/bulk_delete.json`).
    pub(crate) async fn bulk_delete(
        &self,
        database: &str,
        collection_ids: &[String],
        namespace_ids: &[String],
    ) -> Result<(), Status> {
        if !namespace_ids.is_empty() && !namespace_ids.iter().any(String::is_empty) {
            return Ok(());
        }
        self.transactions
            .commit(database, None, &[], || {
                let at = self.store.latest_read_time(database);
                let mut writes: Vec<Write> = Vec::new();
                self.store
                    .scan_descendants(database, &ResourcePath::root(), at, &mut |doc| {
                        let named = doc
                            .path
                            .collection_id()
                            .is_some_and(|id| collection_ids.iter().any(|c| c == id));
                        if collection_ids.is_empty() || named {
                            writes.push(Write {
                                operation: Some(Operation::Delete(Name::document_name(
                                    database, &doc.path,
                                ))),
                                ..Write::default()
                            });
                        }
                        ControlFlow::Continue(())
                    });
                if writes.is_empty() {
                    return Ok(());
                }
                self.commit_writes(database, &writes, None).map(|_| ())
            })
            .await
    }

    /// Deletes the document or collection at `path` in `database` and everything below it,
    /// in one commit. The `Authorization` header is read after the path, as by the RPCs.
    pub(crate) async fn delete_tree(
        &self,
        database: &str,
        path: &str,
        authorization: Option<&str>,
    ) -> Result<(), Status> {
        let name = format!("{database}/documents/{path}");
        let root = names::any(&name)?.path;
        self.admit(database, || auth::from_header(authorization))?;
        self.transactions
            .commit(database, None, &[], || {
                let at = self.store.latest_read_time(database);
                let mut doomed: Vec<ResourcePath> = Vec::new();
                if root.is_document() && self.store.get(database, &root, at).is_some() {
                    doomed.push(root.clone());
                }
                self.store
                    .scan_descendants(database, &root, at, &mut |doc| {
                        doomed.push((*doc.path).clone());
                        ControlFlow::Continue(())
                    });
                if doomed.is_empty() {
                    return Ok(());
                }
                let writes: Vec<Write> = doomed
                    .iter()
                    .map(|path| Write {
                        operation: Some(Operation::Delete(Name::document_name(database, path))),
                        ..Write::default()
                    })
                    .collect();
                self.commit_writes(database, &writes, None).map(|_| ())
            })
            .await
    }
}
