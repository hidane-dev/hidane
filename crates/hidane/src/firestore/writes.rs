//! Applying one `Write` inside a commit.

use std::collections::HashMap;

use hidane_core::{
    field_path::{FieldPath, Fields, apply_mask},
    path::ResourcePath,
    store::{ReadTime, StoredDocument, WriteBatch},
    transform,
};
use hidane_proto::google::firestore::v1::{
    Precondition, Value, Write, WriteResult, document_transform::FieldTransform,
    precondition::ConditionType, write::Operation,
};
use tonic::Status;

use super::{names, validate};

/// The document a write targets, for duplicate detection.
pub fn target(write: &Write) -> Option<&str> {
    match &write.operation {
        Some(Operation::Update(doc)) => Some(&doc.name),
        Some(Operation::Delete(name) | Operation::Verify(name)) => Some(name),
        Some(Operation::Transform(transform)) => Some(&transform.document),
        None => None,
    }
}

/// The official emulator's rules for a commit that verifies a document (the SDKs' transactions
/// do, for documents they read without writing): a document is verified at most once, and
/// never also written.
pub fn check_verifies(writes: &[Write]) -> Result<(), Status> {
    let mut verified: HashMap<&str, bool> = HashMap::new();
    for write in writes {
        let Some(name) = target(write) else {
            continue;
        };
        let verify = matches!(write.operation, Some(Operation::Verify(_)));
        let conflict = match (verified.get(name), verify) {
            (Some(true), true) => "verified more than once",
            (Some(true), false) => "verified and then written",
            (Some(false), true) => "written and then verified",
            (Some(false), false) | (None, _) => {
                verified.entry(name).or_insert(verify);
                continue;
            }
        };
        return Err(Status::invalid_argument(format!(
            "the same document cannot be {conflict} in a single request"
        )));
    }
    Ok(())
}

/// Checks everything that does not depend on stored data, and returns the target document.
/// Commits call this before waiting for locks: the official emulator rejects an invalid write
/// at once, even when its document is locked, and leaves the transaction open.
pub fn validate(database: &str, write: &Write) -> Result<ResourcePath, Status> {
    let name = target(write).ok_or_else(|| Status::invalid_argument("Write has no operation."))?;
    let parsed = names::document(name)?;
    if parsed.database != database {
        return Err(Status::invalid_argument(format!(
            "Document \"{name}\" is not in database \"{database}\"."
        )));
    }
    match &write.operation {
        Some(Operation::Update(document)) => {
            validate::fields(&document.fields)?;
            if let Some(mask) = &write.update_mask {
                parse_mask(&mask.field_paths)?;
            }
        }
        Some(Operation::Verify(_)) => {
            if write
                .current_document
                .as_ref()
                .and_then(|p| p.condition_type.as_ref())
                .is_none()
            {
                return Err(Status::invalid_argument(
                    "a verify must contain a precondition",
                ));
            }
            if write.update_mask.is_some() {
                return Err(Status::invalid_argument(
                    "a verify must not specify a document mask.",
                ));
            }
            if !write.update_transforms.is_empty() {
                return Err(Status::invalid_argument(
                    "a verify must not specify a update transform.",
                ));
            }
        }
        _ => {}
    }
    Ok(parsed.path)
}

fn parse_mask(paths: &[String]) -> Result<Vec<FieldPath>, Status> {
    paths
        .iter()
        .map(|p| FieldPath::parse(p).map_err(Status::invalid_argument))
        .collect()
}

/// Applies `write` to `batch` (which must belong to `database`). Nothing is written when an
/// error is returned, so a caller may continue with other writes.
pub fn apply(
    batch: &mut dyn WriteBatch,
    database: &str,
    write: &Write,
) -> Result<WriteResult, Status> {
    let path = validate(database, write)?;
    let name = target(write).expect("validated");
    let existing = batch.get(&path);
    check_precondition(write.current_document.as_ref(), existing.as_deref(), name)?;

    match &write.operation {
        Some(Operation::Delete(_)) => {
            batch.delete(&path);
            Ok(WriteResult::default())
        }
        // Only the precondition, checked above. The result carries the stored version; for a
        // missing document the official emulator reports a time just before the commit.
        Some(Operation::Verify(_)) => Ok(WriteResult {
            update_time: Some(
                existing
                    .as_ref()
                    .map_or_else(|| batch.commit_time().to_timestamp(), |doc| doc.update_time),
            ),
            transform_results: Vec::new(),
        }),
        Some(Operation::Update(document)) => {
            let mut fields = match &write.update_mask {
                None => document.fields.clone(),
                Some(mask) => {
                    let mask = parse_mask(&mask.field_paths)?;
                    let mut fields = existing
                        .as_ref()
                        .map(|doc| doc.fields())
                        .unwrap_or_default();
                    apply_mask(&mut fields, &document.fields, &mask);
                    fields
                }
            };
            // Transforms run after the update, in order.
            let transform_results =
                apply_transforms(&mut fields, &write.update_transforms, batch.commit_time())?;
            Ok(store(
                batch,
                &path,
                existing.as_deref(),
                fields,
                transform_results,
            ))
        }
        Some(Operation::Transform(document_transform)) => {
            // A standalone transform starts from the stored document, or an empty one.
            let mut fields = existing
                .as_ref()
                .map(|doc| doc.fields())
                .unwrap_or_default();
            let transform_results = apply_transforms(
                &mut fields,
                &document_transform.field_transforms,
                batch.commit_time(),
            )?;
            Ok(store(
                batch,
                &path,
                existing.as_deref(),
                fields,
                transform_results,
            ))
        }
        None => unreachable!("target() returned a name"),
    }
}

fn apply_transforms(
    fields: &mut Fields,
    transforms: &[FieldTransform],
    commit_time: ReadTime,
) -> Result<Vec<Value>, Status> {
    let commit_time = commit_time.to_timestamp();
    let results = transforms
        .iter()
        .map(|t| transform::apply(fields, t, &commit_time).map_err(Status::invalid_argument))
        .collect::<Result<Vec<_>, _>>()?;
    if !transforms.is_empty() {
        // Operands can introduce nested arrays or reserved names.
        validate::fields(fields)?;
    }
    Ok(results)
}

/// Stores `fields` unless they equal what is stored: then the document, and its update_time,
/// stay as they are (observed on the official emulator, also for no-op transforms).
fn store(
    batch: &mut dyn WriteBatch,
    path: &ResourcePath,
    existing: Option<&StoredDocument>,
    fields: Fields,
    transform_results: Vec<Value>,
) -> WriteResult {
    if let Some(existing) = existing
        && existing.fields() == fields
    {
        return WriteResult {
            update_time: Some(existing.update_time),
            transform_results,
        };
    }
    batch.set(path, fields);
    WriteResult {
        update_time: Some(batch.commit_time().to_timestamp()),
        transform_results,
    }
}

/// Production Firestore's wording for exists violations: the official emulator prints its
/// internal Datastore key here instead (see docs/parity-exceptions.md).
fn check_precondition(
    precondition: Option<&Precondition>,
    existing: Option<&StoredDocument>,
    name: &str,
) -> Result<(), Status> {
    match precondition.and_then(|p| p.condition_type.as_ref()) {
        None => Ok(()),
        Some(ConditionType::Exists(true)) if existing.is_none() => {
            Err(Status::not_found(format!("No document to update: {name}")))
        }
        Some(ConditionType::Exists(false)) if existing.is_some() => Err(Status::already_exists(
            format!("Document already exists: {name}"),
        )),
        Some(ConditionType::Exists(_)) => Ok(()),
        Some(ConditionType::UpdateTime(required)) => {
            let required = ReadTime::from_timestamp(required).0;
            let stored = existing.map_or(0, |doc| ReadTime::from_timestamp(&doc.update_time).0);
            if stored == required {
                Ok(())
            } else {
                Err(Status::failed_precondition(format!(
                    "the stored version ({stored}) does not match the required base version ({required})"
                )))
            }
        }
    }
}
