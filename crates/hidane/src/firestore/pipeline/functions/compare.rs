//! Comparisons: `equal`, `not_equal`, the ranges, `equal_any`, `not_equal_any`.
//!
//! Values are equal when Firestore orders them together (`1 == 1.0`, `-0.0 == 0`), null equals
//! null and NaN equals NaN; an absent value equals only another. A range compares values of one
//! type (numbers mix integers, doubles and the BSON numbers): null is `<=` and `>=` null, and
//! NaN (a double or a decimal128) only NaN.

use std::cmp::Ordering;

use hidane_core::order::{compare, type_rank};
use hidane_proto::google::firestore::v1::{Value, value::ValueType};

use super::{Def, Signature, args, requires};
use crate::firestore::pipeline::{
    expr::{Call, Eval, Scope},
    value,
};

pub(super) const DEFS: &[Def] = &[
    Def {
        name: "equal",
        signatures: &[Signature::fixed("equal(x: any, y: any): boolean", 2)],
        check: None,
        eval: |call, scope| decide(call, scope, equality, |o| o == Some(Ordering::Equal)),
    },
    Def {
        name: "not_equal",
        signatures: &[Signature::fixed("not_equal(x: any, y: any): boolean", 2)],
        check: None,
        eval: |call, scope| decide(call, scope, equality, |o| o != Some(Ordering::Equal)),
    },
    Def {
        name: "less_than",
        signatures: &[Signature::fixed("less_than(x: any, y: any): boolean", 2)],
        check: None,
        eval: |call, scope| decide(call, scope, ordering, |o| o.is_some_and(Ordering::is_lt)),
    },
    Def {
        name: "less_than_or_equal",
        signatures: &[Signature::fixed(
            "less_than_or_equal(x: any, y: any): boolean",
            2,
        )],
        check: None,
        eval: |call, scope| decide(call, scope, ordering, |o| o.is_some_and(Ordering::is_le)),
    },
    Def {
        name: "greater_than",
        signatures: &[Signature::fixed("greater_than(x: any, y: any): boolean", 2)],
        check: None,
        eval: |call, scope| decide(call, scope, ordering, |o| o.is_some_and(Ordering::is_gt)),
    },
    Def {
        name: "greater_than_or_equal",
        signatures: &[Signature::fixed(
            "greater_than_or_equal(x: any, y: any): boolean",
            2,
        )],
        check: None,
        eval: |call, scope| decide(call, scope, ordering, |o| o.is_some_and(Ordering::is_ge)),
    },
    Def {
        name: "equal_any",
        signatures: &[Signature::fixed(
            "equal_any(value: any, search_space: array[any]): boolean",
            2,
        )],
        check: None,
        eval: |call, scope| any(call, scope, false),
    },
    Def {
        name: "not_equal_any",
        signatures: &[Signature::fixed(
            "not_equal_any(value: any, search_space: array[any]): boolean",
            2,
        )],
        check: None,
        eval: |call, scope| any(call, scope, true),
    },
];

/// Whether two values are equal, as `equal` decides it.
pub(crate) fn same(a: &Value, b: &Value) -> bool {
    type_rank(a) == type_rank(b) && compare(a, b) == Ordering::Equal
}

/// How `a` and `b` compare for `equal`: `None` for values that are never equal.
fn equality(a: Option<&Value>, b: Option<&Value>) -> Option<Ordering> {
    match (a, b) {
        (None, None) => Some(Ordering::Equal),
        (Some(a), Some(b)) if same(a, b) => Some(Ordering::Equal),
        _ => None,
    }
}

/// How `a` and `b` compare for a range: `None` for values of different types.
pub(crate) fn ordering(a: Option<&Value>, b: Option<&Value>) -> Option<Ordering> {
    let (a, b) = match (a, b) {
        (None, None) => return Some(Ordering::Equal),
        (Some(a), Some(b)) => (a, b),
        _ => return None,
    };
    let nan = hidane_core::bson::is_nan;
    if nan(a) || nan(b) {
        return (nan(a) && nan(b)).then_some(Ordering::Equal);
    }
    (type_rank(a) == type_rank(b)).then(|| compare(a, b))
}

fn decide(
    call: &Call,
    scope: &Scope<'_>,
    how: fn(Option<&Value>, Option<&Value>) -> Option<Ordering>,
    accept: fn(Option<Ordering>) -> bool,
) -> Eval {
    let values = args(call, scope)?;
    Ok(Some(value::boolean(accept(how(
        values[0].as_ref(),
        values[1].as_ref(),
    )))))
}

/// `equal_any` (or, `negate`d, `not_equal_any`): null for a null or absent search space.
fn any(call: &Call, scope: &Scope<'_>, negate: bool) -> Eval {
    let values = args(call, scope)?;
    let space = match values[1].as_ref() {
        None => return super::null(),
        Some(v) if value::is_null(v) => return super::null(),
        Some(Value {
            value_type: Some(ValueType::ArrayValue(array)),
        }) => &array.values,
        Some(other) => return Err(requires("eq_any", "Array", other)),
    };
    let found = values[0]
        .as_ref()
        .is_some_and(|v| space.iter().any(|e| same(v, e)));
    Ok(Some(value::boolean(found != negate)))
}
