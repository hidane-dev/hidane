//! What a decision reports besides the decision: the values `debug()` received and, for the
//! coverage report (`crate::coverage`), what each expression evaluated to.

use std::hash::{Hash, Hasher};

use super::{
    Timestamp,
    constraint::Comparator,
    value::{Error, NANOS, Val},
};
use crate::{
    ast::Expr,
    diag::Position,
    paths::{UNKNOWN_ID, UNKNOWN_RUN},
};

/// A value as `debug()` logs it and the coverage report counts it: the official emulator's
/// value message, which also holds what only rules compute (sets, map diffs, constraints) and
/// the path segments a list does not know.
#[derive(Debug, Clone)]
pub enum Observed {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Bytes(Vec<u8>),
    Timestamp(Timestamp),
    /// Seconds and nanoseconds, both of the duration's sign.
    Duration(i64, i32),
    /// Latitude and longitude.
    LatLng(f64, f64),
    Path(Vec<Segment>),
    /// In the map's order.
    Map(Vec<(String, Observed)>),
    List(Vec<Observed>),
    Set(Vec<Observed>),
    /// Every key of the two maps: the other map's, in its order, then those only in this one.
    MapDiff(Vec<(String, KeyStatus)>),
    /// What a list knows of a field: each filter's comparator and value.
    Constraint(Vec<(Comparator, Observed)>),
}

/// A segment of a path: a name, or what a list does not know, one document ID (`capture`) or
/// any number of segments (`glob_capture`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Segment {
    Simple(String),
    Capture,
    GlobCapture,
}

/// What `a.diff(b)` says of a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyStatus {
    /// Only in `a`.
    Added,
    /// Only in `b`.
    Removed,
    Changed,
    Unchanged,
}

impl KeyStatus {
    /// The official name: `ADDED`, `REMOVED`, `CHANGED` or `UNCHANGED`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Added => "ADDED",
            Self::Removed => "REMOVED",
            Self::Changed => "CHANGED",
            Self::Unchanged => "UNCHANGED",
        }
    }
}

/// Values are the same as the official emulator's messages are: floats by their bits (every
/// NaN alike, `-0.0` apart from `0.0`), maps and lists in their order.
impl PartialEq for Observed {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Null, Self::Null) => true,
            (Self::Bool(a), Self::Bool(b)) => a == b,
            (Self::Int(a), Self::Int(b)) => a == b,
            (Self::Float(a), Self::Float(b)) => bits(*a) == bits(*b),
            (Self::String(a), Self::String(b)) => a == b,
            (Self::Bytes(a), Self::Bytes(b)) => a == b,
            (Self::Timestamp(a), Self::Timestamp(b)) => a == b,
            (Self::Duration(a, b), Self::Duration(c, d)) => a == c && b == d,
            (Self::LatLng(a, b), Self::LatLng(c, d)) => {
                bits(*a) == bits(*c) && bits(*b) == bits(*d)
            }
            (Self::Path(a), Self::Path(b)) => a == b,
            (Self::Map(a), Self::Map(b)) => a == b,
            (Self::List(a), Self::List(b)) | (Self::Set(a), Self::Set(b)) => a == b,
            (Self::MapDiff(a), Self::MapDiff(b)) => a == b,
            (Self::Constraint(a), Self::Constraint(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for Observed {}

impl Hash for Observed {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            Self::Null => {}
            Self::Bool(b) => b.hash(state),
            Self::Int(i) => i.hash(state),
            Self::Float(f) => bits(*f).hash(state),
            Self::String(s) => s.hash(state),
            Self::Bytes(b) => b.hash(state),
            Self::Timestamp(t) => t.hash(state),
            Self::Duration(s, n) => (s, n).hash(state),
            Self::LatLng(a, b) => (bits(*a), bits(*b)).hash(state),
            Self::Path(p) => p.hash(state),
            Self::Map(m) => m.hash(state),
            Self::List(items) | Self::Set(items) => items.hash(state),
            Self::MapDiff(d) => d.hash(state),
            Self::Constraint(c) => c.hash(state),
        }
    }
}

fn bits(f: f64) -> u64 {
    if f.is_nan() {
        f64::NAN.to_bits()
    } else {
        f.to_bits()
    }
}

/// What an expression of the coverage report evaluated to once.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Outcome {
    Value(Observed),
    /// An evaluation error: where the official emulator places it, and its text. A comparison
    /// a list's filter does not decide has neither.
    Error {
        position: Option<Position>,
        message: String,
    },
    /// Not evaluated: the other side of `&&` or `||` decided, or `?:` took the other branch.
    Skipped,
}

/// Which expression of the coverage report a value belongs to: an expression by its byte range,
/// or the body of a function from one of its `let`s (by the keyword's offset).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum NodeKey {
    Expr(usize, usize),
    Body(usize),
}

impl NodeKey {
    pub fn of(e: &Expr) -> Self {
        Self::Expr(e.span.start, e.span.end)
    }
}

/// An expression's value as the evaluator had it.
#[derive(Clone)]
pub(crate) enum Traced {
    Value(Val),
    Error(Error),
    Skipped,
}

/// What one decision reports besides the decision: the value of each `debug()` call, in order,
/// and, when values are traced ([`Trace::with_values`]), what each expression of the coverage
/// report evaluated to, for [`crate::coverage::Coverage::record`].
#[derive(Default)]
pub struct Trace {
    pub(crate) debug: Vec<Observed>,
    pub(crate) values: Option<Vec<(NodeKey, Traced)>>,
}

impl Trace {
    /// A trace of `debug()` alone.
    pub fn new() -> Self {
        Self::default()
    }

    /// A trace of `debug()` and of every expression's value.
    pub fn with_values() -> Self {
        Self {
            debug: Vec::new(),
            values: Some(Vec::new()),
        }
    }

    /// The values `debug()` received, in order.
    pub fn debug(&self) -> &[Observed] {
        &self.debug
    }

    pub(crate) fn tracing(&self) -> bool {
        self.values.is_some()
    }

    pub(crate) fn record(&mut self, key: NodeKey, result: &Result<Val, Error>) {
        if let Some(values) = &mut self.values {
            values.push((
                key,
                match result {
                    Ok(v) => Traced::Value(v.clone()),
                    Err(e) => Traced::Error(e.clone()),
                },
            ));
        }
    }

    pub(crate) fn skip(&mut self, key: NodeKey) {
        if let Some(values) = &mut self.values {
            values.push((key, Traced::Skipped));
        }
    }

    /// Where the values traced next start.
    pub(crate) fn mark(&self) -> usize {
        self.values.as_ref().map_or(0, Vec::len)
    }

    /// Counts what each statement traced again for each statement evaluated after it: the
    /// official emulator evaluates the statements of a method as a growing `||` of them, the
    /// first alone, then the first two, and so on, and counts every evaluation (but runs each
    /// `debug()` once). `marks` holds where each statement's values start, in order.
    pub(crate) fn repeat(&mut self, marks: &[usize]) {
        let Some(values) = &mut self.values else {
            return;
        };
        let end = values.len();
        for (i, &start) in marks.iter().enumerate() {
            let stop = marks.get(i + 1).copied().unwrap_or(end);
            for _ in i + 1..marks.len() {
                for j in start..stop {
                    let again = values[j].clone();
                    values.push(again);
                }
            }
        }
    }
}

/// A value as `debug()` and the coverage report show it.
pub(crate) fn observe(v: &Val) -> Observed {
    match v {
        // What reading it would leave undecided only shows inside a map.
        Val::Null | Val::Unknown => Observed::Null,
        Val::Bool(b) => Observed::Bool(*b),
        Val::Int(i) => Observed::Int(*i),
        Val::Float(f) => Observed::Float(*f),
        Val::Str(s) => Observed::String(s.to_string()),
        Val::Bytes(b) => Observed::Bytes(b.to_vec()),
        Val::List(items) => Observed::List(items.iter().map(observe).collect()),
        Val::Set(items) => Observed::Set(items.iter().map(observe).collect()),
        Val::Map(map) => Observed::Map(map.iter().map(|(k, v)| (k.clone(), observe(v))).collect()),
        Val::MapDiff(diff) => Observed::MapDiff(diff.statuses.clone()),
        Val::Timestamp(seconds, nanos) => Observed::Timestamp(Timestamp {
            seconds: *seconds,
            nanos: i32::try_from(*nanos).unwrap_or(0),
        }),
        Val::Duration(nanos) => Observed::Duration(
            i64::try_from(nanos / NANOS).unwrap_or(0),
            i32::try_from(nanos % NANOS).unwrap_or(0),
        ),
        Val::LatLng(lat, lng) => Observed::LatLng(*lat, *lng),
        Val::Path(segments) => Observed::Path(
            segments
                .iter()
                .map(|s| match s.as_str() {
                    UNKNOWN_ID => Segment::Capture,
                    UNKNOWN_RUN => Segment::GlobCapture,
                    s => Segment::Simple(s.to_owned()),
                })
                .collect(),
        ),
        Val::Constraint(simple) => Observed::Constraint(
            simple
                .iter()
                .map(|s| (s.comparator, observe(&s.value)))
                .collect(),
        ),
    }
}
