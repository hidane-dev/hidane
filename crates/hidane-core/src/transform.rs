//! Field transforms (`serverTimestamp`, `increment`, `maximum`, `minimum`, `arrayUnion`,
//! `arrayRemove`).
//!
//! Semantics as measured on the official emulator v1.22.0 (`tools/oracle/transforms.py`):
//!
//! - `REQUEST_TIME` is the commit's time truncated to milliseconds, the same for every field of
//!   the commit.
//! - `increment`: a missing or non-numeric field becomes the operand; two integers add with
//!   saturation at `i64::MIN` / `i64::MAX`; otherwise both are added as doubles.
//! - `maximum` / `minimum`: a missing or non-numeric field becomes the operand; equal values
//!   (`3` and `3.0`, `-0.0` and `0.0`) keep the stored value and its type; NaN on either side
//!   gives NaN.
//! - `arrayUnion` appends operand elements not already present; `arrayRemove` removes every
//!   element equal to an operand. Equality is Firestore value equality (`1 == 1.0`, NaN equals
//!   NaN, maps and arrays compare element-wise). A field that is not an array starts empty, so
//!   `arrayRemove` on it leaves an empty array.
//! - Transforms run after the write's update, in order; several may target the same field, and
//!   a path through a non-map value replaces that value with a map.

use std::cmp::Ordering;

use hidane_proto::google::firestore::v1::{
    ArrayValue, Value,
    document_transform::{
        FieldTransform,
        field_transform::{ServerValue, TransformType},
    },
    value::ValueType,
};
use prost_types::Timestamp;

use crate::{
    field_path::{FieldPath, Fields, get, set},
    order::{Number, compare, compare_number},
};

/// Applies `transform` to `fields` and returns its transform result. Errors are the official
/// `INVALID_ARGUMENT` messages.
pub fn apply(
    fields: &mut Fields,
    transform: &FieldTransform,
    commit_time: &Timestamp,
) -> Result<Value, String> {
    let path = FieldPath::parse(&transform.field_path)?;
    let current = get(fields, &path);
    let (stored, result) = match transform
        .transform_type
        .as_ref()
        .ok_or("A field transform needs a transform type.")?
    {
        TransformType::SetToServerValue(kind) => {
            if *kind != ServerValue::RequestTime as i32 {
                return Err("Unsupported server value.".into());
            }
            let value = timestamp(request_time(commit_time));
            (value.clone(), value)
        }
        TransformType::Increment(operand) => {
            let operand_number = number(operand).ok_or(NOT_A_NUMBER)?;
            let value = match current.and_then(number) {
                Some(current) => from_number(add(current, operand_number)),
                None => operand.clone(),
            };
            (value.clone(), value)
        }
        TransformType::Maximum(operand) => {
            let value = extreme(current, operand, Ordering::Greater)?;
            (value.clone(), value)
        }
        TransformType::Minimum(operand) => {
            let value = extreme(current, operand, Ordering::Less)?;
            (value.clone(), value)
        }
        TransformType::AppendMissingElements(operands) => {
            let mut elements = array_elements(current);
            for operand in &operands.values {
                if !elements.iter().any(|e| equal(e, operand)) {
                    elements.push(operand.clone());
                }
            }
            (array(elements), null())
        }
        TransformType::RemoveAllFromArray(operands) => {
            let mut elements = array_elements(current);
            elements.retain(|e| !operands.values.iter().any(|operand| equal(e, operand)));
            (array(elements), null())
        }
    };
    set(fields, &path, stored);
    Ok(result)
}

const NOT_A_NUMBER: &str = "Input must be int64 or double.";

/// `REQUEST_TIME` has millisecond precision.
pub fn request_time(commit_time: &Timestamp) -> Timestamp {
    Timestamp {
        seconds: commit_time.seconds,
        nanos: commit_time.nanos - commit_time.nanos % 1_000_000,
    }
}

fn extreme(current: Option<&Value>, operand: &Value, keep: Ordering) -> Result<Value, String> {
    let operand_number = number(operand).ok_or(NOT_A_NUMBER)?;
    let Some(current_value) = current.filter(|v| number(v).is_some()) else {
        return Ok(operand.clone());
    };
    let current_number = number(current_value).unwrap_or(Number::Integer(0));
    if is_nan(current_number) {
        return Ok(current_value.clone());
    }
    if is_nan(operand_number) {
        return Ok(operand.clone());
    }
    // Equal values keep the stored one, type included.
    Ok(if compare_number(operand_number, current_number) == keep {
        operand.clone()
    } else {
        current_value.clone()
    })
}

fn is_nan(n: Number) -> bool {
    matches!(n, Number::Double(d) if d.is_nan())
}

fn add(a: Number, b: Number) -> Number {
    match (a, b) {
        (Number::Integer(x), Number::Integer(y)) => Number::Integer(x.saturating_add(y)),
        (x, y) => Number::Double(as_f64(x) + as_f64(y)),
    }
}

#[allow(clippy::cast_precision_loss)]
fn as_f64(n: Number) -> f64 {
    match n {
        Number::Integer(i) => i as f64,
        Number::Double(d) => d,
    }
}

fn number(value: &Value) -> Option<Number> {
    value.value_type.as_ref().and_then(Number::of)
}

fn from_number(n: Number) -> Value {
    Value {
        value_type: Some(match n {
            Number::Integer(i) => ValueType::IntegerValue(i),
            Number::Double(d) => ValueType::DoubleValue(d),
        }),
    }
}

fn equal(a: &Value, b: &Value) -> bool {
    compare(a, b) == Ordering::Equal
}

fn array_elements(current: Option<&Value>) -> Vec<Value> {
    match current.and_then(|v| v.value_type.as_ref()) {
        Some(ValueType::ArrayValue(array)) => array.values.clone(),
        _ => Vec::new(),
    }
}

fn array(values: Vec<Value>) -> Value {
    Value {
        value_type: Some(ValueType::ArrayValue(ArrayValue { values })),
    }
}

fn timestamp(ts: Timestamp) -> Value {
    Value {
        value_type: Some(ValueType::TimestampValue(ts)),
    }
}

fn null() -> Value {
    Value {
        value_type: Some(ValueType::NullValue(0)),
    }
}

#[cfg(test)]
mod tests {
    use hidane_proto::google::firestore::v1::MapValue;

    use super::*;

    fn int(i: i64) -> Value {
        Value {
            value_type: Some(ValueType::IntegerValue(i)),
        }
    }

    fn double(d: f64) -> Value {
        Value {
            value_type: Some(ValueType::DoubleValue(d)),
        }
    }

    fn run(fields: &mut Fields, path: &str, kind: TransformType) -> Value {
        let transform = FieldTransform {
            field_path: path.to_owned(),
            transform_type: Some(kind),
        };
        apply(
            fields,
            &transform,
            &Timestamp {
                seconds: 10,
                nanos: 123_456_789,
            },
        )
        .unwrap()
    }

    fn field(fields: &Fields, name: &str) -> Value {
        fields[name].clone()
    }

    #[test]
    fn server_time_has_millisecond_precision() {
        let mut fields = Fields::new();
        let result = run(&mut fields, "t", TransformType::SetToServerValue(1));
        assert_eq!(
            result,
            timestamp(Timestamp {
                seconds: 10,
                nanos: 123_000_000
            })
        );
        assert_eq!(field(&fields, "t"), result);
    }

    #[test]
    fn increments_saturate_in_both_directions() {
        let mut fields: Fields = [
            ("hi".into(), int(i64::MAX - 1)),
            ("lo".into(), int(i64::MIN + 1)),
        ]
        .into();
        assert_eq!(
            run(&mut fields, "hi", TransformType::Increment(int(5))),
            int(i64::MAX)
        );
        assert_eq!(
            run(&mut fields, "lo", TransformType::Increment(int(-5))),
            int(i64::MIN)
        );
    }

    #[test]
    fn extremes_keep_the_stored_value_when_equal_and_propagate_nan() {
        let mut fields: Fields = [
            ("neg_zero".into(), double(-0.0)),
            ("stored_nan".into(), double(f64::NAN)),
            ("three".into(), int(3)),
        ]
        .into();
        let kept = run(&mut fields, "neg_zero", TransformType::Maximum(double(0.0)));
        let Some(ValueType::DoubleValue(d)) = kept.value_type else {
            panic!()
        };
        assert!(d == 0.0 && d.is_sign_negative(), "the stored -0.0 is kept");
        let nan = run(&mut fields, "stored_nan", TransformType::Minimum(int(1)));
        assert!(matches!(nan.value_type, Some(ValueType::DoubleValue(d)) if d.is_nan()));
        assert_eq!(
            run(&mut fields, "three", TransformType::Minimum(double(3.0))),
            int(3)
        );
        assert_eq!(
            run(&mut fields, "three", TransformType::Maximum(double(3.5))),
            double(3.5)
        );
    }

    #[test]
    fn non_numeric_operands_are_rejected_with_the_official_message() {
        for kind in [
            TransformType::Increment(Value {
                value_type: Some(ValueType::StringValue("x".into())),
            }),
            TransformType::Maximum(Value {
                value_type: Some(ValueType::BooleanValue(true)),
            }),
            TransformType::Minimum(Value { value_type: None }),
        ] {
            let transform = FieldTransform {
                field_path: "a".into(),
                transform_type: Some(kind),
            };
            let err = apply(&mut Fields::new(), &transform, &Timestamp::default()).unwrap_err();
            assert_eq!(err, "Input must be int64 or double.");
        }
    }

    #[test]
    fn array_operations_use_value_equality() {
        let map = |v: Value| Value {
            value_type: Some(ValueType::MapValue(MapValue {
                fields: [("k".to_owned(), v)].into(),
            })),
        };
        let mut fields: Fields = [(
            "a".into(),
            array(vec![int(1), map(int(1)), double(f64::NAN)]),
        )]
        .into();
        let operands = ArrayValue {
            values: vec![double(1.0), map(double(1.0)), double(f64::NAN), int(2)],
        };
        run(
            &mut fields,
            "a",
            TransformType::AppendMissingElements(operands.clone()),
        );
        assert_eq!(array_elements(fields.get("a")).len(), 4, "only 2 is new");
        run(
            &mut fields,
            "a",
            TransformType::RemoveAllFromArray(operands),
        );
        assert!(array_elements(fields.get("a")).is_empty());
    }
}
