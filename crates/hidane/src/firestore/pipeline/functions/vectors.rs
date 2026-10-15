//! Vectors: `cosine_distance`, `dot_product`, `euclidean_distance`, `vector_length`.
//!
//! The distances take two vectors of the same length (an array is not a vector) and give a
//! double, computed in plain double arithmetic, element by element in order. The cosine
//! distance is `1 - x·y / sqrt(|x|² |y|²)`, one square root of the product: two square roots
//! would give `2.2e-16`, not `0.0`, for `cosine_distance(v, v)` with `v = [1, 2]`.

use hidane_core::order::{is_vector, vector_elements};
use hidane_proto::google::firestore::v1::{Value, value::ValueType};
use tonic::Status;

use super::{Def, Signature, args, requires};
use crate::firestore::pipeline::{
    expr::{Call, Eval, Scope},
    value,
};

pub(super) const DEFS: &[Def] = &[
    Def {
        name: "cosine_distance",
        signatures: &[Signature::fixed(
            "cosine_distance(x: vector, y: vector): float64",
            2,
        )],
        check: None,
        eval: |call, scope| {
            distance(call, scope, "cosine_distance", |x, y| {
                1.0 - dot(x, y) / (dot(x, x) * dot(y, y)).sqrt()
            })
        },
    },
    Def {
        name: "dot_product",
        signatures: &[Signature::fixed(
            "dot_product(x: vector, y: vector): float64",
            2,
        )],
        check: None,
        eval: |call, scope| distance(call, scope, "dot_product", dot),
    },
    Def {
        name: "euclidean_distance",
        signatures: &[Signature::fixed(
            "euclidean_distance(x: vector, y: vector): float64",
            2,
        )],
        check: None,
        eval: |call, scope| {
            distance(call, scope, "euclidean_distance", |x, y| {
                x.iter()
                    .zip(y)
                    .fold(0.0, |sum, (a, b)| sum + (a - b) * (a - b))
                    .sqrt()
            })
        },
    },
    Def {
        name: "vector_length",
        signatures: &[Signature::fixed("vector_length(vector: vector): int64", 1)],
        check: None,
        eval: |call, scope| {
            let values = args(call, scope)?;
            Ok(Some(match vector("vector_length", values[0].as_ref())? {
                #[allow(clippy::cast_possible_wrap)]
                Some(elements) => value::int(elements.len() as i64),
                None => value::null(),
            }))
        },
    },
];

/// A vector operand, its elements as doubles: `None` for null or absent, an error for any
/// other type.
fn vector(function: &str, v: Option<&Value>) -> Result<Option<Vec<f64>>, Status> {
    match v {
        None => Ok(None),
        Some(v) => match &v.value_type {
            Some(ValueType::MapValue(map)) if is_vector(map) => Ok(Some(
                vector_elements(map)
                    .iter()
                    .map(|e| match e.value_type {
                        Some(ValueType::DoubleValue(d)) => d,
                        #[allow(clippy::cast_precision_loss)]
                        Some(ValueType::IntegerValue(i)) => i as f64,
                        _ => f64::NAN,
                    })
                    .collect(),
            )),
            _ if value::is_null(v) => Ok(None),
            _ => Err(requires(function, "Vector", v)),
        },
    }
}

/// `measure` of the two vector arguments, both checked before a null one makes the result
/// null.
fn distance(
    call: &Call,
    scope: &Scope<'_>,
    function: &str,
    measure: fn(&[f64], &[f64]) -> f64,
) -> Eval {
    let values = args(call, scope)?;
    let x = vector(function, values[0].as_ref())?;
    let y = vector(function, values[1].as_ref())?;
    let Some((x, y)) = x.zip(y) else {
        return super::null();
    };
    if x.len() != y.len() {
        return Err(Status::invalid_argument(format!(
            "{function}(...) arguments must be the same length."
        )));
    }
    Ok(Some(value::double(measure(&x, &y))))
}

fn dot(x: &[f64], y: &[f64]) -> f64 {
    x.iter().zip(y).fold(0.0, |sum, (a, b)| sum + a * b)
}
