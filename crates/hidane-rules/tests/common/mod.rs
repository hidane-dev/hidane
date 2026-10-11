//! What the evaluator tests share: Firestore REST values and documents.

#![allow(dead_code)]

use hidane_rules::eval::{Document, Documents, Map, Timestamp, Value};
use serde_json::Value as Json;

/// The path of a document of the default database, from `databases`.
pub fn document_path(document: &str) -> Vec<String> {
    ["databases", "(default)", "documents"]
        .into_iter()
        .chain(document.split('/'))
        .map(String::from)
        .collect()
}

/// A Firestore REST value (`{"integerValue": "1"}`), as the server converts it.
pub fn value(j: &Json) -> Value {
    let (kind, v) = j.as_object().unwrap().iter().next().unwrap();
    match kind.as_str() {
        "nullValue" => Value::Null,
        "booleanValue" => Value::Bool(v.as_bool().unwrap()),
        "integerValue" => Value::Int(v.as_str().unwrap().parse().unwrap()),
        "doubleValue" => Value::Float(v.as_f64().unwrap()),
        "stringValue" => Value::String(v.as_str().unwrap().to_owned()),
        "bytesValue" => Value::Bytes(unbase64(v.as_str().unwrap())),
        "timestampValue" => Value::Timestamp(timestamp(v.as_str().unwrap())),
        "geoPointValue" => Value::LatLng(
            v["latitude"].as_f64().unwrap(),
            v["longitude"].as_f64().unwrap(),
        ),
        // `projects/p/databases/(default)/documents/…`, from `databases`.
        "referenceValue" => Value::Path(
            v.as_str()
                .unwrap()
                .split('/')
                .skip(2)
                .map(String::from)
                .collect(),
        ),
        "arrayValue" => Value::List(
            v.get("values")
                .and_then(Json::as_array)
                .map_or_else(Vec::new, |items| items.iter().map(value).collect()),
        ),
        "mapValue" => Value::Map(fields(v.get("fields").unwrap_or(&Json::Null))),
        other => panic!("REST value {other}"),
    }
}

pub fn fields(j: &Json) -> Map {
    j.as_object()
        .map(|o| o.iter().map(|(k, v)| (k.clone(), value(v))).collect())
        .unwrap_or_default()
}

/// `2020-01-02T03:04:05.123Z`.
fn timestamp(s: &str) -> Timestamp {
    let n = |r: std::ops::Range<usize>| s[r].parse::<i64>().unwrap();
    let (y, m, d) = (n(0..4), n(5..7), n(8..10));
    let y2 = y - i64::from(m <= 2);
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    let fraction = s[19..s.len() - 1].trim_start_matches('.');
    Timestamp {
        seconds: days * 86_400 + n(11..13) * 3600 + n(14..16) * 60 + n(17..19),
        nanos: format!("{fraction:0<9}").parse().unwrap(),
    }
}

fn unbase64(s: &str) -> Vec<u8> {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0);
    for c in s.bytes().filter(|c| *c != b'=') {
        acc = acc << 6 | TABLE.iter().position(|x| *x == c).unwrap() as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

/// Stored documents, the same before and after the request.
pub struct Store(pub Vec<Document>);

impl Documents for Store {
    fn get(&self, path: &[String]) -> Option<Document> {
        self.0.iter().find(|d| d.path == path).cloned()
    }

    fn get_after(&self, path: &[String]) -> Option<Document> {
        self.get(path)
    }
}

/// The current time, as a request carries it.
pub fn now() -> Timestamp {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    Timestamp {
        seconds: now.as_secs() as i64,
        nanos: (now.subsec_millis() * 1_000_000) as i32,
    }
}
