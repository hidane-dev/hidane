//! The accumulators of the `aggregate` stage: `count`, `count_if`, `count_distinct`, `sum`,
//! `average`, `minimum`, `maximum`, `first`, `last`, `array_agg`, `array_agg_distinct`.
//!
//! Each reads its argument in every row of a group. A null or absent value counts for none
//! but `array_agg` and `array_agg_distinct` (and `first` and `last`, which take the row's),
//! which keep null. `sum` and `average` read numbers alone, adding them as an enterprise
//! database's aggregations do (`crate::firestore::aggregation::enterprise_sum`); `minimum` and
//! `maximum` compare in Firestore's order across types. Without any value they give null.

use std::cmp::Ordering;

use hidane_core::order::compare;
use hidane_proto::google::firestore::v1::{Value, value::ValueType};
use tonic::Status;

use super::{
    expr::Expr,
    functions::{Signature, compare::same},
    value,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Count,
    CountIf,
    CountDistinct,
    Sum,
    Average,
    Minimum,
    Maximum,
    First,
    Last,
    ArrayAgg,
    ArrayAggDistinct,
}

pub(crate) struct Def {
    pub(crate) name: &'static str,
    pub(crate) signatures: &'static [Signature],
    pub(crate) kind: Kind,
}

const DEFS: &[Def] = &[
    Def {
        name: "count",
        signatures: &[Signature {
            text: "count(x: any?): int64",
            min: 0,
            max: Some(1),
        }],
        kind: Kind::Count,
    },
    Def {
        name: "count_if",
        signatures: &[Signature::fixed("count_if(x: boolean?): int64", 1)],
        kind: Kind::CountIf,
    },
    Def {
        name: "count_distinct",
        signatures: &[Signature::fixed("count_distinct(x: any?): int64", 1)],
        kind: Kind::CountDistinct,
    },
    Def {
        name: "sum",
        signatures: &[Signature::fixed("sum(x: number?): number", 1)],
        kind: Kind::Sum,
    },
    Def {
        name: "average",
        signatures: &[Signature::fixed("average(x: number?): float64", 1)],
        kind: Kind::Average,
    },
    Def {
        name: "minimum",
        signatures: &[Signature::fixed("minimum(x: any?): any?", 1)],
        kind: Kind::Minimum,
    },
    Def {
        name: "maximum",
        signatures: &[Signature::fixed("maximum(x: any?): any?", 1)],
        kind: Kind::Maximum,
    },
    Def {
        name: "first",
        signatures: &[Signature::fixed("first(x: any?): any?", 1)],
        kind: Kind::First,
    },
    Def {
        name: "last",
        signatures: &[Signature::fixed("last(x: any?): any?", 1)],
        kind: Kind::Last,
    },
    Def {
        name: "array_agg",
        signatures: &[Signature::fixed("array_agg(x: any?): array[any]", 1)],
        kind: Kind::ArrayAgg,
    },
    Def {
        name: "array_agg_distinct",
        signatures: &[Signature::fixed(
            "array_agg_distinct(x: any?): array[any]",
            1,
        )],
        kind: Kind::ArrayAggDistinct,
    },
];

pub(crate) fn find(name: &str) -> Result<&'static Def, Status> {
    DEFS.iter()
        .find(|def| def.name == name)
        .ok_or_else(|| super::functions::does_not_exist(name))
}

/// An accumulator of the stage: what it computes, over which expression.
#[derive(Debug, Clone)]
pub(crate) struct Accumulator {
    pub(crate) kind: Kind,
    pub(crate) arg: Option<Expr>,
}

/// The accumulator's result over a group, given its argument in each row (`None` for absent;
/// every row's `None` for `count()` without one).
pub(crate) fn compute(
    kind: Kind,
    values: &[Option<Value>],
    has_arg: bool,
) -> Result<Value, Status> {
    let present = || values.iter().flatten().filter(|v| !value::is_null(v));
    let count = |n: usize| value::int(i64::try_from(n).unwrap_or(i64::MAX));
    Ok(match kind {
        Kind::Count if !has_arg => count(values.len()),
        Kind::Count => count(present().count()),
        Kind::CountIf => count(
            values
                .iter()
                .flatten()
                .filter(|v| matches!(v.value_type, Some(ValueType::BooleanValue(true))))
                .count(),
        ),
        Kind::CountDistinct => count(distinct(present()).len()),
        Kind::Sum | Kind::Average => {
            let numbers: Vec<&ValueType> = present()
                .filter_map(|v| v.value_type.as_ref())
                .filter(|v| matches!(v, ValueType::IntegerValue(_) | ValueType::DoubleValue(_)))
                .collect();
            match crate::firestore::aggregation::enterprise_sum(&numbers)? {
                None => value::null(),
                Some(sum) if kind == Kind::Sum => value::of(sum),
                #[allow(clippy::cast_precision_loss)]
                Some(sum) => {
                    let total = match sum {
                        ValueType::IntegerValue(i) => i as f64,
                        ValueType::DoubleValue(d) => d,
                        _ => unreachable!("a sum is a number"),
                    };
                    value::double(total / numbers.len() as f64)
                }
            }
        }
        Kind::Minimum | Kind::Maximum => {
            let pick = if kind == Kind::Minimum {
                Ordering::Less
            } else {
                Ordering::Greater
            };
            present()
                .fold(None::<&Value>, |best, v| match best {
                    Some(b) if compare(v, b) != pick => Some(b),
                    _ => Some(v),
                })
                .cloned()
                .unwrap_or_else(value::null)
        }
        Kind::First => values
            .first()
            .cloned()
            .flatten()
            .unwrap_or_else(value::null),
        Kind::Last => values.last().cloned().flatten().unwrap_or_else(value::null),
        Kind::ArrayAgg => value::array(values.iter().flatten().cloned().collect()),
        Kind::ArrayAggDistinct => value::array(
            distinct(values.iter().flatten())
                .into_iter()
                .cloned()
                .collect(),
        ),
    })
}

/// The values once each, as `equal` tells them apart, in the order first seen.
fn distinct<'a>(values: impl Iterator<Item = &'a Value>) -> Vec<&'a Value> {
    let mut seen: Vec<&Value> = Vec::new();
    for v in values {
        if !seen.iter().any(|s| same(s, v)) {
            seen.push(v);
        }
    }
    seen
}
