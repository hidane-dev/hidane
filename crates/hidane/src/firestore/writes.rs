//! Applying one `Write` inside a commit.

use hidane_core::{
    field_path::{FieldPath, Fields, apply_mask},
    store::{ReadTime, WriteBatch},
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
        Some(Operation::Delete(name)) => Some(name),
        Some(Operation::Transform(transform)) => Some(&transform.document),
        None => None,
    }
}

/// Applies `write` to `batch` (which must belong to `database`). Nothing is written when an
/// error is returned, so a caller may continue with other writes.
pub fn apply(
    batch: &mut dyn WriteBatch,
    database: &str,
    write: &Write,
) -> Result<WriteResult, Status> {
    let name = target(write).ok_or_else(|| Status::invalid_argument("Write has no operation."))?;
    let parsed = names::document(name)?;
    if parsed.database != database {
        return Err(Status::invalid_argument(format!(
            "Document \"{name}\" is not in database \"{database}\"."
        )));
    }
    let path = parsed.path;
    let existing = batch.get(&path);
    check_precondition(write.current_document.as_ref(), existing.as_deref(), name)?;

    match &write.operation {
        Some(Operation::Delete(_)) => {
            batch.delete(&path);
            Ok(WriteResult::default())
        }
        Some(Operation::Update(document)) => {
            validate::fields(&document.fields)?;
            let mut fields = match &write.update_mask {
                None => document.fields.clone(),
                Some(mask) => {
                    let mask = mask
                        .field_paths
                        .iter()
                        .map(|p| FieldPath::parse(p).map_err(Status::invalid_argument))
                        .collect::<Result<Vec<_>, _>>()?;
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
    path: &hidane_core::path::ResourcePath,
    existing: Option<&hidane_core::store::StoredDocument>,
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
    existing: Option<&hidane_core::store::StoredDocument>,
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
