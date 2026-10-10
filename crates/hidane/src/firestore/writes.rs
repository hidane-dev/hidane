//! Applying one `Write` inside a commit.

use hidane_core::{
    field_path::{FieldPath, apply_mask},
    store::{ReadTime, WriteBatch},
};
use hidane_proto::google::firestore::v1::{
    Precondition, Write, WriteResult, precondition::ConditionType, write::Operation,
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
    if !write.update_transforms.is_empty()
        || matches!(write.operation, Some(Operation::Transform(_)))
    {
        return Err(Status::unimplemented(
            "Field transforms are not implemented yet (https://github.com/hidane-dev/hidane/issues/23)",
        ));
    }
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
            let fields = match &write.update_mask {
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
            // Writing what is already stored leaves update_time alone (observed on the
            // official emulator).
            if let Some(existing) = &existing
                && existing.fields() == fields
            {
                return Ok(WriteResult {
                    update_time: Some(existing.update_time),
                    transform_results: Vec::new(),
                });
            }
            batch.set(&path, fields);
            Ok(WriteResult {
                update_time: Some(batch.commit_time().to_timestamp()),
                transform_results: Vec::new(),
            })
        }
        Some(Operation::Transform(_)) | None => unreachable!("handled above"),
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
