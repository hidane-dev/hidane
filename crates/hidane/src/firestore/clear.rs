//! Clearing data (`POST /reset`, `DELETE /emulator/v1/…/documents[/{path}]`), with the official
//! emulator's scope: a reset drops every database, the database endpoint drops one, and a path
//! drops the document or collection there and everything below it.
//!
//! Unlike the official emulator, which tells attached listeners nothing on a reset, every clear
//! reaches them: a cleared database as a `Cleared` event, a deleted path as an ordinary commit.

use std::ops::ControlFlow;

use hidane_core::path::ResourcePath;
use hidane_proto::google::firestore::v1::{Write, write::Operation};
use tonic::Status;

use super::{FirestoreService, auth, names, names::Name};

impl FirestoreService {
    /// Drops every document of `database` and tells its listeners.
    pub(crate) async fn clear_database(&self, database: &str) {
        // No target, so no lock to wait for; the database's lock orders the clear among
        // commits for the change feed.
        let _ = self
            .transactions
            .commit(database, None, &[], || {
                let at = self.store.clear_database(database);
                self.changes.publish_cleared(database, at);
                Ok::<(), Status>(())
            })
            .await;
    }

    /// `POST /reset`: every database, and every open transaction. With `--seed_from_export`,
    /// every database is seeded again on its next access.
    pub(crate) async fn reset(&self) {
        for database in self.store.database_names() {
            self.clear_database(&database).await;
        }
        if let Some(seeder) = &self.seeder {
            seeder.forget();
        }
        self.transactions.clear();
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
        auth::from_header(authorization)?;
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
                self.commit_writes(database, &writes).map(|_| ())
            })
            .await
    }
}
