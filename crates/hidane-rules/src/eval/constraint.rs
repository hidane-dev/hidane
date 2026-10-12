//! What a list knows of a field its query filters on, when the filters do not give its value:
//! a `constraint`, as the official emulator calls it (`tools/oracle/rules_list.py`).
//!
//! A constraint is the field's filters, each a comparator and a value, each once (`n > 0 &&
//! n != 3` is `GT 0`, `NEQ 3`; `not-in [1, 2]` is `NEQ 1`, `NEQ 2`; `is not null` is `NEQ null`;
//! `array-contains 'a'` is `LIST_CONTAINS 'a'`; `super::list` has the filters with null and
//! NaN). A comparison with it is true when one of the filters alone implies it (`GT 5` implies
//! `> 4`, `>= 5.0` and `!= 1`; `GTE 1` and `LTE 1` together do not imply `== 1`), and otherwise
//! an error without a message, which a denial shows as `evaluation error at L5:22`: never
//! false. A filter implies nothing of a value of another type, but:
//!
//! - `!= null` always holds, and `EQ null` implies `== null`;
//! - `false` is less than `true` (`EQ true` implies `!= false`);
//! - comparisons with NaN go as in IEEE 754, where no number is equal to, less than or greater
//!   than NaN: `EQ NaN` lets no value through and so implies any comparison with a number, and a
//!   filter with another number implies `!= NaN` and nothing else of NaN;
//! - a float in a filter does not compare with an int in the rule (`>= 1.5` does not imply
//!   `> 1`) and is never equal to one (`EQ 1.0` implies `!= 1`), though an int in a filter does
//!   compare with a float (`> 5` implies `> 4.5`, `EQ 1` implies `== 1.0`);
//! - `<`, `<=`, `>` and `>=` with a value of a type without an order (a bool, null, a list, a
//!   map, a set, a latlng or a path) are a type error, whatever the filters.
//!
//! `x in c` holds when `c` has `LIST_CONTAINS x`, and `c.hasAny(list)` is whether it has one of
//! the list's items; `c in list` is false, `c is constraint` true, and anything else is the
//! error any other value of a type without that operation gives.

use std::cmp::Ordering;

use super::{
    trace::{observe, shows},
    value::{Error, R, Val, eq, strict_eq, utf16_cmp},
};
use crate::ast::BinaryOp;

/// How a filter of a constraint compares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Comparator {
    Eq,
    Neq,
    Lt,
    Lte,
    Gt,
    Gte,
    ListContains,
}

impl Comparator {
    /// The official name: `EQ`, `NEQ`, `LT`, `LTE`, `GT`, `GTE` or `LIST_CONTAINS`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Eq => "EQ",
            Self::Neq => "NEQ",
            Self::Lt => "LT",
            Self::Lte => "LTE",
            Self::Gt => "GT",
            Self::Gte => "GTE",
            Self::ListContains => "LIST_CONTAINS",
        }
    }
}

/// One filter of a constraint.
#[derive(Debug, Clone)]
pub(crate) struct Simple {
    pub comparator: Comparator,
    pub value: Val,
}

impl Simple {
    /// Whether two filters are one, as the official emulator keeps each filter of a field
    /// once: the same comparator, and values of the same type that are equal (every NaN alike,
    /// `1` apart from `1.0`).
    pub fn same(&self, other: &Self) -> bool {
        self.comparator == other.comparator && shows(&observe(&self.value), &other.value)
    }
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
    let ordered = matches!(
        v,
        Val::Timestamp(..)
            | Val::Duration(_)
            | Val::Float(_)
            | Val::Int(_)
            | Val::Str(_)
            | Val::Bytes(_)
    );
    if matches!(
        op,
        BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge
    ) && !ordered
    {
        return Err(Error::type_error(
            v.type_name(),
            "timestamp,duration,float,int,string,bytes",
        ));
    }
    let implied =
        (op == BinaryOp::Ne && matches!(v, Val::Null)) || c.iter().any(|s| s.implies(op, v));
    if implied {
        Ok(Val::Bool(true))
    } else {
        Err(Error::undecided())
    }
}

fn is_number(v: &Val) -> bool {
    matches!(v, Val::Int(_) | Val::Float(_))
}

fn is_nan(v: &Val) -> bool {
    matches!(v, Val::Float(f) if f.is_nan())
}

impl Simple {
    /// Whether every value this filter lets through satisfies `x op v`.
    fn implies(&self, op: BinaryOp, v: &Val) -> bool {
        use BinaryOp::{Eq, Ge, Gt, Le, Lt, Ne};
        use Ordering::{Equal, Greater, Less};
        if self.comparator == Comparator::ListContains {
            return false;
        }
        if is_number(&self.value) && is_number(v) {
            // `NEQ NaN` lets any number through, and a filter with NaN that compares otherwise
            // none.
            if is_nan(&self.value) {
                return self.comparator != Comparator::Neq || (op == Ne && is_nan(v));
            }
            if is_nan(v) {
                return op == Ne;
            }
            // A float in a filter has no order with an int, and is never equal to one.
            if let (Val::Float(_), Val::Int(_)) = (&self.value, v) {
                return self.comparator == Comparator::Eq && op == Ne;
            }
        }
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
        (Val::Bool(a), Val::Bool(b)) => Some(a.cmp(b)),
        (Val::Str(a), Val::Str(b)) => Some(utf16_cmp(a, b)),
        (Val::Bytes(a), Val::Bytes(b)) => Some(a.cmp(b)),
        (Val::Timestamp(s, n), Val::Timestamp(t, m)) => Some((s, n).cmp(&(t, m))),
        (a, b) if eq(a, b) => Some(Ordering::Equal),
        _ => None,
    }
}
