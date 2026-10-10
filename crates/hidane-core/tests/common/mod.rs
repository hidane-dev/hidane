//! ProtoJSON → `Value` for the tests (the fixture stores values in their REST form).

use base64::Engine as _;
use hidane_proto::google::{
    firestore::v1::{ArrayValue, MapValue, Value, value::ValueType},
    r#type::LatLng,
};
use prost_types::Timestamp;
use serde_json::Value as Json;

pub fn value(json: &Json) -> Value {
    let (kind, inner) = json
        .as_object()
        .and_then(|o| o.iter().next())
        .expect("a Value object with one field");
    let value_type = match kind.as_str() {
        "nullValue" => ValueType::NullValue(0),
        "booleanValue" => ValueType::BooleanValue(inner.as_bool().unwrap()),
        "integerValue" => ValueType::IntegerValue(match inner {
            Json::String(s) => s.parse().unwrap(),
            other => other.as_i64().unwrap(),
        }),
        "doubleValue" => ValueType::DoubleValue(match inner {
            Json::String(s) if s == "NaN" => f64::NAN,
            Json::String(s) if s == "Infinity" => f64::INFINITY,
            Json::String(s) if s == "-Infinity" => f64::NEG_INFINITY,
            other => other.as_f64().unwrap(),
        }),
        "timestampValue" => {
            let t = chrono::DateTime::parse_from_rfc3339(inner.as_str().unwrap()).unwrap();
            ValueType::TimestampValue(Timestamp {
                seconds: t.timestamp(),
                nanos: i32::try_from(t.timestamp_subsec_nanos()).unwrap(),
            })
        }
        "stringValue" => ValueType::StringValue(inner.as_str().unwrap().to_owned()),
        "bytesValue" => ValueType::BytesValue(
            base64::engine::general_purpose::STANDARD
                .decode(inner.as_str().unwrap())
                .unwrap(),
        ),
        "referenceValue" => ValueType::ReferenceValue(inner.as_str().unwrap().to_owned()),
        "geoPointValue" => ValueType::GeoPointValue(LatLng {
            latitude: inner["latitude"].as_f64().unwrap_or(0.0),
            longitude: inner["longitude"].as_f64().unwrap_or(0.0),
        }),
        "arrayValue" => ValueType::ArrayValue(ArrayValue {
            values: inner["values"]
                .as_array()
                .map(|vs| vs.iter().map(value).collect())
                .unwrap_or_default(),
        }),
        "mapValue" => ValueType::MapValue(MapValue {
            fields: inner["fields"]
                .as_object()
                .map(|fs| fs.iter().map(|(k, v)| (k.clone(), value(v))).collect())
                .unwrap_or_default(),
        }),
        other => panic!("unsupported value kind {other}"),
    };
    Value {
        value_type: Some(value_type),
    }
}
