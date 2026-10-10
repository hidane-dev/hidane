//! Firestore value ordering.
//!
//! Values of different types sort by type, in this order (documented in
//! <https://firebase.google.com/docs/firestore/manage-data/data-types#value_type_ordering> and
//! confirmed on the official emulator):
//!
//! null < booleans < NaN < numbers < timestamps < strings < bytes < references < geo points
//! < arrays < vectors < maps
//!
//! Within a type:
//!
//! - numbers compare by exact numeric value, integers and doubles mixed: `1 == 1.0`,
//!   `-0.0 == 0`, `2^53 + 1 > 2^53 as double`, `i64::MIN == -2^63 as double`; every NaN is
//!   equal to every other NaN;
//! - strings and map keys compare by UTF-8 bytes (not UTF-16 code units);
//! - references and document names compare segment by segment; segments of the form
//!   `__id<i64>__` sort first, numerically, then other segments by UTF-8 bytes;
//! - arrays compare element by element, then by length;
//! - vectors (`{__type__: "__vector__", value: [...]}`) compare by length, then element by
//!   element;
//! - maps compare key, value, key, value, … in key order, then by size.

use std::cmp::Ordering;

use hidane_proto::google::{
    firestore::v1::{ArrayValue, MapValue, Value, value::ValueType},
    r#type::LatLng,
};
use prost_types::Timestamp;

/// Rank of a value's type in the cross-type order. NaN is ranked with numbers; the number
/// comparison puts it first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TypeRank {
    Null,
    Boolean,
    Number,
    Timestamp,
    String,
    Bytes,
    Reference,
    GeoPoint,
    Array,
    Vector,
    Map,
    /// Pipeline expressions (field / variable references, functions, pipelines). They are
    /// never stored, so their position is arbitrary; they compare equal to each other.
    Expression,
}

pub(crate) const VECTOR_TYPE_KEY: &str = "__type__";
pub(crate) const VECTOR_TYPE: &str = "__vector__";
pub(crate) const VECTOR_VALUES_KEY: &str = "value";

/// Whether a map is the wire form of a vector embedding.
pub fn is_vector(map: &MapValue) -> bool {
    matches!(
        map.fields.get(VECTOR_TYPE_KEY).and_then(|v| v.value_type.as_ref()),
        Some(ValueType::StringValue(t)) if t == VECTOR_TYPE
    )
}

/// The elements of a vector map (empty if malformed).
pub fn vector_elements(map: &MapValue) -> &[Value] {
    match map
        .fields
        .get(VECTOR_VALUES_KEY)
        .and_then(|v| v.value_type.as_ref())
    {
        Some(ValueType::ArrayValue(array)) => &array.values,
        _ => &[],
    }
}

pub fn type_rank(value: &Value) -> TypeRank {
    match &value.value_type {
        None | Some(ValueType::NullValue(_)) => TypeRank::Null,
        Some(ValueType::BooleanValue(_)) => TypeRank::Boolean,
        Some(ValueType::IntegerValue(_) | ValueType::DoubleValue(_)) => TypeRank::Number,
        Some(ValueType::TimestampValue(_)) => TypeRank::Timestamp,
        Some(ValueType::StringValue(_)) => TypeRank::String,
        Some(ValueType::BytesValue(_)) => TypeRank::Bytes,
        Some(ValueType::ReferenceValue(_)) => TypeRank::Reference,
        Some(ValueType::GeoPointValue(_)) => TypeRank::GeoPoint,
        Some(ValueType::ArrayValue(_)) => TypeRank::Array,
        Some(ValueType::MapValue(map)) if is_vector(map) => TypeRank::Vector,
        Some(ValueType::MapValue(_)) => TypeRank::Map,
        Some(
            ValueType::FieldReferenceValue(_)
            | ValueType::VariableReferenceValue(_)
            | ValueType::FunctionValue(_)
            | ValueType::PipelineValue(_),
        ) => TypeRank::Expression,
    }
}

/// Firestore's total order over values.
pub fn compare(a: &Value, b: &Value) -> Ordering {
    let (rank_a, rank_b) = (type_rank(a), type_rank(b));
    if rank_a != rank_b {
        return rank_a.cmp(&rank_b);
    }
    match (&a.value_type, &b.value_type) {
        (Some(ValueType::BooleanValue(x)), Some(ValueType::BooleanValue(y))) => x.cmp(y),
        (Some(x), Some(y)) if rank_a == TypeRank::Number => compare_numbers(x, y),
        (Some(ValueType::TimestampValue(x)), Some(ValueType::TimestampValue(y))) => {
            compare_timestamps(x, y)
        }
        (Some(ValueType::StringValue(x)), Some(ValueType::StringValue(y))) => x.cmp(y),
        (Some(ValueType::BytesValue(x)), Some(ValueType::BytesValue(y))) => x.cmp(y),
        (Some(ValueType::ReferenceValue(x)), Some(ValueType::ReferenceValue(y))) => {
            compare_paths(x.split('/'), y.split('/'))
        }
        (Some(ValueType::GeoPointValue(x)), Some(ValueType::GeoPointValue(y))) => {
            compare_geo_points(x, y)
        }
        (Some(ValueType::ArrayValue(x)), Some(ValueType::ArrayValue(y))) => compare_arrays(x, y),
        (Some(ValueType::MapValue(x)), Some(ValueType::MapValue(y)))
            if rank_a == TypeRank::Vector =>
        {
            compare_vectors(x, y)
        }
        (Some(ValueType::MapValue(x)), Some(ValueType::MapValue(y))) => compare_maps(x, y),
        // Null and expressions: equal within their rank.
        _ => Ordering::Equal,
    }
}

/// A number as Firestore sees it.
#[derive(Debug, Clone, Copy)]
pub enum Number {
    Integer(i64),
    Double(f64),
}

impl Number {
    pub fn of(value: &ValueType) -> Option<Self> {
        match value {
            ValueType::IntegerValue(i) => Some(Self::Integer(*i)),
            ValueType::DoubleValue(d) => Some(Self::Double(*d)),
            _ => None,
        }
    }
}

fn compare_numbers(a: &ValueType, b: &ValueType) -> Ordering {
    match (Number::of(a), Number::of(b)) {
        (Some(a), Some(b)) => compare_number(a, b),
        _ => Ordering::Equal,
    }
}

pub fn compare_number(a: Number, b: Number) -> Ordering {
    match (a, b) {
        (Number::Integer(x), Number::Integer(y)) => x.cmp(&y),
        (Number::Double(x), Number::Double(y)) => compare_doubles(x, y),
        (Number::Integer(x), Number::Double(y)) => compare_integer_double(x, y),
        (Number::Double(x), Number::Integer(y)) => compare_integer_double(y, x).reverse(),
    }
}

/// NaN first (all NaN equal), then numeric order with `-0.0 == 0.0`.
pub fn compare_doubles(a: f64, b: f64) -> Ordering {
    match (a.is_nan(), b.is_nan()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        (false, false) => a.partial_cmp(&b).unwrap_or(Ordering::Equal),
    }
}

/// Exact comparison of an `i64` with an `f64`, with no rounding of either side.
pub fn compare_integer_double(i: i64, d: f64) -> Ordering {
    const TWO_POW_63: f64 = 9_223_372_036_854_775_808.0;
    if d.is_nan() {
        return Ordering::Greater;
    }
    if d >= TWO_POW_63 {
        return Ordering::Less;
    }
    if d < -TWO_POW_63 {
        return Ordering::Greater;
    }
    // -2^63 <= d < 2^63, so its integer part fits in an i64 exactly.
    let whole = d.trunc();
    #[allow(clippy::cast_possible_truncation)]
    let whole_int = whole as i64;
    i.cmp(&whole_int).then_with(|| {
        let fraction = d - whole;
        if fraction > 0.0 {
            Ordering::Less
        } else if fraction < 0.0 {
            Ordering::Greater
        } else {
            Ordering::Equal
        }
    })
}

pub fn compare_timestamps(a: &Timestamp, b: &Timestamp) -> Ordering {
    (a.seconds, a.nanos).cmp(&(b.seconds, b.nanos))
}

fn compare_geo_points(a: &LatLng, b: &LatLng) -> Ordering {
    compare_doubles(a.latitude, b.latitude).then_with(|| compare_doubles(a.longitude, b.longitude))
}

fn compare_arrays(a: &ArrayValue, b: &ArrayValue) -> Ordering {
    compare_sequences(&a.values, &b.values)
}

fn compare_sequences(a: &[Value], b: &[Value]) -> Ordering {
    a.iter()
        .zip(b)
        .map(|(x, y)| compare(x, y))
        .find(|o| o.is_ne())
        .unwrap_or_else(|| a.len().cmp(&b.len()))
}

fn compare_vectors(a: &MapValue, b: &MapValue) -> Ordering {
    let (x, y) = (vector_elements(a), vector_elements(b));
    x.len().cmp(&y.len()).then_with(|| compare_sequences(x, y))
}

fn compare_maps(a: &MapValue, b: &MapValue) -> Ordering {
    // `fields` is a BTreeMap, so both iterate in UTF-8 key order.
    a.fields
        .iter()
        .zip(&b.fields)
        .map(|((ka, va), (kb, vb))| ka.cmp(kb).then_with(|| compare(va, vb)))
        .find(|o| o.is_ne())
        .unwrap_or_else(|| a.fields.len().cmp(&b.fields.len()))
}

/// A segment of the form `__id<i64>__` (legacy numeric IDs), the number written the one way
/// Java prints it: no `+`, no leading zero, no `-0`. The official emulator rejects other
/// strings that start with `__id` and end with `__` (and `__id0__`, see `names`).
pub fn numeric_id(segment: &str) -> Option<i64> {
    let digits = segment.strip_prefix("__id")?.strip_suffix("__")?;
    let magnitude = digits.strip_prefix('-').unwrap_or(digits);
    let canonical = match magnitude.as_bytes() {
        [b'0'] => magnitude.len() == digits.len(),
        [] | [b'0', ..] => false,
        bytes => bytes.iter().all(u8::is_ascii_digit),
    };
    if canonical { digits.parse().ok() } else { None }
}

/// Numeric IDs first (numerically), then other segments by UTF-8 bytes.
pub fn compare_segments(a: &str, b: &str) -> Ordering {
    match (numeric_id(a), numeric_id(b)) {
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => a.cmp(b),
    }
}

/// Document names and references: segment by segment, then the shorter path first.
pub fn compare_paths<'a>(
    a: impl IntoIterator<Item = &'a str>,
    b: impl IntoIterator<Item = &'a str>,
) -> Ordering {
    let (mut a, mut b) = (a.into_iter(), b.into_iter());
    loop {
        match (a.next(), b.next()) {
            (Some(x), Some(y)) => match compare_segments(x, y) {
                Ordering::Equal => {}
                other => return other,
            },
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::numeric_id;

    #[test]
    fn numeric_ids_are_written_one_way() {
        for (segment, expected) in [
            ("__id5__", Some(5)),
            ("__id-3__", Some(-3)),
            ("__id0__", Some(0)),
            ("__id9223372036854775807__", Some(i64::MAX)),
            ("__id-9223372036854775808__", Some(i64::MIN)),
            ("__id-0__", None),
            ("__id007__", None),
            ("__id+5__", None),
            ("__id__", None),
            ("__id-__", None),
            ("__id 5__", None),
            ("__id9223372036854775808__", None),
            ("id5", None),
        ] {
            assert_eq!(numeric_id(segment), expected, "{segment}");
        }
    }
}
