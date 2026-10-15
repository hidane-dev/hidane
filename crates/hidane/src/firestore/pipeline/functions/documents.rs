//! References: `document_id`, `collection_id`, `parent`.
//!
//! Each reads a reference: a document's ID, the ID of its collection, and the document that
//! collection belongs to (the database root, `…/documents`, for a top-level collection), in
//! the reference's own database. The database root has none of them: null.

use hidane_core::path::ResourcePath;
use hidane_proto::google::firestore::v1::{Value, value::ValueType};

use super::{Def, Signature, args, requires};
use crate::firestore::{
    names::{self, Name},
    pipeline::{
        expr::{Call, Eval, Scope},
        value,
    },
};

pub(super) const DEFS: &[Def] = &[
    Def {
        name: "document_id",
        signatures: &[Signature::fixed(
            "document_id(ref: document_reference): any",
            1,
        )],
        check: None,
        eval: |call, scope| {
            reference(call, scope, "document_id", |name| {
                name.path
                    .is_document()
                    .then(|| name.path.last())
                    .flatten()
                    .map(value::string)
            })
        },
    },
    Def {
        name: "collection_id",
        signatures: &[Signature::fixed(
            "collection_id(ref: document_reference): string",
            1,
        )],
        check: None,
        eval: |call, scope| {
            reference(call, scope, "collection_id", |name| {
                collection(&name.path).and_then(|c| c.last().map(value::string))
            })
        },
    },
    Def {
        name: "parent",
        signatures: &[Signature::fixed(
            "parent(ref: document_reference): document_reference?",
            1,
        )],
        check: None,
        eval: |call, scope| {
            reference(call, scope, "parent", |name| {
                let parent = collection(&name.path)?.parent()?;
                Some(value::of(ValueType::ReferenceValue(Name::document_name(
                    &name.database,
                    &parent,
                ))))
            })
        },
    },
];

/// The collection a reference names or belongs to: none for the database root.
fn collection(path: &ResourcePath) -> Option<ResourcePath> {
    if path.is_document() {
        path.parent()
    } else {
        (!path.is_empty()).then(|| path.clone())
    }
}

/// `f` of the reference argument, null when it gives nothing; null for a null or absent
/// argument, an error for any other type.
fn reference(
    call: &Call,
    scope: &Scope<'_>,
    function: &str,
    f: fn(&Name) -> Option<Value>,
) -> Eval {
    let values = args(call, scope)?;
    let name = match values[0].as_ref() {
        None => return super::null(),
        Some(Value {
            value_type: Some(ValueType::ReferenceValue(reference)),
        }) => names::any(reference)?,
        Some(v) if value::is_null(v) => return super::null(),
        Some(v) => return Err(requires(function, "Reference", v)),
    };
    Ok(Some(f(&name).unwrap_or_else(value::null)))
}
