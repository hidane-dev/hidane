//! Export and import in the official emulator's format (`hidane_core::export`,
//! `docs/export-format.md`): the files, and the endpoints firebase-tools calls.
//!
//! - `POST /emulator/v1/projects/{p}:export` (also `…/projects/{p}/databases/{d}:export`)
//!   with `{database, export_directory, export_name}` writes `export_directory/export_name/`;
//!   firebase-tools calls it for `--export-on-exit` and `emulators:export`. The project in the
//!   path is not used.
//! - `POST …:import` with `{database, export_directory}`, where `export_directory` names an
//!   `.overall_export_metadata` file, writes that export's documents into `database`.
//! - `--seed_from_export` (`firebase emulators:start --import`) reads an export at startup;
//!   `firestore::seed` hands it to every database on first access.
//! - `POST …:bulkDelete` with `{database, collection_ids, namespace_ids}` deletes, as Datastore's
//!   bulk delete does, the documents of every collection with one of the IDs, or every document
//!   for the namespace `""` (`tests/fixtures/bulk_delete.json`, #36).
//!
//! The bodies are read as protobuf-java's JSON parser reads them, leniently ([`crate::gson`]):
//! either spelling of a key, `null` for an empty value, a number or a boolean as its text; any
//! other key or shape is "Payload isn't valid for request." Error messages are the official
//! emulator's (`tests/fixtures/export_import.json`).

use std::{
    fs,
    io::{self, BufWriter, ErrorKind, Write as _},
    path::Path,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    body::Body,
    http::{HeaderMap, Method},
    response::Response,
};
use hidane_core::{
    export::{self, ExportedDocument, OverallError, Pointer, entity, log},
    store::StoredDocument,
};
use serde_json::Value as Json;
use tonic::Status;

use crate::{FirestoreService, firestore::auth, rest};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verb {
    Export,
    Import,
    BulkDelete,
}

/// `/emulator/v1/projects/{p}:export`, `/emulator/v1/projects/{p}/databases/{d}:import`, …
pub(crate) fn route(path: &str) -> Option<Verb> {
    let rest = path.strip_prefix("/emulator/v1/projects/")?;
    let (target, verb) = rest.rsplit_once(':')?;
    let verb = match verb {
        "export" => Verb::Export,
        "import" => Verb::Import,
        "bulkDelete" => Verb::BulkDelete,
        _ => return None,
    };
    let segments: Vec<&str> = target.split('/').collect();
    match segments.as_slice() {
        [project] if !project.is_empty() => Some(verb),
        [project, "databases", database] if !project.is_empty() && !database.is_empty() => {
            Some(verb)
        }
        _ => None,
    }
}

/// The largest body read (as REST bodies).
const MAX_BODY: usize = 16 * 1024 * 1024;

pub(crate) async fn handle(
    firestore: FirestoreService,
    verb: Verb,
    method: &Method,
    headers: &HeaderMap,
    body: Body,
) -> Response {
    if method != Method::POST {
        return crate::not_found_response();
    }
    if verb == Verb::BulkDelete {
        return bulk_delete(firestore, headers, body).await;
    }
    let result = async {
        auth::from_header(crate::authorization(headers))?;
        let body = axum::body::to_bytes(body, MAX_BODY)
            .await
            .map_err(|_| invalid_payload())?;
        let request = Request::parse(&body, verb)?;
        match verb {
            Verb::Export => {
                firestore
                    .export(&request.database, &request.directory, &request.name)
                    .await
            }
            Verb::Import => {
                firestore
                    .import_file(&request.database, &request.directory)
                    .await
            }
            Verb::BulkDelete => unreachable!("answered by bulk_delete"),
        }
    }
    .await;
    match result {
        Ok(()) => crate::empty_json(),
        Err(status) => rest::error(&status),
    }
}

/// `:bulkDelete`: the body, its database, the `Authorization` header (between the checks of
/// `--single_project_mode`), then the IDs, in that order (`tests/fixtures/bulk_delete.json`).
async fn bulk_delete(firestore: FirestoreService, headers: &HeaderMap, body: Body) -> Response {
    // A form is not a request this endpoint reads, as on the official emulator.
    if headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/x-www-form-urlencoded"))
    {
        return crate::not_found_response();
    }
    let result = async {
        let body = axum::body::to_bytes(body, MAX_BODY)
            .await
            .map_err(|_| invalid_payload())?;
        let request = Request::parse(&body, Verb::BulkDelete)?;
        crate::firestore::names::database(&request.database)?;
        firestore.admit(&request.database, || {
            auth::from_header(crate::authorization(headers))
        })?;
        if request.collection_ids.is_empty() && request.namespace_ids.is_empty() {
            return Err(Status::failed_precondition(
                "collection_ids and namespace_ids cannot both be empty.",
            ));
        }
        firestore
            .bulk_delete(
                &request.database,
                &request.collection_ids,
                &request.namespace_ids,
            )
            .await
    }
    .await;
    match result {
        Ok(()) => crate::empty_json(),
        Err(status) => rest::error(&status),
    }
}

fn invalid_payload() -> Status {
    Status::invalid_argument("Payload isn't valid for request.")
}

#[derive(Debug, Default)]
struct Request {
    database: String,
    directory: String,
    name: String,
    collection_ids: Vec<String>,
    namespace_ids: Vec<String>,
}

/// A scalar as protobuf-java reads it into a string: `null` empty, a number or a boolean as its
/// text.
fn text(value: Json) -> Result<String, Status> {
    match value {
        Json::Null => Ok(String::new()),
        Json::String(s) => Ok(s),
        Json::Number(n) => Ok(n.to_string()),
        Json::Bool(b) => Ok(b.to_string()),
        Json::Array(_) | Json::Object(_) => Err(invalid_payload()),
    }
}

/// A repeated string: `null` empty, each element a scalar but `null`.
fn texts(value: Json) -> Result<Vec<String>, Status> {
    match value {
        Json::Null => Ok(Vec::new()),
        Json::Array(values) => values
            .into_iter()
            .map(|value| match value {
                Json::Null => Err(invalid_payload()),
                value => text(value),
            })
            .collect(),
        _ => Err(invalid_payload()),
    }
}

impl Request {
    fn parse(body: &[u8], verb: Verb) -> Result<Self, Status> {
        let json = if body.iter().all(u8::is_ascii_whitespace) {
            Json::Null
        } else {
            crate::gson::strict(body)
                .and_then(|text| serde_json::from_str(&text).ok())
                .ok_or_else(invalid_payload)?
        };
        let fields = match json {
            Json::Null => serde_json::Map::new(),
            Json::Object(fields) => fields,
            _ => return Err(invalid_payload()),
        };
        let mut request = Self::default();
        for (key, value) in fields {
            match (key.as_str(), verb) {
                ("database", _) => request.database = text(value)?,
                ("export_directory" | "exportDirectory", Verb::Export | Verb::Import) => {
                    request.directory = text(value)?;
                }
                ("export_name" | "exportName", Verb::Export) => request.name = text(value)?,
                ("collection_ids" | "collectionIds", Verb::BulkDelete) => {
                    request.collection_ids = texts(value)?;
                }
                ("namespace_ids" | "namespaceIds", Verb::BulkDelete) => {
                    request.namespace_ids = texts(value)?;
                }
                _ => return Err(invalid_payload()),
            }
        }
        Ok(request)
    }
}

fn now_micros() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_micros()).unwrap_or(i64::MAX))
}

/// Writes `documents` of `projects/{project}/databases/{database_id}` as the export
/// `directory/name`. An empty export is its overall metadata file alone.
pub(crate) fn write(
    directory: &Path,
    name: &str,
    project: &str,
    database_id: &str,
    documents: &[Arc<StoredDocument>],
) -> io::Result<()> {
    let started = now_micros();
    let root = directory.join(name);
    let pointer = if documents.is_empty() {
        fs::create_dir_all(&root)?;
        None
    } else {
        let metadata = root.join(export::KIND_METADATA);
        let kind = metadata.parent().expect("the kind directory");
        fs::create_dir_all(kind)?;
        let output = BufWriter::new(fs::File::create(kind.join(export::OUTPUT))?);
        let mut log = log::Writer::new(output);
        for doc in documents {
            log.add_record(&entity::encode(
                project,
                database_id,
                &doc.path,
                &doc.fields(),
            ))?;
        }
        let bytes = log.len();
        log.into_inner().flush()?;
        fs::write(
            &metadata,
            export::export_metadata(name, started, now_micros()),
        )?;
        Some(Pointer {
            path: export::KIND_METADATA.to_owned(),
            entities: documents.len() as u64,
            bytes,
        })
    };
    fs::write(
        root.join(format!("{name}.overall_export_metadata")),
        export::overall_metadata(pointer.as_ref()),
    )
}

/// Reads the export whose `.overall_export_metadata` file is `path`. The error is the
/// official emulator's message.
pub fn read(path: &Path) -> Result<Vec<ExportedDocument>, String> {
    const UNPARSABLE: &str = "Failed to parse overall export metadata file";
    let data = fs::read(path).map_err(|_| UNPARSABLE.to_owned())?;
    let pointers = export::parse_overall_metadata(&data).map_err(|err| {
        match err {
            OverallError::Version => "Overall export metadata file version not supported",
            OverallError::Malformed => UNPARSABLE,
        }
        .to_owned()
    })?;
    let base = path.parent().unwrap_or(Path::new(""));
    let mut documents = Vec::new();
    for pointer in pointers {
        let metadata_path = base.join(&pointer);
        let metadata =
            fs::read(&metadata_path).map_err(|err| java_message(&metadata_path, &err))?;
        let outputs = export::parse_export_metadata(&metadata).map_err(|_| {
            format!(
                "Failed to parse export metadata file:{}",
                metadata_path.display()
            )
        })?;
        let kind = metadata_path.parent().unwrap_or(Path::new(""));
        for output in outputs {
            let output_path = kind.join(output);
            let data = fs::read(&output_path)
                .map_err(|_| format!("Failed parse entity file:{}", output_path.display()))?;
            for record in log::records(&data) {
                let record = record.map_err(|err| err.to_string())?;
                documents.push(entity::decode(&record).map_err(|_| "Invalid record".to_owned())?);
            }
        }
    }
    Ok(documents)
}

/// A file that cannot be opened, as Java's `FileNotFoundException` puts it.
fn java_message(path: &Path, err: &io::Error) -> String {
    let reason = match err.kind() {
        ErrorKind::NotFound => "No such file or directory".to_owned(),
        ErrorKind::PermissionDenied => "Permission denied".to_owned(),
        _ if path.is_dir() => "Is a directory".to_owned(),
        _ => err.to_string(),
    };
    format!("{} ({reason})", path.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes() {
        assert_eq!(route("/emulator/v1/projects/p:export"), Some(Verb::Export));
        assert_eq!(
            route("/emulator/v1/projects/p/databases/(default):import"),
            Some(Verb::Import)
        );
        for path in [
            "/emulator/v1/projects:export",
            "/emulator/v1/x:export",
            "/emulator/v1/projects/p:export/",
            "/emulator/v1/projects/p:exportx",
            "/emulator/v1/projects/p/databases:export",
            "/emulator/v1/projects/p/documents/d:export",
        ] {
            assert_eq!(route(path), None, "{path}");
        }
    }

    #[test]
    fn bodies_are_read_as_protobuf_json() {
        let request = Request::parse(
            br#"{"database":1,"exportDirectory":"d","export_name":null}"#,
            Verb::Export,
        )
        .unwrap();
        assert_eq!(
            (
                request.database.as_str(),
                request.directory.as_str(),
                request.name.as_str()
            ),
            ("1", "d", "")
        );
        assert!(Request::parse(b"", Verb::Export).is_ok());
        assert!(Request::parse(b"[]", Verb::Export).is_err());
        assert!(Request::parse(br#"{"export_name":"n"}"#, Verb::Import).is_err());
        assert!(Request::parse(br#"{"database":{}}"#, Verb::Import).is_err());
    }
}
