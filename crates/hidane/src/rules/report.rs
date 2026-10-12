//! What `GET :securityRules` and `GET :ruleCoverage` answer and what `debug()` prints, in the
//! official emulator's formats (`tests/fixtures/rules_coverage.json`).
//!
//! - The JSON is protobuf-java's JsonFormat: two spaces of indentation, `[{` and `}, {` between
//!   the messages of a list, empty strings and zero numbers left out (but not a value's own
//!   field), 64-bit integers as strings, doubles as Java prints them. In the HTML report the
//!   same JSON also escapes `<`, `>`, `&`, `=` and `'`, as Gson does in a page.
//! - `debug(x)` prints the value message in protobuf's text format, with a tab after the first
//!   field's name (`int_value: \t 2`, `map_value \t {`), then an empty line. Map entries come
//!   sorted by key.

use std::fmt::Write as _;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use hidane_rules::{
    Position,
    coverage::Report,
    eval::{Observed, Outcome, Segment, java_double},
};

use super::{File, json_string};

/// A JSON value, printed as JsonFormat prints it.
enum Json {
    /// A number, a bool or null, as written.
    Raw(String),
    Str(String),
    Array(Vec<Json>),
    Object(Vec<(&'static str, Json)>),
    /// An object with keys of the data (a map's).
    Map(Vec<(String, Json)>),
}

impl Json {
    fn write(&self, out: &mut String, indent: usize, html: bool) {
        match self {
            Self::Raw(text) => out.push_str(text),
            Self::Str(s) => out.push_str(&string(s, html)),
            Self::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    item.write(out, indent, html);
                }
                out.push(']');
            }
            Self::Object(fields) => {
                let fields = fields.iter().map(|(k, v)| (*k, v));
                object(out, fields, indent, html);
            }
            Self::Map(fields) => {
                let fields = fields.iter().map(|(k, v)| (k.as_str(), v));
                object(out, fields, indent, html);
            }
        }
    }
}

fn object<'a>(
    out: &mut String,
    fields: impl Iterator<Item = (&'a str, &'a Json)>,
    indent: usize,
    html: bool,
) {
    out.push_str("{\n");
    let mut empty = true;
    for (key, value) in fields {
        if !empty {
            out.push_str(",\n");
        }
        empty = false;
        let _ = write!(
            out,
            "{:width$}{}: ",
            "",
            string(key, html),
            width = indent + 2
        );
        value.write(out, indent + 2, html);
    }
    if !empty {
        out.push('\n');
    }
    let _ = write!(out, "{:indent$}}}", "");
}

fn string(s: &str, html: bool) -> String {
    let json = json_string(s);
    if !html {
        return json;
    }
    let mut out = String::with_capacity(json.len());
    for c in json.chars() {
        match c {
            '<' | '>' | '&' | '=' | '\'' => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out
}

fn render(json: &Json, html: bool) -> String {
    let mut out = String::new();
    json.write(&mut out, 0, html);
    out
}

/// `{rules: {files: […]}}`, the files as they were loaded.
fn rules(files: &[File]) -> (&'static str, Json) {
    let files = files
        .iter()
        .map(|file| {
            let mut fields = vec![("content", Json::Str(file.content.clone()))];
            if let Some(name) = file.name.as_ref().filter(|n| !n.is_empty()) {
                fields.push(("name", Json::Str(name.clone())));
            }
            if !file.fingerprint.is_empty() {
                fields.push(("fingerprint", Json::Str(STANDARD.encode(&file.fingerprint))));
            }
            Json::Object(fields)
        })
        .collect();
    ("rules", Json::Object(vec![("files", Json::Array(files))]))
}

/// The body of `GET :securityRules`.
pub(crate) fn security_rules(files: &[File]) -> String {
    render(&Json::Object(vec![rules(files)]), false) + "\n"
}

fn coverage_json(files: &[File], report: &[Report<'_>]) -> Json {
    let mut fields = vec![rules(files)];
    if !report.is_empty() {
        fields.push(("report", Json::Array(report.iter().map(node).collect())));
    }
    Json::Object(fields)
}

/// The body of `GET :ruleCoverage`.
pub(crate) fn coverage(files: &[File], report: &[Report<'_>]) -> String {
    render(&coverage_json(files, report), false) + "\n"
}

/// The page of `GET :ruleCoverage.html`: the rules with each expression marked, and what it
/// evaluated to on hover. hidane's own page around the official emulator's data
/// (`docs/parity-exceptions.md`).
pub(crate) fn coverage_html(files: &[File], report: &[Report<'_>]) -> String {
    let data = render(&coverage_json(files, report), true);
    include_str!("coverage.html").replace("/*DATA*/{}", &data)
}

fn node(report: &Report<'_>) -> Json {
    let mut fields = vec![("sourcePosition", position(&report.position))];
    if !report.values.is_empty() {
        let values = report
            .values
            .iter()
            .map(|(outcome, count)| {
                Json::Object(vec![
                    ("value", self::outcome(outcome)),
                    ("count", Json::Raw(count.to_string())),
                ])
            })
            .collect();
        fields.push(("values", Json::Array(values)));
    }
    if !report.children.is_empty() {
        fields.push((
            "children",
            Json::Array(report.children.iter().map(node).collect()),
        ));
    }
    Json::Object(fields)
}

fn position(p: &Position) -> Json {
    let fields = [
        ("line", p.line),
        ("column", p.column),
        ("currentOffset", p.current_offset),
        ("endOffset", p.end_offset),
    ];
    Json::Object(
        fields
            .into_iter()
            .filter(|(_, n)| *n != 0)
            .map(|(k, n)| (k, Json::Raw(n.to_string())))
            .collect(),
    )
}

fn outcome(outcome: &Outcome) -> Json {
    match outcome {
        Outcome::Value(v) => value(v),
        Outcome::Error { position, message } => {
            let mut fields = Vec::new();
            if let Some(p) = position {
                fields.push(("sourcePosition", self::position(p)));
            }
            if !message.is_empty() {
                fields.push(("causeMessage", Json::Str(message.clone())));
            }
            Json::Object(vec![("undefined", Json::Object(fields))])
        }
        Outcome::Skipped => Json::Object(Vec::new()),
    }
}

fn double(f: f64) -> Json {
    if f.is_finite() {
        Json::Raw(java_double(f))
    } else {
        Json::Str(java_double(f))
    }
}

/// A value message: the one field its type sets.
fn value(v: &Observed) -> Json {
    let list = |items: &[Observed]| {
        let mut fields = Vec::new();
        if !items.is_empty() {
            fields.push(("values", Json::Array(items.iter().map(value).collect())));
        }
        Json::Object(fields)
    };
    let (name, inner) = match v {
        Observed::Null => ("nullValue", Json::Raw("null".to_owned())),
        Observed::Bool(b) => ("boolValue", Json::Raw(b.to_string())),
        Observed::Int(i) => ("intValue", Json::Str(i.to_string())),
        Observed::Float(f) => ("floatValue", double(*f)),
        Observed::String(s) => ("stringValue", Json::Str(s.clone())),
        Observed::Bytes(b) => ("bytesValue", Json::Str(STANDARD.encode(b))),
        Observed::Timestamp(t) => (
            "timestampValue",
            Json::Str(crate::rest::timestamp(t.seconds, t.nanos)),
        ),
        Observed::Duration(seconds, nanos) => {
            ("durationValue", Json::Str(duration(*seconds, *nanos)))
        }
        Observed::LatLng(lat, lng) => {
            let mut fields = Vec::new();
            for (name, x) in [("latitude", *lat), ("longitude", *lng)] {
                if x.to_bits() != 0 {
                    fields.push((name, double(x)));
                }
            }
            ("latlngValue", Json::Object(fields))
        }
        Observed::Path(segments) => {
            let mut fields = Vec::new();
            if !segments.is_empty() {
                let segments = segments
                    .iter()
                    .map(|s| {
                        Json::Object(vec![match s {
                            Segment::Simple(name) => ("simple", Json::Str(name.clone())),
                            Segment::Capture => ("capture", Json::Object(Vec::new())),
                            Segment::GlobCapture => ("globCapture", Json::Object(Vec::new())),
                        }])
                    })
                    .collect();
                fields.push(("segments", Json::Array(segments)));
            }
            ("pathValue", Json::Object(fields))
        }
        Observed::Map(entries) => {
            let mut fields = Vec::new();
            if !entries.is_empty() {
                let entries = entries.iter().map(|(k, v)| (k.clone(), value(v))).collect();
                fields.push(("fields", Json::Map(entries)));
            }
            ("mapValue", Json::Object(fields))
        }
        Observed::List(items) => ("listValue", list(items)),
        Observed::Set(items) => ("setValue", list(items)),
        Observed::MapDiff(statuses) => {
            let mut fields = Vec::new();
            if !statuses.is_empty() {
                let statuses = statuses
                    .iter()
                    .map(|(k, s)| (k.clone(), Json::Str(s.name().to_owned())))
                    .collect();
                fields.push(("keyStatuses", Json::Map(statuses)));
            }
            ("mapDiffValue", Json::Object(fields))
        }
        Observed::Constraint(simple) => {
            let mut fields = Vec::new();
            if !simple.is_empty() {
                let simple = simple
                    .iter()
                    .map(|(comparator, v)| {
                        Json::Object(vec![
                            ("comparator", Json::Str(comparator.name().to_owned())),
                            ("value", value(v)),
                        ])
                    })
                    .collect();
                fields.push(("simpleConstraints", Json::Array(simple)));
            }
            ("constraintValue", Json::Object(fields))
        }
    };
    Json::Object(vec![(name, inner)])
}

/// A duration as ProtoJSON prints it: seconds with 0, 3, 6 or 9 fractional digits, then `s`.
fn duration(seconds: i64, nanos: i32) -> String {
    let sign = if seconds < 0 || nanos < 0 { "-" } else { "" };
    let (seconds, nanos) = (seconds.unsigned_abs(), nanos.unsigned_abs());
    let fraction = if nanos == 0 {
        String::new()
    } else if nanos % 1_000_000 == 0 {
        format!(".{:03}", nanos / 1_000_000)
    } else if nanos % 1000 == 0 {
        format!(".{:06}", nanos / 1000)
    } else {
        format!(".{nanos:09}")
    };
    format!("{sign}{seconds}{fraction}s")
}

/// What the official emulator prints for the values of `debug()` calls: each value message
/// in protobuf's text format with a tab after its field's name, then an empty line.
pub(crate) fn debug_text(values: &[Observed]) -> String {
    let mut out = String::new();
    for v in values {
        let mut text = String::new();
        text_value(&mut text, 0, v);
        if let Some(space) = text.find(' ') {
            text.replace_range(space..=space, " \t ");
        }
        out.push_str(&text);
        out.push('\n');
    }
    out
}

/// The field a value message sets, at `indent`.
fn text_value(out: &mut String, indent: usize, v: &Observed) {
    let pad = |out: &mut String, indent: usize| {
        let _ = write!(out, "{:indent$}", "");
    };
    let scalar = |out: &mut String, name: &str, text: &str| {
        pad(out, indent);
        let _ = writeln!(out, "{name}: {text}");
    };
    let open = |out: &mut String, name: &str| {
        pad(out, indent);
        let _ = writeln!(out, "{name} {{");
    };
    let close = |out: &mut String| {
        pad(out, indent);
        out.push_str("}\n");
    };
    let inner = indent + 2;
    match v {
        Observed::Null => scalar(out, "null_value", "NULL_VALUE"),
        Observed::Bool(b) => scalar(out, "bool_value", &b.to_string()),
        Observed::Int(i) => scalar(out, "int_value", &i.to_string()),
        Observed::Float(f) => scalar(out, "float_value", &java_double(*f)),
        Observed::String(s) => scalar(out, "string_value", &quoted(s.as_bytes())),
        Observed::Bytes(b) => scalar(out, "bytes_value", &quoted(b)),
        Observed::Timestamp(t) => {
            open(out, "timestamp_value");
            seconds_nanos(out, inner, t.seconds, t.nanos);
            close(out);
        }
        Observed::Duration(seconds, nanos) => {
            open(out, "duration_value");
            seconds_nanos(out, inner, *seconds, *nanos);
            close(out);
        }
        Observed::LatLng(lat, lng) => {
            open(out, "latlng_value");
            for (name, x) in [("latitude", *lat), ("longitude", *lng)] {
                if x.to_bits() != 0 {
                    let _ = writeln!(out, "{:inner$}{name}: {}", "", java_double(x));
                }
            }
            close(out);
        }
        Observed::Path(segments) => {
            open(out, "path_value");
            for segment in segments {
                let _ = writeln!(out, "{:inner$}segments {{", "");
                let deeper = inner + 2;
                match segment {
                    Segment::Simple(name) => {
                        let _ = writeln!(out, "{:deeper$}simple: {}", "", quoted(name.as_bytes()));
                    }
                    Segment::Capture => {
                        let _ = writeln!(out, "{:deeper$}capture {{\n{:deeper$}}}", "", "");
                    }
                    Segment::GlobCapture => {
                        let _ = writeln!(out, "{:deeper$}glob_capture {{\n{:deeper$}}}", "", "");
                    }
                }
                let _ = writeln!(out, "{:inner$}}}", "");
            }
            close(out);
        }
        Observed::Map(entries) => {
            open(out, "map_value");
            let mut entries: Vec<_> = entries.iter().collect();
            entries.sort_by(|(a, _), (b, _)| a.encode_utf16().cmp(b.encode_utf16()));
            for (key, value) in entries {
                entry(out, inner, "fields", key, |out, at| {
                    let _ = writeln!(out, "{:at$}value {{", "");
                    text_value(out, at + 2, value);
                    let _ = writeln!(out, "{:at$}}}", "");
                });
            }
            close(out);
        }
        Observed::List(items) | Observed::Set(items) => {
            let name = if matches!(v, Observed::Set(_)) {
                "set_value"
            } else {
                "list_value"
            };
            open(out, name);
            for item in items {
                let _ = writeln!(out, "{:inner$}values {{", "");
                text_value(out, inner + 2, item);
                let _ = writeln!(out, "{:inner$}}}", "");
            }
            close(out);
        }
        Observed::MapDiff(statuses) => {
            open(out, "map_diff_value");
            let mut statuses: Vec<_> = statuses.iter().collect();
            statuses.sort_by(|(a, _), (b, _)| a.encode_utf16().cmp(b.encode_utf16()));
            for (key, status) in statuses {
                entry(out, inner, "key_statuses", key, |out, at| {
                    let _ = writeln!(out, "{:at$}value: {}", "", status.name());
                });
            }
            close(out);
        }
        Observed::Constraint(simple) => {
            open(out, "constraint_value");
            for (comparator, value) in simple {
                let deeper = inner + 2;
                let _ = writeln!(out, "{:inner$}simple_constraints {{", "");
                let _ = writeln!(out, "{:deeper$}comparator: {}", "", comparator.name());
                let _ = writeln!(out, "{:deeper$}value {{", "");
                text_value(out, deeper + 2, value);
                let _ = writeln!(out, "{:deeper$}}}", "");
                let _ = writeln!(out, "{:inner$}}}", "");
            }
            close(out);
        }
    }
}

/// A map entry message (`fields { key: … value … }`) at `indent`.
fn entry(
    out: &mut String,
    indent: usize,
    name: &str,
    key: &str,
    value: impl FnOnce(&mut String, usize),
) {
    let inner = indent + 2;
    let _ = writeln!(out, "{:indent$}{name} {{", "");
    let _ = writeln!(out, "{:inner$}key: {}", "", quoted(key.as_bytes()));
    value(out, inner);
    let _ = writeln!(out, "{:indent$}}}", "");
}

fn seconds_nanos(out: &mut String, indent: usize, seconds: i64, nanos: i32) {
    if seconds != 0 {
        let _ = writeln!(out, "{:indent$}seconds: {seconds}", "");
    }
    if nanos != 0 {
        let _ = writeln!(out, "{:indent$}nanos: {nanos}", "");
    }
}

/// Bytes as protobuf's text format quotes them: C escapes, and octal for other bytes outside
/// printable ASCII.
fn quoted(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() + 2);
    out.push('"');
    for &b in bytes {
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
            _ => {
                let _ = write!(out, "\\{b:03o}");
            }
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use hidane_rules::eval::{Comparator, KeyStatus, Timestamp};

    use super::*;

    fn text(v: Observed) -> String {
        debug_text(&[v])
    }

    #[test]
    fn debug_lines_as_the_official_emulator_prints_them() {
        assert_eq!(text(Observed::Null), "null_value: \t NULL_VALUE\n\n");
        assert_eq!(text(Observed::Int(2)), "int_value: \t 2\n\n");
        assert_eq!(text(Observed::Float(1e8)), "float_value: \t 1.0E8\n\n");
        assert_eq!(text(Observed::Float(-0.0)), "float_value: \t -0.0\n\n");
        assert_eq!(
            text(Observed::String("a\"b'é\u{1}".to_owned())),
            "string_value: \t \"a\\\"b\\'\\303\\251\\001\"\n\n"
        );
        assert_eq!(text(Observed::List(Vec::new())), "list_value \t {\n}\n\n");
        assert_eq!(
            text(Observed::Map(vec![
                ("b".to_owned(), Observed::Int(1)),
                ("a".to_owned(), Observed::Bool(true)),
            ])),
            "map_value \t {\n  fields {\n    key: \"a\"\n    value {\n      bool_value: true\n    \
             }\n  }\n  fields {\n    key: \"b\"\n    value {\n      int_value: 1\n    }\n  }\n}\n\n"
        );
        assert_eq!(
            text(Observed::Timestamp(Timestamp {
                seconds: 1,
                nanos: 500_000_000
            })),
            "timestamp_value \t {\n  seconds: 1\n  nanos: 500000000\n}\n\n"
        );
        assert_eq!(
            text(Observed::Path(vec![
                Segment::Simple("c".to_owned()),
                Segment::Capture
            ])),
            "path_value \t {\n  segments {\n    simple: \"c\"\n  }\n  segments {\n    capture {\n    \
             }\n  }\n}\n\n"
        );
        assert_eq!(
            text(Observed::MapDiff(vec![("a".to_owned(), KeyStatus::Added)])),
            "map_diff_value \t {\n  key_statuses {\n    key: \"a\"\n    value: ADDED\n  }\n}\n\n"
        );
        assert_eq!(
            text(Observed::Constraint(vec![(
                Comparator::Gt,
                Observed::Int(1)
            )])),
            "constraint_value \t {\n  simple_constraints {\n    comparator: GT\n    value {\n      \
             int_value: 1\n    }\n  }\n}\n\n"
        );
    }

    #[test]
    fn durations_print_as_protojson() {
        assert_eq!(duration(90, 0), "90s");
        assert_eq!(duration(-1, -500_000_000), "-1.500s");
        assert_eq!(duration(3723, 4), "3723.000000004s");
        assert_eq!(duration(0, 0), "0s");
        assert_eq!(duration(0, -1000), "-0.000001s");
    }

    #[test]
    fn html_escapes_as_gson_does() {
        assert_eq!(
            string("<a>&='\"", true),
            "\"\\u003ca\\u003e\\u0026\\u003d\\u0027\\\"\""
        );
        assert_eq!(string("<a>", false), "\"<a>\"");
    }
}
