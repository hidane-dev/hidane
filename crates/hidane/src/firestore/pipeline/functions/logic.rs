//! Booleans: `and`, `or`, `xor`, `nor`, `not`, and what tells absent values and errors apart
//! (`exists`, `is_absent`, `is_error`).
//!
//! Logic is three-valued: a null or absent operand is null. `and` stops at its first false
//! operand and `or` (and `nor`) at its first true one, left to right; an operand that is not a
//! boolean before then is an error.

use hidane_proto::google::firestore::v1::{Value, value::ValueType};
use tonic::Status;

use super::{Def, Signature, args, requires};
use crate::firestore::pipeline::{
    expr::{Call, Eval, Scope},
    value,
};

pub(super) const DEFS: &[Def] = &[
    Def {
        name: "and",
        signatures: &[Signature::variadic("and(x: boolean?*): boolean?", 2)],
        check: None,
        eval: |call, scope| junction(call, scope, "and", false),
    },
    Def {
        name: "or",
        signatures: &[Signature::variadic("or(x: boolean?*): boolean?", 2)],
        check: None,
        eval: |call, scope| junction(call, scope, "or", true),
    },
    Def {
        name: "nor",
        signatures: &[Signature::variadic("nor(x: boolean*): boolean", 2)],
        check: None,
        eval: |call, scope| {
            Ok(
                junction(call, scope, "nor", true)?.map(|v| match v.value_type {
                    Some(ValueType::BooleanValue(b)) => value::boolean(!b),
                    _ => v,
                }),
            )
        },
    },
    Def {
        name: "xor",
        signatures: &[Signature::variadic("xor(x: boolean*): boolean", 2)],
        check: None,
        eval: xor,
    },
    Def {
        name: "not",
        signatures: &[Signature::fixed("not(x: boolean): boolean", 1)],
        check: None,
        eval: |call, scope| {
            let values = args(call, scope)?;
            Ok(Some(match boolean("not", values[0].as_ref())? {
                Some(b) => value::boolean(!b),
                None => value::null(),
            }))
        },
    },
    Def {
        name: "exists",
        signatures: &[Signature::fixed("exists(value: any?): boolean", 1)],
        check: None,
        eval: |call, scope| Ok(Some(value::boolean(scope.eval(&call.args[0])?.is_some()))),
    },
    Def {
        name: "is_absent",
        signatures: &[Signature::fixed("is_absent(value: any?): boolean", 1)],
        check: None,
        eval: |call, scope| Ok(Some(value::boolean(scope.eval(&call.args[0])?.is_none()))),
    },
    Def {
        name: "is_error",
        signatures: &[Signature::fixed("is_error(try: any): boolean", 1)],
        check: None,
        eval: |call, scope| Ok(Some(value::boolean(scope.eval(&call.args[0]).is_err()))),
    },
];

/// A boolean operand: `None` for null or absent, an error for any other type.
pub(crate) fn boolean(function: &str, v: Option<&Value>) -> Result<Option<bool>, Status> {
    match v {
        None => Ok(None),
        Some(v) => match v.value_type {
            Some(ValueType::BooleanValue(b)) => Ok(Some(b)),
            _ if value::is_null(v) => Ok(None),
            _ => Err(requires(function, "Boolean", v)),
        },
    }
}

/// `and` (`stop` false) or `or` (`stop` true): the first operand equal to `stop` decides.
fn junction(call: &Call, scope: &Scope<'_>, name: &str, stop: bool) -> Eval {
    let mut null = false;
    for arg in &call.args {
        match boolean(name, scope.eval(arg)?.as_ref())? {
            Some(b) if b == stop => return Ok(Some(value::boolean(stop))),
            Some(_) => {}
            None => null = true,
        }
    }
    Ok(Some(if null {
        value::null()
    } else {
        value::boolean(!stop)
    }))
}

fn xor(call: &Call, scope: &Scope<'_>) -> Eval {
    let mut null = false;
    let mut odd = false;
    for v in args(call, scope)? {
        match boolean("xor", v.as_ref())? {
            Some(b) => odd ^= b,
            None => null = true,
        }
    }
    Ok(Some(if null {
        value::null()
    } else {
        value::boolean(odd)
    }))
}
