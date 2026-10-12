//! Checks on document contents, with the official emulator's messages.
//!
//! The official emulator checks a commit in passes (`tools/oracle/commit_checks.py`): the names
//! of every write first (`names`, `field_path`, `transform`: reserved names at any depth, the
//! types of special values and the shapes of vectors, field paths, transforms' operands), the
//! values of every write later (`values`: name lengths, nested arrays, vectors' elements, value
//! lengths, depth), the size of each document when it is applied (`size`).

use hidane_core::{
    field_path::{FieldPath, Fields},
    order::{Number, is_vector},
    path::ResourcePath,
};
use hidane_proto::google::firestore::v1::{
    MapValue, Value,
    document_transform::{
        FieldTransform,
        field_transform::{ServerValue, TransformType},
    },
    value::ValueType,
};
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

/// The names in `fields`, in order and depth first: reserved names, and the special values
/// (maps with a `__type__` key) they spell.
pub fn names(fields: &Fields) -> Result<(), Status> {
    for (name, value) in fields {
        if reserved_name(name) {
            return Err(Status::invalid_argument(format!(
                "field name {name} is reserved"
            )));
        }
        value_names(value)?;
    }
    Ok(())
}

fn value_names(value: &Value) -> Result<(), Status> {
    match &value.value_type {
        Some(ValueType::ArrayValue(array)) => array.values.iter().try_for_each(value_names),
        // A `__type__` key spells a special value on the wire; only vectors exist. A vector's
        // shape is checked here, its elements with the values; another type is an invalid
        // nested entity, also with the values.
        Some(ValueType::MapValue(map)) if map.fields.contains_key(TYPE_KEY) => {
            match map.fields[TYPE_KEY].value_type.as_ref() {
                Some(ValueType::StringValue(t)) if t == VECTOR_TYPE => vector(map).map(drop),
                Some(ValueType::StringValue(_)) => Ok(()),
                other => Err(Status::invalid_argument(format!(
                    "Field __type__ must be a string; founds {}.",
                    kind(other)
                ))),
            }
        }
        Some(ValueType::MapValue(map)) => {
            for (name, nested) in &map.fields {
                if reserved_name(name) {
                    return Err(Status::invalid_argument(format!(
                        "field name '{name}' is reserved."
                    )));
                }
                value_names(nested)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Field names of the form `__…__` are reserved; `____` is not one.
fn reserved_name(name: &str) -> bool {
    name.len() > 4 && name.starts_with("__") && name.ends_with("__")
}

/// Names that a `find_nearest` result field may not take.
pub fn reserved(name: &str) -> bool {
    name.len() >= 4 && name.starts_with("__") && name.ends_with("__")
}

/// A field path of an update mask or a field transform: one that parses and names no reserved
/// field, which the message names with the rest of the path. `__name__` alone is the document's
/// name; in a longer path it is refused.
pub fn field_path(path: &str) -> Result<FieldPath, Status> {
    if path.is_empty() {
        return Err(Status::invalid_argument(
            "Invalid empty property path string.",
        ));
    }
    let parsed = FieldPath::parse(path).map_err(Status::invalid_argument)?;
    let segments = parsed.segments();
    if segments.len() > 1 && segments.iter().any(|s| s == "__name__") {
        return Err(Status::invalid_argument("the name __key__ is reserved"));
    }
    if let Some(at) = segments
        .iter()
        .position(|s| s != "__name__" && reserved_name(s))
    {
        return Err(Status::invalid_argument(format!(
            "Invalid reserved name in field path {}",
            segments[at..].join(".")
        )));
    }
    Ok(parsed)
}

/// A field transform, with the names of its write: its path, its operand's names, then the
/// operand's type.
pub fn transform(transform: &FieldTransform) -> Result<(), Status> {
    field_path(&transform.field_path)?;
    match &transform.transform_type {
        None => Err(Status::invalid_argument(
            "Operation type must be specified for field transformation.",
        )),
        Some(TransformType::SetToServerValue(kind)) => {
            if *kind == ServerValue::RequestTime as i32 {
                Ok(())
            } else {
                Err(Status::invalid_argument(
                    "ServerValue type must be specified for set_to_server_value transformation.",
                ))
            }
        }
        Some(
            TransformType::Increment(operand)
            | TransformType::Maximum(operand)
            | TransformType::Minimum(operand),
        ) => {
            value_names(operand)?;
            match operand.value_type.as_ref().and_then(Number::of) {
                Some(_) => Ok(()),
                None => Err(Status::invalid_argument("Input must be int64 or double.")),
            }
        }
        Some(
            TransformType::AppendMissingElements(operands)
            | TransformType::RemoveAllFromArray(operands),
        ) => operands.values.iter().try_for_each(value_names),
    }
}

/// What the fields may hold once a write's transforms are applied: no nested array and no
/// vector with NaN. The official emulator stores an array union's operands as they are
/// (docs/parity-exceptions.md).
pub fn transformed(fields: &Fields) -> Result<(), Status> {
    fields
        .values()
        .try_for_each(|value| transformed_value(value, false))
}

fn transformed_value(value: &Value, inside_array: bool) -> Result<(), Status> {
    match &value.value_type {
        Some(ValueType::ArrayValue(array)) => {
            if inside_array {
                return Err(Status::invalid_argument("Nested arrays are not allowed"));
            }
            array
                .values
                .iter()
                .try_for_each(|element| transformed_value(element, true))
        }
        Some(ValueType::MapValue(map)) if is_vector(map) => vector_elements(map),
        Some(ValueType::MapValue(map)) => map
            .fields
            .values()
            .try_for_each(|nested| transformed_value(nested, false)),
        _ => Ok(()),
    }
}

/// A document's key: collection and document IDs of at most 1500 bytes.
pub fn key(path: &ResourcePath) -> Result<(), Status> {
    for (i, id) in path.segments().enumerate() {
        if id.len() > MAX_NAME_BYTES {
            let element = if i % 2 == 0 { "kind" } else { "name" };
            return Err(Status::invalid_argument(format!(
                "The key path element {element} is longer than {MAX_NAME_BYTES} bytes."
            )));
        }
    }
    Ok(())
}

/// The key, then the fields of a document in order, as the official emulator converts them to
/// a Datastore entity: a field's name (not empty, at most 1500 bytes), then its value. An
/// array's elements are checked in order: an array in it is a nested array, a vector may not
/// hold NaN, a string or bytes at most 1,048,487 bytes. A map is a nested entity, whose names
/// count from the top-level field (an array as `array`) and may not be empty, with 20 levels
/// of maps and arrays at most; any problem inside one is only "an invalid nested entity".
pub fn values(path: &ResourcePath, fields: &Fields) -> Result<(), Status> {
    key(path)?;
    for (name, value) in fields {
        if name.is_empty() {
            return Err(Status::invalid_argument(
                "The property.name is the empty string.",
            ));
        }
        if name.len() > MAX_NAME_BYTES {
            return Err(Status::invalid_argument(format!(
                "The property.name is longer than {MAX_NAME_BYTES} bytes."
            )));
        }
        match &value.value_type {
            Some(ValueType::ArrayValue(array)) => {
                for element in &array.values {
                    property_value(element, "array", true)?;
                }
            }
            _ => property_value(value, name, false)?,
        }
    }
    Ok(())
}

/// A property's value, or an element of an array (`inside_array`); `property` names it in
/// messages.
fn property_value(value: &Value, property: &str, inside_array: bool) -> Result<(), Status> {
    match &value.value_type {
        Some(ValueType::ArrayValue(_)) if inside_array => {
            Err(Status::invalid_argument("Nested arrays are not allowed"))
        }
        Some(ValueType::MapValue(map)) if is_vector(map) => vector_elements(map),
        Some(ValueType::MapValue(_))
            if !nested_ok(value, property, if inside_array { 2 } else { 1 }) =>
        {
            Err(Status::invalid_argument(format!(
                "Property {property} contains an invalid nested entity."
            )))
        }
        _ if too_long(value) => Err(Status::invalid_argument(format!(
            "The value of property \"{property}\" is longer than {MAX_VALUE_BYTES} bytes."
        ))),
        _ => Ok(()),
    }
}

/// A stored vector's elements (its shape is checked with the names): no NaN, which query
/// vectors may hold.
fn vector_elements(map: &MapValue) -> Result<(), Status> {
    if vector(map)?.iter().any(|d| d.is_nan()) {
        return Err(Status::invalid_argument(
            "Vector cannot contain NaN values.",
        ));
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
        Some(ValueType::MapValue(map)) if map.fields.contains_key(TYPE_KEY) => {
            is_vector(map) && vector_elements(map).is_ok()
        }
        Some(ValueType::MapValue(map)) => {
            depth <= MAX_DEPTH
                && map.fields.iter().all(|(key, nested)| {
                    let path = format!("{path}.{key}");
                    !key.is_empty()
                        && path.len() <= MAX_NAME_BYTES
                        && match &nested.value_type {
                            // An array is named `array` in the path, whatever its key.
                            Some(ValueType::ArrayValue(array)) => {
                                let path = format!("{}.array", &path[..path.len() - key.len() - 1]);
                                depth < MAX_DEPTH
                                    && array.values.iter().all(|e| {
                                        !matches!(e.value_type, Some(ValueType::ArrayValue(_)))
                                            && !too_long(e)
                                            && nested_ok(e, &path, depth + 2)
                                    })
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
