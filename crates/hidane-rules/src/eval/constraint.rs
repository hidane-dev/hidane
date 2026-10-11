//! What a list knows of a field its query filters on, when the filters do not give its value:
//! a `constraint`, as the official emulator calls it (`tools/oracle/rules_list.py`).
//!
//! A constraint is the field's filters, each a comparator and a value (`n > 0 && n != 3` is
//! `GT 0`, `NEQ 3`; `not-in [1, 2]` is `NEQ 1`, `NEQ 2`; `!= null` is `NEQ null`;
//! `array-contains 'a'` is `LIST_CONTAINS 'a'`). A comparison with it is true when one of the
//! filters alone implies it (`GT 5` implies `> 4`, `>= 5.0` and `!= 1`; `GTE 1` and `LTE 1`
//! together do not imply `== 1`), and otherwise an error without a message, which a denial
//! shows as `evaluation error at L5:22`: never false. `!= null` always holds. A float in a
//! filter does not compare with an int in the rule (`>= 1.5` does not imply `> 1`), though an
//! int in a filter does compare with a float (`> 5` implies `> 4.5`).
//!
//! `x in c` holds when `c` has `LIST_CONTAINS x`, and `c.hasAny(list)` is whether it has one of
//! the list's items; `c in list` is false, `c is constraint` true, and anything else is the
//! error any other value of a type without that operation gives.

use std::cmp::Ordering;

use super::value::{Error, R, Val, eq, strict_eq, utf16_cmp};
use crate::ast::BinaryOp;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Comparator {
    Eq,
    Neq,
    Lt,
    Lte,
    Gt,
    Gte,
    ListContains,
}

/// One filter of a constraint.
#[derive(Debug, Clone)]
pub(crate) struct Simple {
    pub comparator: Comparator,
    pub value: Val,
}

/// `l op r` when either side is a constraint; `None` when neither is, or the operator treats a
/// constraint like any other value.
pub(crate) fn binary(op: BinaryOp, l: &Val, r: &Val) -> Option<R> {
    let comparison = matches!(
        op,
        BinaryOp::Eq | BinaryOp::Ne | BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge
    );
    Some(match (l, r) {
        (Val::Constraint(_), Val::Constraint(_)) if comparison || op == BinaryOp::In => {
            Err(Error::type_error(
                "constraint",
                "bool,duration,float,int,latlng,map,null,string,bytes,timestamp,path,list,\
                 vector,set",
            ))
        }
        (Val::Constraint(c), v) if comparison => implies(c, op, v),
        (v, Val::Constraint(c)) if comparison => implies(c, flip(op), v),
        (v, Val::Constraint(c)) if op == BinaryOp::In => {
            let contains = c
                .iter()
                .any(|s| s.comparator == Comparator::ListContains && strict_eq(&s.value, v));
            if contains {
                Ok(Val::Bool(true))
            } else {
                Err(Error::undecided())
            }
        }
        _ => return None,
    })
}

/// `c.hasAny(items)`.
pub(crate) fn has_any(c: &[Simple], items: &[Val]) -> bool {
    c.iter().any(|s| {
        s.comparator == Comparator::ListContains && items.iter().any(|i| strict_eq(&s.value, i))
    })
}

/// `v op c` as `c op' v`.
fn flip(op: BinaryOp) -> BinaryOp {
    match op {
        BinaryOp::Lt => BinaryOp::Gt,
        BinaryOp::Le => BinaryOp::Ge,
        BinaryOp::Gt => BinaryOp::Lt,
        BinaryOp::Ge => BinaryOp::Le,
        other => other,
    }
}

/// `c op v`: true if one filter of `c` implies it, else undecided.
fn implies(c: &[Simple], op: BinaryOp, v: &Val) -> R {
    let implied = match v {
        Val::Null => op == BinaryOp::Ne,
        _ => c.iter().any(|s| s.implies(op, v)),
    };
    if implied {
        Ok(Val::Bool(true))
    } else {
        Err(Error::undecided())
    }
}

impl Simple {
    /// Whether every value this filter lets through satisfies `x op v`.
    fn implies(&self, op: BinaryOp, v: &Val) -> bool {
        use BinaryOp::{Eq, Ge, Gt, Le, Lt, Ne};
        use Ordering::{Equal, Greater, Less};
        // How the filter's value compares with `v`.
        let Some(order) = relation(&self.value, v) else {
            return false;
        };
        match (self.comparator, order) {
            (Comparator::Eq, Equal) => matches!(op, Eq | Le | Ge),
            (Comparator::Eq, Less) => matches!(op, Ne | Lt | Le),
            (Comparator::Eq, Greater) => matches!(op, Ne | Gt | Ge),
            (Comparator::Neq, Equal) => op == Ne,
            (Comparator::Gt, Equal | Greater) | (Comparator::Gte, Greater) => {
                matches!(op, Gt | Ge | Ne)
            }
            (Comparator::Gte, Equal) => op == Ge,
            (Comparator::Lt, Equal | Less) | (Comparator::Lte, Less) => matches!(op, Lt | Le | Ne),
            (Comparator::Lte, Equal) => op == Le,
            _ => false,
        }
    }
}

/// How a filter's value compares with a rule's, if they compare.
fn relation(filter: &Val, rule: &Val) -> Option<Ordering> {
    match (filter, rule) {
        (Val::Int(a), Val::Int(b)) => Some(a.cmp(b)),
        (Val::Float(a), Val::Float(b)) => a.partial_cmp(b),
        (Val::Int(a), Val::Float(b)) => (*a as f64).partial_cmp(b),
        (Val::Float(_), Val::Int(_)) => None,
        (Val::Str(a), Val::Str(b)) => Some(utf16_cmp(a, b)),
        (Val::Bytes(a), Val::Bytes(b)) => Some(a.cmp(b)),
        (Val::Timestamp(s, n), Val::Timestamp(t, m)) => Some((s, n).cmp(&(t, m))),
        (a, b) if eq(a, b) => Some(Ordering::Equal),
        _ => None,
    }
}
