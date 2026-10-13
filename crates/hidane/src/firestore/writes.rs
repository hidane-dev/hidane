//! Applying one `Write` inside a commit.

use std::{collections::HashMap, sync::Arc};

use hidane_core::{
    field_path::{FieldPath, Fields, apply_mask},
    normalize::normalize_value,
    path::ResourcePath,
    store::{ReadTime, StoredDocument, WriteBatch},
    transform,
};
use hidane_proto::google::firestore::v1::{
    Precondition, Value, Write, WriteResult, document_transform::FieldTransform,
    precondition::ConditionType, write::Operation,
};
use hidane_rules::eval::Method;
use tonic::Status;

use super::{
    guard::{self, CheckedWrite},
    names, validate,
};

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

/// What a write is to the official emulator, which stores documents as Datastore entities and
/// collapses a commit's writes to one document into one mutation.
#[derive(Clone, Copy)]
enum Mutation {
    /// An `exists: false` precondition.
    Insert,
    /// An `exists: true` or `update_time` precondition.
    Update,
    /// No precondition (with or without a mask), or a standalone transform.
    Upsert,
    Delete,
}

impl Mutation {
    fn name(self) -> &'static str {
        match self {
            Mutation::Insert => "insert",
            Mutation::Update => "update",
            Mutation::Upsert => "upsert",
            Mutation::Delete => "delete",
        }
    }
}

/// The official emulator's rules for a commit that writes one document more than once
/// (`tools/oracle/same_document_writes.py`), checked before any write is applied: an insert
/// cannot follow anything but a delete, an update cannot follow a delete, nor can a delete with
/// an `exists: true` precondition; and only the first write may carry an `update_time`
/// precondition, which a later one may repeat. What follows is checked against the writes so far
/// collapsed into one: an insert or an update stays one under upserts and updates, a delete then
/// an insert or an upsert is an upsert, and anything then a delete is a delete. Verifies are
/// left to `check_verifies`.
pub fn check_sequences(writes: &[Write]) -> Result<(), Status> {
    // Per document, the collapsed mutation and the first write's version precondition.
    let mut documents: HashMap<&str, (Mutation, Option<i64>)> = HashMap::new();
    for write in writes {
        let Some(name) = target(write) else {
            continue;
        };
        let condition = write
            .current_document
            .as_ref()
            .and_then(|p| p.condition_type.as_ref());
        let version = match condition {
            Some(ConditionType::UpdateTime(time)) => Some(ReadTime::from_timestamp(time).0),
            _ => None,
        };
        let mutation = match (&write.operation, condition) {
            (Some(Operation::Verify(_)) | None, _) => continue,
            (Some(Operation::Delete(_)), _) => Mutation::Delete,
            (_, Some(ConditionType::Exists(false))) => Mutation::Insert,
            (_, Some(ConditionType::Exists(true) | ConditionType::UpdateTime(_))) => {
                Mutation::Update
            }
            (_, None) => Mutation::Upsert,
        };
        let Some((collapsed, base)) = documents.get_mut(name) else {
            documents.insert(name, (mutation, version));
            continue;
        };
        let refused = match (*collapsed, mutation) {
            (Mutation::Delete, Mutation::Insert) => false,
            (_, Mutation::Insert) | (Mutation::Delete, Mutation::Update) => true,
            (Mutation::Delete, Mutation::Delete) => {
                matches!(condition, Some(ConditionType::Exists(true)))
            }
            _ => false,
        };
        if refused {
            return Err(Status::invalid_argument(format!(
                "Cannot {} then {} an entity in the same request.",
                collapsed.name(),
                mutation.name()
            )));
        }
        if let Some(version) = version
            && *base != Some(version)
        {
            let base = base.map_or_else(
                || "null".to_owned(),
                |base| format!("ExactVersion[exactBaseVersion={base}]"),
            );
            return Err(Status::invalid_argument(format!(
                "Cannot collapse mutations with different version preconditions ({base} and \
                 ExactVersion[exactBaseVersion={version}])"
            )));
        }
        *collapsed = match (*collapsed, mutation) {
            (_, Mutation::Delete) => Mutation::Delete,
            (Mutation::Insert | Mutation::Update, _) => *collapsed,
            _ => Mutation::Upsert,
        };
    }
    Ok(())
}

/// Whether a commit checks its verifies (`check_verifies`). The Write stream does not: a verify
/// there is only its precondition, applied in order.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Verifies {
    Checked,
    Applied,
}

/// Checks everything about `writes` that does not depend on stored data, in the official
/// emulator's order (`tools/oracle/commit_checks.py`), and returns their documents: the
/// verifies of the commit, the names of every write, the sequences of writes to one document,
/// then the values of every write. Preconditions and document sizes are checked when the writes
/// are applied. Commits call this before waiting for locks: the official emulator rejects an
/// invalid write at once, even when its document is locked, and leaves the transaction open.
pub fn check(
    database: &str,
    writes: &[Write],
    verifies: Verifies,
) -> Result<Vec<ResourcePath>, Status> {
    if verifies == Verifies::Checked {
        check_verifies(writes)?;
    }
    let targets = writes
        .iter()
        .map(|write| check_names(database, write))
        .collect::<Result<Vec<_>, _>>()?;
    check_sequences(writes)?;
    for (write, path) in writes.iter().zip(&targets) {
        check_values(path, write)?;
    }
    Ok(targets)
}

/// The names of one write, and its shape: what kind of write may carry a mask or transforms,
/// its document's name (which must be in `database`), a verify's precondition, then its fields'
/// names, its mask's paths and its transforms, in that order.
pub fn check_names(database: &str, write: &Write) -> Result<ResourcePath, Status> {
    let kind = match &write.operation {
        None => return Err(Status::invalid_argument("empty write operation")),
        Some(Operation::Update(_)) => None,
        Some(Operation::Delete(_)) => Some("delete"),
        Some(Operation::Verify(_)) => Some("verify"),
        Some(Operation::Transform(_)) => Some("transform"),
    };
    if let Some(kind) = kind {
        if write.update_mask.is_some() {
            return Err(Status::invalid_argument(format!(
                "a {kind} must not specify a document mask."
            )));
        }
        if !write.update_transforms.is_empty() {
            return Err(Status::invalid_argument(format!(
                "a {kind} must not specify a update transform."
            )));
        }
    }
    let name = target(write).expect("an operation");
    let parsed = names::document(name)?;
    // The official emulator acknowledges such a write and keeps it where no read finds it
    // (docs/parity-exceptions.md).
    if parsed.database != database {
        return Err(Status::invalid_argument(format!(
            "Document \"{name}\" is not in database \"{database}\"."
        )));
    }
    match &write.operation {
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
        }
        Some(Operation::Update(document)) => {
            validate::names(&document.fields)?;
            if let Some(mask) = &write.update_mask {
                for path in &mask.field_paths {
                    validate::field_path(path)?;
                }
            }
            validate::transforms(&write.update_transforms)?;
        }
        Some(Operation::Transform(transform)) => {
            if transform.field_transforms.is_empty() {
                return Err(Status::invalid_argument(
                    "The list of FieldTransform must not be empty.",
                ));
            }
            validate::transforms(&transform.field_transforms)?;
        }
        Some(Operation::Delete(_)) | None => {}
    }
    Ok(parsed.path)
}

/// The values of one write whose names are checked: the document an update or a transform
/// writes, its key, then its fields.
pub fn check_values(path: &ResourcePath, write: &Write) -> Result<(), Status> {
    match &write.operation {
        Some(Operation::Update(document)) => validate::values(path, &document.fields),
        Some(Operation::Transform(_)) => validate::key(path),
        _ => Ok(()),
    }
}

fn parse_mask(paths: &[String]) -> Result<Vec<FieldPath>, Status> {
    paths
        .iter()
        .map(|p| FieldPath::parse(p).map_err(Status::invalid_argument))
        .collect()
}

/// How Security Rules check `write` (`tools/oracle/rules_writes.py`): an `exists`
/// precondition decides between create and update, else whether the document exists, which
/// makes the write an upsert; a delete is a delete, and a verify a get of its document
/// (`tools/oracle/rules_verify.py`). The document after the write is left for the caller to
/// fill in.
///
/// `request.transforms` lists the paths of the field transforms in order, and
/// `request.writeFields` those, then the paths of the update mask; each is null for a write
/// without them (`tools/oracle/rules_request_fields.py`).
pub(crate) fn rules_method(batch: &dyn WriteBatch, write: &Write) -> Option<CheckedWrite> {
    let path = names::document(target(write)?).ok()?.path;
    let existing = batch.get(&path);
    let (method, upsert) = match &write.operation {
        Some(Operation::Verify(_)) => (Method::Get, false),
        Some(Operation::Delete(_)) => (Method::Delete, false),
        Some(Operation::Update(_) | Operation::Transform(_)) => match write
            .current_document
            .as_ref()
            .and_then(|p| p.condition_type.as_ref())
        {
            Some(ConditionType::Exists(false)) => (Method::Create, false),
            Some(ConditionType::Exists(true)) => (Method::Update, false),
            _ if existing.is_some() => (Method::Update, true),
            _ => (Method::Create, true),
        },
        None => return None,
    };
    let field_transforms = match &write.operation {
        Some(Operation::Transform(transform)) => transform.field_transforms.as_slice(),
        Some(Operation::Update(_)) => write.update_transforms.as_slice(),
        _ => &[],
    };
    let transforms = (!field_transforms.is_empty()).then(|| {
        field_transforms
            .iter()
            .map(|t| guard::transform_path(&t.field_path))
            .collect::<Vec<_>>()
    });
    let mask = match &write.operation {
        Some(Operation::Update(_)) => write
            .update_mask
            .as_ref()
            .and_then(|mask| parse_mask(&mask.field_paths).ok()),
        _ => None,
    };
    let write_fields = match (&transforms, &mask) {
        (None, None) => None,
        (transforms, mask) => Some(
            transforms
                .iter()
                .flatten()
                .cloned()
                .chain(
                    mask.as_deref()
                        .map(guard::rules_paths)
                        .into_iter()
                        .flatten(),
                )
                .collect(),
        ),
    };
    Some(CheckedWrite {
        method,
        upsert,
        path,
        before: existing,
        after: None,
        write_fields,
        transforms,
    })
}

/// Applies `write`, which `check` has checked, to `batch`. Nothing is written when an error is
/// returned, so a caller may continue with other writes.
pub fn apply(batch: &mut dyn WriteBatch, write: &Write) -> Result<WriteResult, Status> {
    let name = target(write).expect("checked");
    let path = names::document(name)?.path;
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
            // Normalised first, so a value that only differs below a microsecond is no change
            // and does not move (#108).
            let mut input = document.fields.clone();
            input.values_mut().for_each(normalize_value);
            let mut fields = match &write.update_mask {
                None => input,
                Some(mask) => {
                    let mask = parse_mask(&mask.field_paths)?;
                    let mut fields = existing
                        .as_ref()
                        .map(|doc| doc.fields())
                        .unwrap_or_default();
                    apply_mask(&mut fields, &input, &mask);
                    fields
                }
            };
            // Transforms run after the update, in order.
            let transform_results =
                apply_transforms(&mut fields, &write.update_transforms, batch.commit_time())?;
            validate::size(&path, &fields)?;
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
            validate::size(&path, &fields)?;
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

/// Runs `transforms` in request order, for their results. The fields they change then land as
/// a masked update of their paths would put them (#108): the official emulator moves them to
/// the end in the order of a Java `HashMap`, and leaves unchanged ones in place.
fn apply_transforms(
    fields: &mut Fields,
    transforms: &[FieldTransform],
    commit_time: ReadTime,
) -> Result<Vec<Value>, Status> {
    if transforms.is_empty() {
        return Ok(Vec::new());
    }
    let commit_time = commit_time.to_timestamp();
    let mut transformed = fields.clone();
    let results = transforms
        .iter()
        .map(|t| {
            transform::apply(&mut transformed, t, &commit_time).map_err(Status::invalid_argument)
        })
        .collect::<Result<Vec<_>, _>>()?;
    // An array union can bring in nested arrays and vectors with NaN.
    validate::transformed(&transformed)?;
    let paths = transforms
        .iter()
        .map(|t| FieldPath::parse(&t.field_path).map_err(Status::invalid_argument))
        .collect::<Result<Vec<_>, _>>()?;
    apply_mask(fields, &transformed, &paths);
    Ok(results)
}

/// The documents a commit writes (verifies aside), each as it was before the commit and whether
/// one of the commit's writes deleted it.
#[derive(Default)]
pub struct Touched(HashMap<ResourcePath, (Option<Arc<StoredDocument>>, bool)>);

impl Touched {
    /// Notes the document `write` writes; call it before applying the write.
    pub fn record(&mut self, batch: &dyn WriteBatch, write: &Write) {
        let Some(path) = written(write) else {
            return;
        };
        let entry = self
            .0
            .entry(path)
            .or_insert_with_key(|path| (batch.get(path), false));
        if matches!(write.operation, Some(Operation::Delete(_))) {
            entry.1 = true;
        }
    }

    /// Settles the commit's write results, once every write is applied. The official emulator
    /// collapses a commit's writes to one document into one mutation (`check_sequences`), so
    /// each of them reports the document as the whole commit leaves it: no update time when it
    /// is deleted, else its update time. Writes that leave a document as it was, bit for bit,
    /// change nothing and report the time it had, unless one of them deleted it, which makes it
    /// a new document (`tools/oracle/same_document_writes.py`, #262).
    pub fn settle(
        &self,
        batch: &mut dyn WriteBatch,
        writes: &[Write],
        results: &mut [WriteResult],
    ) {
        for (path, (before, deleted)) in &self.0 {
            let update_time = match (before, batch.get(path)) {
                (_, None) => None,
                (Some(before), Some(after)) if !deleted && after.has_fields(&before.fields()) => {
                    batch.revert(path);
                    Some(before.update_time)
                }
                (_, Some(after)) => Some(after.update_time),
            };
            for (write, result) in writes.iter().zip(results.iter_mut()) {
                if written(write).as_ref() == Some(path) {
                    result.update_time = update_time;
                }
            }
        }
    }
}

/// The document a write changes: none for a verify.
fn written(write: &Write) -> Option<ResourcePath> {
    if matches!(write.operation, Some(Operation::Verify(_))) {
        return None;
    }
    names::document(target(write)?).ok().map(|name| name.path)
}

/// Stores `fields` unless they are what is stored, bit for bit: then the document, and its
/// update_time, stay as they are (observed on the official emulator, also for no-op
/// transforms). NaN over NaN is no change; `-0.0` over `0.0` is one
/// (`tools/oracle/negative_zero.py`).
fn store(
    batch: &mut dyn WriteBatch,
    path: &ResourcePath,
    existing: Option<&StoredDocument>,
    mut fields: Fields,
    transform_results: Vec<Value>,
) -> WriteResult {
    // Timestamps are stored to the microsecond, as Firestore does.
    for value in fields.values_mut() {
        normalize_value(value);
    }
    if let Some(existing) = existing
        && existing.has_fields(&fields)
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
