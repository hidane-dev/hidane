//! For arbitrary values: the byte encoding orders exactly like `compare`, and `compare` is a
//! consistent total order.

use std::cmp::Ordering;

use hidane_core::{
    key::{encode_path, encode_value},
    order::{compare, compare_integer_double, compare_paths},
};
use hidane_proto::google::{
    firestore::v1::{ArrayValue, MapValue, Value, value::ValueType},
    r#type::LatLng,
};
use proptest::prelude::*;
use prost_types::Timestamp;

fn wrap(value_type: ValueType) -> Value {
    Value {
        value_type: Some(value_type),
    }
}

fn edge_doubles() -> impl Strategy<Value = f64> {
    prop_oneof![
        Just(f64::NAN),
        Just(f64::INFINITY),
        Just(f64::NEG_INFINITY),
        Just(0.0),
        Just(-0.0),
        Just(5e-324),
        Just(-5e-324),
        Just(1.0),
        Just(-1.0),
        Just(9_007_199_254_740_992.0),
        Just(9_007_199_254_740_994.0),
        Just(9_223_372_036_854_775_808.0),
        Just(-9_223_372_036_854_775_808.0),
        Just(9_223_372_036_854_774_784.0),
        Just(f64::MAX),
        Just(f64::MIN),
    ]
}

fn edge_integers() -> impl Strategy<Value = i64> {
    prop_oneof![
        Just(i64::MIN),
        Just(i64::MIN + 1),
        Just(i64::MAX),
        Just(i64::MAX - 1),
        Just(0),
        Just(1),
        Just(-1),
        Just(9_007_199_254_740_992),
        Just(9_007_199_254_740_993),
        Just(-9_007_199_254_740_993),
        Just(9_223_372_036_854_774_784),
        Just(9_223_372_036_854_775_296),
    ]
}

fn number() -> impl Strategy<Value = Value> {
    prop_oneof![
        any::<i64>().prop_map(ValueType::IntegerValue),
        edge_integers().prop_map(ValueType::IntegerValue),
        any::<f64>().prop_map(ValueType::DoubleValue),
        edge_doubles().prop_map(ValueType::DoubleValue),
        // Integral doubles near integers, to exercise ties between the two kinds.
        edge_integers().prop_map(|i| ValueType::DoubleValue(i as f64)),
        (-1000i64..1000).prop_map(|i| ValueType::DoubleValue(i as f64)),
        (-1000i64..1000).prop_map(ValueType::IntegerValue),
    ]
    .prop_map(wrap)
}

fn segment() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("a".to_owned()),
        Just("a\u{0}".to_owned()),
        Just("aa".to_owned()),
        Just("c".to_owned()),
        Just("c-b".to_owned()),
        Just("\u{ff5e}".to_owned()),
        Just("\u{1f600}".to_owned()),
        Just("__id5__".to_owned()),
        Just("__id10__".to_owned()),
        Just("__id-1__".to_owned()),
        "[a-z]{1,3}",
    ]
}

fn path() -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec(segment(), 1..6)
}

fn leaf() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(wrap(ValueType::NullValue(0))),
        any::<bool>().prop_map(|b| wrap(ValueType::BooleanValue(b))),
        number(),
        number(),
        (-62_135_596_800i64..253_402_300_800, 0i32..1_000_000_000).prop_map(|(seconds, nanos)| {
            wrap(ValueType::TimestampValue(Timestamp { seconds, nanos }))
        }),
        prop_oneof![".{0,4}", "[ab\u{0}]{0,3}"]
            .prop_map(|s: String| wrap(ValueType::StringValue(s))),
        prop::collection::vec(any::<u8>(), 0..4).prop_map(|b| wrap(ValueType::BytesValue(b))),
        path().prop_map(|p| wrap(ValueType::ReferenceValue(p.join("/")))),
        (-90.0f64..=90.0, -180.0f64..=180.0).prop_map(|(latitude, longitude)| {
            wrap(ValueType::GeoPointValue(LatLng {
                latitude,
                longitude,
            }))
        }),
        prop::collection::vec(prop_oneof![edge_doubles(), any::<f64>()], 0..4).prop_map(vector),
    ]
}

fn vector(elements: Vec<f64>) -> Value {
    let values = elements
        .into_iter()
        .map(|d| wrap(ValueType::DoubleValue(d)))
        .collect();
    wrap(ValueType::MapValue(MapValue {
        fields: [
            (
                "__type__".to_owned(),
                wrap(ValueType::StringValue("__vector__".to_owned())),
            ),
            (
                "value".to_owned(),
                wrap(ValueType::ArrayValue(ArrayValue { values })),
            ),
        ]
        .into(),
    }))
}

fn any_value() -> impl Strategy<Value = Value> {
    leaf().prop_recursive(3, 24, 4, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..4)
                .prop_map(|values| wrap(ValueType::ArrayValue(ArrayValue { values }))),
            prop::collection::btree_map(
                prop_oneof![
                    Just(String::new()),
                    "[ab]{1,2}",
                    Just("\u{1f600}".to_owned())
                ],
                inner,
                0..4
            )
            .prop_map(|fields| wrap(ValueType::MapValue(MapValue { fields }))),
        ]
    })
}

/// Exact `i64` vs `f64` comparison by decomposing the double into mantissa and exponent.
fn reference_integer_double(i: i64, d: f64) -> Ordering {
    if d.is_nan() {
        return Ordering::Greater;
    }
    if d.is_infinite() {
        return if d > 0.0 {
            Ordering::Less
        } else {
            Ordering::Greater
        };
    }
    if d == 0.0 {
        return i.cmp(&0);
    }
    let bits = d.to_bits();
    let sign: i128 = if bits >> 63 == 1 { -1 } else { 1 };
    let exponent = i32::try_from((bits >> 52) & 0x7ff).unwrap();
    let fraction = i128::from(bits & ((1 << 52) - 1));
    let (mantissa, exp) = if exponent == 0 {
        (fraction, -1074)
    } else {
        (fraction | (1 << 52), exponent - 1075)
    };
    let m = sign * mantissa; // d = m * 2^exp
    let i = i128::from(i);
    if exp >= 0 {
        if exp > 70 {
            return if m > 0 {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        }
        i.cmp(&(m << exp))
    } else if -exp > 70 {
        // |d| < 2^53 * 2^-71 < 1, and d != 0.
        match i.cmp(&0) {
            Ordering::Equal => 0.cmp(&m),
            other => other,
        }
    } else {
        // Compare i * 2^k with m. If i * 2^k overflows i128 its magnitude exceeds 2^127, far
        // beyond |m| < 2^53, so the sign of i decides.
        match i.checked_mul(1i128 << -exp) {
            Some(scaled) => scaled.cmp(&m),
            None => i.cmp(&0),
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4096))]

    #[test]
    fn encoding_orders_like_compare(a in any_value(), b in any_value()) {
        prop_assert_eq!(encode_value(&a).cmp(&encode_value(&b)), compare(&a, &b));
    }

    #[test]
    fn compare_is_antisymmetric(a in any_value(), b in any_value()) {
        prop_assert_eq!(compare(&a, &b), compare(&b, &a).reverse());
    }

    #[test]
    fn numbers_compare_exactly(i in prop_oneof![any::<i64>(), edge_integers()],
                               d in prop_oneof![any::<f64>(), edge_doubles(),
                                                edge_integers().prop_map(|i| i as f64)]) {
        prop_assert_eq!(compare_integer_double(i, d), reference_integer_double(i, d));
    }

    #[test]
    fn path_encoding_orders_like_compare_paths(a in path(), b in path()) {
        let (ka, kb) = (encode_path(a.iter().map(String::as_str)),
                        encode_path(b.iter().map(String::as_str)));
        prop_assert_eq!(ka.cmp(&kb),
                        compare_paths(a.iter().map(String::as_str), b.iter().map(String::as_str)));
    }
}
