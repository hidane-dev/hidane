//! Documents as App Engine `EntityProto`s, the records of an export's `output-*` files.
//!
//! The mapping, as the official emulator writes it (`docs/export-format.md`):
//!
//! - the key names the project as the app `dev~{project}`, the database (unless `(default)`)
//!   and the path, a numeric document ID (`__id7__`) as an `id`, any other as a `name`;
//! - fields are properties in field order, after the empty arrays, which are "indexed"
//!   properties of their own meaning; an array is one property per element, marked `multiple`;
//! - a map is a nested entity without a key, stored as a string of meaning 19; a vector is a
//!   nested entity whose elements are `multiple` properties named `__vector__`;
//! - timestamps are microseconds (meaning 7), bytes are strings of meaning 14, geo points are
//!   points of meaning 9; `-0.0` is written as `0.0`.
//!
//! Reading, a reference in a top-level indexed property (as a managed export of a Native mode
//! database writes most fields) belongs to the importing project, whatever its app says: the
//! official emulator imports it so. It is decoded with an empty project
//! ([`ExportedDocument::fields_in`] fills it in); references anywhere else keep theirs.

use std::collections::BTreeMap;

use super::{
    ExportedDocument,
    wire::{Field, Malformed, Reader, Writer},
};
use crate::{
    order::{
        VECTOR_TYPE, VECTOR_TYPE_KEY, VECTOR_VALUES_KEY, is_vector, numeric_id, vector_elements,
    },
    path::ResourcePath,
    store::ReadTime,
};
use hidane_proto::google::{
    firestore::v1::{ArrayValue, MapValue, Value, value::ValueType},
    r#type::LatLng,
};

const DEFAULT_DATABASE: &str = "(default)";
const VECTOR_PROPERTY: &str = "__vector__";

// EntityProto
const KEY: u32 = 13;
const PROPERTY: u32 = 14;
const RAW_PROPERTY: u32 = 15;
const ENTITY_GROUP: u32 = 16;
// Reference (the key)
const APP: u32 = 13;
const PATH: u32 = 14;
const DATABASE_ID: u32 = 23;
// Path
const ELEMENT: u32 = 1;
const ELEMENT_TYPE: u32 = 2;
const ELEMENT_ID: u32 = 3;
const ELEMENT_NAME: u32 = 4;
// Property
const MEANING: u32 = 1;
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
const REFERENCE_DATABASE_ID: u32 = 23;
// Meanings
const GD_WHEN: u64 = 7;
const GEORSS_POINT: u64 = 9;
const BLOB: u64 = 14;
const BYTESTRING: u64 = 16;
const ENTITY_PROTO: u64 = 19;
const EMPTY_LIST: u64 = 24;

/// Encodes the document at `path` of `projects/{project}/databases/{database_id}`.
pub fn encode(
    project: &str,
    database_id: &str,
    path: &ResourcePath,
    fields: &BTreeMap<String, Value>,
) -> Vec<u8> {
    let segments: Vec<&str> = path.segments().collect();
    let mut w = Writer::default();
    w.message(KEY, |key| {
        key.bytes(APP, format!("dev~{project}").as_bytes());
        key.message(PATH, |p| {
            for pair in segments.chunks(2) {
                p.group(ELEMENT, |e| {
                    element(e, pair, ELEMENT_TYPE, ELEMENT_ID, ELEMENT_NAME)
                });
            }
        });
        if database_id != DEFAULT_DATABASE {
            key.bytes(DATABASE_ID, database_id.as_bytes());
        }
    });
    properties(&mut w, fields);
    w.message(ENTITY_GROUP, |p| {
        if let Some(root) = segments.chunks(2).next() {
            p.group(ELEMENT, |e| {
                element(e, root, ELEMENT_TYPE, ELEMENT_ID, ELEMENT_NAME)
            });
        }
    });
    w.into_bytes()
}

/// A collection ID and a document ID.
fn element(w: &mut Writer, pair: &[&str], kind: u32, id: u32, name: u32) {
    w.bytes(kind, pair[0].as_bytes());
    let doc = pair.get(1).copied().unwrap_or_default();
    match numeric_id(doc) {
        Some(n) => w.int(id, n),
        None => w.bytes(name, doc.as_bytes()),
    }
}

fn array(value: &Value) -> Option<&[Value]> {
    match &value.value_type {
        Some(ValueType::ArrayValue(array)) => Some(&array.values),
        _ => None,
    }
}

fn properties(w: &mut Writer, fields: &BTreeMap<String, Value>) {
    for (name, value) in fields {
        if array(value).is_some_and(<[Value]>::is_empty) {
            w.message(PROPERTY, |p| {
                p.uint(MEANING, EMPTY_LIST);
                p.bytes(NAME, name.as_bytes());
                p.uint(MULTIPLE, 0);
                p.message(VALUE, |_| {});
            });
        }
    }
    for (name, value) in fields {
        match array(value) {
            Some(values) => {
                for element in values {
                    property(w, name, element, true);
                }
            }
            None => property(w, name, value, false),
        }
    }
}

fn property(w: &mut Writer, name: &str, value: &Value, multiple: bool) {
    let meaning = match &value.value_type {
        Some(ValueType::TimestampValue(_)) => Some(GD_WHEN),
        Some(ValueType::BytesValue(_)) => Some(BLOB),
        Some(ValueType::GeoPointValue(_)) => Some(GEORSS_POINT),
        Some(ValueType::MapValue(_)) => Some(ENTITY_PROTO),
        _ => None,
    };
    w.message(RAW_PROPERTY, |p| {
        if let Some(meaning) = meaning {
            p.uint(MEANING, meaning);
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
        Some(ValueType::TimestampValue(ts)) => w.int(INT64, ReadTime::from_timestamp(ts).0),
        Some(ValueType::StringValue(s)) => w.bytes(STRING, s.as_bytes()),
        Some(ValueType::BytesValue(b)) => w.bytes(STRING, b),
        Some(ValueType::ReferenceValue(name)) => reference(w, name),
        Some(ValueType::GeoPointValue(point)) => w.group(POINT, |p| {
            p.double(POINT_X, point.latitude);
            p.double(POINT_Y, point.longitude);
        }),
        Some(ValueType::MapValue(map)) => w.bytes(STRING, &nested(map)),
        // Null; arrays are expanded by the caller; pipeline expressions are never stored.
        _ => {}
    }
}

fn reference(w: &mut Writer, name: &str) {
    let Some((project, database, path)) = name
        .strip_prefix("projects/")
        .and_then(|rest| rest.split_once("/databases/"))
        .and_then(|(project, rest)| {
            let (database, path) = rest.split_once("/documents/")?;
            Some((project, database, path))
        })
    else {
        return;
    };
    let segments: Vec<&str> = path.split('/').collect();
    w.group(REFERENCE, |r| {
        r.bytes(REFERENCE_APP, format!("dev~{project}").as_bytes());
        for pair in segments.chunks(2) {
            r.group(REFERENCE_ELEMENT, |e| {
                element(e, pair, REFERENCE_TYPE, REFERENCE_ID, REFERENCE_NAME);
            });
        }
        if database != DEFAULT_DATABASE {
            r.bytes(REFERENCE_DATABASE_ID, database.as_bytes());
        }
    });
}

/// A map as a nested entity: an empty key, its properties, an empty entity group.
fn nested(map: &MapValue) -> Vec<u8> {
    let mut w = Writer::default();
    w.message(KEY, |key| {
        key.bytes(APP, b"");
        key.message(PATH, |_| {});
    });
    if is_vector(map) {
        for element in vector_elements(map) {
            property(&mut w, VECTOR_PROPERTY, element, true);
        }
    } else {
        properties(&mut w, &map.fields);
    }
    w.message(ENTITY_GROUP, |_| {});
    w.into_bytes()
}

/// Decodes an entity, as written by the official emulator or by [`encode`]. Properties may
/// be indexed or not, in any order; the key's app is ignored.
pub fn decode(bytes: &[u8]) -> Result<ExportedDocument, Malformed> {
    let mut database_id = DEFAULT_DATABASE.to_owned();
    let mut segments = None;
    let mut fields = BTreeMap::new();
    for field in Reader::new(bytes) {
        let (number, value) = field?;
        match number {
            KEY => {
                let mut path = Vec::new();
                for field in value.fields().ok_or(Malformed)? {
                    let (number, value) = field?;
                    match number {
                        PATH => {
                            for field in value.fields().ok_or(Malformed)? {
                                let (number, value) = field?;
                                if number == ELEMENT {
                                    path.extend(decode_element(
                                        value,
                                        ELEMENT_TYPE,
                                        ELEMENT_ID,
                                        ELEMENT_NAME,
                                    )?);
                                }
                            }
                        }
                        DATABASE_ID => database_id = text(value)?,
                        _ => {}
                    }
                }
                segments = Some(path);
            }
            PROPERTY => decode_property(&mut fields, value, Relative::Yes)?,
            RAW_PROPERTY => decode_property(&mut fields, value, Relative::No)?,
            _ => {}
        }
    }
    let segments = segments.ok_or(Malformed)?;
    if segments.is_empty() {
        return Err(Malformed);
    }
    if database_id.is_empty() {
        DEFAULT_DATABASE.clone_into(&mut database_id);
    }
    Ok(ExportedDocument {
        database_id,
        path: ResourcePath::from_segments(segments),
        fields,
    })
}

fn text(field: Field<'_>) -> Result<String, Malformed> {
    String::from_utf8(field.as_bytes().ok_or(Malformed)?.to_vec()).map_err(|_| Malformed)
}

fn decode_element(
    field: Field<'_>,
    kind: u32,
    id: u32,
    name: u32,
) -> Result<[String; 2], Malformed> {
    let (mut collection, mut document) = (None, None);
    for field in field.fields().ok_or(Malformed)? {
        let (number, value) = field?;
        if number == kind {
            collection = Some(text(value)?);
        } else if number == id {
            #[allow(clippy::cast_possible_wrap)]
            let n = value.as_varint().ok_or(Malformed)? as i64;
            document = Some(format!("__id{n}__"));
        } else if number == name {
            document = Some(text(value)?);
        }
    }
    Ok([collection.ok_or(Malformed)?, document.ok_or(Malformed)?])
}

/// Whether references are relative to the importing project.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Relative {
    Yes,
    No,
}

fn decode_property(
    fields: &mut BTreeMap<String, Value>,
    field: Field<'_>,
    relative: Relative,
) -> Result<(), Malformed> {
    let (mut meaning, mut name, mut multiple, mut value) = (0, None, false, &[][..]);
    for field in field.fields().ok_or(Malformed)? {
        let (number, field) = field?;
        match number {
            MEANING => meaning = field.as_varint().ok_or(Malformed)?,
            NAME => name = Some(text(field)?),
            MULTIPLE => multiple = field.as_varint().ok_or(Malformed)? != 0,
            VALUE => value = field.as_bytes().ok_or(Malformed)?,
            _ => {}
        }
    }
    let name = name.ok_or(Malformed)?;
    if meaning == EMPTY_LIST {
        fields
            .entry(name)
            .or_insert_with(|| array_value(Vec::new()));
        return Ok(());
    }
    let value = decode_value(value, meaning, relative)?;
    if multiple {
        let entry = fields
            .entry(name)
            .or_insert_with(|| array_value(Vec::new()));
        match &mut entry.value_type {
            Some(ValueType::ArrayValue(array)) => array.values.push(value),
            _ => *entry = array_value(vec![value]),
        }
    } else {
        fields.insert(name, value);
    }
    Ok(())
}

fn array_value(values: Vec<Value>) -> Value {
    Value {
        value_type: Some(ValueType::ArrayValue(ArrayValue { values })),
    }
}

fn decode_value(bytes: &[u8], meaning: u64, relative: Relative) -> Result<Value, Malformed> {
    let mut value_type = ValueType::NullValue(0);
    for field in Reader::new(bytes) {
        let (number, field) = field?;
        value_type = match number {
            INT64 => {
                #[allow(clippy::cast_possible_wrap)]
                let n = field.as_varint().ok_or(Malformed)? as i64;
                if meaning == GD_WHEN {
                    ValueType::TimestampValue(ReadTime(n).to_timestamp())
                } else {
                    ValueType::IntegerValue(n)
                }
            }
            BOOLEAN => ValueType::BooleanValue(field.as_varint().ok_or(Malformed)? != 0),
            STRING => {
                let bytes = field.as_bytes().ok_or(Malformed)?;
                match meaning {
                    BLOB | BYTESTRING => ValueType::BytesValue(bytes.to_vec()),
                    ENTITY_PROTO => return decode_nested(bytes),
                    _ => match String::from_utf8(bytes.to_vec()) {
                        Ok(s) => ValueType::StringValue(s),
                        Err(err) => ValueType::BytesValue(err.into_bytes()),
                    },
                }
            }
            DOUBLE => ValueType::DoubleValue(f64::from_bits(field.as_fixed64().ok_or(Malformed)?)),
            POINT => {
                let mut point = LatLng::default();
                for field in field.fields().ok_or(Malformed)? {
                    let (number, field) = field?;
                    let x = f64::from_bits(field.as_fixed64().ok_or(Malformed)?);
                    match number {
                        POINT_X => point.latitude = x,
                        POINT_Y => point.longitude = x,
                        _ => {}
                    }
                }
                ValueType::GeoPointValue(point)
            }
            REFERENCE => ValueType::ReferenceValue(decode_reference(field, relative)?),
            _ => continue,
        };
    }
    Ok(Value {
        value_type: Some(value_type),
    })
}

fn decode_reference(field: Field<'_>, relative: Relative) -> Result<String, Malformed> {
    let (mut project, mut database, mut path) =
        (String::new(), DEFAULT_DATABASE.to_owned(), Vec::new());
    for field in field.fields().ok_or(Malformed)? {
        let (number, field) = field?;
        match number {
            REFERENCE_APP => {
                let app = text(field)?;
                // `dev~project` (`s~project` in production): the partition goes.
                if relative == Relative::No {
                    project = app
                        .split_once('~')
                        .map_or(app.clone(), |(_, p)| p.to_owned());
                }
            }
            REFERENCE_ELEMENT => path.extend(decode_element(
                field,
                REFERENCE_TYPE,
                REFERENCE_ID,
                REFERENCE_NAME,
            )?),
            REFERENCE_DATABASE_ID => {
                let id = text(field)?;
                if !id.is_empty() {
                    database = id;
                }
            }
            _ => {}
        }
    }
    Ok(format!(
        "projects/{project}/databases/{database}/documents/{}",
        path.join("/")
    ))
}

/// A nested entity: a map, or a vector when its properties are `__vector__`.
fn decode_nested(bytes: &[u8]) -> Result<Value, Malformed> {
    let mut fields = BTreeMap::new();
    for field in Reader::new(bytes) {
        let (number, field) = field?;
        if number == PROPERTY || number == RAW_PROPERTY {
            decode_property(&mut fields, field, Relative::No)?;
        }
    }
    if let Some(elements) = fields.remove(VECTOR_PROPERTY) {
        let elements = match elements.value_type {
            Some(ValueType::ArrayValue(_)) => elements,
            _ => array_value(vec![elements]),
        };
        fields = BTreeMap::from([
            (
                VECTOR_TYPE_KEY.to_owned(),
                Value {
                    value_type: Some(ValueType::StringValue(VECTOR_TYPE.to_owned())),
                },
            ),
            (VECTOR_VALUES_KEY.to_owned(), elements),
        ]);
    }
    Ok(Value {
        value_type: Some(ValueType::MapValue(MapValue { fields })),
    })
}
