//! Checks on document contents, with the official emulator's messages.

use hidane_core::field_path::Fields;
use hidane_proto::google::firestore::v1::{MapValue, Value, value::ValueType};
use tonic::Status;

const TYPE_KEY: &str = "__type__";
const VECTOR_TYPE: &str = "__vector__";
const VECTOR_VALUES_KEY: &str = "value";
const MAX_DIMENSIONS: usize = 2048;

pub fn fields(fields: &Fields) -> Result<(), Status> {
    for (name, value) in fields {
        field_name(name, true)?;
        value_ok(value, name, false)?;
    }
    Ok(())
}

fn field_name(name: &str, top_level: bool) -> Result<(), Status> {
    if reserved(name) {
        return Err(Status::invalid_argument(if top_level {
            format!("field name {name} is reserved")
        } else {
            format!("field name '{name}' is reserved.")
        }));
    }
    Ok(())
}

pub fn reserved(name: &str) -> bool {
    name.len() >= 4 && name.starts_with("__") && name.ends_with("__")
}

/// `property` names where the value sits in the official emulator's messages: the document
/// field it is under, or `array` inside an array.
fn value_ok(value: &Value, property: &str, inside_array: bool) -> Result<(), Status> {
    match &value.value_type {
        Some(ValueType::ArrayValue(array)) => {
            if inside_array {
                return Err(Status::invalid_argument("Nested arrays are not allowed"));
            }
            for element in &array.values {
                value_ok(element, "array", true)?;
            }
        }
        // A `__type__` key spells a special value on the wire; only vectors exist.
        Some(ValueType::MapValue(map)) if map.fields.contains_key(TYPE_KEY) => {
            match map.fields[TYPE_KEY].value_type.as_ref() {
                Some(ValueType::StringValue(t)) if t == VECTOR_TYPE => {
                    // Stored vectors cannot hold NaN; query vectors can.
                    if vector(map)?.iter().any(|d| d.is_nan()) {
                        return Err(Status::invalid_argument(
                            "Vector cannot contain NaN values.",
                        ));
                    }
                }
                Some(ValueType::StringValue(_)) => {
                    return Err(Status::invalid_argument(format!(
                        "Property {property} contains an invalid nested entity."
                    )));
                }
                other => {
                    return Err(Status::invalid_argument(format!(
                        "Field __type__ must be a string; founds {}.",
                        kind(other)
                    )));
                }
            }
        }
        Some(ValueType::MapValue(map)) => {
            for (name, nested) in &map.fields {
                field_name(name, false)?;
                value_ok(nested, property, false)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// The official emulator's (Datastore's) name for a value's type.
fn kind(value: Option<&ValueType>) -> &'static str {
    match value {
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
        Some(ValueType::MapValue(_)) => "MAP",
        Some(_) => "EXPRESSION",
    }
}

/// The elements of a vector (`{__type__: "__vector__", value: [doubles]}`), checked in the
/// official emulator's order; NaN elements are the caller's business.
pub fn vector(map: &MapValue) -> Result<Vec<f64>, Status> {
    let values = match map.fields.get(VECTOR_VALUES_KEY).map(|v| &v.value_type) {
        None => {
            return Err(Status::invalid_argument(
                "Vector map is missing key 'value'.",
            ));
        }
        Some(Some(ValueType::ArrayValue(array))) => &array.values,
        Some(_) => {
            return Err(Status::invalid_argument(
                "Vector value type must be an array.",
            ));
        }
    };
    if map.fields.len() > 2 {
        return Err(Status::invalid_argument(
            "Vector value map has extraneous entries.",
        ));
    }
    if values.is_empty() {
        return Err(Status::invalid_argument(
            "Cannot have a zero length vector.",
        ));
    }
    if values.len() > MAX_DIMENSIONS {
        return Err(Status::invalid_argument(format!(
            "Vectors must be at most {MAX_DIMENSIONS} dimensions."
        )));
    }
    let elements = values
        .iter()
        .map(|v| match v.value_type {
            Some(ValueType::DoubleValue(d)) => Some(d),
            _ => None,
        })
        .collect::<Option<Vec<f64>>>()
        .ok_or_else(|| {
            Status::invalid_argument("Vector must only contain values of type 'double'")
        })?;
    Ok(elements)
}
