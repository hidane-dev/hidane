//! `POST /emulator/v1/projects/{p}:export` and `:import` in Datastore mode, and `--import-data`
//! (`tests/fixtures/datastore_export.json`): an export of a database's entities, every namespace,
//! in the official emulator's format ([`hidane_core::export::datastore`]), read back as it reads
//! it.
//!
//! The database is `projects/{p}/databases/{d}`, the default one as an empty `d`: in this mode
//! `(default)` names none. An export without a name is `datastore_export_{seconds}`. Its records
//! are written in key order, where the official emulator writes them in an order of its own
//! (D093).

use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use hidane_core::export::datastore;
use hidane_proto::{
    google::datastore::v1::{Key, key::path_element::IdType},
    ordered::Properties,
};
use tonic::Status;

use super::{
    DatastoreService, Stored,
    keys::{Database, Id, Located},
};

/// `projects/{p}/databases/{d}`, as the official emulator reads it in Datastore mode.
fn database(name: &str) -> Result<Database, Status> {
    let invalid = || Status::invalid_argument(format!("Invalid database path: {name}"));
    let rest = name.strip_prefix("projects/").ok_or_else(invalid)?;
    let (project, database) = rest.split_once("/databases/").ok_or_else(invalid)?;
    if project.is_empty() || project.contains('/') || database.contains('/') {
        return Err(invalid());
    }
    if database == "(default)" {
        return Err(Status::invalid_argument(
            "\"(default)\" is an invalid database id.",
        ));
    }
    Ok(Database {
        project: project.to_owned(),
        database: database.to_owned(),
    })
}

/// Where a key read from an export lives: its namespace and path.
fn located(key: &Key) -> Option<Located> {
    let path = key
        .path
        .iter()
        .map(|element| {
            let id = match element.id_type.as_ref()? {
                IdType::Id(n) => Id::Id(*n),
                IdType::Name(name) => Id::Name(name.clone()),
            };
            Some((element.kind.clone(), id))
        })
        .collect::<Option<Vec<_>>>()?;
    Some(Located {
        namespace: key
            .partition_id
            .as_ref()
            .map(|p| p.namespace_id.clone())
            .unwrap_or_default(),
        path,
    })
}

impl DatastoreService {
    /// `:export`: every entity of `database`, as one snapshot, to `directory/name`.
    pub(crate) async fn export(
        &self,
        database: &str,
        directory: &str,
        name: &str,
    ) -> Result<(), Status> {
        let database = self::database(database)?;
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
            format!("datastore_export_{seconds}")
        } else {
            name.to_owned()
        };
        let records: Vec<Vec<u8>> = {
            let mut data = self.data();
            data.database(&database)
                .entities
                .iter()
                .map(|(located, stored)| {
                    datastore::encode(&database.key(located), &stored.properties)
                })
                .collect()
        };
        tokio::task::spawn_blocking(move || {
            crate::export::write_records(&directory, &name, records.into_iter())
        })
        .await
        .ok()
        .and_then(Result::ok)
        .ok_or_else(|| Status::internal("failed to write export"))
    }

    /// `:import`: the entities of the export whose `.overall_export_metadata` file is `path`,
    /// written into `database`.
    pub(crate) async fn import_file(&self, database: &str, path: &str) -> Result<(), Status> {
        let database = self::database(database)?;
        let path = PathBuf::from(path);
        let entities = tokio::task::spawn_blocking(move || crate::export::read_datastore(&path))
            .await
            .map_err(|err| Status::internal(err.to_string()))?
            .map_err(Status::invalid_argument)?;
        self.write_entities(&database, entities);
        Ok(())
    }

    /// `--import-data`: each entity of an export, in the project and database its key names.
    pub fn seed(&self, entities: Vec<(Key, Properties)>) {
        let mut by_database: Vec<(Database, Vec<(Key, Properties)>)> = Vec::new();
        for (key, properties) in entities {
            let partition = key.partition_id.clone().unwrap_or_default();
            let database = Database {
                project: partition.project_id,
                database: partition.database_id,
            };
            match by_database.iter_mut().find(|(d, _)| *d == database) {
                Some((_, list)) => list.push((key, properties)),
                None => by_database.push((database, vec![(key, properties)])),
            }
        }
        for (database, entities) in by_database {
            self.write_entities(&database, entities);
        }
    }

    /// Writes `entities` into `database` in one commit, whatever is there already.
    fn write_entities(&self, database: &Database, entities: Vec<(Key, Properties)>) {
        let mut data = self.data();
        let micros = data.next_micros();
        let stored = data.database(database);
        for (key, properties) in entities {
            let Some(located) = located(&key) else {
                continue;
            };
            let created = stored.entities.get(&located).map_or(micros, |p| p.created);
            stored.entities.insert(
                located,
                Stored {
                    properties,
                    version: micros,
                    created,
                    updated: micros,
                },
            );
        }
    }
}
