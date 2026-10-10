//! Checks on document contents, with the official emulator's messages.

use hidane_core::{field_path::Fields, order::is_vector, path::ResourcePath};
use hidane_proto::google::firestore::v1::{MapValue, Value, value::ValueType};
use tonic::Status;

const TYPE_KEY: &str = "__type__";
const VECTOR_TYPE: &str = "__vector__";
const VECTOR_VALUES_KEY: &str = "value";
const MAX_DIMENSIONS: usize = 2048;
/// Firestore's limits, which the official emulator enforces with Datastore's messages.
const MAX_VALUE_BYTES: usize = 1_048_487;
const MAX_NAME_BYTES: usize = 1500;
const MAX_DEPTH: usize = 20;
const MAX_DOCUMENT_BYTES: usize = 1_048_576;

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

/// The limits on a document's ID and fields, checked after their contents, as the official
/// emulator stores a document as a Datastore entity: IDs and names of at most 1500 bytes
/// (nested names count from the top-level field, an array as `array`), strings and bytes of at
/// most 1,048,487 bytes, and 20 levels of maps and arrays. A problem inside a map is only
/// "an invalid nested entity".
pub fn limits(path: &ResourcePath, fields: &Fields) -> Result<(), Status> {
    for (i, id) in path.segments().enumerate() {
        if id.len() > MAX_NAME_BYTES {
            let element = if i % 2 == 0 { "kind" } else { "name" };
            return Err(Status::invalid_argument(format!(
                "The key path element {element} is longer than {MAX_NAME_BYTES} bytes."
            )));
        }
    }
    for (name, value) in fields {
        if name.len() > MAX_NAME_BYTES {
            return Err(Status::invalid_argument(format!(
                "The property.name is longer than {MAX_NAME_BYTES} bytes."
            )));
        }
        let (property, ok) = match &value.value_type {
            Some(ValueType::ArrayValue(array)) => {
                for element in &array.values {
                    if too_long(element) {
                        return Err(Status::invalid_argument(format!(
                            "The value of property \"array\" is longer than {MAX_VALUE_BYTES} bytes."
                        )));
                    }
                }
                (
                    "array",
                    array.values.iter().all(|e| nested_ok(e, "array", 2)),
                )
            }
            Some(ValueType::MapValue(map)) if !is_vector(map) => {
                (name.as_str(), nested_ok(value, name, 1))
            }
            _ if too_long(value) => {
                return Err(Status::invalid_argument(format!(
                    "The value of property \"{name}\" is longer than {MAX_VALUE_BYTES} bytes."
                )));
            }
            _ => continue,
        };
        if !ok {
            return Err(Status::invalid_argument(format!(
                "Property {property} contains an invalid nested entity."
            )));
        }
    }
    Ok(())
}

fn too_long(value: &Value) -> bool {
    match &value.value_type {
        Some(ValueType::StringValue(s)) => s.len() > MAX_VALUE_BYTES,
        Some(ValueType::BytesValue(b)) => b.len() > MAX_VALUE_BYTES,
        _ => false,
    }
}

/// Whether `value`, at `depth` levels of maps and arrays under the property path `path`,
/// fits a nested entity.
fn nested_ok(value: &Value, path: &str, depth: usize) -> bool {
    match &value.value_type {
        Some(ValueType::MapValue(map)) if !is_vector(map) => {
            depth <= MAX_DEPTH
                && map.fields.iter().all(|(key, nested)| {
                    let path = format!("{path}.{key}");
                    path.len() <= MAX_NAME_BYTES
                        && match &nested.value_type {
                            // An array is named `array` in the path, whatever its key.
                            Some(ValueType::ArrayValue(array)) => {
                                let path = format!("{}.array", &path[..path.len() - key.len() - 1]);
                                depth < MAX_DEPTH
                                    && array
                                        .values
                                        .iter()
                                        .all(|e| !too_long(e) && nested_ok(e, &path, depth + 2))
                            }
                            _ => !too_long(nested) && nested_ok(nested, &path, depth + 1),
                        }
                })
        }
        _ => !too_long(value),
    }
}

/// A document's size by Firestore's storage size rules: its name, each field's name and value,
/// and 32 bytes. The official emulator counts its Datastore entity's encoding instead, which
/// comes to a few tens of bytes more (docs/parity-exceptions.md).
pub fn size(path: &ResourcePath, fields: &Fields) -> Result<(), Status> {
    let total = name_size(path.segments())
        + fields
            .iter()
            .map(|(name, value)| name.len() + 1 + value_size(value))
            .sum::<usize>()
        + 32;
    if total > MAX_DOCUMENT_BYTES {
        return Err(Status::invalid_argument(format!(
            "maximum entity size is {MAX_DOCUMENT_BYTES} bytes"
        )));
    }
    Ok(())
}

fn name_size<'a>(segments: impl Iterator<Item = &'a str>) -> usize {
    segments.map(|s| s.len() + 1).sum::<usize>() + 16
}

fn value_size(value: &Value) -> usize {
    match &value.value_type {
        None | Some(ValueType::NullValue(_) | ValueType::BooleanValue(_)) => 1,
        Some(
            ValueType::IntegerValue(_) | ValueType::DoubleValue(_) | ValueType::TimestampValue(_),
        ) => 8,
        Some(ValueType::GeoPointValue(_)) => 16,
        Some(ValueType::StringValue(s)) => s.len() + 1,
        Some(ValueType::BytesValue(b)) => b.len(),
        Some(ValueType::ReferenceValue(name)) => name_size(
            name.split_once("/documents/")
                .map_or("", |(_, path)| path)
                .split('/'),
        ),
        Some(ValueType::ArrayValue(array)) => array.values.iter().map(value_size).sum(),
        Some(ValueType::MapValue(map)) => map
            .fields
            .iter()
            .map(|(key, value)| key.len() + 1 + value_size(value))
            .sum(),
        Some(_) => 0,
    }
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
