//! Making values: `array`, `map`, `scalar` and `array` of a subpipeline, `current_document`.

use hidane_core::field_path::Fields;
use hidane_proto::google::firestore::v1::Value;
use tonic::Status;

use super::{Def, Signature};
use crate::firestore::pipeline::{
    expr::{Expr, Row, Scope},
    value,
};

pub(super) const DEFS: &[Def] = &[
    Def {
        name: "array",
        signatures: &[Signature::variadic("array(x: any*): array[any]", 0)],
        check: None,
        eval: |call, scope| {
            if let [Expr::Pipeline(pipeline)] = call.args.as_slice() {
                let rows = scope.run(pipeline)?;
                return Ok(Some(value::array(rows.iter().map(row_value).collect())));
            }
            let mut values = Vec::with_capacity(call.args.len());
            for arg in &call.args {
                values.push(scope.eval(arg)?.unwrap_or_else(value::null));
            }
            Ok(Some(value::array(values)))
        },
    },
    Def {
        name: "map",
        signatures: &[Signature::variadic("map(x: any*): map[string, any]", 0)],
        check: Some(check_map),
        eval: |call, scope| {
            let mut fields = Fields::new();
            for [key, value] in call.args.as_chunks::<2>().0 {
                let key = key.literal_string().unwrap_or_default();
                if let Some(v) = scope.eval(value)? {
                    fields.insert(key.to_owned(), v);
                }
            }
            Ok(Some(value::map(fields)))
        },
    },
    Def {
        name: "scalar",
        signatures: &[Signature::fixed("scalar(pipeline: pipeline): any?", 1)],
        check: None,
        eval: |call, scope| {
            let Expr::Pipeline(pipeline) = &call.args[0] else {
                return scope.eval(&call.args[0]);
            };
            let rows = scope.run(pipeline)?;
            match rows.as_slice() {
                [] => Ok(Some(value::null())),
                [row] => Ok(Some(row_value(row))),
                _ => Err(Status::invalid_argument(
                    "Subpipeline returned multiple results",
                )),
            }
        },
    },
    Def {
        name: "current_document",
        signatures: &[Signature::fixed("current_document(): map[string, any]", 0)],
        check: None,
        eval: |_, scope| Ok(Some(current_document(scope))),
    },
];

/// `map(key, value, ...)`: an even number of arguments, each key a string literal, once.
fn check_map(args: &[Expr]) -> Result<(), Status> {
    if !args.len().is_multiple_of(2) {
        return Err(Status::invalid_argument(
            "'map' function must have an even number of arguments",
        ));
    }
    let mut keys: Vec<&str> = Vec::new();
    for [key, _] in args.as_chunks::<2>().0 {
        let Some(key) = key.literal_string() else {
            return Err(Status::invalid_argument(
                "map keys must be constants/literals",
            ));
        };
        if keys.contains(&key) {
            return Err(Status::invalid_argument(format!(
                "map(...) constructor given duplicate key: {key}"
            )));
        }
        keys.push(key);
    }
    Ok(())
}

/// A subpipeline's result as one value: its field when it has one, else its fields as a map.
fn row_value(row: &Row) -> Value {
    match row.fields.iter().next() {
        Some((_, v)) if row.fields.len() == 1 => v.clone(),
        _ => value::map(row.fields.clone()),
    }
}

/// The row's fields with its name and times, as `current_document()` gives them.
fn current_document(scope: &Scope<'_>) -> Value {
    let mut fields = scope.row.fields.clone();
    if let Some(doc) = &scope.row.doc {
        fields.insert(
            "__name__".to_owned(),
            value::of(
                hidane_proto::google::firestore::v1::value::ValueType::ReferenceValue(
                    crate::firestore::names::Name::document_name(scope.env.database, &doc.path),
                ),
            ),
        );
        if let Some((created, updated)) = doc.times {
            fields.insert("__create_time__".to_owned(), value::timestamp(created));
            fields.insert("__update_time__".to_owned(), value::timestamp(updated));
        }
    }
    value::map(fields)
}
