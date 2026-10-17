//! Datastore values: how an entity's properties are checked before they are written, with the
//! official emulator's messages (`tests/fixtures/datastore_entities.json`), and how they are
//! stored (timestamps to the microsecond, the partitions of keys filled in).

use hidane_proto::{
    google::datastore::v1::{Entity, PropertyMask, Value, value::ValueType},
    ordered::Properties,
};
use tonic::Status;

use super::keys::{self, Database, Last, Rules};

/// The longest property name, and the longest indexed string or blob, in bytes.
const MAX_INDEXED: usize = 1500;
/// The longest string or blob that is not indexed.
const MAX_UNINDEXED: usize = 1_048_487;
/// `meaning` 22 marks a blob.
const BLOB_MEANING: i32 = 22;
/// The meanings the official emulator knows, for any value.
const MAX_MEANING: i32 = 25;

fn reserved(name: &str) -> bool {
    name.len() >= 4 && name.starts_with("__") && name.ends_with("__")
}

/// Checks the properties of an entity that is written (or of an entity value), in order.
pub fn check_properties(properties: &Properties, database: &Database) -> Result<(), Status> {
    for (name, value) in properties {
        if name.is_empty() {
            return Err(Status::invalid_argument(
                "The property.name is the empty string.",
            ));
        }
        if reserved(name) {
            return Err(Status::invalid_argument(format!(
                "The property.name \"{name}\" is reserved."
            )));
        }
        if name.len() > MAX_INDEXED {
            return Err(Status::invalid_argument(format!(
                "The property.name is longer than {MAX_INDEXED} bytes."
            )));
        }
        check_value(name, value, database, false)?;
    }
    Ok(())
}

/// Checks `value`, the value of property `name` (or an element of its array, `in_array`).
fn check_value(
    name: &str,
    value: &Value,
    database: &Database,
    in_array: bool,
) -> Result<(), Status> {
    let Some(value_type) = &value.value_type else {
        return Err(Status::invalid_argument(format!(
            "The value \"{name}\" does not contain a value."
        )));
    };
    if value.meaning != 0 {
        if !(0..=MAX_MEANING).contains(&value.meaning) {
            return Err(Status::invalid_argument(format!(
                "Unknown value meaning {}.",
                value.meaning
            )));
        }
        if value.meaning == BLOB_MEANING && !matches!(value_type, ValueType::BlobValue(_)) {
            return Err(Status::invalid_argument(format!(
                "Value \"{name}\" has a meaning {BLOB_MEANING} which does not match blob_value field."
            )));
        }
    }
    let too_long = |len: usize| {
        let limit = if value.exclude_from_indexes {
            MAX_UNINDEXED
        } else {
            MAX_INDEXED
        };
        (len > limit).then(|| {
            Status::invalid_argument(format!(
                "The value of property \"{name}\" is longer than {limit} bytes."
            ))
        })
    };
    match value_type {
        ValueType::StringValue(s) => {
            if let Some(error) = too_long(s.len()) {
                return Err(error);
            }
        }
        ValueType::BlobValue(b) => {
            if let Some(error) = too_long(b.len()) {
                return Err(error);
            }
        }
        ValueType::GeoPointValue(point) => {
            if !(-90.0..=90.0).contains(&point.latitude) {
                return Err(Status::invalid_argument(
                    "Geo point latitude outside permitted range -90.0 to 90.0.",
                ));
            }
            if !(-180.0..=180.0).contains(&point.longitude) {
                return Err(Status::invalid_argument(
                    "Geo point longitude outside permitted range -180.0 to 180.0.",
                ));
            }
        }
        ValueType::KeyValue(key) => {
            keys::check(key, database, Rules::Read, Last::Complete)
                .map(drop)
                // A key value may name another project.
                .or_else(|status| {
                    if status.message().starts_with("mismatched databases") {
                        Ok(())
                    } else {
                        Err(status)
                    }
                })?;
        }
        ValueType::EntityValue(entity) => check_properties(&entity.properties, database)?,
        ValueType::ArrayValue(array) => {
            if in_array {
                return Err(Status::invalid_argument("Nested arrays are not allowed"));
            }
            if value.exclude_from_indexes {
                return Err(Status::invalid_argument(
                    "Exclude from indexes cannot be set on a list value",
                ));
            }
            for element in &array.values {
                check_value(name, element, database, true)?;
            }
        }
        ValueType::NullValue(_)
        | ValueType::BooleanValue(_)
        | ValueType::IntegerValue(_)
        | ValueType::DoubleValue(_)
        | ValueType::TimestampValue(_) => {}
    }
    Ok(())
}

/// The properties as they are stored: timestamps truncated to the microsecond, and keys
/// without a partition given the database's.
pub fn normalize(properties: &mut Properties, database: &Database) {
    for value in properties.values_mut() {
        normalize_value(value, database);
    }
}

fn normalize_value(value: &mut Value, database: &Database) {
    match &mut value.value_type {
        Some(ValueType::TimestampValue(ts)) => ts.nanos -= ts.nanos % 1_000,
        Some(ValueType::KeyValue(key)) => fill_partition(key, database),
        Some(ValueType::EntityValue(entity)) => {
            if let Some(key) = &mut entity.key {
                fill_partition(key, database);
            }
            normalize(&mut entity.properties, database);
        }
        Some(ValueType::ArrayValue(array)) => {
            for element in &mut array.values {
                normalize_value(element, database);
            }
        }
        _ => {}
    }
}

/// A key's partition, with the request's project (and database) where it names none.
pub fn fill_partition(key: &mut hidane_proto::google::datastore::v1::Key, database: &Database) {
    let partition = key.partition_id.get_or_insert_with(Default::default);
    if partition.project_id.is_empty() {
        partition.project_id.clone_from(&database.project);
        partition.database_id.clone_from(&database.database);
    }
}

/// The properties a property mask keeps: each path names a property, or with dots a property
/// of an entity value.
pub fn masked(entity: &Entity, mask: &PropertyMask) -> Properties {
    let mut out = Properties::new();
    for (name, value) in &entity.properties {
        let below: Vec<&str> = mask
            .paths
            .iter()
            .filter_map(|path| {
                if path == name {
                    Some("")
                } else {
                    path.strip_prefix(name.as_str())?.strip_prefix('.')
                }
            })
            .collect();
        if below.is_empty() {
            continue;
        }
        if below.contains(&"") {
            out.insert(name.clone(), value.clone());
            continue;
        }
        if let Some(ValueType::EntityValue(inner)) = &value.value_type {
            let inner_mask = PropertyMask {
                paths: below.iter().map(|p| (*p).to_owned()).collect(),
            };
            let mut kept = value.clone();
            kept.value_type = Some(ValueType::EntityValue(Entity {
                key: inner.key.clone(),
                properties: masked(inner, &inner_mask),
            }));
            out.insert(name.clone(), kept);
        }
    }
    out
}
