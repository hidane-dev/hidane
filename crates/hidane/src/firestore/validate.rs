//! Checks on document contents, with the official emulator's messages.

use hidane_core::{field_path::Fields, order::is_vector};
use hidane_proto::google::firestore::v1::{Value, value::ValueType};
use tonic::Status;

pub fn fields(fields: &Fields) -> Result<(), Status> {
    for (name, value) in fields {
        field_name(name)?;
        value_ok(value, false)?;
    }
    Ok(())
}

fn field_name(name: &str) -> Result<(), Status> {
    if name.len() >= 4 && name.starts_with("__") && name.ends_with("__") {
        return Err(Status::invalid_argument(format!(
            "field name {name} is reserved"
        )));
    }
    Ok(())
}

fn value_ok(value: &Value, inside_array: bool) -> Result<(), Status> {
    match &value.value_type {
        Some(ValueType::ArrayValue(array)) => {
            if inside_array {
                return Err(Status::invalid_argument("Nested arrays are not allowed"));
            }
            for element in &array.values {
                value_ok(element, true)?;
            }
        }
        // A vector's `__type__` key is how vectors are spelled on the wire.
        Some(ValueType::MapValue(map)) if is_vector(map) => {}
        Some(ValueType::MapValue(map)) => {
            for (name, nested) in &map.fields {
                field_name(name)?;
                value_ok(nested, false)?;
            }
        }
        _ => {}
    }
    Ok(())
}
