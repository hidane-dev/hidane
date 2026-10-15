//! Types: `type` and `is_type`.

use hidane_core::order::is_vector;
use hidane_proto::google::firestore::v1::value::ValueType;

use super::{Def, Signature, one_of};
use crate::firestore::pipeline::value;

/// The types `is_type` takes.
const TYPES: &[&str] = &[
    "null",
    "array",
    "boolean",
    "bytes",
    "timestamp",
    "geo_point",
    "number",
    "int32",
    "int64",
    "float64",
    "decimal128",
    "map",
    "reference",
    "string",
    "vector",
    "max_key",
    "min_key",
    "object_id",
    "regex",
    "bson_timestamp",
];

pub(super) const DEFS: &[Def] = &[
    Def {
        name: "type",
        signatures: &[Signature::fixed("type(input: any): string", 1)],
        check: None,
        eval: |call, scope| {
            Ok(Some(match scope.eval(&call.args[0])? {
                Some(v) => value::string(value::type_name(&v)),
                None => value::null(),
            }))
        },
    },
    Def {
        name: "is_type",
        signatures: &[Signature::fixed(
            "is_type(value: any, type: (null|array|boolean|bytes|timestamp|geo_point|number|int32|\
             int64|float64|decimal128|map|reference|string|vector|max_key|min_key|object_id|regex|\
             bson_timestamp)): boolean",
            2,
        )],
        check: Some(|args| one_of("is_type", "type", &args[1], TYPES)),
        eval: |call, scope| {
            let Some(v) = scope.eval(&call.args[0])? else {
                return super::null();
            };
            let wanted = call.args[1].literal_string().unwrap_or_default();
            let is = match (&v.value_type, wanted) {
                (None | Some(ValueType::NullValue(_)), "null")
                | (Some(ValueType::ArrayValue(_)), "array")
                | (Some(ValueType::BooleanValue(_)), "boolean")
                | (Some(ValueType::BytesValue(_)), "bytes")
                | (Some(ValueType::TimestampValue(_)), "timestamp")
                | (Some(ValueType::GeoPointValue(_)), "geo_point")
                | (Some(ValueType::IntegerValue(_) | ValueType::DoubleValue(_)), "number")
                | (Some(ValueType::IntegerValue(_)), "int64")
                | (Some(ValueType::DoubleValue(_)), "float64")
                | (Some(ValueType::ReferenceValue(_)), "reference")
                | (Some(ValueType::StringValue(_)), "string") => true,
                (Some(ValueType::MapValue(map)), "map") => !is_vector(map),
                (Some(ValueType::MapValue(map)), "vector") => is_vector(map),
                _ => false,
            };
            Ok(Some(value::boolean(is)))
        },
    },
];
