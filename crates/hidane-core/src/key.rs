//! Order-preserving byte encoding of values and document names.
//!
//! For any two values `a` and `b`:
//!
//! ```text
//! order::compare(a, b) == encode_value(a).cmp(&encode_value(b))
//! ```
//!
//! Values that compare equal (`1` and `1.0`, `-0.0` and `0`, two NaNs, `{a: 1}` and
//! `{a: 1.0}`) encode to identical bytes, so an equality filter is a single key lookup and a
//! range filter is a single key range. Every encoding is self-delimiting, so encoded values can
//! be concatenated into composite index keys (field value, then document name).
//!
//! Layout: a type tag, then a type-specific payload.
//!
//! | tag  | type       | payload |
//! |------|------------|---------|
//! | 0x05 | null       | — |
//! | 0x0A | boolean    | 0x00 false / 0x01 true |
//! | 0x0F | NaN        | — |
//! | 0x14 | number     | nearest double (8 bytes, sortable), then the integer's distance to it (2 bytes, sortable) |
//! | 0x19 | timestamp  | seconds (8 bytes, sortable), nanos (4 bytes) |
//! | 0x1E | string     | escaped UTF-8, terminator |
//! | 0x23 | bytes      | escaped bytes, terminator |
//! | 0x28 | reference  | path encoding of the resource name |
//! | 0x2D | geo point  | latitude, longitude (8 bytes each, sortable) |
//! | 0x32 | array      | element encodings, then 0x00 |
//! | 0x37 | vector     | length (4 bytes), element encodings |
//! | 0x3C | map        | (escaped key, terminator, value encoding)*, then 0x00 0x00 |
//! | 0x41 | expression | — |
//!
//! Escaping: inside strings, bytes, keys and path segments 0x00 becomes 0x00 0xFF and the
//! terminator is 0x00 0x01, so a prefix sorts before every extension of it.
//!
//! Numbers: an integer and a double compare by exact value. The integer is rounded to the
//! nearest double; rounding is monotonic, so different rounded values already order the
//! numbers. When the rounded values are equal, the integer's exact distance from that double
//! (at most 1024 in magnitude, 0 for doubles) breaks the tie.

use hidane_proto::google::firestore::v1::{Value, value::ValueType};

use crate::order::{is_vector, numeric_id, vector_elements};

const NULL: u8 = 0x05;
const BOOLEAN: u8 = 0x0A;
const NAN: u8 = 0x0F;
const NUMBER: u8 = 0x14;
const TIMESTAMP: u8 = 0x19;
const STRING: u8 = 0x1E;
const BYTES: u8 = 0x23;
const REFERENCE: u8 = 0x28;
const GEO_POINT: u8 = 0x2D;
const ARRAY: u8 = 0x32;
const VECTOR: u8 = 0x37;
const MAP: u8 = 0x3C;
const EXPRESSION: u8 = 0x41;

const SEQUENCE_END: u8 = 0x00;
const ESCAPE: [u8; 2] = [0x00, 0xFF];
const TERMINATOR: [u8; 2] = [0x00, 0x01];
const MAP_END: [u8; 2] = [0x00, 0x00];

const PATH_END: u8 = 0x00;
const PATH_NUMERIC_SEGMENT: u8 = 0x01;
const PATH_STRING_SEGMENT: u8 = 0x02;
const PATH_AFTER_DESCENDANTS: u8 = 0x03;

/// Encodes `value` so that byte order equals Firestore value order.
pub fn encode_value(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    write_value(value, &mut out);
    out
}

/// Escaped bytes plus terminator, as used for strings and map keys. Prefix of a
/// collection-group index key.
pub fn encode_escaped(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    write_escaped(bytes, &mut out);
    out
}

pub fn write_value(value: &Value, out: &mut Vec<u8>) {
    match &value.value_type {
        None | Some(ValueType::NullValue(_)) => out.push(NULL),
        Some(ValueType::BooleanValue(b)) => out.extend([BOOLEAN, u8::from(*b)]),
        Some(ValueType::IntegerValue(i)) => write_integer(*i, out),
        Some(ValueType::DoubleValue(d)) => write_double(*d, out),
        Some(ValueType::TimestampValue(ts)) => {
            out.push(TIMESTAMP);
            out.extend(sortable_i64(ts.seconds));
            out.extend(ts.nanos.to_be_bytes());
        }
        Some(ValueType::StringValue(s)) => {
            out.push(STRING);
            write_escaped(s.as_bytes(), out);
        }
        Some(ValueType::BytesValue(b)) => {
            out.push(BYTES);
            write_escaped(b, out);
        }
        Some(ValueType::ReferenceValue(name)) => {
            out.push(REFERENCE);
            write_path(name.split('/'), out);
        }
        Some(ValueType::GeoPointValue(point)) => {
            out.push(GEO_POINT);
            out.extend(sortable_f64(point.latitude));
            out.extend(sortable_f64(point.longitude));
        }
        Some(ValueType::ArrayValue(array)) => {
            out.push(ARRAY);
            for element in &array.values {
                write_value(element, out);
            }
            out.push(SEQUENCE_END);
        }
        Some(ValueType::MapValue(map)) if is_vector(map) => {
            let elements = vector_elements(map);
            out.push(VECTOR);
            out.extend(
                u32::try_from(elements.len())
                    .unwrap_or(u32::MAX)
                    .to_be_bytes(),
            );
            for element in elements {
                write_value(element, out);
            }
        }
        Some(ValueType::MapValue(map)) => {
            out.push(MAP);
            // `fields` is a BTreeMap: UTF-8 key order, as Firestore compares maps.
            for (key, field) in &map.fields {
                write_escaped(key.as_bytes(), out);
                write_value(field, out);
            }
            out.extend(MAP_END);
        }
        Some(
            ValueType::FieldReferenceValue(_)
            | ValueType::VariableReferenceValue(_)
            | ValueType::FunctionValue(_)
            | ValueType::PipelineValue(_),
        ) => out.push(EXPRESSION),
    }
}

/// Encodes a document name (or any slash-separated resource path) so that byte order equals
/// `__name__` order. Collection-group scans use the same encoding: documents in a group sort
/// by their full path.
pub fn encode_path<'a>(segments: impl IntoIterator<Item = &'a str>) -> Vec<u8> {
    let mut out = Vec::new();
    write_path(segments, &mut out);
    out
}

pub fn write_path<'a>(segments: impl IntoIterator<Item = &'a str>, out: &mut Vec<u8>) {
    write_path_prefix(segments, out);
    out.push(PATH_END);
}

/// The encoding of `segments` without the end marker: every path that starts with these
/// segments (the path itself and all its descendants) has an encoding that starts with it.
pub fn encode_path_prefix<'a>(segments: impl IntoIterator<Item = &'a str>) -> Vec<u8> {
    let mut out = Vec::new();
    write_path_prefix(segments, &mut out);
    out
}

/// An exclusive upper bound for the encodings of `prefix` and all its descendants: their next
/// byte after the prefix is the end marker or a segment marker, all below this one.
pub fn path_subtree_end(prefix: &[u8]) -> Vec<u8> {
    let mut end = prefix.to_vec();
    end.push(PATH_AFTER_DESCENDANTS);
    end
}

fn write_path_prefix<'a>(segments: impl IntoIterator<Item = &'a str>, out: &mut Vec<u8>) {
    for segment in segments {
        match numeric_id(segment) {
            Some(id) => {
                out.push(PATH_NUMERIC_SEGMENT);
                out.extend(sortable_i64(id));
            }
            None => {
                out.push(PATH_STRING_SEGMENT);
                write_escaped(segment.as_bytes(), out);
            }
        }
    }
}

fn write_integer(i: i64, out: &mut Vec<u8>) {
    #[allow(clippy::cast_precision_loss)]
    let nearest = i as f64;
    // `nearest` is an integral double within [-2^63, 2^63], so the conversion is exact.
    #[allow(clippy::cast_possible_truncation)]
    let distance = i128::from(i) - nearest as i128;
    out.push(NUMBER);
    out.extend(sortable_f64(nearest));
    out.extend(sortable_i16(
        i16::try_from(distance).expect("|i - round(i)| <= 1024"),
    ));
}

fn write_double(d: f64, out: &mut Vec<u8>) {
    if d.is_nan() {
        out.push(NAN);
        return;
    }
    out.push(NUMBER);
    out.extend(sortable_f64(d));
    out.extend(sortable_i16(0));
}

fn write_escaped(bytes: &[u8], out: &mut Vec<u8>) {
    for &b in bytes {
        if b == 0x00 {
            out.extend(ESCAPE);
        } else {
            out.push(b);
        }
    }
    out.extend(TERMINATOR);
}

/// Big-endian with the sign bit flipped: byte order equals numeric order.
fn sortable_i64(v: i64) -> [u8; 8] {
    (v.cast_unsigned() ^ (1 << 63)).to_be_bytes()
}

fn sortable_i16(v: i16) -> [u8; 2] {
    (v.cast_unsigned() ^ (1 << 15)).to_be_bytes()
}

/// IEEE-754 bits made byte-comparable: flip every bit of negatives, only the sign bit of
/// positives. `-0.0` is folded into `0.0` first because Firestore treats them as equal.
fn sortable_f64(v: f64) -> [u8; 8] {
    let v = if v == 0.0 { 0.0 } else { v };
    let bits = v.to_bits();
    let bits = if bits >> 63 == 1 {
        !bits
    } else {
        bits ^ (1 << 63)
    };
    bits.to_be_bytes()
}
