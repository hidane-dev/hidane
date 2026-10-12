//! What a decision reports besides the decision: the values `debug()` received and, for the
//! coverage report (`crate::coverage`), what each expression evaluated to.

use std::hash::{Hash, Hasher};

use super::{
    Method, Timestamp,
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
    /// What the official emulator does not know, such as a collection group's `request.path`
    /// (`undefined`).
    Undefined,
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
            (Self::Null, Self::Null) | (Self::Undefined, Self::Undefined) => true,
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

fn bits(f: f64) -> u64 {
    if f.is_nan() {
        f64::NAN.to_bits()
    } else {
        f.to_bits()
    }
}

/// Hashes `v` as what it shows ([`observe`]), without building that.
pub(crate) fn hash_val<H: Hasher>(v: &Val, state: &mut H) {
    match v {
        Val::Null => 0u8.hash(state),
        Val::Unknown => 15u8.hash(state),
        Val::Bool(b) => (1u8, b).hash(state),
        Val::Int(i) => (2u8, i).hash(state),
        Val::Float(f) => (3u8, bits(*f)).hash(state),
        Val::Str(s) => (4u8, &**s).hash(state),
        Val::Bytes(b) => (5u8, &**b).hash(state),
        Val::Timestamp(seconds, nanos) => (6u8, seconds, nanos).hash(state),
        Val::Duration(nanos) => (7u8, nanos).hash(state),
        Val::LatLng(lat, lng) => (8u8, bits(*lat), bits(*lng)).hash(state),
        Val::Path(segments) => (9u8, &**segments).hash(state),
        Val::Map(map) => {
            (10u8, map.len()).hash(state);
            for (k, v) in map.iter() {
                k.hash(state);
                hash_val(v, state);
            }
        }
        Val::List(items) | Val::Set(items) => {
            (
                if matches!(v, Val::Set(_)) { 12u8 } else { 11u8 },
                items.len(),
            )
                .hash(state);
            for item in items.iter() {
                hash_val(item, state);
            }
        }
        Val::MapDiff(diff) => (13u8, &diff.statuses).hash(state),
        Val::Constraint(simple) => {
            (14u8, simple.len()).hash(state);
            for s in simple.iter() {
                s.comparator.hash(state);
                hash_val(&s.value, state);
            }
        }
    }
}

/// Whether `o` is what `v` shows ([`observe`]).
pub(crate) fn shows(o: &Observed, v: &Val) -> bool {
    match (o, v) {
        (Observed::Null, Val::Null) | (Observed::Undefined, Val::Unknown) => true,
        (Observed::Bool(a), Val::Bool(b)) => a == b,
        (Observed::Int(a), Val::Int(b)) => a == b,
        (Observed::Float(a), Val::Float(b)) => bits(*a) == bits(*b),
        (Observed::String(a), Val::Str(b)) => a.as_str() == &**b,
        (Observed::Bytes(a), Val::Bytes(b)) => a.as_slice() == &**b,
        (Observed::Timestamp(t), Val::Timestamp(seconds, nanos)) => {
            t.seconds == *seconds && i64::from(t.nanos) == i64::from(*nanos)
        }
        (Observed::Duration(..), Val::Duration(_)) | (Observed::Path(_), Val::Path(_)) => {
            *o == observe(v)
        }
        (Observed::LatLng(a, b), Val::LatLng(c, d)) => bits(*a) == bits(*c) && bits(*b) == bits(*d),
        (Observed::Map(a), Val::Map(b)) => {
            a.len() == b.len()
                && a.iter()
                    .zip(b.iter())
                    .all(|((k, x), (l, y))| k == l && shows(x, y))
        }
        (Observed::List(a), Val::List(b)) | (Observed::Set(a), Val::Set(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| shows(x, y))
        }
        (Observed::MapDiff(a), Val::MapDiff(b)) => *a == b.statuses,
        (Observed::Constraint(a), Val::Constraint(b)) => {
            a.len() == b.len()
                && a.iter()
                    .zip(b.iter())
                    .all(|((c, x), s)| *c == s.comparator && shows(x, &s.value))
        }
        _ => false,
    }
}

/// What an expression of the coverage report evaluated to once.
#[derive(Debug, Clone, PartialEq, Eq)]
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

/// What an `allow` statement's condition came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    False,
    Error,
}

/// One evaluation of the statements of a method, as the Emulator UI's request monitor shows it
/// ([`Trace::with_evaluations`]): what `request` and `resource` held, and what each statement
/// evaluated came to, in order, by the line of its condition (or of the statement, without
/// one). A request evaluates the statements of one method, of both for an upsert (`create`'s
/// first), and of each way a list's filter may be satisfied; none when no statement applies.
#[derive(Debug, Clone)]
pub struct Evaluation {
    pub method: Method,
    pub request: Observed,
    pub resource: Observed,
    pub statements: Vec<(u32, Verdict)>,
}

/// What one decision reports besides the decision: the value of each `debug()` call, in order;
/// when values are traced ([`Trace::with_values`]), what each expression of the coverage report
/// evaluated to, for [`crate::coverage::Coverage::record`]; and when evaluations are
/// ([`Trace::with_evaluations`]), each [`Evaluation`].
#[derive(Default)]
pub struct Trace {
    pub(crate) debug: Vec<Observed>,
    pub(crate) values: Option<Vec<(NodeKey, Traced)>>,
    pub(crate) evaluations: Option<Vec<Evaluation>>,
}

impl Trace {
    /// A trace of `debug()` alone.
    pub fn new() -> Self {
        Self::default()
    }

    /// A trace of `debug()` and of every expression's value.
    pub fn with_values() -> Self {
        Self {
            values: Some(Vec::new()),
            ..Self::default()
        }
    }

    /// This trace, also reporting each [`Evaluation`].
    #[must_use]
    pub fn with_evaluations(mut self) -> Self {
        self.evaluations = Some(Vec::new());
        self
    }

    /// The values `debug()` received, in order.
    pub fn debug(&self) -> &[Observed] {
        &self.debug
    }

    /// The evaluations, in order, if they are reported.
    pub fn evaluations(&self) -> &[Evaluation] {
        self.evaluations.as_deref().unwrap_or_default()
    }

    /// Starts an evaluation of `method`'s statements with `request` and `resource`; returns
    /// where its verdicts go.
    pub(crate) fn evaluating(
        &mut self,
        method: Method,
        request: &Val,
        resource: &Val,
    ) -> Option<usize> {
        let evaluations = self.evaluations.as_mut()?;
        evaluations.push(Evaluation {
            method,
            request: observe(request),
            resource: observe(resource),
            statements: Vec::new(),
        });
        Some(evaluations.len() - 1)
    }

    /// Records what the statement whose condition is at `line` came to, in evaluation `at`.
    pub(crate) fn verdict(&mut self, at: Option<usize>, line: u32, verdict: Verdict) {
        if let (Some(at), Some(evaluations)) = (at, &mut self.evaluations) {
            evaluations[at].statements.push((line, verdict));
        }
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

    /// Counts what each statement traced again for each statement evaluated after it, as the
    /// official emulator's report counts them: as if it evaluated a growing `||` of the
    /// statements, the first alone, then the first two, and so on (but it runs each `debug()`
    /// once). `marks` holds where each statement's values start, in order.
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
        Val::Null => Observed::Null,
        // What reading it would leave undecided only shows inside a map.
        Val::Unknown => Observed::Undefined,
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
