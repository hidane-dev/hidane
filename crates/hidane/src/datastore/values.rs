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
/// The meanings the official emulator knows, for any value.
const MAX_MEANING: i32 = 26;
/// The most index values one entity may have.
pub const MAX_INDEX_ENTRIES: usize = 5000;
/// The largest entity, as [`entity_size`] counts it.
pub const MAX_ENTITY_BYTES: usize = 1_048_576;

fn reserved(name: &str) -> bool {
    name.len() >= 4 && name.starts_with("__") && name.ends_with("__")
}

/// Checks the properties of an entity that is written, in order. Nested arrays, the number of
/// index values and the entity's size come later ([`nested_arrays`], [`index_entries`],
/// [`entity_size`]).
pub fn check_properties(properties: &Properties, database: &Database) -> Result<(), Status> {
    check_nested(properties, database, "")
}

/// Checks the properties of an entity value whose properties are named `prefix` and theirs.
fn check_nested(properties: &Properties, database: &Database, prefix: &str) -> Result<(), Status> {
    for (name, value) in properties {
        // A property of an entity value: its name below the entity's top is checked first, in
        // place of its own name's length.
        let flattened = format!("{prefix}{name}");
        if !prefix.is_empty() && flattened.len() > MAX_INDEXED {
            return Err(Status::invalid_argument(format!(
                "The flattened property.name is longer than {MAX_INDEXED} bytes."
            )));
        }
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
        check_value(name, &flattened, value, database)?;
    }
    Ok(())
}

/// Checks a value's meaning, as the official emulator reads it into its own entities.
fn check_meaning(name: &str, value: &Value, value_type: &ValueType) -> Result<(), Status> {
    let m = value.meaning;
    if m == 0 {
        return Ok(());
    }
    let invalid = |message: String| Err(Status::invalid_argument(message));
    if matches!(value_type, ValueType::ArrayValue(_)) {
        return invalid("A Value containing a list_value cannot specify a meaning.".into());
    }
    if !(0..=MAX_MEANING).contains(&m) {
        return invalid(format!("Unknown value meaning {m}."));
    }
    if matches!(m, 9 | 14 | 21 | 24 | 25 | 26) {
        return invalid(format!("Unexpected value meaning {m}."));
    }
    let mismatch = |field: &str| {
        invalid(format!(
            "Value \"{name}\" has a meaning {m} which does not match {field} field."
        ))
    };
    if m == 18 {
        return match value_type {
            ValueType::TimestampValue(_) => mismatch("timestamp_value"),
            ValueType::EntityValue(_) => mismatch("entity_value"),
            _ => invalid("Projection property cannot be used.".into()),
        };
    }
    let (field, matching) = match m {
        7 | 13 => (
            "integer_value",
            matches!(value_type, ValueType::IntegerValue(_)),
        ),
        16 | 19 | 22 => ("blob_value", matches!(value_type, ValueType::BlobValue(_))),
        20 => (
            "entity_value",
            matches!(value_type, ValueType::EntityValue(_)),
        ),
        23 => (
            "geo_point_value",
            matches!(value_type, ValueType::GeoPointValue(_)),
        ),
        _ => (
            "string_value",
            matches!(value_type, ValueType::StringValue(_)),
        ),
    };
    if !matching {
        return mismatch(field);
    }
    match (m, value_type) {
        (15, _) if !value.exclude_from_indexes => invalid("Indexed value has meaning 15.".into()),
        (19, _) => invalid(
            "Blob value has invalid entity proto meaning but that feature is not enabled for Cloud V1."
                .into(),
        ),
        (22, _) if !value.exclude_from_indexes => invalid("Indexed value has meaning 22.".into()),
        (23, _) => invalid("OnestoreEntity.PropertyValue meaning does not match value fields.".into()),
        // An integer that means a time, in the range of timestamps.
        (7, ValueType::IntegerValue(n))
            if (-62_135_596_800_000_000..=253_402_300_799_999_999).contains(n) =>
        {
            invalid(
                "A timestamp in range [0001-01-01T00:00:00Z, 9999-12-31T23:59:59.999999Z] must be stored as a timestamp value."
                    .into(),
            )
        }
        (20, ValueType::EntityValue(entity)) => match entity.properties.keys().next() {
            Some(first) => invalid(format!("The user entity property \"{first}\" is not allowed.")),
            None => Ok(()),
        },
        _ => Ok(()),
    }
}

/// Checks `value`, the value of property `name` (or an element of its array), `flattened` its
/// name below the entity's top.
fn check_value(
    name: &str,
    flattened: &str,
    value: &Value,
    database: &Database,
) -> Result<(), Status> {
    let Some(value_type) = &value.value_type else {
        return Err(Status::invalid_argument(format!(
            "The value \"{name}\" does not contain a value."
        )));
    };
    check_meaning(name, value, value_type)?;
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
        ValueType::EntityValue(entity) => {
            check_nested(&entity.properties, database, &format!("{flattened}."))?;
        }
        ValueType::ArrayValue(array) => {
            if value.exclude_from_indexes {
                return Err(Status::invalid_argument(
                    "Exclude from indexes cannot be set on a list value",
                ));
            }
            for element in &array.values {
                if !matches!(element.value_type, Some(ValueType::ArrayValue(_))) {
                    check_value(name, flattened, element, database)?;
                }
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

/// Whether an array holds an array, which a write refuses once its mutations are read.
pub fn nested_arrays(properties: &Properties) -> bool {
    fn nested(value: &Value) -> bool {
        match &value.value_type {
            Some(ValueType::ArrayValue(array)) => array.values.iter().any(|element| {
                matches!(element.value_type, Some(ValueType::ArrayValue(_)))
                    || matches!(&element.value_type, Some(ValueType::EntityValue(e)) if nested_arrays(&e.properties))
            }),
            Some(ValueType::EntityValue(entity)) => nested_arrays(&entity.properties),
            _ => false,
        }
    }
    properties.values().any(nested)
}

/// How many index values an entity has: each indexed value, each indexed element of an array,
/// the properties of entity values (not themselves); at most [`MAX_INDEX_ENTRIES`].
pub fn index_entries(properties: &Properties) -> usize {
    fn count(value: &Value) -> usize {
        if value.exclude_from_indexes {
            return 0;
        }
        match &value.value_type {
            Some(ValueType::ArrayValue(array)) => array.values.iter().map(count).sum(),
            Some(ValueType::EntityValue(entity)) => index_entries(&entity.properties),
            _ => 1,
        }
    }
    properties.values().map(count).sum()
}

/// A key's size: each element's kind and name (one more byte each) or ID (8 bytes), the
/// namespace's, and 16 bytes.
fn key_size(namespace: &str, path: impl Iterator<Item = (usize, Option<usize>)>) -> usize {
    let ns = if namespace.is_empty() {
        0
    } else {
        namespace.len() + 1
    };
    path.map(|(kind, name)| kind + 1 + name.map_or(8, |n| n + 1))
        .sum::<usize>()
        + ns
        + 16
}

fn key_value_size(key: &hidane_proto::google::datastore::v1::Key) -> usize {
    use hidane_proto::google::datastore::v1::key::path_element::IdType;
    let namespace = key
        .partition_id
        .as_ref()
        .map_or("", |p| p.namespace_id.as_str());
    key_size(
        namespace,
        key.path.iter().map(|e| {
            let name = match &e.id_type {
                Some(IdType::Name(name)) => Some(name.len()),
                _ => None,
            };
            (e.kind.len(), name)
        }),
    )
}

fn value_size(value: &Value) -> usize {
    match &value.value_type {
        None | Some(ValueType::NullValue(_) | ValueType::BooleanValue(_)) => 1,
        Some(
            ValueType::IntegerValue(_) | ValueType::DoubleValue(_) | ValueType::TimestampValue(_),
        ) => 8,
        Some(ValueType::GeoPointValue(_)) => 16,
        Some(ValueType::StringValue(s)) => s.len() + 1,
        Some(ValueType::BlobValue(b)) => b.len(),
        Some(ValueType::KeyValue(key)) => key_value_size(key),
        Some(ValueType::ArrayValue(array)) if array.values.is_empty() => 1,
        Some(ValueType::ArrayValue(array)) => array.values.iter().map(value_size).sum(),
        Some(ValueType::EntityValue(entity))
            if entity.key.is_none() && entity.properties.is_empty() =>
        {
            1
        }
        Some(ValueType::EntityValue(entity)) => {
            entity
                .key
                .as_ref()
                .map_or(0, |key| key_value_size(key) + 32)
                + properties_size(&entity.properties)
        }
    }
}

fn properties_size(properties: &Properties) -> usize {
    properties
        .iter()
        .map(|(name, value)| name.len() + 1 + value_size(value))
        .sum()
}

/// An entity's size as the official emulator limits it ([`MAX_ENTITY_BYTES`]), the measure of
/// Firestore's documents: its key ([`key_size`]), each property's name (one more byte) and value
/// (null and booleans 1, numbers and times 8, geo points 16, a string one more than its bytes,
/// a blob its bytes, a key its size, an array its elements, an entity value its properties and
/// key with 32 bytes, 1 when empty), and 32 bytes.
pub fn entity_size(located: &keys::Checked, properties: &Properties) -> usize {
    let path = located.path.iter().map(|(kind, id)| {
        let name = match id {
            Some(keys::Id::Name(name)) => Some(name.len()),
            _ => None,
        };
        (kind.len(), name)
    });
    key_size(&located.namespace, path) + properties_size(properties) + 32
}

/// Checks a property mask's paths: names separated by dots, each an identifier or quoted with
/// backquotes.
pub fn check_paths(paths: &[String]) -> Result<(), Status> {
    for path in paths {
        if path.is_empty() {
            return Err(Status::invalid_argument(
                "Invalid empty property path string.",
            ));
        }
        if !path_ok(path) {
            return Err(Status::invalid_argument(format!(
                "Invalid property path \"{path}\". Unquoted property paths must match regex ([a-zA-Z_][a-zA-Z_0-9]*), and quoted property paths must match regex (`(?:[^`\\\\]|(?:\\\\.))+`)"
            )));
        }
    }
    Ok(())
}

fn path_ok(path: &str) -> bool {
    let mut chars = path.chars().peekable();
    loop {
        match chars.next() {
            Some('`') => {
                let mut empty = true;
                loop {
                    match chars.next() {
                        Some('`') => break,
                        Some('\\') => {
                            if chars.next().is_none() {
                                return false;
                            }
                        }
                        Some(_) => {}
                        None => return false,
                    }
                    empty = false;
                }
                if empty {
                    return false;
                }
            }
            Some(c) if c.is_ascii_alphabetic() || c == '_' => {
                while chars
                    .peek()
                    .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_')
                {
                    chars.next();
                }
            }
            _ => return false,
        }
        match chars.next() {
            None => return true,
            Some('.') => {}
            Some(_) => return false,
        }
    }
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
