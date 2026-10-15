//! Arrays: `sum`, `array_concat`, `array_contains`, `array_contains_all`, `array_contains_any`,
//! `array_length`, `array_reverse`, `array_first`, `array_first_n`, `array_last`, `array_last_n`,
//! `array_get`, `array_index_of`, `array_index_of_all`, `array_slice`, `array_filter`,
//! `array_transform`, `maximum`, `minimum`, `maximum_n`, `minimum_n`.
//!
//! Every argument is read first: one of another type than the function takes is an error
//! (`array_first` and `array_last` call themselves `first` and `last` in it), then a null or
//! absent one makes the result null. Elements match a value as `equal` decides it: null matches
//! null, an absent value nothing. Counts and indexes are integers; a negative index counts from
//! the end, and one outside the array gives absent, as `array_first` and `array_last` of an
//! empty one do. `array_filter` and `array_transform` evaluate their last argument once per
//! element, with the element bound to the variable their alias names (and, for
//! `array_transform` with two aliases, its index to the second).
//!
//! `maximum` and `minimum` take an array or two values or more, `maximum_n` and `minimum_n` an
//! array and a count: they compare in Firestore's order across types (NaN below every other
//! number) and skip null values. `sum` of an array adds its numbers, as the `aggregate` stage's
//! `sum` does.
//!
//! The official emulator does not check how many arguments `array_reverse` and `array_slice`
//! get when it reads them against a signature: a count they do not take fails without a
//! message (`UNKNOWN`).

use std::cmp::Ordering;

use hidane_core::order::compare;
use hidane_proto::google::firestore::v1::{Value, value::ValueType};
use tonic::Status;

use super::{Def, Signature, args, compare::same, found, math::integer, null, one_of, requires};
use crate::firestore::pipeline::{
    expr::{Call, Eval, Expr, Scope},
    value,
};

pub(super) const DEFS: &[Def] = &[
    Def {
        name: "sum",
        signatures: &[Signature::fixed("sum(x: array[any]): number", 1)],
        check: None,
        eval: array_sum,
    },
    Def {
        name: "array_concat",
        signatures: &[Signature::variadic(
            "array_concat(arrays: array[any]*): array[any]",
            2,
        )],
        check: None,
        eval: concat,
    },
    Def {
        name: "array_contains",
        signatures: &[Signature::fixed(
            "array_contains(array: array[any], value: any): boolean",
            2,
        )],
        check: None,
        eval: |call, scope| {
            let values = args(call, scope)?;
            let Some(array) = array("array_contains", values[0].as_ref())? else {
                return null();
            };
            Ok(Some(value::boolean(contains(array, values[1].as_ref()))))
        },
    },
    Def {
        name: "array_contains_all",
        signatures: &[Signature::fixed(
            "array_contains_all(array: array[any], search_values: array[any]): boolean",
            2,
        )],
        check: None,
        eval: |call, scope| search(call, scope, "array_contains_all", true),
    },
    Def {
        name: "array_contains_any",
        signatures: &[Signature::fixed(
            "array_contains_any(array: array[any], search_values: array[any]): boolean",
            2,
        )],
        check: None,
        eval: |call, scope| search(call, scope, "array_contains_any", false),
    },
    Def {
        name: "array_length",
        signatures: &[Signature::fixed("array_length(x: array[any]): int64", 1)],
        check: None,
        eval: |call, scope| unary(call, scope, "array_length", |a| Some(int(a.len()))),
    },
    Def {
        name: "array_reverse",
        signatures: ANY_COUNT,
        check: Some(|args| arity(args, 1, 1)),
        eval: |call, scope| {
            unary(call, scope, "array_reverse", |a| {
                Some(value::array(a.iter().rev().cloned().collect()))
            })
        },
    },
    Def {
        name: "array_first",
        signatures: &[Signature::fixed("array_first(array: array[any]): any", 1)],
        check: None,
        eval: |call, scope| unary(call, scope, "first", |a| a.first().cloned()),
    },
    Def {
        name: "array_first_n",
        signatures: &[Signature::fixed(
            "array_first_n(array: array[any], n: int64): array[any]",
            2,
        )],
        check: None,
        eval: |call, scope| ends(call, scope, "array_first_n", |a, n| &a[..n]),
    },
    Def {
        name: "array_last",
        signatures: &[Signature::fixed("array_last(array: array[any]): any", 1)],
        check: None,
        eval: |call, scope| unary(call, scope, "last", |a| a.last().cloned()),
    },
    Def {
        name: "array_last_n",
        signatures: &[Signature::fixed(
            "array_last_n(array: array[any], n: int64): array[any]",
            2,
        )],
        check: None,
        eval: |call, scope| ends(call, scope, "array_last_n", |a, n| &a[a.len() - n..]),
    },
    Def {
        name: "array_get",
        signatures: &[Signature::fixed(
            "array_get(array: array[any], index: int64): any?",
            2,
        )],
        check: None,
        eval: get,
    },
    Def {
        name: "array_index_of",
        signatures: &[Signature::fixed(
            "array_index_of(array: array[any], search: any, direction: (first|last)): int64",
            3,
        )],
        check: Some(|args| one_of("array_index_of", "direction", &args[2], &["first", "last"])),
        eval: index_of,
    },
    Def {
        name: "array_index_of_all",
        signatures: &[Signature::fixed(
            "array_index_of_all(array: array[any], search: any): array[int64]",
            2,
        )],
        check: None,
        eval: index_of_all,
    },
    Def {
        name: "array_slice",
        signatures: ANY_COUNT,
        check: Some(|args| arity(args, 2, 3)),
        eval: slice,
    },
    Def {
        name: "array_filter",
        signatures: &[Signature::fixed(
            "array_filter(array: array[any], alias: string, predicate: boolean): array[any]",
            3,
        )],
        check: Some(|args| alias("array_filter", "alias", &args[1])),
        eval: filter,
    },
    Def {
        name: "array_transform",
        signatures: &[
            Signature::fixed(
                "array_transform(array: array[any], alias: string, transform: any): array[any]",
                3,
            ),
            Signature::fixed(
                "array_transform(array: array[any], alias: string, index_alias: string, \
                 transform: any): array[any]",
                4,
            ),
        ],
        check: Some(|args| {
            alias("array_transform", "alias", &args[1])?;
            match args {
                [_, _, index, _] => alias("array_transform", "index_alias", index),
                _ => Ok(()),
            }
        }),
        eval: transform,
    },
    Def {
        name: "maximum",
        signatures: &[
            Signature::fixed("maximum(input: array[any?]): any?", 1),
            Signature::variadic("maximum(x: any?*): any?", 2),
        ],
        check: None,
        eval: |call, scope| extreme(call, scope, "maximum", Ordering::Greater),
    },
    Def {
        name: "minimum",
        signatures: &[
            Signature::fixed("minimum(input: array[any?]): any?", 1),
            Signature::variadic("minimum(x: any?*): any?", 2),
        ],
        check: None,
        eval: |call, scope| extreme(call, scope, "minimum", Ordering::Less),
    },
    Def {
        name: "maximum_n",
        signatures: &[Signature::fixed(
            "maximum_n(input: array[any?], n: int64): array[any]",
            2,
        )],
        check: None,
        eval: |call, scope| extremes(call, scope, "maximum_n", Ordering::Greater),
    },
    Def {
        name: "minimum_n",
        signatures: &[Signature::fixed(
            "minimum_n(input: array[any?], n: int64): array[any]",
            2,
        )],
        check: None,
        eval: |call, scope| extremes(call, scope, "minimum_n", Ordering::Less),
    },
];

/// The signatures of a function that checks how many arguments it gets itself (`arity`).
const ANY_COUNT: &[Signature] = &[Signature::variadic("", 0)];

/// `UNKNOWN` without a message unless there are `min` to `max` arguments.
fn arity(args: &[Expr], min: usize, max: usize) -> Result<(), Status> {
    if (min..=max).contains(&args.len()) {
        Ok(())
    } else {
        Err(Status::unknown(""))
    }
}

/// The alias of `array_filter` or `array_transform`: a string literal.
fn alias(function: &str, argument: &str, expr: &Expr) -> Result<(), Status> {
    if expr.literal_string().is_some() {
        return Ok(());
    }
    Err(Status::invalid_argument(format!(
        "The '{argument}' argument of the '{function}' function must be a literal value, found: \
         {}",
        found(expr)
    )))
}

/// An array operand: `None` for null or absent, an error naming `Array` for any other type.
fn array<'v>(function: &str, v: Option<&'v Value>) -> Result<Option<&'v [Value]>, Status> {
    let Some(v) = v else {
        return Ok(None);
    };
    match &v.value_type {
        Some(ValueType::ArrayValue(array)) => Ok(Some(&array.values)),
        _ if value::is_null(v) => Ok(None),
        _ => Err(requires(function, "Array", v)),
    }
}

fn int(n: usize) -> Value {
    value::int(i64::try_from(n).unwrap_or(i64::MAX))
}

/// Whether `array` has `v`, as `equal` decides it.
fn contains(array: &[Value], v: Option<&Value>) -> bool {
    v.is_some_and(|v| array.iter().any(|e| same(e, v)))
}

/// A function of one array, `f` giving its result.
fn unary(call: &Call, scope: &Scope<'_>, function: &str, f: fn(&[Value]) -> Option<Value>) -> Eval {
    let v = scope.eval(&call.args[0])?;
    match array(function, v.as_ref())? {
        Some(a) => Ok(f(a)),
        None => null(),
    }
}

fn concat(call: &Call, scope: &Scope<'_>) -> Eval {
    let values = args(call, scope)?;
    let arrays = values
        .iter()
        .map(|v| array("array_concat", v.as_ref()))
        .collect::<Result<Vec<_>, _>>()?;
    let Some(arrays) = arrays.into_iter().collect::<Option<Vec<_>>>() else {
        return null();
    };
    Ok(Some(value::array(arrays.concat())))
}

/// `array_contains_all` (`all`) or `array_contains_any`: whether the first array has every (any)
/// element of the second.
fn search(call: &Call, scope: &Scope<'_>, function: &str, all: bool) -> Eval {
    let values = args(call, scope)?;
    let a = array(function, values[0].as_ref())?;
    let wanted = array(function, values[1].as_ref())?;
    let (Some(a), Some(wanted)) = (a, wanted) else {
        return null();
    };
    let found = |v: &Value| contains(a, Some(v));
    Ok(Some(value::boolean(if all {
        wanted.iter().all(found)
    } else {
        wanted.iter().any(found)
    })))
}

/// `array_first_n` or `array_last_n`: `take` gives the `n` elements, `n` at most the length.
fn ends(
    call: &Call,
    scope: &Scope<'_>,
    function: &str,
    take: fn(&[Value], usize) -> &[Value],
) -> Eval {
    let values = args(call, scope)?;
    let a = array(function, values[0].as_ref())?;
    let n = integer(function, values[1].as_ref())?;
    let (Some(a), Some(n)) = (a, n) else {
        return null();
    };
    let Ok(n) = usize::try_from(n) else {
        return Err(Status::invalid_argument(format!(
            "The function `{function}(...)` requires non-negative count."
        )));
    };
    Ok(Some(value::array(take(a, n.min(a.len())).to_vec())))
}

/// The position `index` names in an array of `len` elements, from the end when negative.
fn position(len: usize, index: i64) -> Option<usize> {
    let at = if index < 0 {
        i64::try_from(len).ok()? + index
    } else {
        index
    };
    usize::try_from(at).ok().filter(|&at| at < len)
}

fn get(call: &Call, scope: &Scope<'_>) -> Eval {
    let values = args(call, scope)?;
    let a = array("array_get", values[0].as_ref())?;
    let index = integer("array_get", values[1].as_ref())?;
    let (Some(a), Some(index)) = (a, index) else {
        return null();
    };
    Ok(position(a.len(), index).map(|at| a[at].clone()))
}

fn index_of(call: &Call, scope: &Scope<'_>) -> Eval {
    let values = args(call, scope)?;
    let Some(a) = array("array_index_of", values[0].as_ref())? else {
        return null();
    };
    let matches = |e: &Value| values[1].as_ref().is_some_and(|v| same(e, v));
    let at = if call.args[2].literal_string() == Some("last") {
        a.iter().rposition(matches)
    } else {
        a.iter().position(matches)
    };
    Ok(Some(at.map_or_else(|| value::int(-1), int)))
}

fn index_of_all(call: &Call, scope: &Scope<'_>) -> Eval {
    let values = args(call, scope)?;
    let Some(a) = array("array_index_of_all", values[0].as_ref())? else {
        return null();
    };
    let indexes = a
        .iter()
        .enumerate()
        .filter(|(_, e)| values[1].as_ref().is_some_and(|v| same(e, v)))
        .map(|(at, _)| int(at))
        .collect();
    Ok(Some(value::array(indexes)))
}

/// `array_slice(array, offset, length?)`: from `offset` (from the end when negative), `length`
/// elements or the rest.
fn slice(call: &Call, scope: &Scope<'_>) -> Eval {
    let values = args(call, scope)?;
    let a = array("array_slice", values[0].as_ref())?;
    let offset = integer("array_slice", values[1].as_ref())?;
    let length = values
        .get(2)
        .map(|v| integer("array_slice", v.as_ref()))
        .transpose()?;
    let (Some(a), Some(offset)) = (a, offset) else {
        return null();
    };
    let length = match length {
        None => None,
        Some(None) => return null(),
        Some(Some(length)) => Some(usize::try_from(length).map_err(|_| {
            Status::invalid_argument("array_slice(...) length must be non-negative")
        })?),
    };
    let start = if offset < 0 {
        position(a.len(), offset).unwrap_or(0)
    } else {
        usize::try_from(offset).unwrap_or(usize::MAX).min(a.len())
    };
    let end = length.map_or(a.len(), |length| start.saturating_add(length).min(a.len()));
    Ok(Some(value::array(a[start..end].to_vec())))
}

fn filter(call: &Call, scope: &Scope<'_>) -> Eval {
    let v = scope.eval(&call.args[0])?;
    let Some(a) = array("array_filter", v.as_ref())? else {
        return null();
    };
    let alias = call.args[1].literal_string().unwrap_or_default();
    let mut kept = Vec::new();
    for element in a {
        match scope.with_local(alias, element.clone(), &call.args[2])? {
            Some(Value {
                value_type: Some(ValueType::BooleanValue(keep)),
            }) => {
                if keep {
                    kept.push(element.clone());
                }
            }
            Some(v) if !value::is_null(&v) => {
                return Err(requires("array_filter", "boolean", &v));
            }
            _ => {}
        }
    }
    Ok(Some(value::array(kept)))
}

fn transform(call: &Call, scope: &Scope<'_>) -> Eval {
    let v = scope.eval(&call.args[0])?;
    let Some(a) = array("array_transform", v.as_ref())? else {
        return null();
    };
    let alias = call.args[1].literal_string().unwrap_or_default();
    let (index, body) = match &call.args[2..] {
        [index, body] => (index.literal_string(), body),
        _ => (None, &call.args[2]),
    };
    let mut transformed = Vec::with_capacity(a.len());
    for (at, element) in a.iter().enumerate() {
        let v = match index {
            Some(index) => scope.with_locals((alias, element.clone()), (index, int(at)), body)?,
            None => scope.with_local(alias, element.clone(), body)?,
        };
        transformed.push(v.unwrap_or_else(value::null));
    }
    Ok(Some(value::array(transformed)))
}

/// The values `maximum` and `minimum` choose from: an array's elements, or the arguments.
fn candidates(
    call: &Call,
    scope: &Scope<'_>,
    function: &str,
) -> Result<Option<Vec<Value>>, Status> {
    let values = args(call, scope)?;
    if let [one] = values.as_slice() {
        return Ok(array(function, one.as_ref())?.map(<[Value]>::to_vec));
    }
    Ok(Some(values.into_iter().flatten().collect()))
}

/// `maximum` (`wanted` greater) or `minimum` (`wanted` less): the first value that compares
/// `wanted` to every other, null without one.
fn extreme(call: &Call, scope: &Scope<'_>, function: &str, wanted: Ordering) -> Eval {
    let Some(values) = candidates(call, scope, function)? else {
        return null();
    };
    let best = values
        .into_iter()
        .filter(|v| !value::is_null(v))
        .reduce(|best, v| {
            if compare(&v, &best) == wanted {
                v
            } else {
                best
            }
        });
    Ok(Some(best.unwrap_or_else(value::null)))
}

/// `maximum_n` (`wanted` greater) or `minimum_n` (`wanted` less): the array's `n` greatest
/// (least) values, the greatest (least) first.
fn extremes(call: &Call, scope: &Scope<'_>, function: &str, wanted: Ordering) -> Eval {
    let values = args(call, scope)?;
    let a = array(function, values[0].as_ref())?;
    let n = integer(function, values[1].as_ref())?;
    let (Some(a), Some(n)) = (a, n) else {
        return null();
    };
    let Ok(n) = usize::try_from(n) else {
        return Err(Status::invalid_argument("n must be non-negative."));
    };
    let mut sorted: Vec<&Value> = a.iter().filter(|v| !value::is_null(v)).collect();
    sorted.sort_by(|x, y| {
        if wanted.is_gt() {
            compare(y, x)
        } else {
            compare(x, y)
        }
    });
    sorted.truncate(n);
    Ok(Some(value::array(sorted.into_iter().cloned().collect())))
}

/// `sum` of an array (`arraySum()` in the SDKs): its numbers added as the `aggregate` stage's
/// `sum` adds them, other elements left out; null without any.
fn array_sum(call: &Call, scope: &Scope<'_>) -> Eval {
    let elements = match scope.eval(&call.args[0])? {
        None => return null(),
        Some(v) => match v.value_type {
            Some(ValueType::ArrayValue(array)) => array.values,
            _ if crate::firestore::pipeline::value::is_null(&v) => return null(),
            _ => return Err(requires("array_sum", "Array", &v)),
        },
    };
    let numbers: Vec<&ValueType> = elements
        .iter()
        .filter_map(|v| v.value_type.as_ref())
        .filter(|v| matches!(v, ValueType::IntegerValue(_) | ValueType::DoubleValue(_)))
        .collect();
    match crate::firestore::aggregation::enterprise_sum(&numbers)? {
        Some(sum) => Ok(Some(crate::firestore::pipeline::value::of(sum))),
        None => null(),
    }
}
