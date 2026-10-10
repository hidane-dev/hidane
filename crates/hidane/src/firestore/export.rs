//! `POST /emulator/v1/projects/{p}:export` and `:import`, the service's side: what is read,
//! what is written, with which errors (`crate::export` has the files and the endpoints).

use std::{
    ops::ControlFlow,
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use hidane_core::{export::ExportedDocument, path::ResourcePath};
use tonic::Status;

use super::{FirestoreService, names, store_error};

impl FirestoreService {
    /// Exports `database` to `directory/name`, as one consistent snapshot. Without a name, the
    /// export is named `firestore_export_{seconds since the epoch}`.
    pub(crate) async fn export(
        &self,
        database: &str,
        directory: &str,
        name: &str,
    ) -> Result<(), Status> {
        names::database(database)?;
        let directory = PathBuf::from(directory);
        if directory.as_os_str().is_empty() || !directory.is_dir() {
            return Err(Status::failed_precondition(
                "export_directory must be a directory",
            ));
        }
        let name = if name.is_empty() {
            let seconds = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            format!("firestore_export_{seconds}")
        } else {
            name.to_owned()
        };
        let at = self.store.latest_read_time(database);
        let mut documents = Vec::new();
        self.store
            .scan_descendants(database, &ResourcePath::root(), at, &mut |doc| {
                documents.push(Arc::clone(doc));
                ControlFlow::Continue(())
            });
        let (project, database_id) = split(database);
        tokio::task::spawn_blocking(move || {
            crate::export::write(&directory, &name, &project, &database_id, &documents)
        })
        .await
        .ok()
        .and_then(Result::ok)
        .ok_or_else(|| Status::internal("failed to write export"))
    }

    /// `:import`: reads the export whose `.overall_export_metadata` file is `path`, then
    /// [`Self::import`]s it.
    pub(crate) async fn import_file(&self, database: &str, path: &str) -> Result<(), Status> {
        names::database(database)?;
        let path = PathBuf::from(path);
        let documents = tokio::task::spawn_blocking(move || crate::export::read(&path))
            .await
            .map_err(|err| Status::internal(err.to_string()))?
            .map_err(Status::invalid_argument)?;
        self.import(database, &documents).await
    }

    /// Writes the documents of an export whose key names `database`'s ID into `database`, in
    /// one commit, keeping the rest of the database. A document that already holds the same
    /// fields keeps its `update_time`. The export's project is not kept; references keep
    /// theirs, except those relative to the importing project.
    pub(crate) async fn import(
        &self,
        database: &str,
        documents: &[ExportedDocument],
    ) -> Result<(), Status> {
        let (project, id) = split(database);
        let documents: Vec<&ExportedDocument> = documents
            .iter()
            .filter(|doc| doc.database_id == id)
            .collect();
        self.transactions
            .commit(database, None, &[], || {
                let commit = self
                    .store
                    .commit(database, &mut |batch| {
                        for doc in &documents {
                            let fields = doc.fields_in(&project);
                            if batch
                                .get(&doc.path)
                                .is_some_and(|existing| existing.has_fields(&fields))
                            {
                                continue;
                            }
                            batch.set(&doc.path, fields);
                        }
                        Ok(())
                    })
                    .map_err(|err| store_error(&err))?;
                self.changes.publish(database, commit);
                Ok(())
            })
            .await
    }
}

/// `projects/{p}/databases/{d}` → `(p, d)`, for a name [`names::database`] accepted.
fn split(database: &str) -> (String, String) {
    let mut parts = database.split('/');
    let project = parts.nth(1).unwrap_or_default().to_owned();
    let id = parts.nth(1).unwrap_or_default().to_owned();
    (project, id)
}
