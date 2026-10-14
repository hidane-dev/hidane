//! How protobuf-java's `JsonFormat` (on Gson) reads ProtoJSON, where it is more lenient than
//! prost-reflect, or less (`tools/oracle/rest.py`, #262). Before prost-reflect reads a request,
//! [`lenient`] rewrites every field that is not a message into the form prost-reflect takes, or
//! refuses the request:
//!
//! - A JSON array of one element stands for that element, at any depth (`{"stringValue":
//!   ["a"]}`, `{"integerValue": [[1]]}`), when the element is a string, a number or a boolean.
//! - A string field takes any of those: a number is its text as written (`1.50`, `1e5`, `-0`;
//!   [`mark_numbers`] keeps it), a boolean `true` or `false`. So does a bytes field, which is
//!   then read as base64.
//! - A boolean field takes `true`, `false`, or the strings `"true"` and `"false"`.
//! - An integer field takes a number or a string that is an integer (`"+1"`, `"01"`), or a
//!   decimal with an exact integer value (`"1e2"`, `1.0`).
//! - A double field takes a number or a decimal string (`"1."`, `".5"`, `"+1"`, `"1e5"`) within
//!   the range of a double (not `"1e400"`), or exactly `"NaN"`, `"Infinity"` or `"-Infinity"`
//!   (not `"nan"` or `"inf"`).
//! - An enum field takes a value's name, or its number (as a number or a string); the null
//!   value (`nullValue`) takes `null`, `"NULL_VALUE"` or any number.

use prost_reflect::{Kind, MessageDescriptor};
use serde_json::Value as Json;

/// Before a number's text in a request, until the field it is given for is known.
const NUMBER: &str = "\u{0}number\u{0}";

/// Replaces each number of `json`, parsed from `text`, with its text as written (after
/// [`NUMBER`]), which a string field keeps; [`lenient`] reads them. Leaves `json` as it is when
/// the numbers cannot be matched with their text (an object with a key twice), or when a string
/// of the request already starts with [`NUMBER`].
pub(crate) fn mark_numbers(text: &[u8], json: &mut Json) {
    fn collect<'a>(json: &'a mut Json, out: &mut Vec<&'a mut Json>, clash: &mut bool) {
        if matches!(json, Json::Number(_)) {
            out.push(json);
            return;
        }
        match json {
            Json::String(s) => *clash |= s.starts_with(NUMBER),
            Json::Array(items) => items.iter_mut().for_each(|item| collect(item, out, clash)),
            Json::Object(map) => {
                for (key, value) in map.iter_mut() {
                    *clash |= key.starts_with(NUMBER);
                    collect(value, out, clash);
                }
            }
            _ => {}
        }
    }
    let literals = number_literals(text);
    let mut numbers = Vec::new();
    let mut clash = false;
    collect(json, &mut numbers, &mut clash);
    if clash
        || numbers.len() != literals.len()
        || numbers.iter().zip(&literals).any(|(number, literal)| {
            serde_json::from_str::<Json>(literal).ok().as_ref() != Some(*number)
        })
    {
        return;
    }
    for (number, literal) in numbers.into_iter().zip(literals) {
        *number = Json::String(format!("{NUMBER}{literal}"));
    }
}

/// The numbers of a JSON text as written, in order.
fn number_literals(text: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < text.len() {
        match text[i] {
            b'"' => {
                i += 1;
                while i < text.len() && text[i] != b'"' {
                    i += if text[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
            }
            b'-' | b'0'..=b'9' => {
                let start = i;
                while i < text.len()
                    && matches!(text[i], b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')
                {
                    i += 1;
                }
                out.push(String::from_utf8_lossy(&text[start..i]).into_owned());
            }
            _ => i += 1,
        }
    }
    out
}

/// Rewrites the fields of `json` that are not messages, in `desc` and the messages in it, as
/// the module says; `None` when one of them is not valid. Numbers [`mark_numbers`] kept as text
/// and not taken by a string field are numbers again.
pub(crate) fn lenient(json: &mut Json, desc: &MessageDescriptor) -> Option<()> {
    message(json, desc)?;
    unmark(json);
    Some(())
}

fn message(json: &mut Json, desc: &MessageDescriptor) -> Option<()> {
    let Json::Object(object) = json else {
        return Some(());
    };
    for (key, value) in object.iter_mut() {
        let Some(field) = desc
            .get_field_by_json_name(key)
            .or_else(|| desc.get_field_by_name(key))
        else {
            continue;
        };
        if field.is_map() {
            let (Kind::Message(entry), Json::Object(entries)) = (field.kind(), value) else {
                continue;
            };
            let kind = entry.map_entry_value_field().kind();
            for entry_value in entries.values_mut() {
                element(entry_value, &kind)?;
            }
        } else if field.is_list() {
            if let Json::Array(items) = value {
                for item in items {
                    element(item, &field.kind())?;
                }
            }
        } else {
            element(value, &field.kind())?;
        }
    }
    Some(())
}

/// A value of a field (or one element of a repeated field) of kind `kind`.
fn element(value: &mut Json, kind: &Kind) -> Option<()> {
    match kind {
        Kind::Message(message_desc) => match message_desc.full_name() {
            // Well-known types written as one JSON primitive.
            "google.protobuf.Int32Value"
            | "google.protobuf.Int64Value"
            | "google.protobuf.UInt32Value"
            | "google.protobuf.UInt64Value"
            | "google.protobuf.DoubleValue"
            | "google.protobuf.FloatValue"
            | "google.protobuf.BoolValue"
            | "google.protobuf.StringValue"
            | "google.protobuf.BytesValue" => {
                unwrap(value);
                scalar(value, &message_desc.get_field_by_name("value")?.kind())
            }
            "google.protobuf.Timestamp" | "google.protobuf.Duration" => {
                unwrap(value);
                Some(())
            }
            name if name.starts_with("google.protobuf.") => Some(()),
            _ => message(value, message_desc),
        },
        kind => {
            unwrap(value);
            scalar(value, kind)
        }
    }
}

/// An array of one string, number or boolean, at any depth, is that value.
fn unwrap(value: &mut Json) {
    if !matches!(value, Json::Array(_)) {
        return;
    }
    let mut inner: &Json = value;
    while let Json::Array(items) = inner {
        match items.as_slice() {
            [only] => inner = only,
            _ => return,
        }
    }
    if matches!(inner, Json::String(_) | Json::Number(_) | Json::Bool(_)) {
        *value = inner.clone();
    }
}

fn scalar(value: &mut Json, kind: &Kind) -> Option<()> {
    let (text, number) = match &*value {
        Json::String(s) => match s.strip_prefix(NUMBER) {
            Some(literal) => (literal.to_owned(), true),
            None => (s.clone(), false),
        },
        Json::Number(n) => (n.to_string(), true),
        Json::Bool(b) => (b.to_string(), false),
        // Null is the default; objects and arrays are prost-reflect's to refuse.
        _ => return Some(()),
    };
    let boolean = matches!(value, Json::Bool(_));
    match kind {
        Kind::String | Kind::Bytes => *value = Json::String(text),
        Kind::Bool => match (&*value, text.as_str()) {
            (Json::Bool(_), _) => {}
            (Json::String(_), "true") if !number => *value = Json::Bool(true),
            (Json::String(_), "false") if !number => *value = Json::Bool(false),
            _ => return None,
        },
        Kind::Double | Kind::Float => {
            if boolean {
                return None;
            }
            let special = !number && matches!(text.as_str(), "NaN" | "Infinity" | "-Infinity");
            // A decimal past the largest double is out of range, not an infinity.
            if !special && (!decimal(&text) || text.parse::<f64>().is_ok_and(f64::is_infinite)) {
                return None;
            }
            *value = Json::String(text);
        }
        Kind::Int32
        | Kind::Int64
        | Kind::Uint32
        | Kind::Uint64
        | Kind::Sint32
        | Kind::Sint64
        | Kind::Fixed32
        | Kind::Fixed64
        | Kind::Sfixed32
        | Kind::Sfixed64 => {
            if boolean {
                return None;
            }
            *value = Json::String(integer(&text)?);
        }
        Kind::Enum(enum_desc) => {
            if boolean {
                return None;
            }
            let as_number = whole_number(&text);
            if enum_desc.full_name() == "google.protobuf.NullValue" {
                match as_number {
                    Some(_) => *value = Json::Null,
                    None if text == "NULL_VALUE" => *value = Json::String(text),
                    None => return None,
                }
            } else if let Some(n) = as_number.and_then(|n| i32::try_from(n).ok()) {
                *value = match enum_desc.get_value(n) {
                    Some(known) => Json::String(known.name().to_owned()),
                    None => Json::from(n),
                };
            } else {
                *value = Json::String(text);
            }
        }
        Kind::Message(_) => {}
    }
    Some(())
}

/// An integer of `Integer.parseInt`'s syntax: an optional sign, then digits.
fn whole_number(text: &str) -> Option<i64> {
    let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.strip_prefix('+').unwrap_or(text).parse().ok()
}

/// A decimal as `java.math.BigDecimal` reads one: an optional sign, digits with an optional
/// point (at least one digit), an optional exponent.
fn decimal(text: &str) -> bool {
    let text = text.strip_prefix(['+', '-']).unwrap_or(text);
    let (mantissa, exponent) = match text.find(['e', 'E']) {
        Some(at) => (&text[..at], Some(&text[at + 1..])),
        None => (text, None),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    (!whole.is_empty() || !fraction.is_empty())
        && digits(whole)
        && digits(fraction)
        && exponent.is_none_or(|e| {
            let e = e.strip_prefix(['+', '-']).unwrap_or(e);
            !e.is_empty() && digits(e)
        })
}

/// An integer as `JsonFormat` reads one: `Long.parseLong`'s syntax (an optional sign, digits),
/// or a decimal with an exact integer value; its canonical text.
fn integer(text: &str) -> Option<String> {
    let unsigned = text.strip_prefix(['+', '-']).unwrap_or(text);
    let negative = text.starts_with('-');
    if !unsigned.is_empty() && unsigned.bytes().all(|b| b.is_ascii_digit()) {
        let digits = unsigned.trim_start_matches('0');
        return Some(match (digits.is_empty(), negative) {
            (true, _) => "0".to_owned(),
            (false, true) => format!("-{digits}"),
            (false, false) => digits.to_owned(),
        });
    }
    if !decimal(text) {
        return None;
    }
    // An exact integer: the digits, moved by the exponent, with nothing but zeros after the
    // point.
    let (mantissa, exponent) = match unsigned.find(['e', 'E']) {
        Some(at) => (&unsigned[..at], unsigned[at + 1..].parse::<i64>().ok()?),
        None => (unsigned, 0),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = format!("{whole}{fraction}");
    let point = i64::try_from(whole.len()).ok()? + exponent;
    let digits_len = i64::try_from(digits.len()).ok()?;
    let (integral, rest) = if point <= 0 {
        (String::new(), digits.clone())
    } else if point >= digits_len {
        // Scientific notation for large values is the integer's business to range-check.
        if point - digits_len > 40 {
            return None;
        }
        let zeros = usize::try_from(point - digits_len).ok()?;
        (format!("{digits}{}", "0".repeat(zeros)), String::new())
    } else {
        let at = usize::try_from(point).ok()?;
        (digits[..at].to_owned(), digits[at..].to_owned())
    };
    if rest.bytes().any(|b| b != b'0') {
        return None;
    }
    let integral = integral.trim_start_matches('0');
    Some(match (integral.is_empty(), negative) {
        (true, _) => "0".to_owned(),
        (false, true) => format!("-{integral}"),
        (false, false) => integral.to_owned(),
    })
}

/// Numbers [`mark_numbers`] kept as text are numbers again.
fn unmark(json: &mut Json) {
    match json {
        Json::String(s) => {
            if let Some(literal) = s.strip_prefix(NUMBER)
                && let Ok(number) = serde_json::from_str::<Json>(literal)
            {
                *json = number;
            }
        }
        Json::Array(items) => items.iter_mut().for_each(unmark),
        Json::Object(map) => map.values_mut().for_each(unmark),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_keep_their_text() {
        let text = br#"{"a": [1.50, "x\"1", -0, 1e5], "b": {"c": 100000000000000000000000}}"#;
        let mut json: Json = serde_json::from_slice(text).unwrap();
        mark_numbers(text, &mut json);
        assert_eq!(json["a"][0], Json::String(format!("{NUMBER}1.50")));
        assert_eq!(json["a"][2], Json::String(format!("{NUMBER}-0")));
        assert_eq!(
            json["b"]["c"],
            Json::String(format!("{NUMBER}100000000000000000000000"))
        );
        unmark(&mut json);
        assert_eq!(json["a"][3], serde_json::json!(1e5));
    }

    #[test]
    fn a_key_twice_keeps_numbers_as_they_are() {
        let text = br#"{"a": "x", "b": 2, "a": 1}"#;
        let mut json: Json = serde_json::from_slice(text).unwrap();
        mark_numbers(text, &mut json);
        assert_eq!(json["a"], serde_json::json!(1));
    }

    #[test]
    fn integers_and_decimals_read_like_json_format() {
        for (text, expected) in [
            ("1", Some("1")),
            ("+1", Some("1")),
            ("01", Some("1")),
            ("-0", Some("0")),
            ("1e2", Some("100")),
            ("1.0", Some("1")),
            ("1.50e1", Some("15")),
            ("1500e-2", Some("15")),
            ("1.5", None),
            (" 1", None),
            ("0x10", None),
            ("", None),
        ] {
            assert_eq!(integer(text).as_deref(), expected, "{text}");
        }
        for text in ["1", "1.", ".5", "+1", "-1e5", "1E+5"] {
            assert!(decimal(text), "{text}");
        }
        for text in ["nan", "inf", " 1", "1d", "0x10", ".", "e5", "1e", "--1"] {
            assert!(!decimal(text), "{text}");
        }
    }
}
