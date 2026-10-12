//! The values rules compute with, their type names, equality and the official error texts.

use std::rc::Rc;

use indexmap::IndexMap;

use super::{Document, Timestamp, Value};

/// A value while a condition runs. Containers are shared, so reading `resource.data` does not
/// copy the document.
#[derive(Debug, Clone)]
pub(crate) enum Val {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(Rc<str>),
    Bytes(Rc<[u8]>),
    List(Rc<[Val]>),
    Map(Rc<IndexMap<String, Val>>),
    /// Distinct items (by [`loose_eq`]), in the order they were first added.
    Set(Rc<[Val]>),
    MapDiff(Rc<MapDiff>),
    /// Seconds since the epoch and nanoseconds in `0..1e9`.
    Timestamp(i64, u32),
    /// Nanoseconds.
    Duration(i128),
    LatLng(f64, f64),
    Path(Rc<[String]>),
    /// What a list knows of a field its query filters on (`super::constraint`).
    Constraint(Rc<[super::constraint::Simple]>),
    /// What the official emulator cannot evaluate, such as a collection group's `request.path`:
    /// reading it from its map is undecided.
    Unknown,
}

/// The keys `a.diff(b)` sorts out: in `a` only, in `b` only, in both with other values, in
/// both with equal values; and every key with its status in the order the official emulator
/// shows them, `b`'s keys first.
#[derive(Debug)]
pub(crate) struct MapDiff {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<String>,
    pub unchanged: Vec<String>,
    pub statuses: Vec<(String, super::trace::KeyStatus)>,
}

/// An evaluation error, with the official emulator's text and, once known, the byte offset of
/// the token the coverage report points it at (`super::interp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Error {
    pub message: String,
    pub at: Option<usize>,
}

pub(crate) type R = Result<Val, Error>;

pub(crate) const NANOS: i128 = 1_000_000_000;
pub(crate) const MIN_SECONDS: i64 = -62_135_596_800;
pub(crate) const MAX_SECONDS: i64 = 253_402_300_799;
const MAX_DURATION_SECONDS: i128 = 315_576_000_000;

impl Error {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            at: None,
        }
    }

    /// The error, placed at the token starting at byte `at` unless it already has a place. An
    /// error without a message has none.
    pub fn at(mut self, at: usize) -> Self {
        if self.at.is_none() && !self.message.is_empty() {
            self.at = Some(at);
        }
        self
    }

    /// An error without a message: a comparison with a `constraint` that its filters do not
    /// imply. A denial shows it as `evaluation error at L5:22` (the condition's start).
    pub fn undecided() -> Self {
        Self::new(String::new())
    }

    pub fn null() -> Self {
        Self::new("Null value error.".to_owned())
    }

    /// Indexing a path that holds a list's unknown document ID (`paths::UNKNOWN_ID`).
    pub fn unbound_template() -> Self {
        Self::new("Variable  is not bound in path template.".to_owned())
    }

    pub fn property(name: &str) -> Self {
        Self::new(format!("Property {name} is undefined on object."))
    }

    pub fn type_error(received: &str, expected: &str) -> Self {
        Self::new(format!(
            "Type error. Received: [{received}] Expected: [{expected}]."
        ))
    }

    pub fn unsupported(received: &str, expected: &[String]) -> Self {
        Self::new(format!(
            "Unsupported operation error. Received: {received}. Expected: {}.",
            expected.join(", ")
        ))
    }

    pub fn not_found(name: &str) -> Self {
        Self::new(format!("Function not found error: Name: [{name}]."))
    }

    pub fn arity(received: usize, expected: &[String]) -> Self {
        Self::new(format!(
            "Incorrect number of arguments. Received: {received}. Expected: {}.",
            expected.join(", ")
        ))
    }

    pub fn index(index: i64, size: usize) -> Self {
        Self::new(format!(
            "Index out of bound error. Index: [{index}] , size: [{size}]."
        ))
    }

    pub fn range(from: i64, to: i64) -> Self {
        Self::new(format!(
            "Illegal range error. From index: [{from}] , To index: [{to}]."
        ))
    }

    pub fn overflow() -> Self {
        Self::new(
            "Integer value is out of the [-9223372036854775808, 9223372036854775807] range."
                .to_owned(),
        )
    }

    pub fn divide_by_zero() -> Self {
        Self::new("Divide by zero error.".to_owned())
    }

    pub fn conversion(argument: &str, from: &str, to: &str) -> Self {
        Self::new(format!(
            "Type conversion error. Argument: [{argument}], From type: [{from}], To type: [{to}]."
        ))
    }

    pub fn regex(pattern: &str) -> Self {
        Self::new(format!(
            "Invalid regular expression pattern. Pattern: {pattern}."
        ))
    }

    pub fn timestamp() -> Self {
        Self::new(
            "Timestamp is not valid. Seconds must be in range [-62,135,596,800, \
             +253,402,300,799]. Nanos must be in range [0, +999,999,999]."
                .to_owned(),
        )
    }

    pub fn duration() -> Self {
        Self::new(
            "Duration is not valid. Seconds must be in range [-315,576,000,000, \
             +315,576,000,000]. Nanos must be in range [-999,999,999, +999,999,999]. Nanos must \
             have the same sign as seconds."
                .to_owned(),
        )
    }

    /// A lookup the rules may not make: past the budget, or of a path that is not a document
    /// of this database. The path is printed as a protobuf text message.
    pub fn service(function: &str, path: &[String]) -> Self {
        let mut argument = "path_value {\n".to_owned();
        for segment in path {
            argument.push_str("  segments {\n    simple: \"");
            argument.push_str(&text_format_escape(segment));
            argument.push_str("\"\n  }\n");
        }
        argument.push_str("}\n");
        Self::new(format!(
            "Service call error. Function: [{function}], Argument: [{argument}]."
        ))
    }
}

/// A string as protobuf's text format quotes it: C escapes, and octal for other bytes outside
/// printable ASCII.
fn text_format_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'\x07' => out.push_str("\\a"),
            b'\x08' => out.push_str("\\b"),
            b'\x0c' => out.push_str("\\f"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            b'\x0b' => out.push_str("\\v"),
            b'\\' => out.push_str("\\\\"),
            b'\'' => out.push_str("\\'"),
            b'"' => out.push_str("\\\""),
            0x20..0x7f => out.push(char::from(b)),
            _ => out.push_str(&format!("\\{b:03o}")),
        }
    }
    out
}

impl Val {
    pub fn str(s: &str) -> Self {
        Self::Str(Rc::from(s))
    }

    pub fn string(s: String) -> Self {
        Self::Str(Rc::from(s))
    }

    pub fn list(items: Vec<Val>) -> Self {
        Self::List(Rc::from(items))
    }

    pub fn path(segments: Vec<String>) -> Self {
        Self::Path(Rc::from(segments))
    }

    /// The type's name in the official messages and for `is`.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Bool(_) => "bool",
            Self::Int(_) => "int",
            Self::Float(_) => "float",
            Self::Str(_) => "string",
            Self::Bytes(_) => "bytes",
            Self::List(_) => "list",
            Self::Map(_) => "map",
            Self::Set(_) => "set",
            Self::MapDiff(_) => "map_diff",
            Self::Timestamp(..) => "timestamp",
            Self::Duration(_) => "duration",
            Self::LatLng(..) => "latlng",
            Self::Path(_) => "path",
            Self::Constraint(_) => "constraint",
            Self::Unknown => "unknown",
        }
    }

    /// `value is type`. A set is never `set` (as measured), and nothing is a `vector`.
    pub fn is(&self, type_name: &str) -> bool {
        match type_name {
            "number" => matches!(self, Self::Int(_) | Self::Float(_)),
            "set" | "vector" => false,
            name => self.type_name() == name,
        }
    }

    /// A timestamp, or the official error outside years 1 to 9999.
    pub fn timestamp(seconds: i64, nanos: u32) -> R {
        if (MIN_SECONDS..=MAX_SECONDS).contains(&seconds) {
            Ok(Self::Timestamp(seconds, nanos))
        } else {
            Err(Error::timestamp())
        }
    }

    /// A timestamp from nanoseconds since the epoch.
    pub fn timestamp_nanos(nanos: i128) -> R {
        let seconds = nanos.div_euclid(NANOS);
        let seconds = i64::try_from(seconds).map_err(|_| Error::timestamp())?;
        Self::timestamp(seconds, nanos.rem_euclid(NANOS) as u32)
    }

    /// A duration, or the official error beyond 10,000 years either way.
    pub fn duration(nanos: i128) -> R {
        if (nanos / NANOS).abs() <= MAX_DURATION_SECONDS {
            Ok(Self::Duration(nanos))
        } else {
            Err(Error::duration())
        }
    }
}

/// `==` in a condition: an int equals a float of the same value, but inside lists and maps the
/// types must match (`[1] == [1.0]` is false). Values of different types are not equal.
pub(crate) fn eq(a: &Val, b: &Val) -> bool {
    match (a, b) {
        (Val::Int(x), Val::Float(y)) | (Val::Float(y), Val::Int(x)) => (*x as f64) == *y,
        _ => strict_eq(a, b),
    }
}

/// Equality without numeric conversion, as lists, maps, `in` and the list methods use it.
pub(crate) fn strict_eq(a: &Val, b: &Val) -> bool {
    same(a, b, strict_eq)
}

/// Equality where numbers compare by value at any depth, as sets and map diffs use it
/// (`[1].toSet() == [1.0].toSet()`).
pub(crate) fn loose_eq(a: &Val, b: &Val) -> bool {
    match (a, b) {
        (Val::Int(x), Val::Float(y)) | (Val::Float(y), Val::Int(x)) => (*x as f64) == *y,
        _ => same(a, b, loose_eq),
    }
}

fn same(a: &Val, b: &Val, inner: fn(&Val, &Val) -> bool) -> bool {
    match (a, b) {
        (Val::Null, Val::Null) => true,
        (Val::Bool(x), Val::Bool(y)) => x == y,
        (Val::Int(x), Val::Int(y)) => x == y,
        (Val::Float(x), Val::Float(y)) => x == y,
        (Val::Str(x), Val::Str(y)) => x == y,
        (Val::Bytes(x), Val::Bytes(y)) => x == y,
        (Val::List(x), Val::List(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(a, b)| inner(a, b))
        }
        (Val::Map(x), Val::Map(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| inner(v, w)))
        }
        (Val::Set(x), Val::Set(y)) => {
            x.len() == y.len() && x.iter().all(|a| y.iter().any(|b| loose_eq(a, b)))
        }
        (Val::Timestamp(s, n), Val::Timestamp(t, m)) => s == t && n == m,
        (Val::Duration(x), Val::Duration(y)) => x == y,
        (Val::LatLng(a, b), Val::LatLng(c, d)) => a == c && b == d,
        (Val::Path(x), Val::Path(y)) => x == y,
        // Two map diffs are never equal.
        _ => false,
    }
}

/// Whether `items` holds `v`, by `eq`.
pub(crate) fn contains(items: &[Val], v: &Val, eq: fn(&Val, &Val) -> bool) -> bool {
    items.iter().any(|x| eq(x, v))
}

/// The distinct items of `items`, the first of equal ones kept.
pub(crate) fn to_set(items: impl IntoIterator<Item = Val>) -> Val {
    let mut out: Vec<Val> = Vec::new();
    for item in items {
        if !contains(&out, &item, loose_eq) {
            out.push(item);
        }
    }
    Val::Set(Rc::from(out))
}

impl From<&Value> for Val {
    fn from(v: &Value) -> Self {
        match v {
            Value::Null => Self::Null,
            Value::Bool(b) => Self::Bool(*b),
            Value::Int(i) => Self::Int(*i),
            Value::Float(f) => Self::Float(*f),
            Value::String(s) => Self::str(s),
            Value::Bytes(b) => Self::Bytes(Rc::from(b.as_slice())),
            Value::Timestamp(Timestamp { seconds, nanos }) => {
                let total = i128::from(*seconds) * NANOS + i128::from(*nanos);
                Self::Timestamp(
                    total.div_euclid(NANOS) as i64,
                    total.rem_euclid(NANOS) as u32,
                )
            }
            Value::Duration(seconds, nanos) => {
                Self::Duration(i128::from(*seconds) * NANOS + i128::from(*nanos))
            }
            Value::LatLng(lat, lng) => Self::LatLng(*lat, *lng),
            Value::Path(segments) => Self::Path(Rc::from(segments.as_slice())),
            Value::List(items) => Self::List(items.iter().map(Self::from).collect()),
            Value::Map(map) => Self::Map(Rc::new(
                map.iter()
                    .map(|(k, v)| (k.clone(), Self::from(v)))
                    .collect(),
            )),
        }
    }
}

/// A document as `resource` and `get()` give it: `__name__`, `data` and `id`.
pub(crate) fn resource(doc: &Document) -> Val {
    let mut map = IndexMap::with_capacity(3);
    map.insert("__name__".to_owned(), Val::path(doc.path.clone()));
    map.insert(
        "data".to_owned(),
        Val::Map(Rc::new(
            doc.data
                .iter()
                .map(|(k, v)| (k.clone(), Val::from(v)))
                .collect(),
        )),
    );
    map.insert(
        "id".to_owned(),
        Val::str(doc.path.last().map_or("", String::as_str)),
    );
    Val::Map(Rc::new(map))
}

/// A float as Java's `Double.toString` writes it: `1.0`, `0.001`, `1.0E7`, `1.0E-4`,
/// `Infinity`, `NaN`, with the shortest digits that read back the same.
pub(crate) fn java_double(f: f64) -> String {
    if f.is_nan() {
        return "NaN".to_owned();
    }
    if f.is_infinite() {
        return if f > 0.0 { "Infinity" } else { "-Infinity" }.to_owned();
    }
    if f == 0.0 {
        return if f.is_sign_negative() { "-0.0" } else { "0.0" }.to_owned();
    }
    let sign = if f < 0.0 { "-" } else { "" };
    // `{:e}` gives the shortest digits: `1.2345e-7`.
    let sci = format!("{:e}", f.abs());
    let (mantissa, exponent) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let abs = f.abs();
    if (1e-3..1e7).contains(&abs) {
        let point = exponent + 1;
        let (int, frac) = if point <= 0 {
            (
                "0".to_owned(),
                format!("{}{digits}", "0".repeat(point.unsigned_abs() as usize)),
            )
        } else {
            let point = point as usize;
            let padded = format!("{digits:0<point$}");
            (padded[..point].to_owned(), padded[point..].to_owned())
        };
        let frac = if frac.is_empty() {
            "0".to_owned()
        } else {
            frac
        };
        format!("{sign}{int}.{frac}")
    } else {
        let (first, rest) = digits.split_at(1);
        let rest = if rest.is_empty() { "0" } else { rest };
        format!("{sign}{first}.{rest}E{exponent}")
    }
}

/// Java's `Double.parseDouble`: surrounding control characters and spaces are ignored, and
/// `NaN`, `Infinity` and a trailing `d` or `f` are accepted.
pub(crate) fn parse_java_double(s: &str) -> Option<f64> {
    let t = s.trim_matches(|c: char| c <= ' ');
    let (sign, body) = match t.as_bytes().first() {
        Some(b'-') => (-1.0, &t[1..]),
        Some(b'+') => (1.0, &t[1..]),
        _ => (1.0, t),
    };
    match body {
        "NaN" => return Some(f64::NAN),
        "Infinity" => return Some(sign * f64::INFINITY),
        _ => {}
    }
    let body = body.strip_suffix(['d', 'D', 'f', 'F']).unwrap_or(body);
    let bytes = body.as_bytes();
    let mut i = 0;
    let digits = |i: &mut usize| {
        let start = *i;
        while *i < bytes.len() && bytes[*i].is_ascii_digit() {
            *i += 1;
        }
        *i - start
    };
    let mut count = digits(&mut i);
    if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
        count += digits(&mut i);
    }
    if count == 0 {
        return None;
    }
    if i < bytes.len() && matches!(bytes[i], b'e' | b'E') {
        i += 1;
        if i < bytes.len() && matches!(bytes[i], b'+' | b'-') {
            i += 1;
        }
        if digits(&mut i) == 0 {
            return None;
        }
    }
    if i != bytes.len() {
        return None;
    }
    body.parse::<f64>().ok().map(|f| sign * f)
}

/// Java's `Long.parseLong`: an optional sign and decimal digits.
pub(crate) fn parse_java_long(s: &str) -> Option<i64> {
    let digits = s.strip_prefix(['+', '-']).unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.strip_prefix('+').unwrap_or(s).parse().ok()
}

/// A string's length in UTF-16 code units, as Java counts it.
pub(crate) fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// Code units `from..to` of `s`; a surrogate pair cut in half becomes U+FFFD.
pub(crate) fn utf16_slice(s: &str, from: usize, to: usize) -> String {
    let units: Vec<u16> = s.encode_utf16().collect();
    String::from_utf16_lossy(&units[from..to])
}

/// Java's `String.compareTo`: UTF-16 code unit order.
pub(crate) fn utf16_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

/// Whether a path holds a list's unknown parts (`crate::paths::UNKNOWN_ID`, `UNKNOWN_RUN`).
pub(crate) fn has_unknown(segments: &[String]) -> bool {
    segments
        .iter()
        .any(|s| s == crate::paths::UNKNOWN_ID || s == crate::paths::UNKNOWN_RUN)
}

/// A map's value as read: [`Val::Unknown`] is undecided.
pub(crate) fn read(v: Option<&Val>) -> Option<R> {
    v.map(|v| match v {
        Val::Unknown => Err(Error::undecided()),
        v => Ok(v.clone()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn java_doubles() {
        for (f, s) in [
            (1.0, "1.0"),
            (1.5, "1.5"),
            (-2.25, "-2.25"),
            (1_000_000.0, "1000000.0"),
            (10_000_000.0, "1.0E7"),
            (100_000_000.0, "1.0E8"),
            (123_456_789.0, "1.23456789E8"),
            (0.001, "0.001"),
            (0.0001, "1.0E-4"),
            (0.000_123, "1.23E-4"),
            (-0.0, "-0.0"),
            (0.1 + 0.2, "0.30000000000000004"),
            (1e300, "1.0E300"),
            (f64::NAN, "NaN"),
            (f64::NEG_INFINITY, "-Infinity"),
            (9_223_372_036_854_775_807.0, "9.223372036854776E18"),
        ] {
            assert_eq!(java_double(f), s, "{f}");
        }
    }

    #[test]
    fn java_parsing() {
        assert_eq!(parse_java_double(" 1.5 "), Some(1.5));
        assert_eq!(parse_java_double("1e3"), Some(1000.0));
        assert_eq!(parse_java_double("-.5"), Some(-0.5));
        assert_eq!(parse_java_double("2."), Some(2.0));
        assert_eq!(parse_java_double("1.5d"), Some(1.5));
        assert_eq!(parse_java_double("-Infinity"), Some(f64::NEG_INFINITY));
        assert!(parse_java_double("NaN").unwrap().is_nan());
        for bad in ["inf", "abc", "", ".", "1e", "1.5x", "0x10", "nan"] {
            assert_eq!(parse_java_double(bad), None, "{bad}");
        }
        assert_eq!(parse_java_long("+12"), Some(12));
        assert_eq!(parse_java_long("-12"), Some(-12));
        for bad in [" 12", "1.5", "", "+", "9223372036854775808", "0x10"] {
            assert_eq!(parse_java_long(bad), None, "{bad}");
        }
    }

    #[test]
    fn equalities() {
        let list = |items: Vec<Val>| Val::list(items);
        assert!(eq(&Val::Int(1), &Val::Float(1.0)));
        assert!(!eq(&list(vec![Val::Int(1)]), &list(vec![Val::Float(1.0)])));
        assert!(loose_eq(
            &list(vec![Val::Int(1)]),
            &list(vec![Val::Float(1.0)])
        ));
        assert!(!eq(&Val::str("1"), &Val::Int(1)));
        assert!(!eq(&Val::Null, &Val::Bool(false)));
        assert!(!eq(&Val::Float(f64::NAN), &Val::Float(f64::NAN)));
    }

    #[test]
    fn service_errors_print_the_path_as_text() {
        let path: Vec<String> = ["databases", "a\"b", "é"].map(String::from).to_vec();
        assert_eq!(
            Error::service("get", &path).message,
            "Service call error. Function: [get], Argument: [path_value {\n  segments {\n    \
             simple: \"databases\"\n  }\n  segments {\n    simple: \"a\\\"b\"\n  }\n  segments {\n    \
             simple: \"\\303\\251\"\n  }\n}\n]."
        );
    }
}
