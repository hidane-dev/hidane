//! What Firestore does to values when it stores them.

use hidane_proto::google::firestore::v1::{Value, value::ValueType};

/// Applies Firestore's write-time normalization in place.
///
/// Timestamps keep microsecond precision: the official emulator stores
/// `1970-01-01T00:00:00.000000999Z` as the epoch and `…00.000001500Z` as one microsecond, so
/// sub-microsecond digits are truncated, not rounded (`tests/fixtures/value_order.json`).
pub fn normalize_value(value: &mut Value) {
    match &mut value.value_type {
        Some(ValueType::TimestampValue(ts)) => ts.nanos -= ts.nanos % 1_000,
        Some(ValueType::ArrayValue(array)) => array.values.iter_mut().for_each(normalize_value),
        Some(ValueType::MapValue(map)) => map.fields.values_mut().for_each(normalize_value),
        _ => {}
    }
}
