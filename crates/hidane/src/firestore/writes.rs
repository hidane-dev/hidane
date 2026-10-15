//! Applying one `Write` inside a commit.

use std::{
    collections::{HashMap, hash_map::Entry},
    sync::Arc,
};

use hidane_core::{
    field_path::{FieldPath, Fields, apply_mask},
    normalize::normalize_value,
    order::numeric_id,
    path::ResourcePath,
    store::{ReadTime, StoredDocument, WriteBatch},
    transform,
};
use hidane_proto::google::firestore::v1::{
    Value, Write, WriteResult, document_transform::FieldTransform, precondition::ConditionType,
    write::Operation,
};
use hidane_rules::eval::Method;
use prost_types::Timestamp;
use tonic::Status;

use super::{
    guard::{self, CheckedWrite, Sets},
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
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mutation {
    /// An `exists: false` precondition, or an `update_time` one of version 0.
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

/// The writes so far to one document of a commit, collapsed into one mutation.
struct Collapsed {
    /// What the first write was.
    first: Mutation,
    /// What the writes so far are.
    now: Mutation,
    /// The first write's version precondition.
    base: Option<i64>,
}

/// The official emulator's rules for a commit that writes one document more than once
/// (`tools/oracle/same_document_writes.py`), checked before any write is applied: an insert
/// cannot follow anything but a delete, an update cannot follow a delete, nor can a delete with
/// an `exists: true` precondition, and a delete with an `exists: false` one can follow nothing
/// but a delete; only the first write may carry an `update_time` precondition, which a later one
/// may repeat. What follows is checked against the writes so far collapsed into one: anything
/// then a delete is a delete, else an insert or an update when the first write was one (or a
/// delete that needs no document, or needs it), else an upsert. Verifies are left to
/// `check_verifies`.
pub fn check_sequences(writes: &[Write]) -> Result<(), Status> {
    let mut documents: HashMap<&str, Collapsed> = HashMap::new();
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
            // Version 0 is a document that does not exist.
            (_, Some(ConditionType::UpdateTime(_))) if version == Some(0) => Mutation::Insert,
            (_, Some(ConditionType::Exists(true) | ConditionType::UpdateTime(_))) => {
                Mutation::Update
            }
            (_, None) => Mutation::Upsert,
        };
        let Some(collapsed) = documents.get_mut(name) else {
            // A first delete that needs the document, or needs none, counts as an update or an
            // insert for what follows it.
            let first = match (mutation, condition) {
                (Mutation::Delete, Some(ConditionType::Exists(true))) => Mutation::Update,
                (Mutation::Delete, Some(ConditionType::Exists(false))) => Mutation::Insert,
                _ => mutation,
            };
            documents.insert(
                name,
                Collapsed {
                    first,
                    now: mutation,
                    base: version,
                },
            );
            continue;
        };
        let deleted = collapsed.now == Mutation::Delete;
        let refused = match mutation {
            Mutation::Insert => !deleted,
            Mutation::Update => deleted,
            Mutation::Delete => match condition {
                Some(ConditionType::Exists(true)) => deleted,
                Some(ConditionType::Exists(false)) => !deleted,
                _ => false,
            },
            Mutation::Upsert => false,
        };
        if refused {
            return Err(Status::invalid_argument(format!(
                "Cannot {} then {} an entity in the same request.",
                collapsed.now.name(),
                mutation.name()
            )));
        }
        if let Some(version) = version
            && collapsed.base != Some(version)
        {
            let base = collapsed.base.map_or_else(
                || "null".to_owned(),
                |base| format!("ExactVersion[exactBaseVersion={base}]"),
            );
            return Err(Status::invalid_argument(format!(
                "Cannot collapse mutations with different version preconditions ({base} and \
                 ExactVersion[exactBaseVersion={version}])"
            )));
        }
        collapsed.now = match (mutation, collapsed.first) {
            (Mutation::Delete, _) => Mutation::Delete,
            (_, first @ (Mutation::Insert | Mutation::Update)) => first,
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
    enterprise: bool,
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
        check_values(path, write, enterprise)?;
    }
    Ok(targets)
}

/// The names of one write, and its shape: what kind of write may carry a mask or transforms,
/// its document's name (which must be in `database`), a verify's precondition, then its fields'
/// names, its mask's paths and its transforms, then the time of an `update_time` precondition,
/// in that order.
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
                validate::mask_paths(&mask.field_paths)?;
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
    if let Some(ConditionType::UpdateTime(time)) = write
        .current_document
        .as_ref()
        .and_then(|p| p.condition_type.as_ref())
    {
        base_version(time)?;
    }
    Ok(parsed.path)
}

/// The version a timestamp names: its time in microseconds, which must be whole and not
/// before the epoch, for an `update_time` precondition (`tools/oracle/base_versions.py`) and
/// a read's `read_time`, before its age (`tools/oracle/queries.py`, #262).
pub(super) fn base_version(time: &Timestamp) -> Result<i64, Status> {
    if time.nanos % 1_000 != 0 {
        return Err(Status::invalid_argument(
            "timestamp cannot have more than microseconds precision",
        ));
    }
    let version = ReadTime::from_timestamp(time).0;
    if version < 0 {
        return Err(Status::invalid_argument(
            "Only timestamps past epoch are supported.",
        ));
    }
    Ok(version)
}

/// The values of one write whose names are checked: the document an update or a transform
/// writes, its key, then its fields, as an `enterprise` database or a standard one checks them.
pub fn check_values(path: &ResourcePath, write: &Write, enterprise: bool) -> Result<(), Status> {
    match &write.operation {
        Some(Operation::Update(document)) => validate::values(path, &document.fields, enterprise),
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
/// precondition decides between create and update, and so does an `update_time` one (version 0
/// is a create, `tools/oracle/base_versions.py`), else whether the document exists, which makes
/// the write an upsert; a delete is a delete, and a verify a get of its document
/// (`tools/oracle/rules_verify.py`). The document after the write is left for the caller to
/// fill in.
///
/// `request.transforms` lists the paths of the field transforms in order, and
/// `request.writeFields` those, then the paths of the update mask; each is null for a write
/// without them (`tools/oracle/rules_request_fields.py`).
pub(crate) fn rules_method(batch: &dyn WriteBatch, write: &Write) -> Option<CheckedWrite> {
    let path = names::document(target(write)?).ok()?.path;
    let existing = batch.get(&path);
    let condition = write
        .current_document
        .as_ref()
        .and_then(|p| p.condition_type.as_ref());
    let by_existence = if existing.is_some() {
        (Method::Update, true)
    } else {
        (Method::Create, true)
    };
    let (method, upsert) = match &write.operation {
        Some(Operation::Verify(_)) => (Method::Get, false),
        Some(Operation::Delete(_)) => (Method::Delete, false),
        Some(Operation::Update(_) | Operation::Transform(_)) => match condition {
            Some(ConditionType::Exists(false)) => (Method::Create, false),
            Some(ConditionType::UpdateTime(time)) if ReadTime::from_timestamp(time).0 == 0 => {
                (Method::Create, false)
            }
            Some(ConditionType::Exists(true) | ConditionType::UpdateTime(_)) => {
                (Method::Update, false)
            }
            None => by_existence,
        },
        None => return None,
    };
    // What a write after a delete takes (`check_sequences`): an insert after `exists: false`,
    // an update after `exists: true`, else an upsert.
    let then = match (method, condition) {
        (Method::Delete, Some(ConditionType::Exists(false))) => (Method::Create, false),
        (Method::Delete, Some(ConditionType::Exists(true))) => (Method::Update, false),
        (Method::Delete, _) => by_existence,
        _ => (method, upsert),
    };
    let field_transforms = match &write.operation {
        Some(Operation::Transform(transform)) => transform.field_transforms.as_slice(),
        Some(Operation::Update(_)) => write.update_transforms.as_slice(),
        _ => &[],
    };
    let transforms = field_transforms
        .iter()
        .map(|t| guard::transform_path(&t.field_path))
        .collect();
    let sets = match &write.operation {
        Some(Operation::Update(_)) => match write.update_mask.as_ref() {
            Some(mask) => parse_mask(&mask.field_paths).map_or(Sets::Whole, Sets::Mask),
            None => Sets::Whole,
        },
        _ => Sets::Nothing,
    };
    Some(CheckedWrite {
        method,
        upsert,
        path,
        before: existing,
        after: None,
        sets,
        transforms,
        then,
        deleted: method == Method::Delete,
    })
}

/// Applies `write`, which `check` has checked, to `batch`. Nothing is written when an error is
/// returned, so a caller may continue with other writes. A write that is not the commit's first
/// to its document (`Touched::record`) can only repeat the first one's `update_time`
/// precondition (`check_sequences`), which the official emulator checks once, against the
/// document as it was before the commit: it is not checked again.
pub fn apply(
    batch: &mut dyn WriteBatch,
    write: &Write,
    first: bool,
    enterprise: bool,
) -> Result<WriteResult, Status> {
    let name = target(write).expect("checked");
    let parsed = names::document(name)?;
    let path = parsed.path.clone();
    let existing = batch.get(&path);
    let precondition = write
        .current_document
        .as_ref()
        .and_then(|p| p.condition_type.as_ref())
        .filter(|condition| first || !matches!(condition, ConditionType::UpdateTime(_)));
    check_precondition(
        precondition,
        matches!(write.operation, Some(Operation::Delete(_))),
        existing.as_deref(),
        &parsed,
        name,
        || batch.deleted_at(&path),
    )?;

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
            let transform_results = apply_transforms(
                &mut fields,
                &write.update_transforms,
                batch.commit_time(),
                enterprise,
            )?;
            validate::size(&path, &fields, enterprise)?;
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
                enterprise,
            )?;
            validate::size(&path, &fields, enterprise)?;
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
    enterprise: bool,
) -> Result<Vec<Value>, Status> {
    if transforms.is_empty() {
        return Ok(Vec::new());
    }
    let commit_time = commit_time.to_timestamp();
    // A transform of `__name__` reads and writes the field `__key__`.
    let paths = transforms
        .iter()
        .map(|t| validate::transform_path(&t.field_path))
        .collect::<Result<Vec<_>, _>>()?;
    let mut transformed = fields.clone();
    let results = transforms
        .iter()
        .zip(&paths)
        .map(|(t, path)| {
            transform::apply_at(&mut transformed, path, t, &commit_time)
                .map_err(Status::invalid_argument)
        })
        .collect::<Result<Vec<_>, _>>()?;
    // An array union can bring in nested arrays and vectors with NaN.
    validate::transformed(&transformed, enterprise)?;
    apply_mask(fields, &transformed, &paths);
    Ok(results)
}

/// The documents a commit writes (verifies aside), each as it was before the commit and whether
/// one of the commit's writes deleted it.
#[derive(Default)]
pub struct Touched(HashMap<ResourcePath, (Option<Arc<StoredDocument>>, bool)>);

impl Touched {
    /// Notes the document `write` writes; call it before applying the write. Whether it is the
    /// commit's first write to that document (a verify writes none).
    pub fn record(&mut self, batch: &dyn WriteBatch, write: &Write) -> bool {
        let Some(path) = written(write) else {
            return true;
        };
        let (first, entry) = match self.0.entry(path) {
            Entry::Occupied(entry) => (false, entry.into_mut()),
            Entry::Vacant(entry) => {
                let before = batch.get(entry.key());
                (true, entry.insert((before, false)))
            }
        };
        if matches!(write.operation, Some(Operation::Delete(_))) {
            entry.1 = true;
        }
        first
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
        let mut update_times = HashMap::with_capacity(self.0.len());
        for (path, (before, deleted)) in &self.0 {
            let update_time = match (before, batch.get(path)) {
                (_, None) => None,
                (Some(before), Some(after)) if !deleted && after.has_fields(&before.fields()) => {
                    batch.revert(path);
                    Some(before.update_time)
                }
                (_, Some(after)) => Some(after.update_time),
            };
            update_times.insert(path, update_time);
        }
        // One pass over the writes: a commit of 500 writes names each document once here, not
        // once per document it touches.
        for (write, result) in writes.iter().zip(results.iter_mut()) {
            if let Some(update_time) = written(write).and_then(|path| update_times.get(&path)) {
                result.update_time = *update_time;
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
/// internal Datastore key here instead (see docs/parity-exceptions.md). A version precondition
/// compares with the document's version: its update time, the time it was deleted (`deleted`),
/// or 0 if it never existed (`tools/oracle/rest.py`, #262). A version past the stored one of a
/// document that exists is not a failed precondition but an invalid argument, which prints the
/// key; and the version a deletion left lets only a delete through: anything else needs the
/// document (`tools/oracle/base_versions.py`).
fn check_precondition(
    precondition: Option<&ConditionType>,
    delete: bool,
    existing: Option<&StoredDocument>,
    parsed: &names::Name,
    name: &str,
    deleted: impl FnOnce() -> Option<ReadTime>,
) -> Result<(), Status> {
    match precondition {
        None => Ok(()),
        Some(ConditionType::Exists(true)) if existing.is_none() => Err(no_document(name)),
        Some(ConditionType::Exists(false)) if existing.is_some() => Err(Status::already_exists(
            format!("Document already exists: {name}"),
        )),
        Some(ConditionType::Exists(_)) => Ok(()),
        Some(ConditionType::UpdateTime(required)) => {
            let required = ReadTime::from_timestamp(required).0;
            let stored = match existing {
                Some(doc) => ReadTime::from_timestamp(&doc.update_time).0,
                None => deleted().map_or(0, |time| time.0),
            };
            if existing.is_some() && required > stored {
                return Err(Status::invalid_argument(format!(
                    "{PAST_STORED}{}",
                    datastore_key(parsed)
                )));
            }
            if stored != required {
                return Err(Status::failed_precondition(format!(
                    "the stored version ({stored}) does not match the required base version ({required})"
                )));
            }
            if existing.is_none() && required != 0 && !delete {
                return Err(no_document(name));
            }
            Ok(())
        }
    }
}

const PAST_STORED: &str = "Invalid base version, it is greater than the stored version: ";

/// Whether a write failed on its precondition (`check_precondition`), which Security Rules come
/// before: a version past the stored one too, though not a failed precondition to the official
/// emulator (`tools/oracle/base_versions.py`).
pub fn is_precondition(status: &Status) -> bool {
    matches!(
        status.code(),
        tonic::Code::NotFound | tonic::Code::AlreadyExists | tonic::Code::FailedPrecondition
    ) || (status.code() == tonic::Code::InvalidArgument
        && status.message().starts_with(PAST_STORED))
}

fn no_document(name: &str) -> Status {
    Status::not_found(format!("No document to update: {name}"))
}

/// The official emulator's Datastore key of a document, as its messages print it: the protocol
/// buffer text of the project (`dev~` and its ID), the path as kinds and names (or legacy
/// numeric IDs), and a database other than `(default)` (`tools/oracle/base_versions.py`).
fn datastore_key(name: &names::Name) -> String {
    let mut parts = name.database.split('/');
    let project = parts.nth(1).unwrap_or_default();
    let database = parts.nth(1).unwrap_or_default();
    let mut key = format!("app: \"dev~{}\"\npath <\n", text_escape(project));
    let segments: Vec<&str> = name.path.segments().collect();
    for pair in segments.chunks(2) {
        let [kind, id] = pair else { continue };
        key.push_str(&format!(
            "  Element {{\n    type: \"{}\"\n",
            text_escape(kind)
        ));
        match numeric_id(id) {
            // Java's protocol buffer text prints larger numbers in hexadecimal, as unsigned.
            Some(n) if n.unsigned_abs() < 2_000_000_000 => key.push_str(&format!("    id: {n}\n")),
            #[allow(clippy::cast_sign_loss)]
            Some(n) => key.push_str(&format!("    id: 0x{:x}\n", n as u64)),
            None => key.push_str(&format!("    name: \"{}\"\n", text_escape(id))),
        }
        key.push_str("  }\n");
    }
    key.push_str(">\n");
    if database != "(default)" {
        key.push_str(&format!("database_id: \"{}\"\n", text_escape(database)));
    }
    key
}

/// A string as that protocol buffer text prints it, by UTF-16 unit: C escapes for some control
/// characters, octal for the others and up to U+00FF, `\u` for the rest.
fn text_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for unit in text.encode_utf16() {
        match unit {
            0x0a => out.push_str("\\n"),
            0x0d => out.push_str("\\r"),
            0x09 => out.push_str("\\t"),
            0x08 => out.push_str("\\b"),
            0x0c => out.push_str("\\f"),
            0x22 => out.push_str("\\\""),
            0x5c => out.push_str("\\\\"),
            0x20..0x7f => out.push(char::from(u8::try_from(unit).expect("ASCII"))),
            0..0x100 => out.push_str(&format!("\\{unit:03o}")),
            _ => out.push_str(&format!("\\u{unit:04x}")),
        }
    }
    out
}
