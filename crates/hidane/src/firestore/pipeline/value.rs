//! Values as pipelines read and make them: the names of their types, as errors and `type()` give
//! them, and constructors.

use hidane_core::{field_path::Fields, order::is_vector};
use hidane_proto::google::firestore::v1::{ArrayValue, MapValue, Value, value::ValueType};
use prost_types::Timestamp;

pub(crate) fn null() -> Value {
    of(ValueType::NullValue(0))
}

pub(crate) fn boolean(b: bool) -> Value {
    of(ValueType::BooleanValue(b))
}

pub(crate) fn int(i: i64) -> Value {
    of(ValueType::IntegerValue(i))
}

pub(crate) fn double(d: f64) -> Value {
    of(ValueType::DoubleValue(d))
}

pub(crate) fn string(s: impl Into<String>) -> Value {
    of(ValueType::StringValue(s.into()))
}

pub(crate) fn bytes(b: Vec<u8>) -> Value {
    of(ValueType::BytesValue(b))
}

pub(crate) fn timestamp(ts: Timestamp) -> Value {
    of(ValueType::TimestampValue(ts))
}

pub(crate) fn array(values: Vec<Value>) -> Value {
    of(ValueType::ArrayValue(ArrayValue { values }))
}

pub(crate) fn map(fields: Fields) -> Value {
    of(ValueType::MapValue(MapValue { fields }))
}

pub(crate) fn of(value_type: ValueType) -> Value {
    Value {
        value_type: Some(value_type),
    }
}

pub(crate) fn is_null(value: &Value) -> bool {
    matches!(value.value_type, None | Some(ValueType::NullValue(_)))
}

/// A value's type as the functions name it in their errors (`… but got `LONG`.`).
pub(crate) fn kind(value: &Value) -> &'static str {
    match &value.value_type {
        None | Some(ValueType::NullValue(_)) => "NULL",
        Some(ValueType::BooleanValue(_)) => "BOOLEAN",
        Some(ValueType::IntegerValue(_)) => "LONG",
        Some(ValueType::DoubleValue(_)) => "DOUBLE",
        Some(ValueType::TimestampValue(_)) => "TIMESTAMP",
        Some(ValueType::StringValue(_)) => "STRING",
        Some(ValueType::BytesValue(_)) => "BYTES",
        Some(ValueType::ReferenceValue(_)) => "ENTITY_REF",
        Some(ValueType::GeoPointValue(_)) => "GEO_POINT",
        Some(ValueType::ArrayValue(_)) => "ARRAY",
        Some(ValueType::MapValue(map)) if is_vector(map) => "VECTOR",
        Some(ValueType::MapValue(_)) => "MAP",
        Some(_) => "UNKNOWN",
    }
}

/// A value's type as `type()` names it.
pub(crate) fn type_name(value: &Value) -> &'static str {
    match &value.value_type {
        None | Some(ValueType::NullValue(_)) => "null",
        Some(ValueType::BooleanValue(_)) => "boolean",
        Some(ValueType::IntegerValue(_)) => "int64",
        Some(ValueType::DoubleValue(_)) => "float64",
        Some(ValueType::TimestampValue(_)) => "timestamp",
        Some(ValueType::StringValue(_)) => "string",
        Some(ValueType::BytesValue(_)) => "bytes",
        Some(ValueType::ReferenceValue(_)) => "reference",
        Some(ValueType::GeoPointValue(_)) => "geo_point",
        Some(ValueType::ArrayValue(_)) => "array",
        Some(ValueType::MapValue(map)) if is_vector(map) => "vector",
        Some(ValueType::MapValue(_)) => "map",
        Some(_) => "unknown",
    }
}

/// A value's kind as the stages name it in their errors (`… but was STRING_VALUE.`).
pub(crate) fn proto_kind(value: &Value) -> &'static str {
    match &value.value_type {
        None => "VALUETYPE_NOT_SET",
        Some(ValueType::NullValue(_)) => "NULL_VALUE",
        Some(ValueType::BooleanValue(_)) => "BOOLEAN_VALUE",
        Some(ValueType::IntegerValue(_)) => "INTEGER_VALUE",
        Some(ValueType::DoubleValue(_)) => "DOUBLE_VALUE",
        Some(ValueType::TimestampValue(_)) => "TIMESTAMP_VALUE",
        Some(ValueType::StringValue(_)) => "STRING_VALUE",
        Some(ValueType::BytesValue(_)) => "BYTES_VALUE",
        Some(ValueType::ReferenceValue(_)) => "REFERENCE_VALUE",
        Some(ValueType::GeoPointValue(_)) => "GEO_POINT_VALUE",
        Some(ValueType::ArrayValue(_)) => "ARRAY_VALUE",
        Some(ValueType::MapValue(_)) => "MAP_VALUE",
        Some(ValueType::FieldReferenceValue(_)) => "FIELD_REFERENCE_VALUE",
        Some(ValueType::VariableReferenceValue(_)) => "VARIABLE_REFERENCE_VALUE",
        Some(ValueType::FunctionValue(_)) => "FUNCTION_VALUE",
        Some(ValueType::PipelineValue(_)) => "PIPELINE_VALUE",
    }
}
