//! Datastore mode's entities as App Engine `EntityProto`s, the records of an export the official
//! emulator writes in `--database-mode datastore-mode` (`tests/fixtures/datastore_export.json`
//! in the `hidane` crate).
//!
//! The mapping differs from [`super::entity`]'s for documents:
//!
//! - the key's app is the project itself (no `dev~`), a namespace is the key's `name_space`;
//! - indexed values are properties, values excluded from indexes raw properties: first every
//!   indexed property in the entity's order, then every raw one; an array is one property per
//!   element, marked `multiple`, each in its own group; an empty array is an indexed property of
//!   its own meaning, where it stands;
//! - a value's meaning is written as the entity holds it, or else its type's: a timestamp's (7, in
//!   microseconds), a geo point's (9), an entity value's (19, a nested entity, keyless or not), a
//!   blob's (16 indexed, 14 not); meaning 22 is 14 with the URI `ZLIB`;
//! - a double zero is `0.0`, whatever its sign.
//!
//! Reading back, properties come in the order the record holds them and array elements in the
//! order of their properties; the meanings a type gives are dropped.

use super::wire::{Field, Malformed, Reader, Writer};
use hidane_proto::google::{
    datastore::v1::{
        ArrayValue, Entity, Key, PartitionId, Value,
        key::{PathElement, path_element::IdType},
        value::ValueType,
    },
    r#type::LatLng,
};
use hidane_proto::ordered::Properties;
use prost_types::Timestamp;

// EntityProto
const KEY: u32 = 13;
const PROPERTY: u32 = 14;
const RAW_PROPERTY: u32 = 15;
const ENTITY_GROUP: u32 = 16;
// Reference (the key)
const APP: u32 = 13;
const PATH: u32 = 14;
const NAME_SPACE: u32 = 20;
const DATABASE_ID: u32 = 23;
// Path
const ELEMENT: u32 = 1;
const ELEMENT_TYPE: u32 = 2;
const ELEMENT_ID: u32 = 3;
const ELEMENT_NAME: u32 = 4;
// Property
const MEANING: u32 = 1;
const MEANING_URI: u32 = 2;
const NAME: u32 = 3;
const MULTIPLE: u32 = 4;
const VALUE: u32 = 5;
// PropertyValue
const INT64: u32 = 1;
const BOOLEAN: u32 = 2;
const STRING: u32 = 3;
const DOUBLE: u32 = 4;
const POINT: u32 = 5;
const POINT_X: u32 = 6;
const POINT_Y: u32 = 7;
const REFERENCE: u32 = 12;
const REFERENCE_APP: u32 = 13;
const REFERENCE_ELEMENT: u32 = 14;
const REFERENCE_TYPE: u32 = 15;
const REFERENCE_ID: u32 = 16;
const REFERENCE_NAME: u32 = 17;
const REFERENCE_NAME_SPACE: u32 = 20;
const REFERENCE_DATABASE_ID: u32 = 23;
// Meanings
const GD_WHEN: i32 = 7;
const GEORSS_POINT: i32 = 9;
const BLOB: i32 = 14;
const BYTESTRING: i32 = 16;
const ENTITY_PROTO: i32 = 19;
const ZLIB: i32 = 22;
const EMPTY_LIST: i32 = 24;
const ZLIB_URI: &[u8] = b"ZLIB";

/// Encodes the entity at `key` with `properties`.
pub fn encode(key: &Key, properties: &Properties) -> Vec<u8> {
    let mut w = Writer::default();
    entity(&mut w, Some(key), properties);
    w.into_bytes()
}

fn entity(w: &mut Writer, key: Option<&Key>, properties: &Properties) {
    w.message(KEY, |k| match key {
        Some(key) => {
            let partition = key.partition_id.clone().unwrap_or_default();
            k.bytes(APP, partition.project_id.as_bytes());
            k.message(PATH, |p| {
                for element in &key.path {
                    p.group(ELEMENT, |e| {
                        path_element(e, element, ELEMENT_TYPE, ELEMENT_ID, ELEMENT_NAME);
                    });
                }
            });
            if !partition.namespace_id.is_empty() {
                k.bytes(NAME_SPACE, partition.namespace_id.as_bytes());
            }
            if !partition.database_id.is_empty() {
                k.bytes(DATABASE_ID, partition.database_id.as_bytes());
            }
        }
        None => {
            k.bytes(APP, b"");
            k.message(PATH, |_| {});
        }
    });
    for indexed in [true, false] {
        for (name, value) in properties {
            match &value.value_type {
                Some(ValueType::ArrayValue(array)) if array.values.is_empty() => {
                    if indexed {
                        w.message(PROPERTY, |p| {
                            p.uint(MEANING, EMPTY_LIST as u64);
                            p.bytes(NAME, name.as_bytes());
                            p.uint(MULTIPLE, 0);
                            p.message(VALUE, |_| {});
                        });
                    }
                }
                Some(ValueType::ArrayValue(array)) => {
                    for element in &array.values {
                        if !element.exclude_from_indexes == indexed {
                            property(w, name, element, true);
                        }
                    }
                }
                _ => {
                    if !value.exclude_from_indexes == indexed {
                        property(w, name, value, false);
                    }
                }
            }
        }
    }
    w.message(ENTITY_GROUP, |g| {
        if let Some(root) = key.and_then(|key| key.path.first()) {
            g.group(ELEMENT, |e| {
                path_element(e, root, ELEMENT_TYPE, ELEMENT_ID, ELEMENT_NAME);
            });
        }
    });
}

fn path_element(w: &mut Writer, element: &PathElement, kind: u32, id: u32, name: u32) {
    w.bytes(kind, element.kind.as_bytes());
    match &element.id_type {
        Some(IdType::Id(n)) => w.int(id, *n),
        Some(IdType::Name(text)) => w.bytes(name, text.as_bytes()),
        None => {}
    }
}

/// The meaning a property is written with, and its URI.
fn meaning(value: &Value) -> (Option<i32>, Option<&'static [u8]>) {
    if value.meaning == ZLIB {
        return (Some(BLOB), Some(ZLIB_URI));
    }
    if value.meaning != 0 {
        return (Some(value.meaning), None);
    }
    let meaning = match &value.value_type {
        Some(ValueType::TimestampValue(_)) => Some(GD_WHEN),
        Some(ValueType::GeoPointValue(_)) => Some(GEORSS_POINT),
        Some(ValueType::EntityValue(_)) => Some(ENTITY_PROTO),
        Some(ValueType::BlobValue(_)) if value.exclude_from_indexes => Some(BLOB),
        Some(ValueType::BlobValue(_)) => Some(BYTESTRING),
        _ => None,
    };
    (meaning, None)
}

fn property(w: &mut Writer, name: &str, value: &Value, multiple: bool) {
    let field = if value.exclude_from_indexes {
        RAW_PROPERTY
    } else {
        PROPERTY
    };
    let (meaning, uri) = meaning(value);
    w.message(field, |p| {
        if let Some(meaning) = meaning {
            #[allow(clippy::cast_sign_loss)]
            p.uint(MEANING, meaning as u64);
        }
        if let Some(uri) = uri {
            p.bytes(MEANING_URI, uri);
        }
        p.bytes(NAME, name.as_bytes());
        p.uint(MULTIPLE, u64::from(multiple));
        p.message(VALUE, |v| property_value(v, value));
    });
}

fn property_value(w: &mut Writer, value: &Value) {
    match &value.value_type {
        Some(ValueType::BooleanValue(b)) => w.uint(BOOLEAN, u64::from(*b)),
        Some(ValueType::IntegerValue(i)) => w.int(INT64, *i),
        Some(ValueType::DoubleValue(d)) => w.double(DOUBLE, if *d == 0.0 { 0.0 } else { *d }),
        Some(ValueType::TimestampValue(ts)) => w.int(INT64, micros(ts)),
        Some(ValueType::StringValue(s)) => w.bytes(STRING, s.as_bytes()),
        Some(ValueType::BlobValue(b)) => w.bytes(STRING, b),
        Some(ValueType::GeoPointValue(point)) => w.group(POINT, |p| {
            p.double(POINT_X, point.latitude);
            p.double(POINT_Y, point.longitude);
        }),
        Some(ValueType::KeyValue(key)) => w.group(REFERENCE, |r| {
            let partition = key.partition_id.clone().unwrap_or_default();
            r.bytes(REFERENCE_APP, partition.project_id.as_bytes());
            for element in &key.path {
                r.group(REFERENCE_ELEMENT, |e| {
                    path_element(e, element, REFERENCE_TYPE, REFERENCE_ID, REFERENCE_NAME);
                });
            }
            if !partition.namespace_id.is_empty() {
                r.bytes(REFERENCE_NAME_SPACE, partition.namespace_id.as_bytes());
            }
            if !partition.database_id.is_empty() {
                r.bytes(REFERENCE_DATABASE_ID, partition.database_id.as_bytes());
            }
        }),
        Some(ValueType::EntityValue(nested)) => {
            let mut inner = Writer::default();
            entity(&mut inner, nested.key.as_ref(), &nested.properties);
            w.bytes(STRING, &inner.into_bytes());
        }
        // Null; arrays are expanded by the caller.
        _ => {}
    }
}

fn micros(ts: &Timestamp) -> i64 {
    ts.seconds
        .saturating_mul(1_000_000)
        .saturating_add(i64::from(ts.nanos / 1000))
}

/// Decodes an entity, as written by the official emulator or by [`encode`]: its key and its
/// properties.
pub fn decode(bytes: &[u8]) -> Result<(Key, Properties), Malformed> {
    let (key, properties) = decode_entity(bytes)?;
    let key = key.ok_or(Malformed)?;
    if key.path.is_empty() {
        return Err(Malformed);
    }
    Ok((key, properties))
}

fn decode_entity(bytes: &[u8]) -> Result<(Option<Key>, Properties), Malformed> {
    let mut key = None;
    let mut properties = Properties::new();
    for field in Reader::new(bytes) {
        let (number, value) = field?;
        match number {
            KEY => key = decode_key(value)?,
            PROPERTY => decode_property(&mut properties, value, false)?,
            RAW_PROPERTY => decode_property(&mut properties, value, true)?,
            _ => {}
        }
    }
    Ok((key, properties))
}

/// A key: `None` for the empty one a keyless entity value holds.
fn decode_key(field: Field<'_>) -> Result<Option<Key>, Malformed> {
    let mut partition = PartitionId::default();
    let mut path = Vec::new();
    for field in field.fields().ok_or(Malformed)? {
        let (number, value) = field?;
        match number {
            APP => partition.project_id = text(value)?,
            PATH => {
                for field in value.fields().ok_or(Malformed)? {
                    let (number, value) = field?;
                    if number == ELEMENT {
                        path.push(decode_element(
                            value,
                            ELEMENT_TYPE,
                            ELEMENT_ID,
                            ELEMENT_NAME,
                        )?);
                    }
                }
            }
            NAME_SPACE => partition.namespace_id = text(value)?,
            DATABASE_ID => partition.database_id = text(value)?,
            _ => {}
        }
    }
    if partition.project_id.is_empty() && path.is_empty() {
        return Ok(None);
    }
    Ok(Some(Key {
        partition_id: Some(partition),
        path,
    }))
}

fn text(field: Field<'_>) -> Result<String, Malformed> {
    String::from_utf8(field.as_bytes().ok_or(Malformed)?.to_vec()).map_err(|_| Malformed)
}

fn decode_element(
    field: Field<'_>,
    kind: u32,
    id: u32,
    name: u32,
) -> Result<PathElement, Malformed> {
    let mut element = PathElement::default();
    for field in field.fields().ok_or(Malformed)? {
        let (number, value) = field?;
        if number == kind {
            element.kind = text(value)?;
        } else if number == id {
            #[allow(clippy::cast_possible_wrap)]
            let n = value.as_varint().ok_or(Malformed)? as i64;
            element.id_type = Some(IdType::Id(n));
        } else if number == name {
            element.id_type = Some(IdType::Name(text(value)?));
        }
    }
    Ok(element)
}

fn decode_property(
    properties: &mut Properties,
    field: Field<'_>,
    raw: bool,
) -> Result<(), Malformed> {
    let (mut meaning, mut uri, mut name, mut multiple, mut bytes) =
        (0i32, None, None, false, &[][..]);
    for field in field.fields().ok_or(Malformed)? {
        let (number, field) = field?;
        match number {
            #[allow(clippy::cast_possible_truncation)]
            MEANING => meaning = field.as_varint().ok_or(Malformed)? as i32,
            MEANING_URI => uri = Some(field.as_bytes().ok_or(Malformed)?),
            NAME => name = Some(text(field)?),
            MULTIPLE => multiple = field.as_varint().ok_or(Malformed)? != 0,
            VALUE => bytes = field.as_bytes().ok_or(Malformed)?,
            _ => {}
        }
    }
    let name = name.ok_or(Malformed)?;
    if meaning == EMPTY_LIST {
        properties.insert(name, array(Vec::new()));
        return Ok(());
    }
    let mut value = decode_value(bytes, meaning)?;
    value.exclude_from_indexes = raw;
    value.meaning = match (meaning, uri) {
        (BLOB, Some(ZLIB_URI)) => ZLIB,
        (GD_WHEN | GEORSS_POINT | ENTITY_PROTO | BLOB | BYTESTRING, _) => 0,
        (meaning, _) => meaning,
    };
    if multiple {
        let entry = properties.entry(name).or_insert_with(|| array(Vec::new()));
        if let Some(ValueType::ArrayValue(array)) = &mut entry.value_type {
            array.values.push(value);
        }
    } else {
        properties.insert(name, value);
    }
    Ok(())
}

fn array(values: Vec<Value>) -> Value {
    Value {
        value_type: Some(ValueType::ArrayValue(ArrayValue { values })),
        ..Value::default()
    }
}

fn decode_value(bytes: &[u8], meaning: i32) -> Result<Value, Malformed> {
    let mut value_type = Some(ValueType::NullValue(0));
    for field in Reader::new(bytes) {
        let (number, field) = field?;
        value_type = Some(match number {
            INT64 => {
                #[allow(clippy::cast_possible_wrap)]
                let n = field.as_varint().ok_or(Malformed)? as i64;
                if meaning == GD_WHEN {
                    ValueType::TimestampValue(Timestamp {
                        seconds: n.div_euclid(1_000_000),
                        #[allow(clippy::cast_possible_truncation)]
                        nanos: (n.rem_euclid(1_000_000) * 1000) as i32,
                    })
                } else {
                    ValueType::IntegerValue(n)
                }
            }
            BOOLEAN => ValueType::BooleanValue(field.as_varint().ok_or(Malformed)? != 0),
            DOUBLE => ValueType::DoubleValue(f64::from_bits(field.as_fixed64().ok_or(Malformed)?)),
            STRING => {
                let data = field.as_bytes().ok_or(Malformed)?;
                match meaning {
                    ENTITY_PROTO => {
                        let (key, properties) = decode_entity(data)?;
                        ValueType::EntityValue(Entity { key, properties })
                    }
                    BLOB | BYTESTRING | ZLIB => ValueType::BlobValue(data.to_vec()),
                    _ => ValueType::StringValue(
                        String::from_utf8(data.to_vec()).map_err(|_| Malformed)?,
                    ),
                }
            }
            POINT => {
                let (mut latitude, mut longitude) = (0.0, 0.0);
                for field in field.fields().ok_or(Malformed)? {
                    let (number, field) = field?;
                    match number {
                        POINT_X => latitude = f64::from_bits(field.as_fixed64().ok_or(Malformed)?),
                        POINT_Y => longitude = f64::from_bits(field.as_fixed64().ok_or(Malformed)?),
                        _ => {}
                    }
                }
                ValueType::GeoPointValue(LatLng {
                    latitude,
                    longitude,
                })
            }
            REFERENCE => {
                let mut partition = PartitionId::default();
                let mut path = Vec::new();
                for field in field.fields().ok_or(Malformed)? {
                    let (number, field) = field?;
                    match number {
                        REFERENCE_APP => partition.project_id = text(field)?,
                        REFERENCE_ELEMENT => path.push(decode_element(
                            field,
                            REFERENCE_TYPE,
                            REFERENCE_ID,
                            REFERENCE_NAME,
                        )?),
                        REFERENCE_NAME_SPACE => partition.namespace_id = text(field)?,
                        REFERENCE_DATABASE_ID => partition.database_id = text(field)?,
                        _ => {}
                    }
                }
                ValueType::KeyValue(Key {
                    partition_id: Some(partition),
                    path,
                })
            }
            _ => continue,
        });
    }
    Ok(Value {
        value_type,
        ..Value::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entity_reads_back() {
        let key = Key {
            partition_id: Some(PartitionId {
                project_id: "p".into(),
                namespace_id: "ns".into(),
                ..PartitionId::default()
            }),
            path: vec![PathElement {
                kind: "T".into(),
                id_type: Some(IdType::Id(7)),
            }],
        };
        let mut properties = Properties::new();
        properties.insert(
            "s".into(),
            Value {
                value_type: Some(ValueType::StringValue("x".into())),
                ..Value::default()
            },
        );
        properties.insert(
            "b".into(),
            Value {
                value_type: Some(ValueType::BlobValue(vec![1])),
                exclude_from_indexes: true,
                ..Value::default()
            },
        );
        let (read_key, read) = decode(&encode(&key, &properties)).unwrap();
        assert_eq!(read_key, key);
        assert_eq!(read, properties);
    }
}
