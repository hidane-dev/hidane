//! Datastore values: how an entity's properties are checked before they are written, and a
//! query's filter operands, with the official emulator's messages
//! (`tests/fixtures/datastore_entities.json`, `datastore_commit_checks.json`,
//! `datastore_checks.json`), and how they are stored (timestamps to the microsecond, the
//! partitions of keys filled in).

use hidane_proto::{
    google::datastore::v1::{Entity, PropertyMask, Value, value::ValueType},
    ordered::Properties,
};
use tonic::Status;

use super::keys::{self, Database, Last, Rules, reserved};

/// The longest property name, and the longest indexed string or blob, in bytes.
const MAX_INDEXED: usize = 1500;
/// The longest string or blob that is not indexed.
const MAX_UNINDEXED: usize = 1_048_487;
/// The meanings the official emulator knows, for any value.
const MAX_MEANING: i32 = 40;
/// The most index values one entity may have.
pub const MAX_INDEX_ENTRIES: usize = 5000;
/// The largest entity, as [`entity_size`] counts it.
pub const MAX_ENTITY_BYTES: usize = 1_048_576;

/// Checks the properties of an entity that is written, in order. Nested arrays, the number of
/// index values and the entity's size come later ([`nested_arrays`], [`index_entries`],
/// [`entity_size`]).
pub fn check_properties(properties: &Properties) -> Result<(), Status> {
    check_nested(properties, "", Use::Write)
}

/// Where a value is checked: as an entity's property that is written, or as the operand of a
/// `RunQuery` filter, which may hold what a write may not (a projection's meaning, a value of a
/// type Datastore does not store).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Use {
    Write,
    Filter,
}

/// The meaning of a vector: an array of doubles.
const VECTOR: i32 = 31;
/// The most dimensions a vector may have.
const MAX_VECTOR: usize = 2048;

/// Checks the properties of an entity value whose properties are named `prefix` and theirs.
fn check_nested(properties: &Properties, prefix: &str, used: Use) -> Result<(), Status> {
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
        if used == Use::Write && reserved(name) {
            return Err(Status::invalid_argument(format!(
                "The property.name \"{name}\" is reserved."
            )));
        }
        if name.len() > MAX_INDEXED {
            return Err(Status::invalid_argument(format!(
                "The property.name is longer than {MAX_INDEXED} bytes."
            )));
        }
        check_value(name, &flattened, value, used)?;
    }
    Ok(())
}

/// The value field a meaning goes with.
fn meaning_field(m: i32) -> &'static str {
    match m {
        7 | 13 | 29 | 30 | 36 => "integer_value",
        16 | 19 | 22 | 33 => "blob_value",
        20 | 35 | 37 => "entity_value",
        23 => "geo_point_value",
        VECTOR => "vector",
        38 | 39 => "null_value",
        _ => "string_value",
    }
}

fn field_matches(field: &str, value_type: &ValueType) -> bool {
    matches!(
        (field, value_type),
        ("integer_value", ValueType::IntegerValue(_))
            | ("blob_value", ValueType::BlobValue(_))
            | ("entity_value", ValueType::EntityValue(_))
            | ("geo_point_value", ValueType::GeoPointValue(_))
            | ("null_value", ValueType::NullValue(_))
            | ("string_value", ValueType::StringValue(_))
    )
}

/// Checks a value's meaning, as the official emulator reads it into its own entities: the
/// meanings it knows (0 to 40), the field each goes with, and what some of them hold. A filter
/// takes a projection's meaning and the types a write refuses.
fn check_meaning(
    name: &str,
    value: &Value,
    value_type: &ValueType,
    used: Use,
) -> Result<(), Status> {
    let m = value.meaning;
    if m == 0 || matches!(value_type, ValueType::ArrayValue(_)) {
        return Ok(());
    }
    let invalid = |message: String| Err(Status::invalid_argument(message));
    if !(0..=MAX_MEANING).contains(&m) {
        return invalid(format!("Unknown value meaning {m}."));
    }
    if matches!(m, 9 | 14 | 21 | 24..=28 | 40) {
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
            _ if used == Use::Filter => Ok(()),
            _ => invalid("Projection property cannot be used.".into()),
        };
    }
    let field = meaning_field(m);
    if !field_matches(field, value_type) {
        return mismatch(field);
    }
    // A type Datastore does not store, once it reads: refused when written.
    let not_stored = |kind: &str| {
        if used == Use::Write {
            invalid(format!("Value type {kind} is not allowed"))
        } else {
            Ok(())
        }
    };
    match (m, value_type) {
        (15, _) if !value.exclude_from_indexes => invalid("Indexed value has meaning 15.".into()),
        (19, _) => invalid(
            "Blob value has invalid entity proto meaning but that feature is not enabled for Cloud V1."
                .into(),
        ),
        (22, _) if !value.exclude_from_indexes => invalid("Indexed value has meaning 22.".into()),
        (23, _) if used == Use::Write => {
            invalid("OnestoreEntity.PropertyValue meaning does not match value fields.".into())
        }
        // An integer that means a time, in the range of timestamps.
        (7, ValueType::IntegerValue(n))
            if (-62_135_596_800_000_000..=253_402_300_799_999_999).contains(n) =>
        {
            invalid(
                "A timestamp in range [0001-01-01T00:00:00Z, 9999-12-31T23:59:59.999999Z] must be stored as a timestamp value."
                    .into(),
            )
        }
        (13, ValueType::IntegerValue(n)) if !(0..=100).contains(n) => {
            invalid("Percent value outside permitted range [0, 100].".into())
        }
        (20, ValueType::EntityValue(entity)) => special_entity(
            "user",
            entity,
            &["email", "auth_domain"],
            &["user_id", "federated_identity", "federated_provider"],
            false,
        ),
        (35, ValueType::EntityValue(entity)) => {
            special_entity("regex", entity, &["pattern", "options"], &[], false)?;
            not_stored("REGEX")
        }
        (37, ValueType::EntityValue(entity)) => {
            special_entity("request timestamp", entity, &["seconds", "increment"], &[], true)?;
            unsigned_parts(entity)?;
            not_stored("REQUEST_TIMESTAMP")
        }
        (32, ValueType::StringValue(text)) => {
            if !decimal128(text) {
                return invalid("Invalid decimal128 string".into());
            }
            not_stored("DECIMAL128")
        }
        (34, ValueType::StringValue(text)) => {
            object_id(text)?;
            not_stored("OBJECT_ID")
        }
        (36, _) => not_stored("INT"),
        (38, _) => not_stored("MIN_KEY"),
        (39, _) => not_stored("MAX_KEY"),
        _ => Ok(()),
    }
}

/// An entity value that stands for something else (a user, a regular expression, a timestamp of
/// a request): no key, only its properties, unindexed and of their type, the required ones there.
fn special_entity(
    kind: &str,
    entity: &Entity,
    required: &[&str],
    optional: &[&str],
    integers: bool,
) -> Result<(), Status> {
    let invalid = |message: String| Err(Status::invalid_argument(message));
    if entity.key.is_some() {
        return invalid(format!("The {kind} entity has a key."));
    }
    for (name, value) in &entity.properties {
        if !required.contains(&name.as_str()) && !optional.contains(&name.as_str()) {
            return invalid(format!(
                "The {kind} entity property \"{name}\" is not allowed."
            ));
        }
        if value.meaning != 0 {
            return invalid(format!(
                "The {kind} entity property \"{name}\" has a meaning."
            ));
        }
        let typed = if integers {
            matches!(value.value_type, Some(ValueType::IntegerValue(_)))
        } else {
            matches!(value.value_type, Some(ValueType::StringValue(_)))
        };
        if !typed {
            return invalid(format!(
                "The {kind} entity property \"{name}\" is the wrong type."
            ));
        }
        if !value.exclude_from_indexes {
            return invalid(format!("The {kind} entity property \"{name}\" is indexed."));
        }
    }
    if let Some(missing) = required
        .iter()
        .find(|name| !entity.properties.contains_key(name))
    {
        return invalid(format!(
            "The {kind} entity is missing required property \"{missing}\"."
        ));
    }
    Ok(())
}

/// An integer as Java's number format writes it: groups of three digits, with commas.
fn grouped(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

/// Whether `text` reads as a 128-bit decimal: a sign, then `NaN`, `Inf` or `Infinity` in any
/// case, or a number as Java's `BigDecimal` reads one, that 34 digits hold at an exponent from
/// -6176 to 6111 (trailing zeros moved into the exponent, or zeros added, as that allows).
fn decimal128(text: &str) -> bool {
    let unsigned = text.strip_prefix(['+', '-']).unwrap_or(text);
    if ["nan", "inf", "infinity"]
        .iter()
        .any(|word| unsigned.eq_ignore_ascii_case(word))
    {
        return true;
    }
    let (mantissa, exponent) = match unsigned.find(['e', 'E']) {
        Some(at) => (&unsigned[..at], Some(&unsigned[at + 1..])),
        None => (unsigned, None),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digit = |c: char| c.is_numeric();
    if (whole.is_empty() && fraction.is_empty())
        || !whole.chars().all(digit)
        || !fraction.chars().all(digit)
    {
        return false;
    }
    let exponent: i64 = match exponent {
        None => 0,
        Some(e) => {
            let digits = e.strip_prefix(['+', '-']).unwrap_or(e);
            if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
                return false;
            }
            match e.parse() {
                Ok(n) => n,
                Err(_) => return false,
            }
        }
    };
    let coefficient: String = whole.chars().chain(fraction.chars()).collect();
    let coefficient = coefficient.trim_start_matches(|c: char| c.to_digit(10) == Some(0));
    let mut q = exponent - fraction.chars().count() as i64;
    if coefficient.is_empty() {
        // Zero: any exponent clamps.
        return true;
    }
    let mut digits = coefficient.chars().count() as i64;
    let mut trailing = coefficient
        .chars()
        .rev()
        .take_while(|c| c.to_digit(10) == Some(0))
        .count() as i64;
    // Too many digits: drop trailing zeros into the exponent.
    while digits > 34 && trailing > 0 {
        digits -= 1;
        trailing -= 1;
        q += 1;
    }
    if digits > 34 {
        return false;
    }
    // Below the smallest exponent: drop trailing zeros, exactly.
    while q < -6176 && trailing > 0 {
        digits -= 1;
        trailing -= 1;
        q += 1;
    }
    if q < -6176 {
        return false;
    }
    // Above the largest: add zeros, while 34 digits hold them.
    if q > 6111 {
        digits += q - 6111;
    }
    digits <= 34
}

/// Checks `value`, the value of property `name` (or an element of its array), `flattened` its
/// name below the entity's top: that it has one, a list's meaning, what it holds (a key, an
/// entity's properties, a point's coordinates, a vector, a list's elements), its meaning, then
/// its length.
fn check_value(name: &str, flattened: &str, value: &Value, used: Use) -> Result<(), Status> {
    let Some(value_type) = &value.value_type else {
        return Err(Status::invalid_argument(format!(
            "The value \"{name}\" does not contain a value."
        )));
    };
    let vector = matches!(value_type, ValueType::ArrayValue(_)) && value.meaning == VECTOR;
    if matches!(value_type, ValueType::ArrayValue(_)) && value.meaning != 0 && !vector {
        return Err(Status::invalid_argument(
            "A Value containing a list_value cannot specify a meaning.",
        ));
    }
    match value_type {
        ValueType::GeoPointValue(point) => {
            for (axis, coordinate, limit) in [
                ("latitude", point.latitude, 90.0),
                ("longitude", point.longitude, 180.0),
            ] {
                if coordinate.is_nan() {
                    return Err(Status::invalid_argument(format!(
                        "Geo point {axis} is not a number."
                    )));
                }
                if !(-limit..=limit).contains(&coordinate) {
                    return Err(Status::invalid_argument(format!(
                        "Geo point {axis} outside permitted range -{limit:.1} to {limit:.1}."
                    )));
                }
            }
        }
        // A key value may name another project or database.
        ValueType::KeyValue(key) => {
            keys::check_unpartitioned(key, Rules::Read, Last::Complete)?;
        }
        ValueType::EntityValue(entity) => {
            if let Some(key) = &entity.key {
                keys::check_unpartitioned(key, Rules::Read, Last::Either)?;
            }
            check_nested(&entity.properties, &format!("{flattened}."), used)?;
        }
        ValueType::ArrayValue(array) if vector => {
            if array.values.is_empty() {
                return Err(Status::invalid_argument(
                    "Cannot have a zero length vector.",
                ));
            }
            if array.values.len() > MAX_VECTOR {
                return Err(Status::invalid_argument(format!(
                    "Vectors must be at most {MAX_VECTOR} dimensions."
                )));
            }
            for element in &array.values {
                match element.value_type {
                    Some(ValueType::DoubleValue(d)) if d.is_nan() => {
                        return Err(Status::invalid_argument(
                            "Vector cannot contain NaN values.",
                        ));
                    }
                    Some(ValueType::DoubleValue(_)) => {}
                    _ => {
                        return Err(Status::invalid_argument(
                            "Vector must only contain values of type 'double'.",
                        ));
                    }
                }
            }
        }
        ValueType::ArrayValue(array) => {
            if value.exclude_from_indexes {
                return Err(Status::invalid_argument(
                    "Exclude from indexes cannot be set on a list value",
                ));
            }
            for element in &array.values {
                if !matches!(element.value_type, Some(ValueType::ArrayValue(_))) {
                    check_value(name, flattened, element, used)?;
                }
            }
        }
        ValueType::NullValue(_)
        | ValueType::BooleanValue(_)
        | ValueType::IntegerValue(_)
        | ValueType::DoubleValue(_)
        | ValueType::TimestampValue(_)
        | ValueType::StringValue(_)
        | ValueType::BlobValue(_) => {}
    }
    check_meaning(name, value, value_type, used)?;
    let length = match value_type {
        ValueType::StringValue(s) => s.len(),
        ValueType::BlobValue(b) => b.len(),
        _ => 0,
    };
    let limit = if value.exclude_from_indexes {
        MAX_UNINDEXED
    } else {
        MAX_INDEXED
    };
    if length > limit {
        return Err(Status::invalid_argument(format!(
            "The value of property \"{name}\" is longer than {limit} bytes."
        )));
    }
    Ok(())
}

/// Checks the operand of a `RunQuery` filter on `name` as a value is checked when written, but
/// for what only a write refuses.
pub fn check_filter_operand(name: &str, value: &Value) -> Result<(), Status> {
    check_value(name, name, value, Use::Filter)
}

/// Checks the operand of a filter of an aggregation's query as the official emulator first reads
/// it there: indexed, each value there, its meaning (and what a meaning makes it hold), a key's
/// path there. The rest comes once the query's index is found ([`check_aggregation_operand_late`]).
pub fn check_aggregation_operand(value: &Value) -> Result<(), Status> {
    if value.exclude_from_indexes {
        return Err(Status::invalid_argument("Filter value must be indexed."));
    }
    aggregation_value(value)
}

/// A value of an aggregation's filter, as first read.
fn aggregation_value(value: &Value) -> Result<(), Status> {
    let invalid = |message: String| Err(Status::invalid_argument(message));
    let Some(value_type) = &value.value_type else {
        return invalid("Value missing type.".into());
    };
    let m = value.meaning;
    // A vector is read later.
    if matches!(value_type, ValueType::ArrayValue(_)) && m == VECTOR {
        return Ok(());
    }
    let type_name = match value_type {
        ValueType::NullValue(_) => "NULL",
        ValueType::BooleanValue(_) => "BOOLEAN",
        ValueType::IntegerValue(_) => "LONG",
        ValueType::DoubleValue(_) => "DOUBLE",
        ValueType::TimestampValue(_) => "TIMESTAMP",
        ValueType::StringValue(_) => "STRING",
        ValueType::BlobValue(_) => "BYTES",
        ValueType::GeoPointValue(_) => "GEO_POINT",
        ValueType::KeyValue(_) => "ENTITY_REF",
        ValueType::EntityValue(_) => "MAP",
        ValueType::ArrayValue(_) => "ARRAY",
    };
    if let ValueType::BlobValue(_) = value_type {
        match m {
            19 => {
                return invalid(
                    "Blob value has invalid entity proto meaning but that feature is not enabled for Cloud V1."
                        .into(),
                );
            }
            25 | 26 => {
                return invalid(
                    "Blob value has non-UTF-8 meaning but that feature is not enabled for Cloud V1."
                        .into(),
                );
            }
            _ => {}
        }
    } else if m != 0 && m != 18 {
        if !(0..=MAX_MEANING).contains(&m) {
            return invalid(format!("Meaning value \"{m}\" is not valid."));
        }
        let takes = match value_type {
            ValueType::IntegerValue(_) => matches!(m, 7 | 13 | 29 | 30 | 36),
            ValueType::StringValue(_) => matches!(m, 1..=6 | 8 | 10..=12 | 15 | 17 | 32 | 34),
            ValueType::GeoPointValue(_) => m == 23,
            ValueType::NullValue(_) => matches!(m, 38 | 39),
            ValueType::EntityValue(_) => matches!(m, 20 | 35 | 37),
            _ => false,
        };
        if !takes {
            return invalid(format!(
                "Meaning \"{m}\" is not valid for type \"{type_name}\"."
            ));
        }
    }
    match value_type {
        ValueType::KeyValue(key) if key.path.is_empty() => {
            return invalid("Cannot convert a key with an empty path.".into());
        }
        ValueType::EntityValue(entity) => {
            match m {
                20 => predefined_entity(
                    "Legacy user",
                    entity,
                    &["email", "auth_domain"],
                    &["user_id", "federated_identity", "federated_provider"],
                    false,
                )?,
                35 => predefined_entity("Regex", entity, &["pattern", "options"], &[], false)?,
                37 => {
                    predefined_entity(
                        "Request timestamp",
                        entity,
                        &["seconds", "increment"],
                        &[],
                        true,
                    )?;
                    unsigned_parts(entity)?;
                }
                _ => {}
            }
            for nested in entity.properties.values() {
                aggregation_value(nested)?;
            }
        }
        ValueType::ArrayValue(array) => {
            for element in &array.values {
                aggregation_value(element)?;
            }
        }
        ValueType::StringValue(text) => {
            if m == 32 && !decimal128(text) {
                return invalid("Invalid decimal128 string".into());
            }
            if m == 34 {
                object_id(text)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Checks the operand of an aggregation's filter on `name` once its index is found: a vector's
/// doubles and dimensions, a key's parts, the length of a string or blob.
pub fn check_aggregation_operand_late(name: &str, value: &Value) -> Result<(), Status> {
    let invalid = |message: String| Err(Status::invalid_argument(message));
    let Some(value_type) = &value.value_type else {
        return Ok(());
    };
    match value_type {
        ValueType::ArrayValue(array) if value.meaning == VECTOR => {
            if array
                .values
                .iter()
                .any(|v| !matches!(v.value_type, Some(ValueType::DoubleValue(_))))
            {
                return invalid("Vector must only contain values of type 'double'.".into());
            }
            if array.values.len() > MAX_VECTOR {
                return invalid(format!("Vectors must be at most {MAX_VECTOR} dimensions."));
            }
        }
        ValueType::ArrayValue(array) => {
            for element in &array.values {
                check_aggregation_operand_late(name, element)?;
            }
        }
        ValueType::EntityValue(entity) => {
            for nested in entity.properties.values() {
                check_aggregation_operand_late(name, nested)?;
            }
        }
        ValueType::KeyValue(key) => aggregation_key(key)?,
        ValueType::StringValue(text) if text.len() > MAX_INDEXED => {
            return invalid(format!(
                "The value of property \"{name}\" is longer than {MAX_INDEXED} bytes."
            ));
        }
        ValueType::BlobValue(bytes) if bytes.len() > MAX_INDEXED => {
            return invalid(format!(
                "The value of property \"{name}\" is longer than {MAX_INDEXED} bytes."
            ));
        }
        _ => {}
    }
    Ok(())
}

/// A key of an aggregation's filter, as the official emulator reads it there.
fn aggregation_key(key: &hidane_proto::google::datastore::v1::Key) -> Result<(), Status> {
    use hidane_proto::google::datastore::v1::key::path_element::IdType;
    let invalid = |message: String| Err(Status::invalid_argument(message));
    let namespace = key
        .partition_id
        .as_ref()
        .map_or("", |p| p.namespace_id.as_str());
    if namespace.len() > 100 {
        return invalid("The namespace is longer than 100 bytes.".into());
    }
    if !namespace
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    {
        return invalid(format!("Illegal string \"{namespace}\" in namespace."));
    }
    if key.path.is_empty() {
        return invalid("Cannot convert a key with an empty path.".into());
    }
    if key.path.len() > 100 {
        return invalid("Key path is too long. Cannot exceed 100 elements.".into());
    }
    let shown = |path: &[hidane_proto::google::datastore::v1::key::PathElement]| {
        path.iter()
            .map(|e| {
                let id = match &e.id_type {
                    Some(IdType::Id(n)) => n.to_string(),
                    Some(IdType::Name(name)) => name.clone(),
                    None => String::new(),
                };
                format!("{}: {id}", e.kind)
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    for (i, element) in key.path.iter().enumerate() {
        if element.kind.is_empty() {
            return invalid("The key path element kind is the empty string.".into());
        }
        if element.kind.len() > MAX_INDEXED {
            return invalid(format!(
                "The key path element kind is longer than {MAX_INDEXED} bytes."
            ));
        }
        if let Some(IdType::Name(name)) = &element.id_type
            && name.len() > MAX_INDEXED
        {
            return invalid(format!(
                "The key path element name is longer than {MAX_INDEXED} bytes."
            ));
        }
        let incomplete = match &element.id_type {
            None => true,
            Some(IdType::Id(n)) => *n == 0,
            Some(IdType::Name(name)) => name.is_empty(),
        };
        if incomplete && i + 1 < key.path.len() {
            let path: String = key
                .path
                .iter()
                .map(|e| {
                    let id = match &e.id_type {
                        Some(IdType::Id(n)) if *n != 0 => n.to_string(),
                        Some(IdType::Name(name)) => name.clone(),
                        _ => String::new(),
                    };
                    format!("/{}/{id}", e.kind)
                })
                .collect();
            return invalid(format!(
                "EntityRef may not have a null resource id that isn't last: {path}"
            ));
        }
        if incomplete {
            return invalid(format!(
                "Key path element must not be incomplete: [{}]",
                shown(&key.path)
            ));
        }
    }
    Ok(())
}

/// An entity value that stands for something else, in an aggregation's filter, with other words
/// and another order than [`special_entity`]: no key, no other property, then each property in
/// turn (the required first, each there): no meaning, unindexed, of its type.
fn predefined_entity(
    kind: &str,
    entity: &Entity,
    required: &[&str],
    optional: &[&str],
    integers: bool,
) -> Result<(), Status> {
    let invalid = |message: String| Err(Status::invalid_argument(message));
    if entity.key.is_some() {
        return invalid("Predefined entity has a non-default key.".into());
    }
    if let Some(name) = entity
        .properties
        .keys()
        .find(|name| !required.contains(&name.as_str()) && !optional.contains(&name.as_str()))
    {
        return invalid(format!("{kind} value contains unexpected property {name}."));
    }
    for name in required.iter().chain(optional) {
        let Some(value) = entity.properties.get(name) else {
            if required.contains(name) {
                return invalid(format!("{kind} value missing property {name}."));
            }
            continue;
        };
        if value.meaning != 0 {
            return invalid(format!(
                "{kind} value property {name} has unexpected meaning {}.",
                value.meaning
            ));
        }
        if !value.exclude_from_indexes {
            return invalid(format!(
                "{kind} value property {name} does not set exclude_from_indexes."
            ));
        }
        let (typed, wanted) = if integers {
            (
                matches!(value.value_type, Some(ValueType::IntegerValue(_))),
                "an integer",
            )
        } else {
            (
                matches!(value.value_type, Some(ValueType::StringValue(_))),
                "a string",
            )
        };
        if !typed {
            return invalid(format!("{kind} value property {name} is not {wanted}."));
        }
    }
    Ok(())
}

/// A request timestamp's seconds and increment: unsigned 32-bit integers.
fn unsigned_parts(entity: &Entity) -> Result<(), Status> {
    for part in ["seconds", "increment"] {
        if let Some(ValueType::IntegerValue(n)) = entity
            .properties
            .get(part)
            .and_then(|v| v.value_type.as_ref())
            && u32::try_from(*n).is_err()
        {
            return Err(Status::invalid_argument(format!(
                "Value ({}) does not represent an unsigned 32-bit integer.",
                grouped(*n)
            )));
        }
    }
    Ok(())
}

/// An object ID: 24 lowercase hexadecimal digits.
fn object_id(text: &str) -> Result<(), Status> {
    if text.chars().count() != 24 {
        return Err(Status::invalid_argument(
            "Object ID hex string has incorrect length.",
        ));
    }
    if !text
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Status::invalid_argument("Invalid object ID hex string."));
    }
    Ok(())
}

/// Whether a value is a vector: an array that means one.
pub fn is_vector(value: &Value) -> bool {
    matches!(value.value_type, Some(ValueType::ArrayValue(_))) && value.meaning == VECTOR
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimals_read_as_a_128_bit_decimal_holds_them() {
        for text in [
            "1",
            "+1",
            "-0",
            ".5",
            "1.",
            "1e5",
            "-1.5E10",
            "NaN",
            "-Inf",
            "INFINITY",
            "1E6144",
            "1E-6176",
            "10E-6177",
            "0E7000",
            "0E-6200",
            "12345678901234567890123456789012340",
        ] {
            assert!(decimal128(text), "{text}");
        }
        for text in [
            "",
            "-",
            " 1",
            "0x1",
            "1_000",
            "Infinite",
            "sNaN",
            "1e",
            "1e+",
            "1E6145",
            "10E6144",
            "1E-6177",
            "12345678901234567890123456789012345",
            "1.2345678901234567890123456789012345",
        ] {
            assert!(!decimal128(text), "{text}");
        }
    }

    #[test]
    fn vectors_hold_up_to_2048_doubles() {
        let vector = |n: usize| Value {
            meaning: VECTOR,
            value_type: Some(ValueType::ArrayValue(
                hidane_proto::google::datastore::v1::ArrayValue {
                    values: vec![
                        Value {
                            value_type: Some(ValueType::DoubleValue(1.0)),
                            ..Value::default()
                        };
                        n
                    ],
                },
            )),
            ..Value::default()
        };
        assert!(check_value("v", "v", &vector(2048), Use::Write).is_ok());
        assert_eq!(
            check_value("v", "v", &vector(2049), Use::Write)
                .unwrap_err()
                .message(),
            "Vectors must be at most 2048 dimensions."
        );
        assert_eq!(
            check_value("v", "v", &vector(0), Use::Write)
                .unwrap_err()
                .message(),
            "Cannot have a zero length vector."
        );
    }

    #[test]
    fn numbers_are_grouped_as_java_writes_them() {
        assert_eq!(grouped(-1), "-1");
        assert_eq!(grouped(4_294_967_296), "4,294,967,296");
        assert_eq!(grouped(-5_000_000_000), "-5,000,000,000");
    }
}
