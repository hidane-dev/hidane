//! Choices: `conditional`, `switch_on`, `coalesce`, `if_null`, `if_absent`, `if_error`. Each
//! evaluates only the arguments it needs.

use super::{Def, Signature, logic::boolean};
use crate::firestore::pipeline::{expr::Scope, value};

pub(super) const DEFS: &[Def] = &[
    Def {
        name: "conditional",
        signatures: &[Signature::fixed(
            "conditional(condition: boolean?, true_case: any?, false_case: any?): any?",
            3,
        )],
        check: None,
        eval: |call, scope| {
            let condition = boolean("cond", scope.eval(&call.args[0])?.as_ref())?;
            scope.eval(&call.args[if condition == Some(true) { 1 } else { 2 }])
        },
    },
    Def {
        name: "switch_on",
        signatures: &[Signature::variadic("switch_on(x: any*): any?", 2)],
        check: None,
        eval: |call, scope| {
            let (cases, default) = call.args.as_chunks::<2>();
            for [condition, result] in cases {
                if is_true(scope, condition)? {
                    return scope.eval(result);
                }
            }
            match default {
                [default] => scope.eval(default),
                _ => Err(tonic::Status::invalid_argument(
                    "all switch cases evaluate to false, and no default provided",
                )),
            }
        },
    },
    Def {
        name: "coalesce",
        signatures: &[Signature::variadic("coalesce(x: any?*): any?", 2)],
        check: None,
        eval: |call, scope| {
            let mut last = None;
            for arg in &call.args {
                last = scope.eval(arg)?;
                if last.as_ref().is_some_and(|v| !value::is_null(v)) {
                    break;
                }
            }
            Ok(last)
        },
    },
    Def {
        name: "if_null",
        signatures: &[Signature::fixed(
            "if_null(left: any?, right: any?): any?",
            2,
        )],
        check: None,
        eval: |call, scope| match scope.eval(&call.args[0])? {
            Some(v) if !value::is_null(&v) => Ok(Some(v)),
            _ => scope.eval(&call.args[1]),
        },
    },
    Def {
        name: "if_absent",
        signatures: &[Signature::fixed(
            "if_absent(value: any?, replacement: any?): any?",
            2,
        )],
        check: None,
        eval: |call, scope| match scope.eval(&call.args[0])? {
            Some(v) => Ok(Some(v)),
            None => scope.eval(&call.args[1]),
        },
    },
    Def {
        name: "if_error",
        signatures: &[Signature::fixed(
            "if_error(try: any?, catch: any?): any?",
            2,
        )],
        check: None,
        eval: |call, scope| match scope.eval(&call.args[0]) {
            Ok(v) => Ok(v),
            Err(_) => scope.eval(&call.args[1]),
        },
    },
];

/// Whether a switch condition is true; any other value is not, without an error.
fn is_true(
    scope: &Scope<'_>,
    expr: &crate::firestore::pipeline::expr::Expr,
) -> Result<bool, tonic::Status> {
    Ok(matches!(
        scope.eval(expr)?.and_then(|v| v.value_type),
        Some(hidane_proto::google::firestore::v1::value::ValueType::BooleanValue(true))
    ))
}
